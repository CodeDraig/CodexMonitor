//! ACP client filesystem callbacks, constrained to explicitly granted roots.
//!
//! This service intentionally does not own session policy or persistence. The runtime
//! dispatcher validates the connection/session and records the returned write receipt.
//! File mutations are serialized per canonical target, backed up before replacement,
//! and installed with an atomic rename.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub(crate) const ACP_CLIENT_FILE_LIMIT: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug)]
struct AllowedRoot {
    canonical: PathBuf,
}

/// Receipt returned after a successful ACP write. The caller should persist this in the
/// callback operation journal before acknowledging the protocol request.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AcpFileWriteReceipt {
    pub(crate) path: String,
    pub(crate) before_sha256: Option<String>,
    pub(crate) after_sha256: String,
    pub(crate) backup_ref: Option<String>,
    pub(crate) created: bool,
}

/// Filesystem handler for `fs/read_text_file` and `fs/write_text_file`.
///
/// The allowed roots are resolved once at construction. Symlink components below an
/// allowed root are rejected on every access, including the final file component.
pub(crate) struct AcpFileService {
    roots: Vec<AllowedRoot>,
    backup_dir: PathBuf,
    locks: Mutex<BTreeMap<PathBuf, Arc<Mutex<()>>>>,
}

impl AcpFileService {
    pub(crate) fn new(
        workspace: PathBuf,
        additional_roots: Vec<PathBuf>,
        backup_dir: PathBuf,
    ) -> Result<Self, String> {
        let mut roots = Vec::new();
        for root in std::iter::once(workspace).chain(additional_roots) {
            let canonical = root.canonicalize().map_err(|err| {
                format!(
                    "Cannot resolve ACP filesystem root {}: {err}",
                    root.display()
                )
            })?;
            if !canonical.is_dir() {
                return Err(format!(
                    "ACP filesystem root is not a directory: {}",
                    canonical.display()
                ));
            }
            if !roots
                .iter()
                .any(|existing: &AllowedRoot| existing.canonical == canonical)
            {
                roots.push(AllowedRoot { canonical });
            }
        }
        if roots.is_empty() {
            return Err("At least one ACP filesystem root is required".to_string());
        }

        if !backup_dir.is_absolute() {
            return Err("ACP backup directory must be an absolute path".to_string());
        }
        let backup_candidate = canonicalize_future_path(&backup_dir)?;
        if roots
            .iter()
            .any(|root| backup_candidate.starts_with(&root.canonical))
        {
            return Err(
                "ACP backup directory must be outside every permitted filesystem root".to_string(),
            );
        }

        let backup_dir = if backup_dir.exists() {
            backup_dir
                .canonicalize()
                .map_err(|err| format!("Cannot resolve ACP backup directory: {err}"))?
        } else {
            fs::create_dir_all(&backup_dir)
                .map_err(|err| format!("Cannot create ACP backup directory: {err}"))?;
            backup_dir
                .canonicalize()
                .map_err(|err| format!("Cannot resolve ACP backup directory: {err}"))?
        };
        if !backup_dir.is_dir() {
            return Err(format!(
                "ACP backup path is not a directory: {}",
                backup_dir.display()
            ));
        }
        if roots
            .iter()
            .any(|root| backup_dir.starts_with(&root.canonical))
        {
            return Err(
                "ACP backup directory must be outside every permitted filesystem root".to_string(),
            );
        }

        Ok(Self {
            roots,
            backup_dir,
            locks: Mutex::new(BTreeMap::new()),
        })
    }

    /// Handle ACP `fs/read_text_file` parameters and return `{ "content": ... }`.
    pub(crate) async fn read(&self, params: &Value) -> Result<Value, String> {
        let path = required_path(params)?;
        let (root, relative) = self.resolve_allowed(&path)?;
        let file = open_read_no_symlinks(root, &relative)?;
        let metadata = file
            .metadata()
            .map_err(|err| format!("Cannot inspect ACP file: {err}"))?;
        if !metadata.is_file() {
            return Err("ACP filesystem path is not a regular file".to_string());
        }
        if metadata.len() > ACP_CLIENT_FILE_LIMIT {
            return Err(format!(
                "ACP file exceeds the {ACP_CLIENT_FILE_LIMIT} byte limit"
            ));
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take(ACP_CLIENT_FILE_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|err| format!("Cannot read ACP file: {err}"))?;
        if bytes.len() as u64 > ACP_CLIENT_FILE_LIMIT {
            return Err(format!(
                "ACP file exceeds the {ACP_CLIENT_FILE_LIMIT} byte limit"
            ));
        }
        let text =
            String::from_utf8(bytes).map_err(|_| "ACP file is not valid UTF-8".to_string())?;
        let content = select_lines(&text, params)?;
        Ok(json!({"content": content}))
    }

    /// Handle ACP `fs/write_text_file` parameters and return a durable write receipt.
    pub(crate) async fn write(&self, params: &Value) -> Result<Value, String> {
        let path = required_path(params)?;
        let content = params
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| "ACP write_text_file requires string content".to_string())?;
        if content.len() as u64 > ACP_CLIENT_FILE_LIMIT {
            return Err(format!(
                "ACP write exceeds the {ACP_CLIENT_FILE_LIMIT} byte limit"
            ));
        }
        let (root, relative) = self.resolve_allowed(&path)?;
        let target = root.join(&relative);
        let lock = self.path_lock(&target)?;
        let _guard = lock
            .lock()
            .map_err(|_| "ACP file write lock is poisoned".to_string())?;
        let _process_guard = self.cross_process_lock(&target)?;

        let (mut parent, leaf) = open_parent_no_symlinks(root, &relative)?;
        let existing = read_existing_at(&parent, &leaf)?;
        let (before, before_hash, mode) = match existing {
            Some((bytes, mode)) => {
                if bytes.len() as u64 > ACP_CLIENT_FILE_LIMIT {
                    return Err(format!(
                        "Existing ACP file exceeds the {ACP_CLIENT_FILE_LIMIT} byte limit"
                    ));
                }
                let text = String::from_utf8(bytes.clone())
                    .map_err(|_| "Existing ACP file is not valid UTF-8".to_string())?;
                (Some(text), Some(sha256(&bytes)), mode)
            }
            None => (None, None, 0o600),
        };

        let backup_ref = if let Some(old) = before.as_ref() {
            Some(self.backup_preimage(old.as_bytes())?)
        } else {
            None
        };

        let temp_name = unique_component(".acp-write")?;
        let temp_file = create_new_at(&parent, &temp_name, mode)?;
        let mut temp_file = TempFileGuard::new(
            parent
                .try_clone()
                .map_err(|err| format!("Cannot retain ACP parent: {err}"))?,
            temp_name.clone(),
            temp_file,
        );
        temp_file
            .file
            .write_all(content.as_bytes())
            .map_err(|err| format!("Cannot stage ACP write: {err}"))?;
        temp_file
            .file
            .sync_all()
            .map_err(|err| format!("Cannot sync staged ACP write: {err}"))?;

        rename_at(&parent, &temp_name, &leaf)?;
        temp_file.disarm();
        sync_directory(&mut parent)?;

        let receipt = AcpFileWriteReceipt {
            path: target.to_string_lossy().into_owned(),
            before_sha256: before_hash,
            after_sha256: sha256(content.as_bytes()),
            backup_ref,
            created: before.is_none(),
        };
        serde_json::to_value(receipt)
            .map_err(|err| format!("Cannot encode ACP write receipt: {err}"))
    }

    fn resolve_allowed(&self, requested: &Path) -> Result<(&Path, PathBuf), String> {
        if !requested.is_absolute() {
            return Err("ACP filesystem paths must be absolute".to_string());
        }
        let normalized = normalize_absolute(requested)?;
        for root in &self.roots {
            if let Ok(relative) = normalized.strip_prefix(&root.canonical) {
                if relative.as_os_str().is_empty() {
                    return Err(
                        "ACP filesystem path must identify a file below an allowed root"
                            .to_string(),
                    );
                }
                return Ok((&root.canonical, relative.to_path_buf()));
            }
        }
        Err("ACP filesystem path is outside the approved roots".to_string())
    }

    fn path_lock(&self, path: &Path) -> Result<Arc<Mutex<()>>, String> {
        let mut locks = self
            .locks
            .lock()
            .map_err(|_| "ACP file lock table is poisoned".to_string())?;
        Ok(locks
            .entry(path.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone())
    }

    fn cross_process_lock(&self, path: &Path) -> Result<CrossProcessLock, String> {
        let name = format!(
            "{}.lock",
            sha256(path.as_os_str().to_string_lossy().as_bytes())
        );
        let path = self.backup_dir.join(name);
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let file = options
            .open(path)
            .map_err(|err| format!("Cannot open ACP write lock: {err}"))?;
        CrossProcessLock::acquire(file)
    }

    fn backup_preimage(&self, bytes: &[u8]) -> Result<String, String> {
        // The backup directory is outside all allowed roots, and each file is created
        // exclusively. The final sync makes the recovery copy durable before rename.
        let name = format!("{}-{}.bak", timestamp_nanos(), uuid::Uuid::new_v4());
        let backup = self.backup_dir.join(name);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&backup)
            .map_err(|err| format!("Cannot create ACP preimage backup: {err}"))?;
        file.write_all(bytes)
            .map_err(|err| format!("Cannot write ACP preimage backup: {err}"))?;
        file.sync_all()
            .map_err(|err| format!("Cannot sync ACP preimage backup: {err}"))?;
        let dir = fs::File::open(&self.backup_dir)
            .map_err(|err| format!("Cannot open ACP backup directory: {err}"))?;
        dir.sync_all()
            .map_err(|err| format!("Cannot sync ACP backup directory: {err}"))?;
        Ok(backup.to_string_lossy().into_owned())
    }
}

fn required_path(params: &Value) -> Result<PathBuf, String> {
    params
        .get("path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| "ACP filesystem request requires a path".to_string())
}

fn normalize_absolute(path: &Path) -> Result<PathBuf, String> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Prefix(_) => normalized.push(component.as_os_str()),
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err("ACP filesystem paths may not contain '..'".to_string())
            }
        }
    }
    Ok(normalized)
}

fn canonicalize_future_path(path: &Path) -> Result<PathBuf, String> {
    let normalized = normalize_absolute(path)?;
    let mut ancestor = normalized.as_path();
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        let name = ancestor
            .file_name()
            .ok_or_else(|| "Cannot resolve ACP backup directory parent".to_string())?;
        suffix.push(name.to_os_string());
        ancestor = ancestor
            .parent()
            .ok_or_else(|| "Cannot resolve ACP backup directory parent".to_string())?;
    }
    let mut resolved = ancestor
        .canonicalize()
        .map_err(|err| format!("Cannot resolve ACP backup directory parent: {err}"))?;
    for component in suffix.into_iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn select_lines(text: &str, params: &Value) -> Result<String, String> {
    let line = params
        .get("line")
        .map(|v| {
            v.as_u64()
                .ok_or_else(|| "ACP line must be a positive integer".to_string())
        })
        .transpose()?
        .unwrap_or(1);
    if line == 0 {
        return Err("ACP line numbers are one-based".to_string());
    }
    let limit = params
        .get("limit")
        .map(|v| {
            v.as_u64()
                .ok_or_else(|| "ACP line limit must be a positive integer".to_string())
        })
        .transpose()?;
    if limit == Some(0) {
        return Err("ACP line limit must be positive".to_string());
    }
    let mut iter = text.split_inclusive('\n').skip((line - 1) as usize);
    let count = limit.unwrap_or(u64::MAX).min(usize::MAX as u64) as usize;
    Ok(iter.by_ref().take(count).collect())
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn timestamp_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn unique_component(prefix: &str) -> Result<String, String> {
    Ok(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

#[cfg(unix)]
fn open_dir_no_symlinks(path: &Path) -> Result<std::fs::File, String> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };
    let mut current =
        std::fs::File::open("/").map_err(|err| format!("Cannot open filesystem root: {err}"))?;
    for component in path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        let name = std::ffi::CString::new(name.as_bytes())
            .map_err(|_| "Path contains a NUL byte".to_string())?;
        let fd = unsafe {
            libc::openat(
                current.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(format!(
                "Cannot safely open ACP directory component: {}",
                std::io::Error::last_os_error()
            ));
        }
        current = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    Ok(current)
}

#[cfg(not(unix))]
fn open_dir_no_symlinks(path: &Path) -> Result<std::fs::File, String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|err| format!("Cannot inspect ACP directory: {err}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("ACP directory contains a symlink or is not a directory".to_string());
    }
    fs::File::open(path).map_err(|err| format!("Cannot open ACP directory: {err}"))
}

#[cfg(unix)]
fn open_child_dir(parent: &std::fs::File, name: &std::ffi::OsStr) -> Result<std::fs::File, String> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };
    let name = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| "Path contains a NUL byte".to_string())?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(format!(
            "Cannot safely open ACP directory component: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(not(unix))]
fn open_child_dir(
    parent: &std::fs::File,
    _name: &std::ffi::OsStr,
) -> Result<std::fs::File, String> {
    let _ = parent;
    Err("Safe component-relative ACP access is unsupported on this platform".to_string())
}

#[cfg(unix)]
fn open_read_no_symlinks(root: &Path, relative: &Path) -> Result<std::fs::File, String> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };
    let mut parent = open_dir_no_symlinks(root)?;
    let components: Vec<_> = relative.components().collect();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        let Component::Normal(name) = component else {
            return Err("Invalid ACP file path".to_string());
        };
        parent = open_child_dir(&parent, name)?;
    }
    let Some(Component::Normal(name)) = components.last() else {
        return Err("Invalid ACP file path".to_string());
    };
    let name = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| "Path contains a NUL byte".to_string())?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(format!(
            "Cannot safely read ACP file: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(not(unix))]
fn open_read_no_symlinks(root: &Path, relative: &Path) -> Result<std::fs::File, String> {
    let target = root.join(relative);
    reject_symlink_components(root, relative)?;
    fs::File::open(target).map_err(|err| format!("Cannot read ACP file: {err}"))
}

fn reject_symlink_components(root: &Path, relative: &Path) -> Result<(), String> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err("Invalid ACP file path".to_string());
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("ACP filesystem paths may not traverse symlinks".to_string())
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => break,
            Err(err) => return Err(format!("Cannot inspect ACP path: {err}")),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn open_parent_no_symlinks(
    root: &Path,
    relative: &Path,
) -> Result<(std::fs::File, std::ffi::OsString), String> {
    let mut parent = open_dir_no_symlinks(root)?;
    let components: Vec<_> = relative.components().collect();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        let Component::Normal(name) = component else {
            return Err("Invalid ACP file path".to_string());
        };
        parent = open_child_dir(&parent, name)?;
    }
    let Some(Component::Normal(leaf)) = components.last() else {
        return Err("Invalid ACP file path".to_string());
    };
    Ok((parent, leaf.to_os_string()))
}

#[cfg(not(unix))]
fn open_parent_no_symlinks(
    root: &Path,
    relative: &Path,
) -> Result<(std::fs::File, std::ffi::OsString), String> {
    reject_symlink_components(root, relative)?;
    let parent = root.join(relative.parent().unwrap_or_else(|| Path::new("")));
    let leaf = relative
        .file_name()
        .ok_or_else(|| "Invalid ACP file path".to_string())?
        .to_os_string();
    Ok((
        fs::File::open(parent).map_err(|err| format!("Cannot open ACP parent directory: {err}"))?,
        leaf,
    ))
}

#[cfg(unix)]
fn read_existing_at(
    parent: &std::fs::File,
    leaf: &std::ffi::OsStr,
) -> Result<Option<(Vec<u8>, u32)>, String> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };
    let leaf = std::ffi::CString::new(leaf.as_bytes())
        .map_err(|_| "Path contains a NUL byte".to_string())?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            leaf.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let err = std::io::Error::last_os_error();
        return if err.kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(format!("Cannot safely read existing ACP file: {err}"))
        };
    }
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let metadata = file
        .metadata()
        .map_err(|err| format!("Cannot inspect existing ACP file: {err}"))?;
    if !metadata.is_file() {
        return Err("ACP write target is not a regular file".to_string());
    }
    let mut bytes = Vec::new();
    file.take(ACP_CLIENT_FILE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| format!("Cannot read existing ACP file: {err}"))?;
    if bytes.len() as u64 > ACP_CLIENT_FILE_LIMIT {
        return Err(format!(
            "Existing ACP file exceeds the {ACP_CLIENT_FILE_LIMIT} byte limit"
        ));
    }
    use std::os::unix::fs::PermissionsExt;
    Ok(Some((bytes, metadata.permissions().mode() & 0o777)))
}

#[cfg(not(unix))]
fn read_existing_at(
    _parent: &std::fs::File,
    _leaf: &std::ffi::OsStr,
) -> Result<Option<(Vec<u8>, u32)>, String> {
    Err("Safe component-relative ACP writes are unsupported on this platform".to_string())
}

#[cfg(unix)]
fn create_new_at(parent: &std::fs::File, name: &str, mode: u32) -> Result<std::fs::File, String> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = std::ffi::CString::new(name).map_err(|_| "Invalid temporary name".to_string())?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode as libc::mode_t,
        )
    };
    if fd < 0 {
        return Err(format!(
            "Cannot create staged ACP file: {}",
            std::io::Error::last_os_error()
        ));
    }
    if unsafe { libc::fchmod(fd, mode as libc::mode_t) } != 0 {
        let err = std::io::Error::last_os_error();
        unsafe {
            libc::close(fd);
        }
        return Err(format!("Cannot preserve ACP file permissions: {err}"));
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(not(unix))]
fn create_new_at(
    _parent: &std::fs::File,
    _name: &str,
    _mode: u32,
) -> Result<std::fs::File, String> {
    Err("Safe component-relative ACP writes are unsupported on this platform".to_string())
}

#[cfg(unix)]
fn rename_at(parent: &std::fs::File, from: &str, to: &std::ffi::OsStr) -> Result<(), String> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
    let from = std::ffi::CString::new(from).map_err(|_| "Invalid temporary name".to_string())?;
    let to = std::ffi::CString::new(to.as_bytes())
        .map_err(|_| "Path contains a NUL byte".to_string())?;
    let result = unsafe {
        libc::renameat(
            parent.as_raw_fd(),
            from.as_ptr(),
            parent.as_raw_fd(),
            to.as_ptr(),
        )
    };
    if result != 0 {
        return Err(format!(
            "Cannot atomically replace ACP file: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn rename_at(_parent: &std::fs::File, _from: &str, _to: &std::ffi::OsStr) -> Result<(), String> {
    Err("Safe component-relative ACP writes are unsupported on this platform".to_string())
}

#[cfg(unix)]
fn sync_directory(directory: &mut std::fs::File) -> Result<(), String> {
    directory
        .sync_all()
        .map_err(|err| format!("Cannot sync ACP target directory: {err}"))
}

#[cfg(not(unix))]
fn sync_directory(_directory: &mut std::fs::File) -> Result<(), String> {
    Ok(())
}

struct CrossProcessLock {
    #[allow(dead_code)]
    file: std::fs::File,
}

impl CrossProcessLock {
    #[cfg(unix)]
    fn acquire(file: std::fs::File) -> Result<Self, String> {
        use std::os::fd::AsRawFd;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(format!(
                "Cannot acquire ACP write lock: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(Self { file })
    }

    #[cfg(not(unix))]
    fn acquire(file: std::fs::File) -> Result<Self, String> {
        Ok(Self { file })
    }
}

struct TempFileGuard {
    parent: std::fs::File,
    name: String,
    file: std::fs::File,
    armed: bool,
}

impl TempFileGuard {
    fn new(parent: std::fs::File, name: String, file: std::fs::File) -> Self {
        Self {
            parent,
            name,
            file,
            armed: true,
        }
    }
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        #[cfg(unix)]
        {
            use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
            if let Ok(name) = std::ffi::CString::new(self.name.as_bytes()) {
                unsafe {
                    libc::unlinkat(self.parent.as_raw_fd(), name.as_ptr(), 0);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;

    fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(future)
    }

    struct Fixture {
        root: PathBuf,
        backups: PathBuf,
        service: AcpFileService,
    }
    impl Fixture {
        fn new() -> Self {
            let base =
                std::env::temp_dir().join(format!("acp-client-files-{}", uuid::Uuid::new_v4()));
            let root = base.join("workspace");
            let backups = base.join("backups");
            fs::create_dir_all(&root).unwrap();
            let service = AcpFileService::new(root.clone(), Vec::new(), backups.clone()).unwrap();
            Self {
                root,
                backups,
                service,
            }
        }
        fn request(&self, path: &Path) -> Value {
            json!({"path": path.to_string_lossy()})
        }
    }

    #[test]
    fn reads_unicode_ranges_and_preserves_newline_fidelity() {
        block_on(async {
            let fixture = Fixture::new();
            let path = fixture.root.join("notes.txt");
            fs::write(&path, "α\r\nβ\nno-final-newline").unwrap();
            let mut params = fixture.request(&path);
            params["line"] = json!(2);
            params["limit"] = json!(2);
            assert_eq!(
                fixture.service.read(&params).await.unwrap()["content"],
                "β\nno-final-newline"
            );
            params["line"] = json!(1);
            params["limit"] = json!(1);
            assert_eq!(
                fixture.service.read(&params).await.unwrap()["content"],
                "α\r\n"
            );
        });
    }

    #[test]
    fn creates_file_and_overwrite_keeps_recoverable_preimage_and_mode() {
        block_on(async {
            let fixture = Fixture::new();
            let path = fixture.root.join("new.txt");
            let mut params = fixture.request(&path);
            params["content"] = json!("first\n");
            let created: AcpFileWriteReceipt =
                serde_json::from_value(fixture.service.write(&params).await.unwrap()).unwrap();
            assert!(created.created);
            assert_eq!(created.before_sha256, None);
            assert_eq!(fs::read_to_string(&path).unwrap(), "first\n");

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
            }
            params["content"] = json!("second");
            let replaced: AcpFileWriteReceipt =
                serde_json::from_value(fixture.service.write(&params).await.unwrap()).unwrap();
            assert!(!replaced.created);
            assert_eq!(replaced.before_sha256, Some(sha256(b"first\n")));
            assert_eq!(replaced.after_sha256, sha256(b"second"));
            let backup = PathBuf::from(replaced.backup_ref.unwrap());
            assert!(backup.starts_with(&fixture.backups));
            assert_eq!(fs::read(backup).unwrap(), b"first\n");
            assert_eq!(fs::read_to_string(path.clone()).unwrap(), "second");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o640
                );
            }
        });
    }

    #[test]
    fn rejects_outside_paths_parent_segments_invalid_utf8_and_oversize_content() {
        block_on(async {
            let fixture = Fixture::new();
            let outside = fixture.root.parent().unwrap().join("outside.txt");
            fs::write(&outside, "secret").unwrap();
            assert!(fixture
                .service
                .read(&fixture.request(&outside))
                .await
                .unwrap_err()
                .contains("outside"));
            let traversal = fixture.request(&fixture.root.join("../outside.txt"));
            assert!(fixture
                .service
                .read(&traversal)
                .await
                .unwrap_err()
                .contains("'..'"));
            let invalid = fixture.root.join("invalid.txt");
            fs::write(&invalid, [0xff, 0xfe]).unwrap();
            assert!(fixture
                .service
                .read(&fixture.request(&invalid))
                .await
                .unwrap_err()
                .contains("UTF-8"));
            let large = fixture.root.join("large.txt");
            fs::write(&large, vec![b'x'; ACP_CLIENT_FILE_LIMIT as usize + 1]).unwrap();
            assert!(fixture
                .service
                .read(&fixture.request(&large))
                .await
                .unwrap_err()
                .contains("byte limit"));
            let mut write = fixture.request(&fixture.root.join("large-write.txt"));
            write["content"] = json!("x".repeat(ACP_CLIENT_FILE_LIMIT as usize + 1));
            assert!(fixture
                .service
                .write(&write)
                .await
                .unwrap_err()
                .contains("byte limit"));
        });
    }

    #[test]
    fn backup_directory_inside_workspace_is_rejected_before_creation() {
        let base = std::env::temp_dir().join(format!("acp-client-files-{}", uuid::Uuid::new_v4()));
        let root = base.join("workspace");
        let backup = root.join("backups");
        fs::create_dir_all(&root).unwrap();
        assert!(AcpFileService::new(root, Vec::new(), backup.clone()).is_err());
        assert!(!backup.exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_components_and_symlink_targets() {
        block_on(async {
            use std::os::unix::fs::symlink;
            let fixture = Fixture::new();
            let outside = fixture.root.parent().unwrap().join("outside-dir");
            fs::create_dir_all(&outside).unwrap();
            fs::write(outside.join("target.txt"), "outside").unwrap();
            symlink(&outside, fixture.root.join("escape-dir")).unwrap();
            assert!(fixture
                .service
                .read(&fixture.request(&fixture.root.join("escape-dir/target.txt")))
                .await
                .is_err());
            symlink(
                outside.join("target.txt"),
                fixture.root.join("escape-file.txt"),
            )
            .unwrap();
            let mut write = fixture.request(&fixture.root.join("escape-file.txt"));
            write["content"] = json!("overwritten");
            assert!(fixture.service.write(&write).await.is_err());
            assert_eq!(
                fs::read_to_string(outside.join("target.txt")).unwrap(),
                "outside"
            );
        });
    }

    #[test]
    fn repeated_same_path_writes_leave_one_complete_atomic_value() {
        let fixture = Fixture::new();
        let path = fixture.root.join("race.txt");
        let root = fixture.root.clone();
        let backup_dir = fixture.backups.clone();
        let service_a = std::sync::Arc::new(fixture.service);
        let service_b =
            std::sync::Arc::new(AcpFileService::new(root, Vec::new(), backup_dir).unwrap());
        let start = std::sync::Arc::new(std::sync::Barrier::new(3));
        let run_write = |service: Arc<AcpFileService>,
                         value: char,
                         start: Arc<std::sync::Barrier>| {
            let path = path.clone();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap();
                let params = json!({"path": path, "content": value.to_string().repeat(128_000)});
                start.wait();
                runtime.block_on(service.write(&params)).unwrap();
            })
        };
        let a = run_write(service_a, 'A', start.clone());
        let b = run_write(service_b, 'B', start.clone());
        start.wait();
        a.join().unwrap();
        b.join().unwrap();
        let result = fs::read_to_string(&path).unwrap();
        assert!(result == "A".repeat(128_000) || result == "B".repeat(128_000));
    }
}

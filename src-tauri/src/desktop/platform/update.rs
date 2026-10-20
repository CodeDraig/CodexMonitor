//! Signed native package staging. This module never finds or changes the active
//! application installation; callers provide an explicit versioned stage root.

use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

const MAX_PACKAGE_BYTES: usize = 2 * 1024 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 50_000;
const MAX_UNPACKED_BYTES: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateManifest {
    pub version: String,
    pub target: String,
    pub architecture: String,
    pub format: String,
    pub package_url: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateConfiguration {
    pub metadata_url: String,
    pub verifying_key_base64: String,
    pub target: String,
    pub architecture: String,
    pub staging_root: PathBuf,
}

impl UpdateConfiguration {
    pub(crate) fn build_service(&self) -> Result<UpdateService, UpdateError> {
        let key_bytes = base64::engine::general_purpose::STANDARD
            .decode(&self.verifying_key_base64)
            .map_err(|error| {
                UpdateError::Configuration(format!("invalid public key encoding: {error}"))
            })?;
        let trusted_public_key: [u8; 32] = key_bytes.try_into().map_err(|_| {
            UpdateError::Configuration(
                "Ed25519 verifying key must decode to exactly 32 bytes".into(),
            )
        })?;
        UpdateService::new(
            self.metadata_url.clone(),
            trusted_public_key,
            self.target.clone(),
            self.architecture.clone(),
            self.staging_root.clone(),
        )
    }
}

/// An update manifest whose exact payload bytes were verified by the configured key.
#[derive(Clone, Debug)]
pub(crate) struct VerifiedManifest(UpdateManifest);

impl VerifiedManifest {
    pub(crate) fn manifest(&self) -> &UpdateManifest {
        &self.0
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SignedEnvelope {
    payload_base64: String,
    signature_base64: String,
}

#[derive(Clone, Debug)]
pub(crate) struct VerifiedPackage {
    pub manifest: UpdateManifest,
    path: PathBuf,
}

impl VerifiedPackage {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstalledPackage {
    pub version: String,
    pub package_root: PathBuf,
    pub executable: PathBuf,
    pub rollback_version: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum UpdateError {
    #[error("update service configuration is invalid: {0}")]
    Configuration(String),
    #[error("update request failed: {0}")]
    Network(String),
    #[error("signed update metadata is invalid: {0}")]
    Metadata(String),
    #[error("the update targets {actual}, but this application is {expected}")]
    WrongTarget { actual: String, expected: String },
    #[error("the update package hash does not match its signed manifest")]
    HashMismatch,
    #[error("package staging failed: {0}")]
    Staging(String),
}

#[derive(Clone)]
pub(crate) struct UpdateService {
    metadata_url: String,
    signing_key: VerifyingKey,
    target: String,
    architecture: String,
    staging_root: PathBuf,
    client: reqwest::Client,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActiveVersion {
    version: String,
    package_root: PathBuf,
    executable: PathBuf,
}

impl UpdateService {
    /// `staging_root` must be a dedicated test or release-staging directory.
    /// It is never inferred from the current executable or desktop launcher.
    pub(crate) fn new(
        metadata_url: String,
        trusted_public_key: [u8; 32],
        target: String,
        architecture: String,
        staging_root: PathBuf,
    ) -> Result<Self, UpdateError> {
        let key = VerifyingKey::from_bytes(&trusted_public_key)
            .map_err(|error| UpdateError::Configuration(error.to_string()))?;
        let parsed = reqwest::Url::parse(&metadata_url)
            .map_err(|error| UpdateError::Configuration(error.to_string()))?;
        if !allowed_update_url(&parsed) {
            return Err(UpdateError::Configuration(
                "metadata URL must use HTTPS or loopback HTTP for isolated tests".into(),
            ));
        }
        if !matches!(target.as_str(), "linux" | "darwin" | "windows") {
            return Err(UpdateError::Configuration(format!(
                "unsupported update target {target:?}"
            )));
        }
        if !matches!(architecture.as_str(), "x86_64" | "aarch64") {
            return Err(UpdateError::Configuration(format!(
                "unsupported update architecture {architecture:?}"
            )));
        }
        Ok(Self {
            metadata_url,
            signing_key: key,
            target,
            architecture,
            staging_root,
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(15 * 60))
                .build()
                .map_err(|error| UpdateError::Configuration(error.to_string()))?,
        })
    }

    pub(crate) async fn check(
        &self,
        current_version: &str,
    ) -> Result<Option<VerifiedManifest>, UpdateError> {
        let response = self
            .client
            .get(&self.metadata_url)
            .send()
            .await
            .map_err(|error| UpdateError::Network(error.to_string()))?
            .error_for_status()
            .map_err(|error| UpdateError::Network(error.to_string()))?;
        let envelope: SignedEnvelope = response
            .json()
            .await
            .map_err(|error| UpdateError::Metadata(error.to_string()))?;
        let manifest = verify_envelope(envelope, &self.signing_key)?;
        self.validate_manifest(&manifest)?;
        let ordering = compare_versions(&manifest.version, current_version)?;
        if ordering.is_lt() {
            return Err(UpdateError::Metadata(format!(
                "refusing downgrade from {current_version} to {}",
                manifest.version
            )));
        }
        Ok(ordering.is_gt().then_some(VerifiedManifest(manifest)))
    }

    pub(crate) async fn download_verified(
        &self,
        verified_manifest: &VerifiedManifest,
    ) -> Result<VerifiedPackage, UpdateError> {
        use futures_util::StreamExt;
        use tokio::io::AsyncWriteExt;
        let manifest = &verified_manifest.0;
        self.validate_manifest(manifest)?;
        let response = self
            .client
            .get(&manifest.package_url)
            .send()
            .await
            .map_err(|error| UpdateError::Network(error.to_string()))?
            .error_for_status()
            .map_err(|error| UpdateError::Network(error.to_string()))?;
        std::fs::create_dir_all(self.staging_root.join("downloads"))
            .map_err(|error| UpdateError::Staging(error.to_string()))?;
        let path = self.staging_root.join("downloads").join(format!(
            "{}-{}.{}",
            manifest.version,
            uuid::Uuid::new_v4(),
            manifest.format
        ));
        let temporary = path.with_extension(format!("{}.partial", manifest.format));
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await
            .map_err(|error| UpdateError::Staging(error.to_string()))?;
        let mut stream = response.bytes_stream();
        let mut hasher = Sha256::new();
        let mut downloaded = 0usize;
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    let _ = tokio::fs::remove_file(&temporary).await;
                    return Err(UpdateError::Network(error.to_string()));
                }
            };
            downloaded = downloaded.saturating_add(chunk.len());
            if downloaded > MAX_PACKAGE_BYTES {
                drop(file);
                let _ = tokio::fs::remove_file(&temporary).await;
                return Err(UpdateError::Metadata(
                    "update package exceeds the 2 GiB limit".into(),
                ));
            }
            if let Err(error) = file.write_all(&chunk).await {
                drop(file);
                let _ = tokio::fs::remove_file(&temporary).await;
                return Err(UpdateError::Staging(error.to_string()));
            }
            hasher.update(&chunk);
        }
        file.flush()
            .await
            .map_err(|error| UpdateError::Staging(error.to_string()))?;
        file.sync_all()
            .await
            .map_err(|error| UpdateError::Staging(error.to_string()))?;
        drop(file);
        if !format!("{:x}", hasher.finalize()).eq_ignore_ascii_case(&manifest.sha256) {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(UpdateError::HashMismatch);
        }
        tokio::fs::rename(&temporary, &path)
            .await
            .map_err(|error| UpdateError::Staging(error.to_string()))?;
        Ok(VerifiedPackage {
            manifest: manifest.clone(),
            path,
        })
    }

    /// Install into this service's explicit versioned staging tree. Nothing
    /// switches the system launcher or overwrites an existing version.
    pub(crate) fn install(
        &self,
        package: VerifiedPackage,
    ) -> Result<InstalledPackage, UpdateError> {
        self.validate_manifest(&package.manifest)?;
        let active_path = self.staging_root.join("active.json");
        let prior = read_active(&active_path)?;
        if let Some(prior) = prior.as_ref() {
            if !compare_versions(&package.manifest.version, &prior.version)?.is_gt() {
                return Err(UpdateError::Staging(format!(
                    "refusing to replace active version {} with non-newer version {}",
                    prior.version, package.manifest.version
                )));
            }
        }
        let bytes = std::fs::read(&package.path)
            .map_err(|error| UpdateError::Staging(error.to_string()))?;
        if !format!("{:x}", Sha256::digest(&bytes)).eq_ignore_ascii_case(&package.manifest.sha256) {
            return Err(UpdateError::HashMismatch);
        }
        let version_dir = self
            .staging_root
            .join("versions")
            .join(safe_component(&package.manifest.version)?);
        std::fs::create_dir_all(&version_dir)
            .map_err(|error| UpdateError::Staging(error.to_string()))?;
        let package_root = version_dir.join("package");
        if package_root.exists() {
            return Err(UpdateError::Staging(format!(
                "refusing to replace existing staged package {}",
                package_root.display()
            )));
        }
        let extracted = version_dir.join(format!("extract-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&extracted).map_err(|error| UpdateError::Staging(error.to_string()))?;
        let staged = extracted.join("package");
        fs::create_dir(&staged).map_err(|error| UpdateError::Staging(error.to_string()))?;
        let extraction = extract_archive(&package.manifest.format, &bytes, &staged)
            .and_then(|_| validate_native_package(&package.manifest, &staged));
        if let Err(error) = extraction {
            let _ = fs::remove_dir_all(&extracted);
            return Err(error);
        }
        fs::rename(&staged, &package_root).map_err(|error| {
            let _ = fs::remove_dir_all(&extracted);
            UpdateError::Staging(format!("could not publish staged package: {error}"))
        })?;
        let _ = fs::remove_dir_all(&extracted);
        let internal = read_native_package(&package_root)?;
        let executable = package_root.join(internal.launcher);
        if let Some(ref prior) = prior {
            atomic_write(
                &self.staging_root.join("rollback.json"),
                &serde_json::to_vec(prior)
                    .map_err(|error| UpdateError::Staging(error.to_string()))?,
            )?;
        }
        let active = ActiveVersion {
            version: package.manifest.version.clone(),
            package_root: package_root.clone(),
            executable: executable.clone(),
        };
        atomic_write(
            &active_path,
            &serde_json::to_vec(&active)
                .map_err(|error| UpdateError::Staging(error.to_string()))?,
        )?;
        Ok(InstalledPackage {
            version: active.version,
            package_root,
            executable,
            rollback_version: prior.map(|value| value.version),
        })
    }

    /// Restore the previous staged version pointer. Version files remain intact.
    pub(crate) fn rollback(&self) -> Result<InstalledPackage, UpdateError> {
        let rollback_path = self.staging_root.join("rollback.json");
        let prior = read_active(&rollback_path)?
            .ok_or_else(|| UpdateError::Staging("no staged rollback version exists".into()))?;
        let versions_root = fs::canonicalize(self.staging_root.join("versions"))
            .map_err(|error| UpdateError::Staging(error.to_string()))?;
        let package_root = fs::canonicalize(&prior.package_root)
            .map_err(|error| UpdateError::Staging(error.to_string()))?;
        let executable = fs::canonicalize(&prior.executable)
            .map_err(|error| UpdateError::Staging(error.to_string()))?;
        if !package_root.starts_with(&versions_root) || !executable.starts_with(&package_root) {
            return Err(UpdateError::Staging(
                "rollback pointer escapes the versioned staging tree".into(),
            ));
        }
        if !package_root.is_dir() || !executable.is_file() {
            return Err(UpdateError::Staging(
                "rollback package or launcher is missing".into(),
            ));
        }
        let current = read_active(&self.staging_root.join("active.json"))?;
        let rollback_version = current.as_ref().map(|version| version.version.clone());
        atomic_write(
            &self.staging_root.join("active.json"),
            &serde_json::to_vec(&prior).map_err(|error| UpdateError::Staging(error.to_string()))?,
        )?;
        if let Some(current) = current.as_ref() {
            atomic_write(
                &rollback_path,
                &serde_json::to_vec(&current)
                    .map_err(|error| UpdateError::Staging(error.to_string()))?,
            )?;
        }
        Ok(InstalledPackage {
            version: prior.version,
            package_root: prior.package_root,
            executable: prior.executable,
            rollback_version,
        })
    }

    fn validate_manifest(&self, manifest: &UpdateManifest) -> Result<(), UpdateError> {
        if manifest.target != self.target {
            return Err(UpdateError::WrongTarget {
                actual: manifest.target.clone(),
                expected: self.target.clone(),
            });
        }
        if manifest.architecture != self.architecture {
            return Err(UpdateError::WrongTarget {
                actual: format!("{}-{}", manifest.target, manifest.architecture),
                expected: format!("{}-{}", self.target, self.architecture),
            });
        }
        let expected_format = if self.target == "linux" {
            "tar.gz"
        } else {
            "zip"
        };
        if manifest.format != expected_format {
            return Err(UpdateError::Metadata(format!(
                "archive format {} does not match {} target packages (expected {expected_format})",
                manifest.format, manifest.target
            )));
        }
        safe_component(&manifest.version)?;
        if manifest.sha256.len() != 64 || !manifest.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(UpdateError::Metadata(
                "sha256 must contain 64 hexadecimal characters".into(),
            ));
        }
        let package_url = reqwest::Url::parse(&manifest.package_url)
            .map_err(|error| UpdateError::Metadata(format!("invalid package URL: {error}")))?;
        if !allowed_update_url(&package_url) {
            return Err(UpdateError::Metadata(
                "package URL must use HTTPS or loopback HTTP for isolated tests".into(),
            ));
        }
        Ok(())
    }
}

fn verify_envelope(
    envelope: SignedEnvelope,
    key: &VerifyingKey,
) -> Result<UpdateManifest, UpdateError> {
    let payload = base64::engine::general_purpose::STANDARD
        .decode(envelope.payload_base64)
        .map_err(|error| UpdateError::Metadata(format!("payload encoding: {error}")))?;
    let signature_bytes = base64::engine::general_purpose::STANDARD
        .decode(envelope.signature_base64)
        .map_err(|error| UpdateError::Metadata(format!("signature encoding: {error}")))?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|error| UpdateError::Metadata(format!("signature length: {error}")))?;
    key.verify(&payload, &signature)
        .map_err(|_| UpdateError::Metadata("signature verification failed".into()))?;
    serde_json::from_slice(&payload)
        .map_err(|error| UpdateError::Metadata(format!("manifest payload: {error}")))
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
struct NativePackageIndex {
    version: String,
    target: String,
    architecture: String,
    format: String,
    launcher: String,
    files: Vec<NativePackageFile>,
}

#[derive(Deserialize)]
struct NativePackageFile {
    path: String,
    size: u64,
    sha256: String,
}

fn read_native_package(root: &Path) -> Result<NativePackageIndex, UpdateError> {
    let bytes = fs::read(root.join("native-package.json")).map_err(|error| {
        UpdateError::Staging(format!("native package index is missing: {error}"))
    })?;
    serde_json::from_slice(&bytes)
        .map_err(|error| UpdateError::Staging(format!("native package index is invalid: {error}")))
}

fn validate_native_package(manifest: &UpdateManifest, root: &Path) -> Result<(), UpdateError> {
    let index = read_native_package(root)?;
    if index.version != manifest.version
        || index.target != manifest.target
        || index.architecture != manifest.architecture
        || index.format != manifest.format
    {
        return Err(UpdateError::Staging(
            "archive index does not match its signed update manifest".into(),
        ));
    }
    let launcher = archive_relative_path(&index.launcher)?;
    if !is_executable_package_file(&root.join(&launcher), manifest.target.as_str())? {
        return Err(UpdateError::Staging(format!(
            "packaged launcher is missing: {}",
            launcher.display()
        )));
    }
    let binaries: &[&str] = match manifest.target.as_str() {
        "linux" => &[
            "bin/codex_monitor_native",
            "bin/codex_monitor_daemon",
            "bin/codex_monitor_daemonctl",
        ],
        "darwin" => &[
            "Codex Monitor.app/Contents/MacOS/codex_monitor_native",
            "Codex Monitor.app/Contents/MacOS/codex_monitor_daemon",
            "Codex Monitor.app/Contents/MacOS/codex_monitor_daemonctl",
        ],
        "windows" => &[
            "bin/codex_monitor_native.exe",
            "bin/codex_monitor_daemon.exe",
            "bin/codex_monitor_daemonctl.exe",
        ],
        _ => return Err(UpdateError::Metadata("unsupported update target".into())),
    };
    for binary in binaries {
        if !is_executable_package_file(&root.join(binary), manifest.target.as_str())? {
            return Err(UpdateError::Staging(format!(
                "complete native package is missing {binary}"
            )));
        }
    }

    let mut expected = std::collections::BTreeSet::new();
    for item in index.files {
        let relative = archive_relative_path(&item.path)?;
        if relative == Path::new("native-package.json") || !expected.insert(relative.clone()) {
            return Err(UpdateError::Staging(format!(
                "duplicate or reserved package path {}",
                item.path
            )));
        }
        let path = root.join(&relative);
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            UpdateError::Staging(format!("package file {}: {error}", item.path))
        })?;
        if !metadata.file_type().is_file() || metadata.len() != item.size {
            return Err(UpdateError::Staging(format!(
                "package file size or type mismatch: {}",
                item.path
            )));
        }
        let bytes = fs::read(&path).map_err(|error| UpdateError::Staging(error.to_string()))?;
        if format!("{:x}", Sha256::digest(&bytes)) != item.sha256.to_ascii_lowercase() {
            return Err(UpdateError::Staging(format!(
                "package file hash mismatch: {}",
                item.path
            )));
        }
    }
    let mut actual = std::collections::BTreeSet::new();
    collect_package_files(root, root, &mut actual)?;
    actual.remove(Path::new("native-package.json"));
    if actual != expected {
        return Err(UpdateError::Staging(
            "archive files do not match the packaged inventory".into(),
        ));
    }
    Ok(())
}

fn is_executable_package_file(path: &Path, target: &str) -> Result<bool, UpdateError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(UpdateError::Staging(error.to_string())),
    };
    if !metadata.is_file() {
        return Ok(false);
    }
    #[cfg(unix)]
    if target != "windows" {
        use std::os::unix::fs::PermissionsExt;
        return Ok(metadata.permissions().mode() & 0o111 != 0);
    }
    let _ = target;
    Ok(true)
}

fn collect_package_files(
    root: &Path,
    directory: &Path,
    files: &mut std::collections::BTreeSet<PathBuf>,
) -> Result<(), UpdateError> {
    for entry in fs::read_dir(directory).map_err(|error| UpdateError::Staging(error.to_string()))? {
        let entry = entry.map_err(|error| UpdateError::Staging(error.to_string()))?;
        let path = entry.path();
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| UpdateError::Staging(error.to_string()))?;
        if metadata.file_type().is_symlink() {
            return Err(UpdateError::Staging(
                "package contains a symbolic link".into(),
            ));
        }
        if metadata.is_dir() {
            collect_package_files(root, &path, files)?;
        } else if metadata.is_file() {
            let relative = path.strip_prefix(root).map_err(|error| {
                UpdateError::Staging(format!("package path escaped staging root: {error}"))
            })?;
            files.insert(relative.to_path_buf());
        } else {
            return Err(UpdateError::Staging(
                "package contains a special file".into(),
            ));
        }
    }
    Ok(())
}

fn extract_archive(format: &str, bytes: &[u8], destination: &Path) -> Result<(), UpdateError> {
    if bytes.len() > MAX_PACKAGE_BYTES {
        return Err(UpdateError::Metadata(
            "update package exceeds the 2 GiB limit".into(),
        ));
    }
    let mut entries_seen = 0usize;
    let mut unpacked_bytes = 0u64;
    let mut archive_root: Option<String> = None;
    match format {
        "tar.gz" => {
            let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
            let mut archive = tar::Archive::new(decoder);
            let entries = archive
                .entries()
                .map_err(|error| UpdateError::Staging(format!("invalid tar archive: {error}")))?;
            for entry in entries {
                let mut entry = entry.map_err(|error| UpdateError::Staging(error.to_string()))?;
                entries_seen += 1;
                if entries_seen > MAX_ARCHIVE_ENTRIES {
                    return Err(UpdateError::Staging("archive has too many entries".into()));
                }
                let entry_type = entry.header().entry_type();
                if entry_type.is_symlink() || entry_type.is_hard_link() {
                    return Err(UpdateError::Staging("archive links are not allowed".into()));
                }
                if !entry_type.is_file() && !entry_type.is_dir() {
                    return Err(UpdateError::Staging(
                        "archive contains a special file".into(),
                    ));
                }
                if entry_type.is_file() {
                    unpacked_bytes = unpacked_bytes.saturating_add(entry.size());
                    if unpacked_bytes > MAX_UNPACKED_BYTES {
                        return Err(UpdateError::Staging(
                            "archive expands beyond the 4 GiB limit".into(),
                        ));
                    }
                }
                let path = entry
                    .path()
                    .map_err(|error| UpdateError::Staging(error.to_string()))?
                    .into_owned();
                let (root_name, relative) = split_archive_path(&path, &mut archive_root)?;
                let _ = root_name;
                let Some(relative) = relative else { continue };
                let output = destination.join(relative);
                if entry_type.is_dir() {
                    fs::create_dir_all(output)
                        .map_err(|error| UpdateError::Staging(error.to_string()))?;
                } else {
                    write_archive_file(&output, &mut entry)?;
                    let mode = entry.header().mode().unwrap_or(0o644);
                    set_archive_mode(&output, mode)?;
                }
            }
        }
        "zip" => {
            let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
                .map_err(|error| UpdateError::Staging(format!("invalid zip archive: {error}")))?;
            if archive.len() > MAX_ARCHIVE_ENTRIES {
                return Err(UpdateError::Staging("archive has too many entries".into()));
            }
            for index in 0..archive.len() {
                let mut entry = archive
                    .by_index(index)
                    .map_err(|error| UpdateError::Staging(error.to_string()))?;
                entries_seen += 1;
                let mode = entry
                    .unix_mode()
                    .unwrap_or(if entry.is_dir() { 0o755 } else { 0o644 });
                if !entry.is_dir() {
                    unpacked_bytes = unpacked_bytes.saturating_add(entry.size());
                    if unpacked_bytes > MAX_UNPACKED_BYTES {
                        return Err(UpdateError::Staging(
                            "archive expands beyond the 4 GiB limit".into(),
                        ));
                    }
                }
                let file_type = mode & 0o170000;
                if file_type == 0o120000 {
                    return Err(UpdateError::Staging("archive links are not allowed".into()));
                }
                if file_type != 0 && file_type != 0o100000 && file_type != 0o040000 {
                    return Err(UpdateError::Staging(
                        "archive contains a special file".into(),
                    ));
                }
                let raw = entry.name().to_string();
                let path = PathBuf::from(raw);
                let (root_name, relative) = split_archive_path(&path, &mut archive_root)?;
                let _ = root_name;
                let Some(relative) = relative else { continue };
                let output = destination.join(relative);
                if entry.is_dir() {
                    fs::create_dir_all(output)
                        .map_err(|error| UpdateError::Staging(error.to_string()))?;
                } else {
                    write_archive_file(&output, &mut entry)?;
                    set_archive_mode(&output, mode)?;
                }
            }
        }
        _ => {
            return Err(UpdateError::Metadata(format!(
                "unsupported archive format {format}"
            )))
        }
    }
    if entries_seen == 0 || archive_root.is_none() {
        return Err(UpdateError::Staging("archive is empty".into()));
    }
    Ok(())
}

fn split_archive_path(
    path: &Path,
    root: &mut Option<String>,
) -> Result<(String, Option<PathBuf>), UpdateError> {
    let text = path
        .to_str()
        .ok_or_else(|| UpdateError::Staging("archive path is not UTF-8".into()))?;
    if text.contains('\\') || path.is_absolute() {
        return Err(UpdateError::Staging(format!(
            "unsafe archive path {text:?}"
        )));
    }
    let mut parts = path.components();
    let first = match parts.next() {
        Some(std::path::Component::Normal(value)) => value.to_string_lossy().into_owned(),
        _ => {
            return Err(UpdateError::Staging(format!(
                "unsafe archive path {text:?}"
            )))
        }
    };
    for part in parts.clone() {
        if !matches!(part, std::path::Component::Normal(_)) {
            return Err(UpdateError::Staging(format!(
                "unsafe archive path {text:?}"
            )));
        }
    }
    if let Some(expected) = root {
        if expected != &first {
            return Err(UpdateError::Staging(
                "archive has multiple root directories".into(),
            ));
        }
    } else {
        *root = Some(first.clone());
    }
    let relative = parts.collect::<PathBuf>();
    Ok((
        first,
        (!relative.as_os_str().is_empty()).then_some(relative),
    ))
}

fn archive_relative_path(path: &str) -> Result<PathBuf, UpdateError> {
    if path.is_empty() || path.contains('\\') {
        return Err(UpdateError::Staging(format!(
            "unsafe package path {path:?}"
        )));
    }
    let parsed = Path::new(path);
    if parsed.is_absolute()
        || parsed
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(UpdateError::Staging(format!(
            "unsafe package path {path:?}"
        )));
    }
    Ok(parsed.to_path_buf())
}

fn write_archive_file(path: &Path, reader: &mut impl Read) -> Result<(), UpdateError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| UpdateError::Staging(error.to_string()))?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| UpdateError::Staging(error.to_string()))?;
    std::io::copy(reader, &mut file).map_err(|error| UpdateError::Staging(error.to_string()))?;
    file.sync_all()
        .map_err(|error| UpdateError::Staging(error.to_string()))
}

#[cfg(unix)]
fn set_archive_mode(path: &Path, mode: u32) -> Result<(), UpdateError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777))
        .map_err(|error| UpdateError::Staging(error.to_string()))
}

#[cfg(not(unix))]
fn set_archive_mode(_path: &Path, _mode: u32) -> Result<(), UpdateError> {
    Ok(())
}

fn safe_component(value: &str) -> Result<&str, UpdateError> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
    {
        return Err(UpdateError::Metadata(format!(
            "unsafe package path component {value:?}"
        )));
    }
    Ok(value)
}

/// Compare plain numeric dotted versions without a dependency on a particular
/// packaging ecosystem. Both versions must use one to four numeric components;
/// ambiguous prerelease strings are rejected instead of being ordered wrongly.
fn compare_versions(left: &str, right: &str) -> Result<std::cmp::Ordering, UpdateError> {
    fn parse(value: &str) -> Result<Vec<u64>, UpdateError> {
        let pieces = value.split('.').collect::<Vec<_>>();
        if pieces.is_empty() || pieces.len() > 4 {
            return Err(UpdateError::Metadata(format!(
                "version must contain one to four numeric components: {value}"
            )));
        }
        pieces
            .into_iter()
            .map(|piece| {
                if piece.is_empty() || !piece.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(UpdateError::Metadata(format!(
                        "version must contain numeric components: {value}"
                    )));
                }
                piece.parse::<u64>().map_err(|_| {
                    UpdateError::Metadata(format!("version component is too large: {value}"))
                })
            })
            .collect()
    }
    let left = parse(left)?;
    let right = parse(right)?;
    let length = left.len().max(right.len());
    for index in 0..length {
        let order = left
            .get(index)
            .copied()
            .unwrap_or(0)
            .cmp(&right.get(index).copied().unwrap_or(0));
        if !order.is_eq() {
            return Ok(order);
        }
    }
    Ok(std::cmp::Ordering::Equal)
}

fn allowed_update_url(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        || (url.scheme() == "http"
            && url.host_str().map_or(false, |host| {
                host == "127.0.0.1" || host == "::1" || host == "localhost"
            }))
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), UpdateError> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| UpdateError::Staging(error.to_string()))?;
    file.write_all(bytes)
        .map_err(|error| UpdateError::Staging(error.to_string()))?;
    file.sync_all()
        .map_err(|error| UpdateError::Staging(error.to_string()))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), UpdateError> {
    let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    write_new_file(&temp, bytes)?;
    match std::fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(error) if path.exists() => {
            let backup = path.with_extension(format!("backup-{}.json", uuid::Uuid::new_v4()));
            std::fs::rename(path, &backup).map_err(|e| {
                UpdateError::Staging(format!("could not preserve prior file after {error}: {e}"))
            })?;
            if let Err(error) = std::fs::rename(&temp, path) {
                let _ = std::fs::rename(backup, path);
                let _ = std::fs::remove_file(&temp);
                return Err(UpdateError::Staging(error.to_string()));
            }
            Ok(())
        }
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            Err(UpdateError::Staging(error.to_string()))
        }
    }
}

fn read_active(path: &Path) -> Result<Option<ActiveVersion>, UpdateError> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| UpdateError::Staging(error.to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(UpdateError::Staging(error.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn staging_install_preserves_and_rolls_back_previous_package() {
        let root = std::env::temp_dir().join(format!("cm-update-stage-{}", uuid::Uuid::new_v4()));
        let key = test_public_key();
        let service = UpdateService::new(
            "http://127.0.0.1/unused.json".into(),
            key,
            test_target().into(),
            test_arch().into(),
            root.clone(),
        )
        .unwrap();
        let first = package(&service, "1.0.0", b"first executable");
        service.install(first).unwrap();
        let second = package(&service, "2.0.0", b"second executable");
        let installed = service.install(second).unwrap();
        assert_eq!(installed.rollback_version.as_deref(), Some("1.0.0"));
        assert!(installed.executable.is_file());
        assert!(root
            .join("versions/2.0.0/package")
            .join(test_daemon_binary())
            .is_file());
        let rollback = service.rollback().unwrap();
        assert_eq!(rollback.version, "1.0.0");
        assert!(rollback.executable.is_file());
        assert!(rollback.package_root.join("native-package.json").is_file());
        assert!(root
            .join("versions/2.0.0/package/native-package.json")
            .is_file());
        let tampered = package(&service, "3.0.0", b"original bytes");
        std::fs::write(tampered.path(), b"modified bytes").unwrap();
        assert!(matches!(
            service.install(tampered),
            Err(UpdateError::HashMismatch)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manifest_checks_target_and_path_components_before_staging() {
        let root = std::env::temp_dir().join(format!("cm-update-target-{}", uuid::Uuid::new_v4()));
        let service = UpdateService::new(
            "http://127.0.0.1/unused.json".into(),
            test_public_key(),
            test_target().into(),
            test_arch().into(),
            root,
        )
        .unwrap();
        let other_target = if test_target() == "windows" {
            "linux"
        } else {
            "windows"
        };
        let mut manifest = manifest("1.0.0", other_target, "x86_64", "zip", b"content");
        assert!(matches!(
            service.validate_manifest(&manifest),
            Err(UpdateError::WrongTarget { .. })
        ));
        manifest.target = test_target().into();
        manifest.architecture = "wrong-arch".into();
        assert!(matches!(
            service.validate_manifest(&manifest),
            Err(UpdateError::WrongTarget { .. })
        ));
    }

    fn package(service: &UpdateService, version: &str, bytes: &[u8]) -> VerifiedPackage {
        let archive = fixture_archive(version, bytes);
        let manifest = manifest(version, test_target(), test_arch(), test_format(), &archive);
        let path = service
            .staging_root
            .join("downloads")
            .join(format!("{version}.{}", test_format()));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, archive).unwrap();
        VerifiedPackage { manifest, path }
    }

    fn manifest(
        version: &str,
        target: &str,
        architecture: &str,
        format: &str,
        bytes: &[u8],
    ) -> UpdateManifest {
        UpdateManifest {
            version: version.into(),
            target: target.into(),
            architecture: architecture.into(),
            format: format.into(),
            package_url: "https://updates.example.test/unused".into(),
            sha256: format!("{:x}", Sha256::digest(bytes)),
        }
    }

    fn test_public_key() -> [u8; 32] {
        SigningKey::from_bytes(&[7u8; 32])
            .verifying_key()
            .to_bytes()
    }

    fn test_target() -> &'static str {
        match std::env::consts::OS {
            "linux" => "linux",
            "macos" => "darwin",
            "windows" => "windows",
            _ => "unknown",
        }
    }

    fn test_arch() -> &'static str {
        match std::env::consts::ARCH {
            "x86_64" => "x86_64",
            "aarch64" => "aarch64",
            _ => "other",
        }
    }

    fn test_format() -> &'static str {
        if test_target() == "linux" {
            "tar.gz"
        } else {
            "zip"
        }
    }

    fn test_daemon_binary() -> &'static str {
        match test_target() {
            "linux" => "bin/codex_monitor_daemon",
            "darwin" => "Codex Monitor.app/Contents/MacOS/codex_monitor_daemon",
            _ => "bin/codex_monitor_daemon.exe",
        }
    }

    fn fixture_archive(version: &str, native_content: &[u8]) -> Vec<u8> {
        let root = std::env::temp_dir().join(format!("cm-update-fixture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let (files, launcher) = match test_target() {
            "linux" => (
                vec![
                    ("bin/codex_monitor_native", native_content.to_vec()),
                    ("bin/codex_monitor_daemon", b"daemon".to_vec()),
                    ("bin/codex_monitor_daemonctl", b"ctl".to_vec()),
                    ("codex-monitor-native", b"#!/bin/sh\n".to_vec()),
                    ("share/codex-monitor/icon.png", b"icon".to_vec()),
                ],
                "codex-monitor-native",
            ),
            "darwin" => (
                vec![
                    (
                        "Codex Monitor.app/Contents/MacOS/codex_monitor_native",
                        native_content.to_vec(),
                    ),
                    (
                        "Codex Monitor.app/Contents/MacOS/codex_monitor_daemon",
                        b"daemon".to_vec(),
                    ),
                    (
                        "Codex Monitor.app/Contents/MacOS/codex_monitor_daemonctl",
                        b"ctl".to_vec(),
                    ),
                    (
                        "Codex Monitor.app/Contents/Resources/icon.png",
                        b"icon".to_vec(),
                    ),
                ],
                "Codex Monitor.app/Contents/MacOS/codex_monitor_native",
            ),
            _ => (
                vec![
                    ("bin/codex_monitor_native.exe", native_content.to_vec()),
                    ("bin/codex_monitor_daemon.exe", b"daemon".to_vec()),
                    ("bin/codex_monitor_daemonctl.exe", b"ctl".to_vec()),
                    ("share/codex-monitor/icon.png", b"icon".to_vec()),
                ],
                "bin/codex_monitor_native.exe",
            ),
        };
        for (name, bytes) in &files {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
            if name.contains("codex_monitor_native")
                || name.contains("codex_monitor_daemon")
                || name.contains("codex_monitor_daemonctl")
                || name == &"codex-monitor-native"
            {
                set_fixture_executable(&root.join(name));
            }
        }
        let inventory = files
            .iter()
            .map(|(name, bytes)| {
                serde_json::json!({
                    "path": name,
                    "size": bytes.len(),
                    "sha256": format!("{:x}", Sha256::digest(bytes)),
                })
            })
            .collect::<Vec<_>>();
        std::fs::write(
            root.join("native-package.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": version,
                "target": test_target(),
                "architecture": test_arch(),
                "format": test_format(),
                "launcher": launcher,
                "files": inventory,
            }))
            .unwrap(),
        )
        .unwrap();
        let archive = if test_format() == "tar.gz" {
            let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            builder.append_dir_all("CodexMonitor", &root).unwrap();
            builder.into_inner().unwrap().finish().unwrap()
        } else {
            let cursor = Cursor::new(Vec::new());
            let mut writer = zip::ZipWriter::new(cursor);
            for entry in walk_files(&root) {
                let relative = entry.strip_prefix(&root).unwrap().to_string_lossy();
                let mode = if relative.contains("codex_monitor_native")
                    || relative.contains("codex_monitor_daemon")
                    || relative.contains("codex_monitor_daemonctl")
                    || relative == "codex-monitor-native"
                {
                    0o755
                } else {
                    0o644
                };
                let options = zip::write::SimpleFileOptions::default().unix_permissions(mode);
                writer
                    .start_file(format!("CodexMonitor/{relative}"), options)
                    .unwrap();
                writer.write_all(&std::fs::read(entry).unwrap()).unwrap();
            }
            writer.finish().unwrap().into_inner()
        };
        let _ = std::fs::remove_dir_all(root);
        archive
    }

    fn walk_files(root: &Path) -> Vec<PathBuf> {
        fn walk(path: &Path, output: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, output)
                } else {
                    output.push(path)
                }
            }
        }
        let mut output = Vec::new();
        walk(root, &mut output);
        output
    }

    #[cfg(unix)]
    fn set_fixture_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(not(unix))]
    fn set_fixture_executable(_path: &Path) {}

    #[test]
    fn version_comparison_rejects_downgrades_and_ambiguous_versions() {
        assert!(compare_versions("0.8", "0.7.99").unwrap().is_gt());
        assert!(compare_versions("0.7.68", "0.7.68.0").unwrap().is_eq());
        assert!(compare_versions("1.0.0-rc1", "0.9.0").is_err());
    }

    #[test]
    fn archive_paths_reject_parent_escape_and_mixed_roots() {
        let mut root = None;
        assert!(split_archive_path(Path::new("CodexMonitor/../escape"), &mut root).is_err());
        root = None;
        split_archive_path(Path::new("CodexMonitor/bin/app"), &mut root).unwrap();
        assert!(split_archive_path(Path::new("Other/bin/app"), &mut root).is_err());
        assert!(archive_relative_path("../escape").is_err());
        assert!(archive_relative_path("/absolute/file").is_err());
    }

    #[test]
    fn zip_extraction_rejects_traversal_and_symlinks() {
        let temp = std::env::temp_dir().join(format!("cm-update-zip-{}", uuid::Uuid::new_v4()));
        let destination = temp.join("out");
        std::fs::create_dir_all(&destination).unwrap();
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                "CodexMonitor/../escape",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        writer.write_all(b"escape").unwrap();
        let archive = writer.finish().unwrap().into_inner();
        assert!(extract_archive("zip", &archive, &destination).is_err());
        assert!(!temp.join("escape").exists());

        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                "CodexMonitor/link",
                zip::write::SimpleFileOptions::default().unix_permissions(0o777),
            )
            .unwrap();
        writer.write_all(b"target").unwrap();
        let mut archive = writer.finish().unwrap().into_inner();
        assert!(patch_zip_unix_mode(
            &mut archive,
            b"CodexMonitor/link",
            0o120777
        ));
        let mut reader = zip::ZipArchive::new(Cursor::new(&archive)).unwrap();
        assert_eq!(
            reader.by_name("CodexMonitor/link").unwrap().unix_mode(),
            Some(0o120777)
        );
        assert!(extract_archive("zip", &archive, &destination).is_err());
        std::fs::remove_dir_all(temp).unwrap();
    }

    /// `SimpleFileOptions::unix_permissions` deliberately masks file-type
    /// bits, so create a regular entry then encode a genuine Unix symlink
    /// type in the ZIP central directory as a real archive producer would.
    fn patch_zip_unix_mode(archive: &mut [u8], name: &[u8], mode: u32) -> bool {
        const EOCD: &[u8; 4] = b"PK\x05\x06";
        const CENTRAL: &[u8; 4] = b"PK\x01\x02";
        let Some(eocd) = archive
            .windows(EOCD.len())
            .rposition(|window| window == EOCD)
        else {
            return false;
        };
        if eocd + 20 > archive.len() {
            return false;
        }
        let Ok(central_offset_bytes) = archive[eocd + 16..eocd + 20].try_into() else {
            return false;
        };
        let central_offset = u32::from_le_bytes(central_offset_bytes) as usize;
        let mut offset = central_offset;
        while offset + 46 <= archive.len() && &archive[offset..offset + 4] == CENTRAL {
            let Ok(filename_len_bytes) = archive[offset + 28..offset + 30].try_into() else {
                return false;
            };
            let Ok(extra_len_bytes) = archive[offset + 30..offset + 32].try_into() else {
                return false;
            };
            let Ok(comment_len_bytes) = archive[offset + 32..offset + 34].try_into() else {
                return false;
            };
            let filename_len = u16::from_le_bytes(filename_len_bytes) as usize;
            let extra_len = u16::from_le_bytes(extra_len_bytes) as usize;
            let comment_len = u16::from_le_bytes(comment_len_bytes) as usize;
            let end = offset + 46 + filename_len + extra_len + comment_len;
            if end > archive.len() {
                return false;
            }
            if &archive[offset + 46..offset + 46 + filename_len] == name {
                // ZIP "version made by" identifies Unix as host system 3.
                archive[offset + 5] = 3;
                archive[offset + 38..offset + 42].copy_from_slice(&(mode << 16).to_le_bytes());
                return true;
            }
            offset = end;
        }
        false
    }

    #[test]
    fn signed_manifest_requires_the_exact_signed_payload() {
        let signer = SigningKey::from_bytes(&[7u8; 32]);
        let body = serde_json::to_vec(&manifest(
            "1.0.0",
            test_target(),
            test_arch(),
            test_format(),
            b"pkg",
        ))
        .unwrap();
        let signature = signer.sign(&body);
        let envelope = SignedEnvelope {
            payload_base64: base64::engine::general_purpose::STANDARD.encode(&body),
            signature_base64: base64::engine::general_purpose::STANDARD
                .encode(signature.to_bytes()),
        };
        assert_eq!(
            verify_envelope(envelope, &signer.verifying_key())
                .unwrap()
                .version,
            "1.0.0"
        );

        let mut changed = body;
        changed[0] ^= 1;
        let envelope = SignedEnvelope {
            payload_base64: base64::engine::general_purpose::STANDARD.encode(changed),
            signature_base64: base64::engine::general_purpose::STANDARD
                .encode(signature.to_bytes()),
        };
        assert!(matches!(
            verify_envelope(envelope, &signer.verifying_key()),
            Err(UpdateError::Metadata(_))
        ));
    }
}

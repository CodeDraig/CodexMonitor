use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};

use crate::utils::normalize_git_path;

fn should_skip_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | "node_modules" | "dist" | "target" | "release-artifacts"
    )
}

pub(crate) fn list_workspace_files_inner(root: &PathBuf, max_files: usize) -> Vec<String> {
    let mut results = Vec::new();
    let walker = WalkBuilder::new(root)
        // Allow hidden entries.
        .hidden(false)
        // Avoid crawling symlink targets.
        .follow_links(false)
        // Don't require git to be present to apply to apply git-related ignore rules.
        .require_git(false)
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                let name = entry.file_name().to_string_lossy();
                return !should_skip_dir(&name);
            }
            true
        })
        .build();

    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        if let Ok(rel_path) = entry.path().strip_prefix(root) {
            let normalized = normalize_git_path(&rel_path.to_string_lossy());
            if !normalized.is_empty() {
                results.push(normalized);
            }
        }
        if results.len() >= max_files {
            break;
        }
    }

    results.sort();
    results
}

const MAX_WORKSPACE_FILE_BYTES: u64 = 400_000;
const MAX_WORKSPACE_BINARY_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct WorkspaceFileResponse {
    content: String,
    truncated: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceFileBinaryResponse {
    pub(crate) content_base64: String,
    pub(crate) mime_type: String,
    pub(crate) byte_length: usize,
}

fn resolve_workspace_file(root: &Path, relative_path: &str) -> Result<PathBuf, String> {
    let canonical_root = root
        .canonicalize()
        .map_err(|err| format!("Failed to resolve workspace root: {err}"))?;
    let candidate = canonical_root.join(relative_path);
    let canonical_path = candidate
        .canonicalize()
        .map_err(|err| format!("Failed to open file: {err}"))?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err("Invalid file path".to_string());
    }
    Ok(canonical_path)
}

fn workspace_file_mime_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" | "oga" => "audio/ogg",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "txt" | "md" | "rs" | "py" | "toml" | "yaml" | "yml" | "xml" | "csv" => "text/plain",
        _ => "application/octet-stream",
    }
}

pub(crate) fn read_workspace_file_inner(
    root: &PathBuf,
    relative_path: &str,
) -> Result<WorkspaceFileResponse, String> {
    let canonical_path = resolve_workspace_file(root, relative_path)?;
    let metadata = std::fs::metadata(&canonical_path)
        .map_err(|err| format!("Failed to read file metadata: {err}"))?;
    if !metadata.is_file() {
        return Err("Path is not a file".to_string());
    }

    let file = File::open(&canonical_path).map_err(|err| format!("Failed to open file: {err}"))?;
    let mut buffer = Vec::new();
    file.take(MAX_WORKSPACE_FILE_BYTES + 1)
        .read_to_end(&mut buffer)
        .map_err(|err| format!("Failed to read file: {err}"))?;

    let truncated = buffer.len() > MAX_WORKSPACE_FILE_BYTES as usize;
    if truncated {
        buffer.truncate(MAX_WORKSPACE_FILE_BYTES as usize);
    }

    let content = String::from_utf8(buffer).map_err(|_| "File is not valid UTF-8".to_string())?;
    Ok(WorkspaceFileResponse { content, truncated })
}

/// Read arbitrary workspace bytes for native media/resource rendering.
///
/// The read is constrained to the canonical workspace root and rejects files above the
/// fixed limit rather than returning a partial binary object.
pub(crate) fn read_workspace_file_binary_inner(
    root: &PathBuf,
    relative_path: &str,
) -> Result<WorkspaceFileBinaryResponse, String> {
    let canonical_path = resolve_workspace_file(root, relative_path)?;
    let metadata = std::fs::metadata(&canonical_path)
        .map_err(|err| format!("Failed to read file metadata: {err}"))?;
    if !metadata.is_file() {
        return Err("Path is not a file".to_string());
    }
    if metadata.len() > MAX_WORKSPACE_BINARY_BYTES {
        return Err(format!(
            "File exceeds the {MAX_WORKSPACE_BINARY_BYTES} byte binary-read limit"
        ));
    }
    let file = File::open(&canonical_path).map_err(|err| format!("Failed to open file: {err}"))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_WORKSPACE_BINARY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| format!("Failed to read file: {err}"))?;
    if bytes.len() as u64 > MAX_WORKSPACE_BINARY_BYTES {
        return Err(format!(
            "File exceeds the {MAX_WORKSPACE_BINARY_BYTES} byte binary-read limit"
        ));
    }
    Ok(WorkspaceFileBinaryResponse {
        byte_length: bytes.len(),
        content_base64: BASE64.encode(bytes),
        mime_type: workspace_file_mime_type(&canonical_path).to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        let root = std::env::temp_dir().join(format!("workspace-files-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn listing_keeps_hidden_files_and_respects_ignores_and_limits() {
        let root = root();
        std::fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
        for name in ["a.txt", "b.txt", ".hidden", "ignored.txt"] {
            std::fs::write(root.join(name), "text").unwrap();
        }
        std::fs::create_dir(root.join("node_modules")).unwrap();
        std::fs::write(root.join("node_modules/dependency.txt"), "text").unwrap();
        assert_eq!(
            list_workspace_files_inner(&root, 100),
            [".gitignore", ".hidden", "a.txt", "b.txt"]
        );
        assert_eq!(list_workspace_files_inner(&root, 2).len(), 2);
    }

    #[test]
    fn reads_text_with_the_same_truncation_and_error_contract() {
        let root = root();
        std::fs::write(root.join("text"), "hello").unwrap();
        let response = read_workspace_file_inner(&root, "text").unwrap();
        assert_eq!(response.content, "hello");
        assert!(!response.truncated);
        std::fs::write(
            root.join("large"),
            vec![b'x'; MAX_WORKSPACE_FILE_BYTES as usize + 1],
        )
        .unwrap();
        let response = read_workspace_file_inner(&root, "large").unwrap();
        assert_eq!(response.content.len(), MAX_WORKSPACE_FILE_BYTES as usize);
        assert!(response.truncated);
        std::fs::write(root.join("binary"), [255]).unwrap();
        assert_eq!(
            read_workspace_file_inner(&root, "binary").err().as_deref(),
            Some("File is not valid UTF-8")
        );
        assert_eq!(
            read_workspace_file_inner(&root, ".").err().as_deref(),
            Some("Path is not a file")
        );
        assert_eq!(
            read_workspace_file_inner(&root, "..").err().as_deref(),
            Some("Invalid file path")
        );
    }

    #[test]
    fn reads_binary_with_base64_mime_and_a_hard_size_limit() {
        use base64::Engine as _;

        let root = root();
        let bytes = [0_u8, 1, 2, 127, 128, 254, 255];
        std::fs::write(root.join("sample.PNG"), bytes).unwrap();
        let response = read_workspace_file_binary_inner(&root, "sample.PNG").unwrap();
        assert_eq!(response.mime_type, "image/png");
        assert_eq!(response.byte_length, bytes.len());
        assert_eq!(BASE64.decode(response.content_base64).unwrap(), bytes);

        std::fs::write(
            root.join("too-large.bin"),
            vec![b'x'; MAX_WORKSPACE_BINARY_BYTES as usize + 1],
        )
        .unwrap();
        assert!(read_workspace_file_binary_inner(&root, "too-large.bin")
            .unwrap_err()
            .contains("binary-read limit"));

        let absolute = root.join("sample.PNG");
        assert_eq!(
            read_workspace_file_binary_inner(&root, absolute.to_str().unwrap())
                .unwrap()
                .byte_length,
            bytes.len()
        );
        let outside_dir =
            std::env::temp_dir().join(format!("workspace-files-outside-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&outside_dir).unwrap();
        let outside = outside_dir.join("outside.png");
        std::fs::write(&outside, bytes).unwrap();
        assert_eq!(
            read_workspace_file_binary_inner(&root, outside.to_str().unwrap())
                .err()
                .as_deref(),
            Some("Invalid file path")
        );
        std::fs::remove_dir_all(outside_dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_that_escape_the_workspace() {
        let root = root();
        let workspace = root.join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(root.join("outside"), "private").unwrap();
        std::os::unix::fs::symlink(root.join("outside"), workspace.join("link")).unwrap();
        assert_eq!(
            read_workspace_file_inner(&workspace, "link")
                .err()
                .as_deref(),
            Some("Invalid file path")
        );
        assert_eq!(
            read_workspace_file_binary_inner(&workspace, "link")
                .err()
                .as_deref(),
            Some("Invalid file path")
        );
    }
}

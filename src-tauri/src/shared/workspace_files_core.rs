use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

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

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct WorkspaceFileResponse {
    content: String,
    truncated: bool,
}

pub(crate) fn read_workspace_file_inner(
    root: &PathBuf,
    relative_path: &str,
) -> Result<WorkspaceFileResponse, String> {
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
    }
}

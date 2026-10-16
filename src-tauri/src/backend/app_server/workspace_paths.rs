use std::collections::HashMap;

pub(super) fn normalize_root_path(value: &str) -> String {
    let normalized = value.replace('\\', "/");
    let normalized = normalized.trim_end_matches('/');
    if normalized.is_empty() {
        return String::new();
    }
    let lower = normalized.to_ascii_lowercase();
    let normalized = if lower.starts_with("//?/unc/") {
        format!("//{}", &normalized[8..])
    } else if lower.starts_with("//?/") || lower.starts_with("//./") {
        normalized[4..].to_string()
    } else {
        normalized.to_string()
    };
    if normalized.is_empty() {
        return String::new();
    }

    let bytes = normalized.as_bytes();
    let is_drive_path =
        bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/';
    if is_drive_path || normalized.starts_with("//") {
        normalized.to_ascii_lowercase()
    } else {
        normalized.to_string()
    }
}

pub(super) fn resolve_workspace_for_cwd(
    cwd: &str,
    workspace_roots: &HashMap<String, String>,
) -> Option<String> {
    let normalized_cwd = normalize_root_path(cwd);
    if normalized_cwd.is_empty() {
        return None;
    }
    workspace_roots
        .iter()
        .filter_map(|(workspace_id, root)| {
            if root.is_empty() {
                return None;
            }
            let is_exact_match = root == &normalized_cwd;
            let is_nested_match = normalized_cwd.len() > root.len()
                && normalized_cwd.starts_with(root)
                && normalized_cwd.as_bytes().get(root.len()) == Some(&b'/');
            if is_exact_match || is_nested_match {
                Some((workspace_id, root.len()))
            } else {
                None
            }
        })
        .max_by_key(|(_, root_len)| *root_len)
        .map(|(workspace_id, _)| workspace_id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_workspace_for_cwd_normalizes_windows_paths() {
        let mut roots = HashMap::new();
        roots.insert("ws-1".to_string(), normalize_root_path("C:\\Dev\\Codex"));
        assert_eq!(
            resolve_workspace_for_cwd("c:/dev/codex", &roots),
            Some("ws-1".to_string())
        );
    }

    #[test]
    fn resolve_workspace_for_cwd_normalizes_windows_namespace_paths() {
        let mut roots = HashMap::new();
        roots.insert("ws-1".to_string(), normalize_root_path("C:\\Dev\\Codex"));
        assert_eq!(
            resolve_workspace_for_cwd("\\\\?\\C:\\Dev\\Codex", &roots),
            Some("ws-1".to_string())
        );
    }

    #[test]
    fn normalize_root_path_normalizes_windows_namespace_unc_paths() {
        assert_eq!(
            normalize_root_path("\\\\?\\UNC\\SERVER\\Share\\Repo\\"),
            "//server/share/repo"
        );
    }

    #[test]
    fn resolve_workspace_for_cwd_matches_nested_paths() {
        let mut roots = HashMap::new();
        roots.insert("ws-1".to_string(), normalize_root_path("/tmp/codex"));
        assert_eq!(
            resolve_workspace_for_cwd("/tmp/codex/subdir/project", &roots),
            Some("ws-1".to_string())
        );
    }

    #[test]
    fn resolve_workspace_for_cwd_prefers_longest_matching_root() {
        let mut roots = HashMap::new();
        roots.insert("ws-parent".to_string(), normalize_root_path("/tmp/codex"));
        roots.insert(
            "ws-child".to_string(),
            normalize_root_path("/tmp/codex/subdir"),
        );
        assert_eq!(
            resolve_workspace_for_cwd("/tmp/codex/subdir/project", &roots),
            Some("ws-child".to_string())
        );
    }
}

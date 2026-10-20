use super::*;

impl NativeApp {
    pub(super) fn prepare_open_in(&mut self, workspace_id: &str, path: &str) {
        match self
            .controller
            .state
            .workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
        {
            Some(workspace) => self.open_in_path = Some(resolve_path(&workspace.path, path)),
            None => self
                .notices
                .push("The original workspace is no longer available on this backend.".into()),
        }
    }

    pub(super) fn show_open_in(&mut self, ctx: &egui::Context) {
        let Some(path) = self.open_in_path.clone() else {
            return;
        };
        let mut open = true;
        let mut chosen = None;
        egui::Window::new("Open in local application")
            .open(&mut open).collapsible(false).resizable(true).show(ctx, |ui| {
                ui.label(path.to_string_lossy());
                if matches!(self.settings.backend_mode, BackendMode::Remote) {
                    ui.weak("The configured application runs on this desktop. Its arguments must support the displayed remote path.");
                }
                for target in &self.settings.open_app_targets {
                    let default = target.id == self.settings.selected_open_app_id;
                    if ui.button(format!("{}{}", target.label, if default { " (default)" } else { "" })).clicked() {
                        chosen = Some(target.clone());
                    }
                }
                if self.settings.open_app_targets.is_empty() {
                    ui.label("Add an application target in Settings.");
                }
            });
        if let Some(target) = chosen {
            self.host(HostAction::OpenInTarget { target, path }, None);
            self.open_in_path = None;
        } else if !open {
            self.open_in_path = None;
        }
    }
}

fn resolve_path(workspace: &str, path: &str) -> PathBuf {
    let bytes = path.as_bytes();
    let windows_absolute = bytes.len() >= 3 && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/');
    if PathBuf::from(path).is_absolute() || windows_absolute || path.starts_with("\\\\") {
        return path.into();
    }
    if path.is_empty() {
        return workspace.into();
    }
    if workspace.contains('\\') {
        format!(
            "{}\\{}",
            workspace.trim_end_matches('\\'),
            path.replace('/', "\\")
        )
        .into()
    } else {
        PathBuf::from(workspace).join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_backend_path_is_preserved_for_local_and_remote_application_targets() {
        assert_eq!(
            resolve_path("/remote/repo", "src/a b.rs"),
            PathBuf::from("/remote/repo/src/a b.rs")
        );
        assert_eq!(
            resolve_path("C:\\repo", "src/a b.rs"),
            PathBuf::from("C:\\repo\\src\\a b.rs")
        );
        assert_eq!(
            resolve_path("/other", "C:\\repo\\a.rs"),
            PathBuf::from("C:\\repo\\a.rs")
        );
        assert_eq!(
            resolve_path("/remote/repo", ""),
            PathBuf::from("/remote/repo")
        );
    }
}

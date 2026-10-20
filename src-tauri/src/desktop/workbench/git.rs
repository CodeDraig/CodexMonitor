use super::{mutate_git, read_git, workspace_params, PanelEffect, WorkbenchRequestKey};
use crate::desktop::app::media::{self, DecodedAttachment};
use crate::desktop::content::{DiffMode, DiffView};
use egui::{self, Ui};
use serde_json::{json, Value};
use std::collections::{hash_map::DefaultHasher, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

const MAX_GIT_MEDIA_CACHE: usize = 12;
const MAX_GIT_MEDIA_CACHE_BYTES: usize = 48 * 1024 * 1024;

#[derive(Debug)]
struct GitMediaResult {
    key: String,
    workspace: String,
    result: Result<DecodedAttachment, String>,
}

#[derive(Debug)]
struct CachedGitMedia {
    decoded: Result<DecodedAttachment, String>,
    last_used: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GitImageSource {
    old: Option<(String, String)>,
    new: Option<(String, String)>,
}

pub(crate) struct GitPanel {
    workspace: Option<String>,
    stale: bool,
    status: Option<Value>,
    diffs: Vec<Value>,
    log: Option<Value>,
    branches: Option<Value>,
    selected: Option<String>,
    error: Option<String>,
    action_message: Option<String>,
    commit_message: String,
    diff_view: DiffView,
    diff_revision: u64,
    confirm_revert: Option<(String, Value)>,
    branch_name: String,
    branch_selected: String,
    issues: Option<Value>,
    pull_requests: Option<Value>,
    selected_pr: Option<u64>,
    pr_diff: Option<Value>,
    diff_mode: DiffMode,
    media_tx: Sender<GitMediaResult>,
    media_rx: Receiver<GitMediaResult>,
    media_pending: HashSet<String>,
    media_cache: HashMap<String, CachedGitMedia>,
    media_textures: HashMap<String, egui::TextureHandle>,
    media_clock: u64,
}

impl std::fmt::Debug for GitPanel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GitPanel")
            .field("workspace", &self.workspace)
            .field("stale", &self.stale)
            .field("selected", &self.selected)
            .field("media_cache_entries", &self.media_cache.len())
            .field("media_pending", &self.media_pending.len())
            .finish_non_exhaustive()
    }
}

impl Default for GitPanel {
    fn default() -> Self {
        let (media_tx, media_rx) = mpsc::channel();
        Self {
            workspace: None,
            stale: false,
            status: None,
            diffs: Vec::new(),
            log: None,
            branches: None,
            selected: None,
            error: None,
            action_message: None,
            commit_message: String::new(),
            diff_view: DiffView::default(),
            diff_revision: 0,
            confirm_revert: None,
            branch_name: String::new(),
            branch_selected: String::new(),
            issues: None,
            pull_requests: None,
            selected_pr: None,
            pr_diff: None,
            diff_mode: DiffMode::Unified,
            media_tx,
            media_rx,
            media_pending: HashSet::new(),
            media_cache: HashMap::new(),
            media_textures: HashMap::new(),
            media_clock: 0,
        }
    }
}

impl GitPanel {
    fn poll_media(&mut self) {
        while let Ok(result) = self.media_rx.try_recv() {
            self.media_pending.remove(&result.key);
            if self.workspace.as_deref() != Some(result.workspace.as_str()) {
                continue;
            }
            self.media_clock = self.media_clock.wrapping_add(1);
            let mut decoded = result.result;
            let incoming_size = decoded_bytes(&decoded);
            while (!self.media_cache.contains_key(&result.key)
                && self.media_cache.len() >= MAX_GIT_MEDIA_CACHE)
                || self.media_cache_bytes().saturating_add(incoming_size)
                    > MAX_GIT_MEDIA_CACHE_BYTES
            {
                if let Some(evict) = self
                    .media_cache
                    .iter()
                    .min_by_key(|(_, entry)| entry.last_used)
                    .map(|(key, _)| key.clone())
                {
                    self.media_cache.remove(&evict);
                    self.media_textures.remove(&evict);
                } else {
                    break;
                }
            }
            if incoming_size > MAX_GIT_MEDIA_CACHE_BYTES {
                decoded = Err("Image preview exceeds the 48 MiB cache limit".into());
            }
            self.media_cache.insert(
                result.key,
                CachedGitMedia {
                    decoded,
                    last_used: self.media_clock,
                },
            );
        }
    }

    fn media_cache_bytes(&self) -> usize {
        self.media_cache
            .values()
            .map(|entry| decoded_bytes(&entry.decoded))
            .sum()
    }

    fn show_image_pair(&mut self, ui: &mut Ui, workspace: &str, path: &str, diff: &Value) {
        let source = image_source(diff);
        ui.horizontal(|ui| {
            for (label, side, source) in [
                ("Before", "old", source.old.as_ref()),
                ("After", "new", source.new.as_ref()),
            ] {
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(label).strong());
                    if let Some((mime, data)) = source {
                        self.show_image(ui, workspace, path, side, mime, data);
                    } else if let Some(mime) = image_mime(diff, side) {
                        ui.label(format!("{mime} image bytes were not supplied."));
                    } else {
                        ui.label("No image on this side.");
                    }
                });
            }
        });
    }

    fn show_image(
        &mut self,
        ui: &mut Ui,
        workspace: &str,
        path: &str,
        side: &str,
        mime: &str,
        data: &str,
    ) {
        let key = git_image_key(workspace, path, side, mime, data);
        self.media_clock = self.media_clock.wrapping_add(1);
        if let Some(cached) = self.media_cache.get_mut(&key) {
            cached.last_used = self.media_clock;
            match &cached.decoded {
                Ok(DecodedAttachment::Image {
                    width,
                    height,
                    rgba,
                }) => {
                    let texture = self.media_textures.entry(key.clone()).or_insert_with(|| {
                        ui.ctx().load_texture(
                            format!("git-image-{}", hash_key(&key)),
                            egui::ColorImage::from_rgba_unmultiplied([*width, *height], rgba),
                            egui::TextureOptions::LINEAR,
                        )
                    });
                    ui.add(egui::Image::new(&*texture).max_size(egui::vec2(360.0, 260.0)));
                }
                Ok(DecodedAttachment::Binary {
                    mime,
                    byte_len,
                    hex_preview,
                    decode_note,
                    original_base64,
                }) => {
                    ui.label(format!("{mime} · {byte_len} bytes"));
                    if let Some(note) = decode_note {
                        ui.colored_label(egui::Color32::GRAY, note);
                    }
                    ui.collapsing("Binary preview", |ui| ui.monospace(hex_preview));
                    if ui.button("Copy original as base64").clicked() {
                        ui.ctx().copy_text(original_base64.clone());
                    }
                }
                Ok(DecodedAttachment::Text {
                    mime,
                    text,
                    truncated,
                    original_base64,
                }) => {
                    ui.label(format!("{mime} · text resource"));
                    ui.add(egui::Label::new(text).selectable(true));
                    if *truncated {
                        ui.label("Text preview truncated.");
                    }
                    if ui.button("Copy complete resource as base64").clicked() {
                        ui.ctx().copy_text(original_base64.clone());
                    }
                }
                Ok(DecodedAttachment::Audio { mime, bytes }) => {
                    ui.label(format!(
                        "Unexpected audio image · {mime} · {} bytes",
                        bytes.len()
                    ));
                }
                Err(error) => {
                    ui.colored_label(
                        egui::Color32::GRAY,
                        format!("Image preview unavailable: {error}"),
                    );
                }
            }
            return;
        }
        if self.media_pending.insert(key.clone()) {
            let sender = self.media_tx.clone();
            let key_for_worker = key.clone();
            let workspace = workspace.to_owned();
            let mime = mime.to_owned();
            let data = data.to_owned();
            thread::spawn(move || {
                let result = media::decode_inline(&mime, &data)
                    .map(|(mime, bytes)| media::decode_attachment(&mime, bytes));
                let _ = sender.send(GitMediaResult {
                    key: key_for_worker,
                    workspace,
                    result,
                });
            });
        }
        ui.ctx().request_repaint_after(Duration::from_millis(80));
        ui.label("Preparing image preview…");
    }
}

impl GitPanel {
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
    pub(crate) fn mark_stale(&mut self) {
        self.stale = true;
    }
    pub(crate) fn show(
        &mut self,
        ui: &mut Ui,
        effects: &mut Vec<PanelEffect>,
        workspace: Option<&str>,
    ) {
        self.poll_media();
        if self.workspace.as_deref() != workspace {
            self.workspace = workspace.map(str::to_owned);
            self.reset_data();
            self.media_cache.clear();
            self.media_textures.clear();
            self.media_pending.clear();
            self.stale = true;
        }
        ui.horizontal(|ui| {
            ui.heading("Git");
            ui.label("Diff view");
            ui.selectable_value(&mut self.diff_mode, DiffMode::Unified, "Unified");
            ui.selectable_value(&mut self.diff_mode, DiffMode::Split, "Split");
            if ui.button("Refresh").clicked() {
                self.stale = true;
            }
        });
        if self.stale {
            if let Some(w) = workspace {
                let p = workspace_params(Some(w));
                for (key, method, params) in [
                    (WorkbenchRequestKey::GitStatus, "get_git_status", p.clone()),
                    (WorkbenchRequestKey::GitDiffs, "get_git_diffs", p.clone()),
                    (
                        WorkbenchRequestKey::GitLog,
                        "get_git_log",
                        json!({"workspaceId":w,"limit":100}),
                    ),
                    (WorkbenchRequestKey::GitBranches, "list_git_branches", p),
                ] {
                    effects.push(PanelEffect::Request {
                        key,
                        request: read_git(method, params),
                    });
                }
                self.stale = false;
            }
        }
        if let Some(err) = &self.error {
            ui.colored_label(egui::Color32::LIGHT_RED, err);
        }
        if let Some(status) = &self.status {
            ui.label(format!(
                "Branch: {}",
                status["branchName"].as_str().unwrap_or("unknown")
            ));
        }
        ui.horizontal(|ui| {
            if ui.button("Stage all").clicked() {
                self.action(
                    effects,
                    "stage_git_all",
                    workspace_params(workspace),
                    "stage all",
                );
            }
            if ui.button("Fetch").clicked() {
                self.action(effects, "fetch_git", workspace_params(workspace), "fetch");
            }
            if ui.button("Pull").clicked() {
                self.action(effects, "pull_git", workspace_params(workspace), "pull");
            }
            if ui.button("Push").clicked() {
                self.action(effects, "push_git", workspace_params(workspace), "push");
            }
            if ui.button("Sync").clicked() {
                self.action(effects, "sync_git", workspace_params(workspace), "sync");
            }
        });
        if let Some(msg) = &self.action_message {
            ui.label(msg);
        }
        ui.horizontal(|ui|{ui.label("Commit message");ui.text_edit_singleline(&mut self.commit_message);if ui.button("Commit staged").clicked()&&!self.commit_message.trim().is_empty(){self.action(effects,"commit_git",json!({"workspaceId":workspace.unwrap_or_default(),"message":self.commit_message.trim()}),"commit");}});
        ui.collapsing("Branches", |ui| {
            if let Some(b) = self.branches.clone() {
                if let Some(branches)=b["branches"].as_array(){egui::ComboBox::from_id_salt("git-branch-select").selected_text(if self.branch_selected.is_empty(){"Select branch"}else{&self.branch_selected}).show_ui(ui,|ui|for branch in branches{if let Some(name)=branch["name"].as_str(){ui.selectable_value(&mut self.branch_selected,name.into(),name);}});if ui.add_enabled(!self.branch_selected.is_empty(),egui::Button::new("Checkout branch")).clicked(){self.action(effects,"checkout_git_branch",json!({"workspaceId":workspace.unwrap_or_default(),"name":self.branch_selected}),"checkout branch");}}
            } else {
                ui.label("Loading branches…");
            }
            ui.horizontal(|ui|{ui.text_edit_singleline(&mut self.branch_name);if ui.add_enabled(!self.branch_name.trim().is_empty(),egui::Button::new("Create branch")).clicked(){self.action(effects,"create_git_branch",json!({"workspaceId":workspace.unwrap_or_default(),"name":self.branch_name.trim()}),"create branch");self.branch_name.clear();}});
        });
        ui.collapsing("Changes", |ui| {
            if let Some(status) = self.status.clone() {
                for file in status["stagedFiles"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .chain(status["unstagedFiles"].as_array().into_iter().flatten())
                {
                    let path = file["path"].as_str().unwrap_or("");
                    ui.horizontal(|ui| {
                        if ui
                            .selectable_label(
                                self.selected.as_deref() == Some(path),
                                format!(
                                    "{} · {}",
                                    path,
                                    file["status"].as_str().unwrap_or("changed")
                                ),
                            )
                            .clicked()
                        {
                            self.selected = Some(path.to_owned());
                        }
                        if ui.small_button("Stage").clicked() {
                            self.action(
                                effects,
                                "stage_git_file",
                                json!({"workspaceId":workspace.unwrap_or_default(),"path":path}),
                                "stage file",
                            );
                        }
                        if ui.small_button("Unstage").clicked() {
                            self.action(
                                effects,
                                "unstage_git_file",
                                json!({"workspaceId":workspace.unwrap_or_default(),"path":path}),
                                "unstage file",
                            );
                        }
                        if ui.small_button("Revert…").clicked() {
                            self.confirm_revert = Some((
                                path.to_owned(),
                                json!({"workspaceId":workspace.unwrap_or_default(),"path":path}),
                            ));
                        }
                    });
                }
            }
        });
        if let Some((path, params)) = self.confirm_revert.clone() {
            ui.horizontal(|ui| {
                ui.colored_label(egui::Color32::YELLOW, format!("Discard changes to {path}?"));
                if ui.button("Confirm revert").clicked() {
                    self.action(effects, "revert_git_file", params, "revert file");
                    self.confirm_revert = None;
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_revert = None;
                }
            });
        }
        if let Some(path) = self.selected.clone() {
            ui.heading(&path);
            if let Some(index) = self
                .diffs
                .iter()
                .position(|d| d["path"].as_str() == Some(&path))
            {
                if is_image_diff(&self.diffs[index]) {
                    let diff = self.diffs[index].clone();
                    egui::ScrollArea::vertical()
                        .max_height(380.0)
                        .show(ui, |ui| {
                            self.show_image_pair(ui, workspace.unwrap_or_default(), &path, &diff);
                        });
                } else {
                    let diff = &self.diffs[index];
                    let identity = format!("{}::{path}", workspace.unwrap_or_default());
                    let diff_revision = self.diff_revision;
                    let diff_mode = self.diff_mode;
                    let diff_view = &mut self.diff_view;
                    egui::ScrollArea::vertical()
                        .max_height(380.0)
                        .show(ui, |ui| {
                            if let Some(patch) = diff["diff"].as_str() {
                                diff_view.show_text_versioned(
                                    ui,
                                    &identity,
                                    patch,
                                    diff_revision,
                                    diff_mode,
                                );
                            } else {
                                diff_view.show_versioned(
                                    ui,
                                    &identity,
                                    diff,
                                    diff_revision,
                                    diff_mode,
                                );
                            }
                        });
                }
            } else {
                ui.label("No textual patch available for this selection.");
            }
        }
        ui.collapsing("Recent commits", |ui| {
            if let Some(log) = &self.log {
                ui.monospace(serde_json::to_string_pretty(log).unwrap_or_default());
            } else {
                ui.label("Loading history…");
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Load GitHub issues").clicked() {
                effects.push(PanelEffect::Request {
                    key: WorkbenchRequestKey::GitIssues,
                    request: read_git("get_github_issues", workspace_params(workspace)),
                });
            }
            if ui.button("Load pull requests").clicked() {
                effects.push(PanelEffect::Request {
                    key: WorkbenchRequestKey::GitPullRequests,
                    request: read_git("get_github_pull_requests", workspace_params(workspace)),
                });
            }
        });
        if let Some(issues) = self.issues.clone() {
            ui.collapsing("GitHub issues", |ui| {
                ui.monospace(serde_json::to_string_pretty(&issues).unwrap_or_default())
            });
        }
        if let Some(prs) = self.pull_requests.clone() {
            ui.collapsing("GitHub pull requests",|ui|{let list=prs["pullRequests"].as_array().or(prs["items"].as_array());if let Some(list)=list{for pr in list{let number=pr["number"].as_u64().unwrap_or(0);ui.horizontal(|ui|{ui.label(format!("#{number} {}",pr["title"].as_str().unwrap_or("(untitled)")));if ui.small_button("View diff").clicked(){self.selected_pr=Some(number);effects.push(PanelEffect::Request{key:WorkbenchRequestKey::GitPullRequestDiff(number),request:read_git("get_github_pull_request_diff",json!({"workspaceId":workspace.unwrap_or_default(),"prNumber":number}))});}if ui.small_button("Checkout").clicked(){self.action(effects,"checkout_github_pull_request",json!({"workspaceId":workspace.unwrap_or_default(),"prNumber":number}),"checkout pull request");}});}}else{ui.monospace(serde_json::to_string_pretty(&prs).unwrap_or_default());}});
        }
        if let Some(diff) = self.pr_diff.clone() {
            egui::CollapsingHeader::new(format!(
                "Pull request #{} diff",
                self.selected_pr.unwrap_or_default()
            ))
            .show(ui, |ui| {
                for f in diff.as_array().into_iter().flatten() {
                    ui.collapsing(f["path"].as_str().unwrap_or("file"), |ui| {
                        let path = f["path"].as_str().unwrap_or("file");
                        let identity =
                            format!("pr-{}-{path}", self.selected_pr.unwrap_or_default(),);
                        if is_image_diff(f) {
                            self.show_image_pair(ui, workspace.unwrap_or_default(), &identity, f);
                        } else if let Some(patch) = f["diff"].as_str() {
                            self.diff_view.show_text_versioned(
                                ui,
                                &identity,
                                patch,
                                self.diff_revision,
                                self.diff_mode,
                            );
                        } else {
                            self.diff_view.show_versioned(
                                ui,
                                &identity,
                                f,
                                self.diff_revision,
                                self.diff_mode,
                            );
                        }
                    });
                }
            });
        }
    }
    fn action(&mut self, effects: &mut Vec<PanelEffect>, method: &str, params: Value, label: &str) {
        effects.push(PanelEffect::Request {
            key: WorkbenchRequestKey::GitAction(label.to_owned()),
            request: mutate_git(method, params),
        });
        self.action_message = Some(format!("{label} requested…"));
    }
    fn reset_data(&mut self) {
        self.diff_revision = self.diff_revision.wrapping_add(1);
        self.status = None;
        self.diffs.clear();
        self.log = None;
        self.branches = None;
        self.issues = None;
        self.pull_requests = None;
        self.pr_diff = None;
        self.error = None;
        self.selected = None;
    }
    pub(crate) fn apply(&mut self, key: &WorkbenchRequestKey, value: Value) {
        match key {
            WorkbenchRequestKey::GitStatus => self.status = Some(value),
            WorkbenchRequestKey::GitDiffs => {
                self.diffs = value.as_array().cloned().unwrap_or_default();
                self.diff_revision = self.diff_revision.wrapping_add(1);
            }
            WorkbenchRequestKey::GitLog => self.log = Some(value),
            WorkbenchRequestKey::GitBranches => self.branches = Some(value),
            WorkbenchRequestKey::GitIssues => self.issues = Some(value),
            WorkbenchRequestKey::GitPullRequests => self.pull_requests = Some(value),
            WorkbenchRequestKey::GitPullRequestDiff(number) => {
                self.selected_pr = Some(*number);
                self.pr_diff = Some(value);
                self.diff_revision = self.diff_revision.wrapping_add(1);
            }
            WorkbenchRequestKey::GitAction(_) => {
                self.action_message = Some("Git operation completed".into());
                self.stale = true;
            }
            _ => {}
        }
    }
}

fn is_image_diff(diff: &Value) -> bool {
    diff["isImage"] == true || diff["oldImageData"].is_string() || diff["newImageData"].is_string()
}

fn image_source(diff: &Value) -> GitImageSource {
    GitImageSource {
        old: diff["oldImageData"].as_str().map(|data| {
            (
                diff["oldImageMime"]
                    .as_str()
                    .or(diff["oldImageMimeType"].as_str())
                    .unwrap_or("application/octet-stream")
                    .to_owned(),
                data.to_owned(),
            )
        }),
        new: diff["newImageData"].as_str().map(|data| {
            (
                diff["newImageMime"]
                    .as_str()
                    .or(diff["newImageMimeType"].as_str())
                    .unwrap_or("application/octet-stream")
                    .to_owned(),
                data.to_owned(),
            )
        }),
    }
}

fn image_mime<'a>(diff: &'a Value, side: &str) -> Option<&'a str> {
    let (mime_key, mime_type_key) = match side {
        "old" => ("oldImageMime", "oldImageMimeType"),
        "new" => ("newImageMime", "newImageMimeType"),
        _ => return None,
    };
    diff[mime_key].as_str().or(diff[mime_type_key].as_str())
}

fn decoded_bytes(decoded: &Result<DecodedAttachment, String>) -> usize {
    match decoded {
        Ok(DecodedAttachment::Image { rgba, .. }) => rgba.len(),
        Ok(DecodedAttachment::Audio { bytes, .. }) => bytes.len(),
        Ok(DecodedAttachment::Text {
            text,
            original_base64,
            ..
        }) => text.len() + original_base64.len(),
        Ok(DecodedAttachment::Binary {
            hex_preview,
            mime,
            original_base64,
            ..
        }) => hex_preview.len() + mime.len() + original_base64.len(),
        Err(error) => error.len(),
    }
}

fn git_image_key(workspace: &str, path: &str, side: &str, mime: &str, data: &str) -> String {
    format!("{workspace}/{path}/{side}/{mime}/{:016x}", hash_key(data))
}

fn hash_key(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_diff_sources_keep_old_and_new_mime_and_bytes() {
        let diff = json!({
            "isBinary": true,
            "isImage": true,
            "oldImageData": "b2xk",
            "newImageData": "bmV3",
            "oldImageMime": "image/png",
            "newImageMime": "image/webp"
        });
        assert!(is_image_diff(&diff));
        assert_eq!(
            image_source(&diff),
            GitImageSource {
                old: Some(("image/png".into(), "b2xk".into())),
                new: Some(("image/webp".into(), "bmV3".into()))
            }
        );
    }

    #[test]
    fn image_diff_keeps_a_honest_unavailable_side_when_bytes_are_omitted() {
        let diff = json!({"isImage": true, "newImageMime": "image/png"});
        assert!(is_image_diff(&diff));
        assert_eq!(image_source(&diff).new, None);
        assert_eq!(image_mime(&diff, "new"), Some("image/png"));
    }

    #[test]
    fn image_cache_identity_includes_workspace_path_side_and_content() {
        let base = git_image_key(
            "backend-a/workspace-1",
            "a.png",
            "old",
            "image/png",
            "bytes",
        );
        assert_ne!(
            base,
            git_image_key(
                "backend-b/workspace-1",
                "a.png",
                "old",
                "image/png",
                "bytes"
            )
        );
        assert_ne!(
            base,
            git_image_key(
                "backend-a/workspace-1",
                "b.png",
                "old",
                "image/png",
                "bytes"
            )
        );
        assert_ne!(
            base,
            git_image_key(
                "backend-a/workspace-1",
                "a.png",
                "new",
                "image/png",
                "bytes"
            )
        );
        assert_ne!(
            base,
            git_image_key(
                "backend-a/workspace-1",
                "a.png",
                "old",
                "image/png",
                "changed"
            )
        );
    }

    #[test]
    fn git_media_cache_is_bounded_by_bytes_and_entries() {
        let image = DecodedAttachment::Image {
            width: 4,
            height: 4,
            rgba: vec![255; 64],
        };
        let mut panel = GitPanel::default();
        for index in 0..(MAX_GIT_MEDIA_CACHE + 1) {
            panel
                .media_tx
                .send(GitMediaResult {
                    key: format!("workspace/file-{index}/new/image/png/hash"),
                    workspace: "workspace".into(),
                    result: Ok(image.clone()),
                })
                .unwrap();
        }
        panel.workspace = Some("workspace".into());
        panel.poll_media();
        assert_eq!(panel.media_cache.len(), MAX_GIT_MEDIA_CACHE);
        assert!(!panel
            .media_cache
            .contains_key("workspace/file-0/new/image/png/hash"));
        assert!(panel.media_cache_bytes() <= MAX_GIT_MEDIA_CACHE_BYTES);
    }

    #[test]
    fn late_image_result_from_old_workspace_is_discarded() {
        let mut panel = GitPanel::default();
        panel.workspace = Some("new-workspace".into());
        panel
            .media_tx
            .send(GitMediaResult {
                key: "old-workspace/file.png/new/image/png/hash".into(),
                workspace: "old-workspace".into(),
                result: Ok(DecodedAttachment::Binary {
                    mime: "image/png".into(),
                    byte_len: 4,
                    hex_preview: "00 01 02 03".into(),
                    decode_note: None,
                    original_base64: "AAECAw==".into(),
                }),
            })
            .unwrap();
        panel.poll_media();
        assert!(panel.media_cache.is_empty());
    }
}

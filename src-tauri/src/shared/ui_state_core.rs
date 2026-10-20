//! Versioned desktop presentation state and a reversible importer for the
//! browser frontend's explicitly allowlisted localStorage values.
//!
//! State files are immutable generation snapshots. A completed write is
//! published by renaming a synced temporary file into the snapshots directory;
//! an interrupted write leaves the previous generation readable.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub const UI_STATE_SCHEMA_VERSION: u32 = 1;
const SNAPSHOT_DIR: &str = "ui-state-v1";
const LEGACY_EXPORT_DIR: &str = "legacy-ui-state-v1";
const WRITER_LOCK_FILE: &str = ".writer.lock";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiStateV1 {
    pub schema_version: u32,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub panels: BTreeMap<String, Value>,
    #[serde(default)]
    pub collapsed_groups: BTreeMap<String, bool>,
    #[serde(default)]
    pub thread_list_sort_key: Option<String>,
    #[serde(default)]
    pub thread_list_organize_mode: Option<String>,
    #[serde(default)]
    pub pinned_threads: BTreeMap<String, i64>,
    #[serde(default)]
    pub custom_names: BTreeMap<String, String>,
    #[serde(default)]
    pub thread_codex_params: BTreeMap<String, Value>,
    #[serde(default)]
    pub prompt_history: BTreeMap<String, Vec<String>>,
}

impl UiStateV1 {
    pub fn empty() -> Self {
        Self::default()
    }

    fn validate(&self) -> Result<(), UiStateError> {
        if self.schema_version != UI_STATE_SCHEMA_VERSION {
            return Err(UiStateError::UnsupportedVersion(self.schema_version));
        }
        for key in self.panels.keys() {
            if !PANEL_KEYS.contains(&key.as_str()) {
                return Err(UiStateError::UnknownKey(format!("panels.{key}")));
            }
        }
        for (key, value) in &self.panels {
            validate_panel_value(key, value)?;
        }
        if self
            .thread_list_sort_key
            .as_deref()
            .is_some_and(|value| !matches!(value, "created_at" | "updated_at"))
        {
            return Err(UiStateError::InvalidField("threadListSortKey".into()));
        }
        if self
            .thread_list_organize_mode
            .as_deref()
            .is_some_and(|value| {
                !matches!(value, "by_project" | "by_project_activity" | "threads_only")
            })
        {
            return Err(UiStateError::InvalidField("threadListOrganizeMode".into()));
        }
        for (thread_key, params) in &self.thread_codex_params {
            validate_thread_params(thread_key, params)?;
        }
        Ok(())
    }
}

fn validate_thread_params(thread_key: &str, value: &Value) -> Result<(), UiStateError> {
    const ALLOWED: &[&str] = &[
        "modelId",
        "effort",
        "serviceTier",
        "accessMode",
        "collaborationModeId",
        "codexArgsOverride",
        "updatedAt",
    ];
    let Some(fields) = value.as_object() else {
        return Err(UiStateError::InvalidField(format!(
            "threadCodexParams.{thread_key}"
        )));
    };
    for (name, field) in fields {
        if !ALLOWED.contains(&name.as_str()) {
            return Err(UiStateError::UnknownKey(format!(
                "threadCodexParams.{thread_key}.{name}"
            )));
        }
        let valid = if name == "updatedAt" {
            field.as_f64().is_some_and(f64::is_finite)
        } else {
            field.is_null() || field.is_string()
        };
        if !valid {
            return Err(UiStateError::InvalidField(format!(
                "threadCodexParams.{thread_key}.{name}"
            )));
        }
    }
    Ok(())
}

impl Default for UiStateV1 {
    fn default() -> Self {
        Self {
            schema_version: UI_STATE_SCHEMA_VERSION,
            revision: 0,
            panels: BTreeMap::new(),
            collapsed_groups: BTreeMap::new(),
            thread_list_sort_key: None,
            thread_list_organize_mode: None,
            pinned_threads: BTreeMap::new(),
            custom_names: BTreeMap::new(),
            thread_codex_params: BTreeMap::new(),
            prompt_history: BTreeMap::new(),
        }
    }
}

const PANEL_KEYS: &[&str] = &[
    "sidebarWidth",
    "rightPanelWidth",
    "chatDiffSplitPositionPercent",
    "planPanelHeight",
    "terminalPanelHeight",
    "debugPanelHeight",
    "composerHeight",
    "sidebarCollapsed",
    "rightPanelCollapsed",
];

fn validate_panel_value(key: &str, value: &Value) -> Result<(), UiStateError> {
    let valid = match key {
        "sidebarCollapsed" | "rightPanelCollapsed" => value.is_boolean(),
        _ => value.as_f64().is_some_and(f64::is_finite),
    };
    if valid {
        Ok(())
    } else {
        Err(UiStateError::InvalidField(format!("panels.{key}")))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyUiStateSnapshot {
    pub schema_version: u32,
    pub exported_at_ms: u64,
    pub state: UiStateV1,
}

impl LegacyUiStateSnapshot {
    pub fn validate(&self) -> Result<(), UiStateError> {
        if self.schema_version != UI_STATE_SCHEMA_VERSION {
            return Err(UiStateError::UnsupportedVersion(self.schema_version));
        }
        self.state.validate()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub imported_fields: usize,
    pub already_present_fields: usize,
    pub conflicts: Vec<String>,
}

#[derive(Debug)]
pub enum UiStateError {
    UnsupportedVersion(u32),
    UnknownKey(String),
    InvalidField(String),
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl std::fmt::Display for UiStateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported UI state schema version {version}")
            }
            Self::UnknownKey(key) => write!(f, "unknown UI state field: {key}"),
            Self::InvalidField(field) => write!(f, "invalid UI state value: {field}"),
            Self::Io(error) => write!(f, "UI state I/O failed: {error}"),
            Self::Json(error) => write!(f, "UI state JSON failed: {error}"),
        }
    }
}

impl std::error::Error for UiStateError {}

impl From<std::io::Error> for UiStateError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for UiStateError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Merge a legacy snapshot without replacing native values. Repeating the
/// same import is idempotent; differing values are reported and left intact.
pub fn import_legacy_snapshot(
    target: &mut UiStateV1,
    snapshot: &LegacyUiStateSnapshot,
) -> Result<ImportReport, UiStateError> {
    target.validate()?;
    snapshot.validate()?;
    let source = &snapshot.state;
    let mut report = ImportReport::default();

    merge_map("panels", &mut target.panels, &source.panels, &mut report);
    merge_map(
        "collapsedGroups",
        &mut target.collapsed_groups,
        &source.collapsed_groups,
        &mut report,
    );
    merge_option(
        "threadListSortKey",
        &mut target.thread_list_sort_key,
        &source.thread_list_sort_key,
        &mut report,
    );
    merge_option(
        "threadListOrganizeMode",
        &mut target.thread_list_organize_mode,
        &source.thread_list_organize_mode,
        &mut report,
    );
    merge_map(
        "pinnedThreads",
        &mut target.pinned_threads,
        &source.pinned_threads,
        &mut report,
    );
    merge_map(
        "customNames",
        &mut target.custom_names,
        &source.custom_names,
        &mut report,
    );
    merge_map(
        "threadCodexParams",
        &mut target.thread_codex_params,
        &source.thread_codex_params,
        &mut report,
    );
    merge_map(
        "promptHistory",
        &mut target.prompt_history,
        &source.prompt_history,
        &mut report,
    );

    if report.imported_fields > 0 {
        target.revision = target.revision.saturating_add(1);
    }
    Ok(report)
}

fn merge_map<T: Clone + PartialEq>(
    prefix: &str,
    target: &mut BTreeMap<String, T>,
    source: &BTreeMap<String, T>,
    report: &mut ImportReport,
) {
    for (key, incoming) in source {
        match target.get(key) {
            None => {
                target.insert(key.clone(), incoming.clone());
                report.imported_fields += 1;
            }
            Some(current) if current == incoming => report.already_present_fields += 1,
            Some(_) => report.conflicts.push(format!("{prefix}.{key}")),
        }
    }
}

fn merge_option<T: Clone + PartialEq>(
    name: &str,
    target: &mut Option<T>,
    source: &Option<T>,
    report: &mut ImportReport,
) {
    let Some(incoming) = source else { return };
    match target {
        None => {
            *target = Some(incoming.clone());
            report.imported_fields += 1;
        }
        Some(current) if current == incoming => report.already_present_fields += 1,
        Some(_) => report.conflicts.push(name.to_owned()),
    }
}

#[derive(Clone, Debug)]
pub struct UiStateStore {
    root: PathBuf,
}

impl UiStateStore {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            root: data_dir.into().join(SNAPSHOT_DIR),
        }
    }

    /// Loads the highest complete, valid generation. Incomplete temporary
    /// files and corrupt generations are ignored; earlier snapshots remain.
    pub fn load(&self) -> Result<UiStateLoad, UiStateError> {
        if !self.root.exists() {
            return Ok(UiStateLoad {
                state: UiStateV1::empty(),
                recovered: false,
            });
        }
        let mut candidates = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|v| v.to_str()) != Some("json") {
                continue;
            }
            if let Ok(state) = read_state(&path) {
                candidates.push((state.revision, path, state));
            }
        }
        candidates.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let Some((_, _path, state)) = candidates.pop() else {
            let has_json = fs::read_dir(&self.root)?.any(|e| {
                e.ok()
                    .is_some_and(|e| e.path().extension().and_then(|v| v.to_str()) == Some("json"))
            });
            return if has_json {
                Err(UiStateError::InvalidField(
                    "all saved generations are invalid".into(),
                ))
            } else {
                Ok(UiStateLoad {
                    state: UiStateV1::empty(),
                    recovered: false,
                })
            };
        };
        let newest_declared_revision = latest_declared_revision(&self.root)?;
        let recovered = newest_declared_revision.is_some_and(|revision| revision > state.revision);
        Ok(UiStateLoad { state, recovered })
    }

    /// Append a complete generation with a unique name, then sync the file and
    /// directory. Existing generations are retained for recovery.
    pub fn save(&self, state: &mut UiStateV1) -> Result<(), UiStateError> {
        state.validate()?;
        fs::create_dir_all(&self.root)?;
        // Keep the lock file itself in place. The OS lock is released when
        // this handle drops, including after a crash or interrupted write.
        // Loads intentionally do not acquire this lock.
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.root.join(WRITER_LOCK_FILE))?;
        lock.lock()?;
        let latest = self.load()?;
        state.revision = state.revision.max(latest.state.revision).saturating_add(1);
        let bytes = serde_json::to_vec_pretty(state)?;
        let id = Uuid::new_v4();
        let temp = self.root.join(format!(".pending-{id}"));
        let final_path = self.root.join(format!("{}-{id}.json", state.revision));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, &final_path)?;
        sync_directory(&self.root)?;
        Ok(())
    }

    pub fn import_legacy(
        &self,
        snapshot: &LegacyUiStateSnapshot,
    ) -> Result<(UiStateV1, ImportReport), UiStateError> {
        let mut loaded = self.load()?.state;
        let report = import_legacy_snapshot(&mut loaded, snapshot)?;
        if report.imported_fields > 0 {
            self.save(&mut loaded)?;
        }
        Ok((loaded, report))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiStateLoad {
    pub state: UiStateV1,
    pub recovered: bool,
}

fn read_state(path: &Path) -> Result<UiStateV1, UiStateError> {
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    let state: UiStateV1 = serde_json::from_slice(&bytes)?;
    state.validate()?;
    Ok(state)
}

fn latest_declared_revision(root: &Path) -> Result<Option<u64>, UiStateError> {
    let mut latest = None;
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.extension().and_then(|v| v.to_str()) != Some("json") {
            continue;
        }
        let Some(file) = path.file_name().and_then(|v| v.to_str()) else {
            continue;
        };
        let Some(revision) = file.split('-').next().and_then(|v| v.parse::<u64>().ok()) else {
            continue;
        };
        latest = Some(latest.map_or(revision, |current: u64| current.max(revision)));
    }
    Ok(latest)
}

fn sync_directory(path: &Path) -> Result<(), std::io::Error> {
    // Directory handles can be opened and synced on Unix. Windows does not
    // expose the same operation through std, while the synced file is durable.
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    Ok(())
}

/// Store the allowlisted legacy export as an immutable snapshot under app data.
/// Identical exports return the existing path, preserving idempotence.
pub fn write_legacy_export(
    data_dir: impl AsRef<Path>,
    snapshot: &LegacyUiStateSnapshot,
) -> Result<PathBuf, UiStateError> {
    snapshot.validate()?;
    let root = data_dir.as_ref().join(LEGACY_EXPORT_DIR);
    fs::create_dir_all(&root)?;
    let bytes = serde_json::to_vec_pretty(snapshot)?;
    for entry in fs::read_dir(&root)? {
        let path = entry?.path();
        if path.extension().and_then(|v| v.to_str()) != Some("json") {
            continue;
        }
        if let Ok(existing_bytes) = fs::read(&path) {
            if let Ok(existing) = serde_json::from_slice::<LegacyUiStateSnapshot>(&existing_bytes) {
                if existing.schema_version == snapshot.schema_version
                    && existing.state == snapshot.state
                {
                    return Ok(path);
                }
            }
        }
    }
    let id = Uuid::new_v4();
    let temp = root.join(format!(".pending-{id}"));
    let destination = root.join(format!("snapshot-{id}.json"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp, &destination)?;
    sync_directory(&root)?;
    Ok(destination)
}

/// Read the newest complete versioned browser export, if one exists. Invalid
/// and temporary files are ignored so a partial handoff cannot hide an older
/// valid snapshot.
pub fn read_latest_legacy_export(
    data_dir: impl AsRef<Path>,
) -> Result<Option<LegacyUiStateSnapshot>, UiStateError> {
    let root = data_dir.as_ref().join(LEGACY_EXPORT_DIR);
    if !root.exists() {
        return Ok(None);
    }
    let mut candidates = Vec::new();
    for entry in fs::read_dir(&root)? {
        let path = entry?.path();
        if path.extension().and_then(|v| v.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else { continue };
        let Ok(snapshot) = serde_json::from_slice::<LegacyUiStateSnapshot>(&bytes) else {
            continue;
        };
        if snapshot.validate().is_ok() {
            candidates.push((snapshot.exported_at_ms, path, snapshot));
        }
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    Ok(candidates.pop().map(|(_, _, snapshot)| snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        sync::{Arc, Barrier},
        thread,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn temp_dir() -> PathBuf {
        let id = Uuid::new_v4();
        std::env::temp_dir().join(format!("codexmonitor-ui-state-{id}"))
    }

    fn snapshot() -> LegacyUiStateSnapshot {
        let mut state = UiStateV1::empty();
        state.panels.insert("sidebarWidth".into(), json!(310));
        state.pinned_threads.insert("w:t".into(), 1000);
        state
            .prompt_history
            .insert("w".into(), vec!["hello".into()]);
        LegacyUiStateSnapshot {
            schema_version: 1,
            exported_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
            state,
        }
    }

    #[test]
    fn save_and_import_are_repeatable_and_report_conflicts() {
        let root = temp_dir();
        let store = UiStateStore::new(&root);
        let incoming = snapshot();
        let (first, report) = store.import_legacy(&incoming).unwrap();
        assert_eq!(report.imported_fields, 3);
        let (again, report) = store.import_legacy(&incoming).unwrap();
        assert_eq!(again, first);
        assert_eq!(report.imported_fields, 0);
        assert_eq!(report.already_present_fields, 3);

        let mut different = incoming.clone();
        different
            .state
            .panels
            .insert("sidebarWidth".into(), json!(350));
        let (_, report) = store.import_legacy(&different).unwrap();
        assert_eq!(report.conflicts, vec!["panels.sidebarWidth"]);
        assert_eq!(
            store.load().unwrap().state.panels["sidebarWidth"],
            json!(310)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interrupted_and_corrupt_generations_recover_prior_state() {
        let root = temp_dir();
        let store = UiStateStore::new(&root);
        let mut state = UiStateV1::empty();
        state.panels.insert("sidebarWidth".into(), json!(300));
        store.save(&mut state).unwrap();
        let generation_dir = root.join(SNAPSHOT_DIR);
        fs::write(generation_dir.join(".pending-interrupted"), b"{").unwrap();
        fs::write(generation_dir.join("999999-corrupt.json"), b"{").unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.state.panels["sidebarWidth"], json!(300));
        assert!(loaded.recovered);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_saves_allocate_unique_complete_generations() {
        const WRITERS: usize = 8;
        const SAVES_PER_WRITER: usize = 8;
        let root = temp_dir();
        let barrier = Arc::new(Barrier::new(WRITERS));
        let mut workers = Vec::new();

        for writer_id in 0..WRITERS {
            let data_dir = root.clone();
            let barrier = barrier.clone();
            workers.push(thread::spawn(move || {
                let store = UiStateStore::new(data_dir);
                barrier.wait();
                let mut state = UiStateV1::empty();
                for save_id in 0..SAVES_PER_WRITER {
                    state.custom_names.clear();
                    state.custom_names.insert(
                        format!("writer-{writer_id}"),
                        format!("complete-snapshot-{save_id}"),
                    );
                    store.save(&mut state).unwrap();
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }

        let generation_dir = root.join(SNAPSHOT_DIR);
        let mut snapshots = fs::read_dir(&generation_dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
            .map(|path| read_state(&path).unwrap())
            .collect::<Vec<_>>();
        let expected_count = (WRITERS * SAVES_PER_WRITER) as u64;
        assert_eq!(snapshots.len(), expected_count as usize);
        snapshots.sort_by_key(|state| state.revision);
        let revisions = snapshots
            .iter()
            .map(|state| state.revision)
            .collect::<Vec<_>>();
        assert_eq!(revisions, (1..=expected_count).collect::<Vec<_>>());
        assert!(snapshots.iter().all(|state| {
            state.custom_names.len() == 1
                && state
                    .custom_names
                    .values()
                    .next()
                    .is_some_and(|value| value.starts_with("complete-snapshot-"))
        }));

        let latest = UiStateStore::new(&root).load().unwrap().state;
        assert_eq!(latest.revision, expected_count);
        assert!(latest.custom_names.len() == 1);
        assert!(latest
            .custom_names
            .values()
            .next()
            .is_some_and(|value| value.starts_with("complete-snapshot-")));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unsupported_versions_and_unallowlisted_fields_fail_closed() {
        let root = temp_dir();
        let mut unsupported = snapshot();
        unsupported.schema_version = 2;
        assert!(matches!(
            unsupported.validate(),
            Err(UiStateError::UnsupportedVersion(2))
        ));
        let mut state = UiStateV1::empty();
        state.panels.insert("credentials".into(), json!("secret"));
        assert!(matches!(state.validate(), Err(UiStateError::UnknownKey(_))));
        let mut state = UiStateV1::empty();
        state
            .thread_codex_params
            .insert("w:t".into(), json!({"apiKey":"secret"}));
        assert!(matches!(state.validate(), Err(UiStateError::UnknownKey(_))));
        fs::create_dir_all(&root).unwrap();
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn composer_height_is_an_allowlisted_numeric_panel_value() {
        let mut state = UiStateV1::empty();
        state.panels.insert("composerHeight".into(), json!(240));
        assert!(state.validate().is_ok());

        state.panels.insert("composerHeight".into(), json!("240"));
        assert!(matches!(
            state.validate(),
            Err(UiStateError::InvalidField(field)) if field == "panels.composerHeight"
        ));
    }

    #[test]
    fn legacy_export_is_idempotent_and_keeps_old_snapshot_when_changed() {
        let root = temp_dir();
        let first = snapshot();
        let path1 = write_legacy_export(&root, &first).unwrap();
        assert_eq!(path1, write_legacy_export(&root, &first).unwrap());
        let mut second = first.clone();
        second.state.thread_list_sort_key = Some("created_at".into());
        second.exported_at_ms = second.exported_at_ms.saturating_add(1);
        let path2 = write_legacy_export(&root, &second).unwrap();
        assert_ne!(path1, path2);
        assert!(path1.exists());
        assert!(path2.exists());
        assert_eq!(read_latest_legacy_export(&root).unwrap().unwrap(), second);
        fs::remove_dir_all(root).unwrap();
    }
}

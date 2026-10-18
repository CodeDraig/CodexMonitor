use super::{id, now, types::*};
use git2::{DiffOptions, Patch, Repository};
use serde_json::{json, Value};
use std::path::Path;

pub(crate) fn git_snapshot(path: &str, base: Option<&str>) -> Value {
    let started = now();
    match git_snapshot_inner(path, base) {
        Ok(mut value) => {
            value["startedAt"] = json!(started);
            value["finishedAt"] = json!(now());
            value
        }
        Err(error) => {
            json!({"available":false,"baseOid":base,"startedAt":started,"finishedAt":now(),"errors":[error],"files":[]})
        }
    }
}
fn git_snapshot_inner(path: &str, base: Option<&str>) -> Result<Value, String> {
    if path.is_empty() {
        return Err("Workspace path is missing".into());
    }
    let repo = Repository::discover(Path::new(path)).map_err(|e| e.to_string())?;
    let head = repo
        .head()
        .ok()
        .and_then(|h| h.target())
        .map(|h| h.to_string());
    let base = base.or(head.as_deref());
    let tree = base
        .map(|oid| repo.revparse_single(oid).and_then(|o| o.peel_to_tree()))
        .transpose()
        .map_err(|e| e.to_string())?;
    let mut options = DiffOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true)
        .include_typechange(true);
    let diff = repo
        .diff_tree_to_workdir_with_index(tree.as_ref(), Some(&mut options))
        .map_err(|e| e.to_string())?;
    let mut files = vec![];
    let mut remaining = 8 * 1024 * 1024usize;
    for (index, delta) in diff.deltas().enumerate() {
        let path = delta
            .new_file()
            .path()
            .or(delta.old_file().path())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut omission = None;
        let mut text = String::new();
        if delta.new_file().size().max(delta.old_file().size()) > 2 * 1024 * 1024 || remaining == 0
        {
            omission = Some("oversize");
        } else {
            match Patch::from_diff(&diff, index) {
                Ok(Some(mut patch)) => match patch.to_buf() {
                    Ok(buffer) => {
                        if buffer.len() > remaining || buffer.len() > 2 * 1024 * 1024 {
                            omission = Some("oversize");
                        } else if let Ok(patch) = std::str::from_utf8(&buffer) {
                            text = patch.to_string();
                            remaining -= buffer.len();
                        } else {
                            omission = Some("binary");
                        }
                    }
                    Err(_) => omission = Some("unavailable"),
                },
                Ok(None) => omission = Some("binary"),
                Err(_) => omission = Some("unavailable"),
            }
            if text.contains("Binary files ") || text.contains("GIT binary patch") {
                omission = Some("binary");
            }
        }
        files.push(json!({"path":path,"status":format!("{:?}",delta.status()),"diff":text,"omission":omission}));
    }
    let after = repo
        .head()
        .ok()
        .and_then(|h| h.target())
        .map(|h| h.to_string());
    // Changes detected here are a lower bound: this is an observation, not an atomic Git snapshot.
    Ok(
        json!({"available":true,"baseOid":base,"headOid":head,"files":files,"concurrentChangeDetected":head!=after,"atomic":false,"errors":[]}),
    )
}

pub(crate) fn outcome(status: &str) -> ExecutionOutcome {
    match status {
        "completed" => ExecutionOutcome::Completed,
        "failed" => ExecutionOutcome::Failed,
        "interrupted" => ExecutionOutcome::Interrupted,
        "inProgress" => ExecutionOutcome::Running,
        _ => ExecutionOutcome::Unknown,
    }
}

pub(crate) fn assemble(
    instance: &RunInstance,
    turn_id: String,
    kind: CaptureKind,
    request_id: String,
    label: String,
    turn: Option<Value>,
    events: Vec<Value>,
    git: Value,
    mut errors: Vec<String>,
) -> RunCapture {
    let status = turn
        .as_ref()
        .and_then(|v| v.get("status"))
        .and_then(Value::as_str)
        .map(outcome)
        .unwrap_or_else(|| instance.execution_outcome.clone());
    let items = if let Some(items) = turn
        .as_ref()
        .and_then(|v| v.get("items"))
        .and_then(Value::as_array)
    {
        items.clone()
    } else {
        errors.push("Full turn history unavailable; showing recorded completed items".into());
        events
            .iter()
            .filter(|e| e["method"] == "item/completed")
            .filter_map(|e| e.pointer("/params/item").cloned())
            .collect()
    };
    let usage = events
        .iter()
        .rev()
        .find(|e| {
            e["method"] == "thread/tokenUsage/updated"
                && e.pointer("/params/turnId").and_then(Value::as_str) == Some(&turn_id)
        })
        .and_then(|e| e.pointer("/params/tokenUsage/last").cloned());
    let observed = json!({"modelReroutes":events.iter().filter(|e| e["method"] == "model/rerouted").collect::<Vec<_>>()});
    if let Some(git_errors) = git["errors"].as_array() {
        errors.extend(
            git_errors
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string),
        );
    }
    let terminal = matches!(
        status,
        ExecutionOutcome::Completed | ExecutionOutcome::Failed | ExecutionOutcome::Interrupted
    );
    if !terminal {
        errors.push("Turn is not known to be terminal".into());
    }
    let complete = errors.is_empty();
    RunCapture {
        execution_id: None,
        schema_version: 1,
        id: id(),
        instance_id: instance.id.clone(),
        request_id,
        kind,
        turn_id,
        label,
        created_at: now(),
        status,
        items,
        usage,
        observed,
        git,
        errors,
        complete,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    #[test]
    fn git_evidence_retains_commits_dirty_state_untracked_and_binary_without_mutation() {
        let dir = std::env::temp_dir().join(format!("run-git-{}", id()));
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init"]);
        git(&dir, &["config", "user.email", "fixture@example.invalid"]);
        git(&dir, &["config", "user.name", "Fixture"]);
        std::fs::write(dir.join("existing"), "original\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-m", "base"]);
        let repo = Repository::open(&dir).unwrap();
        let base = repo.head().unwrap().target().unwrap().to_string();
        std::fs::write(dir.join("existing"), "pre-existing edit\n").unwrap();
        let baseline = git_snapshot(dir.to_str().unwrap(), Some(&base));
        assert!(baseline["files"][0]["diff"]
            .as_str()
            .unwrap()
            .contains("pre-existing edit"));
        std::fs::write(dir.join("committed"), "run commit\n").unwrap();
        git(&dir, &["add", "committed"]);
        git(&dir, &["commit", "-m", "run"]);
        std::fs::write(dir.join("staged"), "stage\n").unwrap();
        git(&dir, &["add", "staged"]);
        std::fs::write(dir.join("untracked"), "untracked\n").unwrap();
        std::fs::write(dir.join("binary"), [0, 1, 0, 255]).unwrap();
        let before_head = repo.head().unwrap().target();
        let index = std::fs::read(repo.path().join("index")).unwrap();
        let snapshot = git_snapshot(dir.to_str().unwrap(), Some(&base));
        let files = snapshot["files"].as_array().unwrap();
        for name in ["existing", "committed", "staged", "untracked", "binary"] {
            assert!(
                files.iter().any(|f| f["path"] == name),
                "missing {name}: {snapshot}"
            );
        }
        assert_eq!(
            files.iter().find(|f| f["path"] == "binary").unwrap()["omission"],
            "binary"
        );
        assert_eq!(repo.head().unwrap().target(), before_head);
        assert_eq!(std::fs::read(repo.path().join("index")).unwrap(), index);
    }
}

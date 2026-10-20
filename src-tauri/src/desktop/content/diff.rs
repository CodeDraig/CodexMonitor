use std::collections::HashMap;

use egui::{Color32, RichText, Ui};
use serde_json::Value;

use super::{hash_text, DiffMode, DiffResponse};

const MAX_DIFF_CACHE_ENTRIES: usize = 512;

#[derive(Debug, Clone)]
pub(super) struct DiffRow {
    pub kind: char,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub text: String,
}

#[derive(Debug, Clone)]
pub(super) struct CachedDiff {
    pub(super) revision: u64,
    pub(super) last_used: u64,
    pub(super) rows: Vec<DiffRow>,
    pub(super) binary: Option<String>,
}

pub(super) fn show_diff(
    cache: &mut HashMap<String, CachedDiff>,
    ui: &mut Ui,
    identity: &str,
    revision: u64,
    mode: DiffMode,
    last_used: u64,
) -> DiffResponse {
    if let Some(entry) = cache.get_mut(identity) {
        entry.last_used = last_used;
    }
    let Some(model) = cache.get(identity) else {
        ui.label(
            RichText::new("Preparing diff…")
                .italics()
                .color(Color32::GRAY),
        );
        return DiffResponse::default();
    };
    if model.revision != revision {
        ui.label(
            RichText::new("Preparing diff…")
                .italics()
                .color(Color32::GRAY),
        );
        return DiffResponse::default();
    }
    if let Some(reason) = &model.binary {
        ui.label(RichText::new(reason).italics().color(Color32::GRAY));
        return DiffResponse::default();
    }

    let mut response = DiffResponse::default();
    let dark = ui.visuals().dark_mode;
    let foreground = ui.visuals().text_color();
    let line_height = ui.text_style_height(&egui::TextStyle::Monospace).max(16.0);
    egui::ScrollArea::vertical()
        .id_salt(("diff-lines", identity))
        .auto_shrink([false, false])
        .show_rows(ui, line_height, model.rows.len(), |ui, rows| {
            response.visible_lines = rows.clone();
            for index in rows {
                let row = &model.rows[index];
                match mode {
                    DiffMode::Unified => {
                        let prefix = match row.kind {
                            '+' => '+',
                            '-' => '-',
                            _ => ' ',
                        };
                        let label = format!(
                            "{:>5} {:>5} {prefix} {}",
                            row.old_line.map(|n| n.to_string()).unwrap_or_default(),
                            row.new_line.map(|n| n.to_string()).unwrap_or_default(),
                            row.text
                        );
                        ui.add(
                            egui::Label::new(
                                RichText::new(label)
                                    .monospace()
                                    .color(row_color(row.kind, dark, foreground)),
                            )
                            .selectable(true),
                        );
                    }
                    DiffMode::Split => {
                        let paired_addition = if row.kind == '-' {
                            model.rows.get(index + 1).filter(|next| next.kind == '+')
                        } else {
                            None
                        };
                        if row.kind == '+'
                            && index > 0
                            && model
                                .rows
                                .get(index - 1)
                                .is_some_and(|previous| previous.kind == '-')
                        {
                            continue;
                        }
                        ui.columns(2, |columns| {
                            columns[0].add(
                                egui::Label::new(
                                    RichText::new(format!(
                                        "{:>5} {}",
                                        row.old_line.map(|n| n.to_string()).unwrap_or_default(),
                                        if row.kind == '+' { "" } else { &row.text }
                                    ))
                                    .monospace()
                                    .color(
                                        if row.kind == '-' {
                                            row_color('-', dark, foreground)
                                        } else {
                                            foreground
                                        },
                                    ),
                                )
                                .selectable(true),
                            );
                            columns[1].add(
                                egui::Label::new(
                                    RichText::new(format!(
                                        "{:>5} {}",
                                        paired_addition
                                            .and_then(|addition| addition.new_line)
                                            .or(row.new_line)
                                            .map(|n| n.to_string())
                                            .unwrap_or_default(),
                                        if let Some(addition) = paired_addition {
                                            &addition.text
                                        } else if row.kind == '-' {
                                            ""
                                        } else {
                                            &row.text
                                        }
                                    ))
                                    .monospace()
                                    .color(
                                        if paired_addition.is_some() || row.kind == '+' {
                                            row_color('+', dark, foreground)
                                        } else {
                                            foreground
                                        },
                                    ),
                                )
                                .selectable(true),
                            );
                        });
                    }
                }
            }
        });
    response
}

pub(super) fn should_prepare(
    cache: &HashMap<String, CachedDiff>,
    pending: &HashMap<String, u64>,
    identity: &str,
    revision: u64,
) -> bool {
    !cache
        .get(identity)
        .is_some_and(|entry| entry.revision == revision)
        && pending.get(identity) != Some(&revision)
}

pub(super) fn insert_cached_diff(
    cache: &mut HashMap<String, CachedDiff>,
    key: String,
    mut value: CachedDiff,
    last_used: u64,
) {
    if !cache.contains_key(&key) && cache.len() >= MAX_DIFF_CACHE_ENTRIES {
        if let Some(evict) = cache
            .iter()
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(key, _)| key.clone())
        {
            cache.remove(&evict);
        }
    }
    value.last_used = last_used;
    cache.insert(key, value);
}

fn row_color(kind: char, dark: bool, foreground: Color32) -> Color32 {
    if !dark {
        return match kind {
            '+' => Color32::from_rgb(25, 108, 58),
            '-' => Color32::from_rgb(175, 45, 45),
            '@' => Color32::from_rgb(42, 85, 145),
            _ => foreground,
        };
    }
    match kind {
        '+' => Color32::from_rgb(125, 224, 157),
        '-' => Color32::from_rgb(250, 139, 139),
        '@' => Color32::from_rgb(125, 170, 240),
        _ => foreground,
    }
}

pub(super) fn parse_diff(raw: &str) -> CachedDiff {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("binary files") || lower.contains("binary file") {
        return CachedDiff {
            revision: 0,
            last_used: 0,
            rows: Vec::new(),
            binary: Some(
                raw.lines()
                    .find(|line| line.to_ascii_lowercase().contains("binary"))
                    .unwrap_or("Binary file change supplied; textual diff is unavailable.")
                    .to_owned(),
            ),
        };
    }
    if raw.trim().is_empty() {
        return CachedDiff {
            revision: 0,
            last_used: 0,
            rows: Vec::new(),
            binary: Some("No textual diff content supplied.".into()),
        };
    }
    let mut rows = Vec::new();
    let mut old = 0usize;
    let mut new = 0usize;
    let mut pending_header = false;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("@@") {
            let old_part = rest.split_whitespace().next().unwrap_or("");
            let new_part = rest.split_whitespace().nth(1).unwrap_or("");
            old = range_start(old_part);
            new = range_start(new_part);
            rows.push(DiffRow {
                kind: '@',
                old_line: None,
                new_line: None,
                text: line.to_owned(),
            });
            pending_header = false;
            continue;
        }
        if line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("diff --git ")
            || line.starts_with("index ")
            || line.starts_with("\\ No newline")
        {
            if rows.is_empty() || pending_header {
                rows.push(DiffRow {
                    kind: ' ',
                    old_line: None,
                    new_line: None,
                    text: line.to_owned(),
                });
                pending_header = true;
            }
            continue;
        }
        let kind = line.chars().next().unwrap_or(' ');
        let text = if matches!(kind, '+' | '-' | ' ') {
            line[1..].to_owned()
        } else {
            line.to_owned()
        };
        let (old_line, new_line) = match kind {
            '+' => {
                let line_num = new;
                new += 1;
                (None, Some(line_num))
            }
            '-' => {
                let line_num = old;
                old += 1;
                (Some(line_num), None)
            }
            _ => {
                let a = old;
                let b = new;
                old += 1;
                new += 1;
                (Some(a), Some(b))
            }
        };
        rows.push(DiffRow {
            kind,
            old_line,
            new_line,
            text,
        });
        pending_header = false;
    }
    CachedDiff {
        revision: 0,
        last_used: 0,
        rows,
        binary: None,
    }
}

fn parse_line_arrays(diff: &Value) -> CachedDiff {
    let old: Vec<String> = diff["oldLines"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|line| line.as_str().unwrap_or("").to_owned())
        .collect();
    let new: Vec<String> = diff["newLines"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|line| line.as_str().unwrap_or("").to_owned())
        .collect();
    let mut rows = Vec::new();
    let shared = old.len().min(new.len());
    for i in 0..shared {
        if old[i] == new[i] {
            rows.push(DiffRow {
                kind: ' ',
                old_line: Some(i + 1),
                new_line: Some(i + 1),
                text: old[i].clone(),
            });
        } else {
            rows.push(DiffRow {
                kind: '-',
                old_line: Some(i + 1),
                new_line: None,
                text: old[i].clone(),
            });
            rows.push(DiffRow {
                kind: '+',
                old_line: None,
                new_line: Some(i + 1),
                text: new[i].clone(),
            });
        }
    }
    for (i, line) in old.iter().enumerate().skip(shared) {
        rows.push(DiffRow {
            kind: '-',
            old_line: Some(i + 1),
            new_line: None,
            text: line.clone(),
        });
    }
    for (i, line) in new.iter().enumerate().skip(shared) {
        rows.push(DiffRow {
            kind: '+',
            old_line: None,
            new_line: Some(i + 1),
            text: line.clone(),
        });
    }
    CachedDiff {
        revision: 0,
        last_used: 0,
        rows,
        binary: None,
    }
}

pub(super) fn parse_value(value: &Value, raw: &str) -> CachedDiff {
    if value["binary"] == true || value["isBinary"] == true || value["kind"] == "image" {
        let path = value["path"].as_str().unwrap_or("Binary or image change");
        let mime = value["mimeType"]
            .as_str()
            .map(|mime| format!(" ({mime})"))
            .unwrap_or_default();
        let dimensions = match (value["width"].as_u64(), value["height"].as_u64()) {
            (Some(width), Some(height)) => format!(" · {width}×{height}"),
            _ => String::new(),
        };
        return CachedDiff {
            revision: 0,
            last_used: 0,
            rows: Vec::new(),
            binary: Some(format!(
                "{path}{mime}{dimensions}; textual diff is unavailable."
            )),
        };
    }
    let raw = if raw.trim().is_empty() {
        value["diff"].as_str().unwrap_or(raw)
    } else {
        raw
    };
    if !raw.trim().is_empty() {
        parse_diff(raw)
    } else if value.is_object() && (value["oldLines"].is_array() || value["newLines"].is_array()) {
        parse_line_arrays(value)
    } else {
        parse_diff(raw)
    }
}

fn range_start(value: &str) -> usize {
    value
        .trim_start_matches(['-', '+'])
        .split(',')
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hunks_and_line_number_transitions() {
        let model = parse_diff("@@ -3,2 +4,2 @@\n same\n-old\n+new\n");
        assert_eq!(model.rows[0].kind, '@');
        assert_eq!(model.rows[1].old_line, Some(3));
        assert_eq!(model.rows[1].new_line, Some(4));
        assert_eq!(model.rows[2].old_line, Some(4));
        assert_eq!(model.rows[3].new_line, Some(5));
    }

    #[test]
    fn binary_changes_have_visible_metadata_fallback() {
        let model = parse_diff("Binary files a/image.png and b/image.png differ");
        assert!(model.binary.as_deref().unwrap().contains("Binary files"));
        assert!(model.rows.is_empty());
    }

    #[test]
    fn structured_old_and_new_lines_keep_unicode() {
        let model = parse_line_arrays(
            &serde_json::json!({"oldLines":["cariad 🐉"],"newLines":["cariad ✨"]}),
        );
        assert_eq!(model.rows.len(), 2);
        assert_eq!(model.rows[0].text, "cariad 🐉");
        assert_eq!(model.rows[1].text, "cariad ✨");
    }

    #[test]
    fn complete_patch_wins_over_a_truncated_old_or_new_line_array() {
        let value = serde_json::json!({
            "oldLines": ["old line"],
            "newLines": null,
            "diff": "@@ -1,1 +1,2 @@\n-old line\n+new line\n+second new line"
        });
        let model = parse_value(&value, "");
        assert!(model
            .rows
            .iter()
            .any(|row| row.kind == '+' && row.text == "new line"));
        assert!(model
            .rows
            .iter()
            .any(|row| row.kind == '+' && row.text == "second new line"));
    }

    #[test]
    fn image_metadata_fallback_is_descriptive() {
        let model = parse_value(
            &serde_json::json!({"kind":"image","path":"art.png","mimeType":"image/png","width":16,"height":9}),
            "",
        );
        let text = model.binary.unwrap();
        assert!(text.contains("art.png"));
        assert!(text.contains("image/png"));
        assert!(text.contains("16×9"));
    }

    #[test]
    fn hundred_thousand_line_diff_builds_only_visible_widgets() {
        let source = format!(
            "@@ -1,100000 +1,100000 @@\n{}",
            (0..100_000)
                .map(|line| format!(" line {line} 🐉\n"))
                .collect::<String>()
        );
        let diff = Value::String(source);
        let context = egui::Context::default();
        let mut view = super::super::DiffView::default();
        let mut visible = 0..0;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::Vec2::new(900.0, 600.0),
            )),
            ..Default::default()
        };
        let mut output = context.run_ui(input, |ui| {
            visible = view
                .show(ui, "large-diff", &diff, DiffMode::Unified)
                .visible_lines;
        });
        output.textures_delta.clear();
        assert!(!visible.is_empty());
        assert!(visible.start < 100_000);
        assert!(visible.end <= 100_000);
        assert!(visible.len() < 100, "rendered {} rows", visible.len());
        assert_eq!(view.cache["large-diff"].rows.len(), 100_001);
    }

    #[test]
    fn diff_cache_is_bounded_without_replacing_an_existing_key() {
        let mut cache = HashMap::new();
        let value = parse_diff(" line");
        for index in 0..MAX_DIFF_CACHE_ENTRIES {
            insert_cached_diff(
                &mut cache,
                format!("diff-{index}"),
                value.clone(),
                index as u64,
            );
        }
        insert_cached_diff(&mut cache, "diff-0".into(), value.clone(), 50_000);
        insert_cached_diff(&mut cache, "new-visible-diff".into(), value, 50_001);
        assert_eq!(cache.len(), MAX_DIFF_CACHE_ENTRIES);
        assert!(cache.contains_key("diff-0"));
        assert!(cache.contains_key("new-visible-diff"));
    }

    #[test]
    fn version_gate_skips_unchanged_payload_without_hashing_or_parsing() {
        let mut cache = HashMap::new();
        let pending = HashMap::new();
        assert!(should_prepare(&cache, &pending, "file.rs", 7));
        let mut parsed = parse_value(&Value::String("@@ -1 +1 @@\n-old\n+new".into()), "");
        parsed.revision = 7;
        insert_cached_diff(&mut cache, "file.rs".into(), parsed, 1);
        assert!(!should_prepare(&cache, &pending, "file.rs", 7));
        assert!(should_prepare(&cache, &pending, "file.rs", 8));
        let pending = HashMap::from([("file.rs".to_owned(), 8)]);
        assert!(!should_prepare(&cache, &pending, "file.rs", 8));
        // A stale completion must not become authoritative after a newer request.
        assert_ne!(pending.get("file.rs"), Some(&7));
    }
}

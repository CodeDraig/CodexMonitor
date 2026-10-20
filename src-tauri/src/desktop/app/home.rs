//! Home surface for existing local/daemon usage data and cached conversations.
//! It only emits effects; it never initializes Codex or creates a conversation.

use crate::{
    client::contracts::BackendScope, desktop::controller::State as ControllerState,
    types::LocalUsageSnapshot,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HomeUsageQuery {
    pub backend_scope: BackendScope,
    pub days: u32,
    pub workspace_id: Option<String>,
    pub workspace_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HomeEffect {
    LoadUsage(HomeUsageQuery),
    SelectConversation {
        workspace_id: String,
        thread_id: String,
    },
}

pub(crate) struct HomePanel {
    backend_scope: Option<BackendScope>,
    current_workspace_id: Option<String>,
    workspace_filter: Option<String>,
    days: u32,
    requested_query: Option<HomeUsageQuery>,
    loading_query: Option<HomeUsageQuery>,
    snapshot: Option<LocalUsageSnapshot>,
    error: Option<String>,
}

impl Default for HomePanel {
    fn default() -> Self {
        Self {
            backend_scope: None,
            current_workspace_id: None,
            workspace_filter: None,
            days: 30,
            requested_query: None,
            loading_query: None,
            snapshot: None,
            error: None,
        }
    }
}

impl HomePanel {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Returns a tray summary only while the loaded snapshot still matches
    /// the active backend, range, and workspace selection.
    pub(crate) fn tray_usage_summary(&self) -> Option<String> {
        let query = self.requested_query.as_ref()?;
        if self.loading_query.is_some()
            || query.days != self.days
            || query.workspace_id != self.workspace_filter
            || self.backend_scope.as_ref() != Some(&query.backend_scope)
        {
            return None;
        }
        let snapshot = self.snapshot.as_ref()?;
        let total_tokens: i64 = snapshot.days.iter().map(|day| day.total_tokens).sum();
        Some(format!(
            "{}-day total: {} tokens",
            query.days,
            format_count(total_tokens)
        ))
    }

    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        backend_scope: &BackendScope,
        state: &ControllerState,
        current_workspace_id: Option<&str>,
    ) -> Vec<HomeEffect> {
        self.sync_context(backend_scope, current_workspace_id, state);
        let mut effects = Vec::new();
        ui.heading("Home");
        ui.horizontal_wrapped(|ui| {
            ui.label("Usage scope");
            let selected_label = self
                .workspace_filter
                .as_deref()
                .and_then(|id| {
                    state
                        .workspaces
                        .iter()
                        .find(|workspace| workspace.id.as_str() == id)
                })
                .map(|workspace| workspace.name.as_str())
                .unwrap_or("All workspaces");
            egui::ComboBox::from_id_salt("home-usage-scope")
                .selected_text(selected_label)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.workspace_filter, None, "All workspaces");
                    if let Some(workspace) = self
                        .workspace_filter
                        .as_deref()
                        .and_then(|id| {
                            state
                                .workspaces
                                .iter()
                                .find(|workspace| workspace.id.as_str() == id)
                        })
                        .or_else(|| {
                            self.current_workspace_id.as_deref().and_then(|id| {
                                state
                                    .workspaces
                                    .iter()
                                    .find(|workspace| workspace.id.as_str() == id)
                            })
                        })
                    {
                        ui.selectable_value(
                            &mut self.workspace_filter,
                            Some(workspace.id.clone()),
                            format!("Current workspace · {}", workspace.name),
                        );
                    }
                });
            ui.label("Range");
            egui::ComboBox::from_id_salt("home-usage-days")
                .selected_text(format!("{} days", self.days))
                .show_ui(ui, |ui| {
                    for days in [7, 30, 90] {
                        ui.selectable_value(&mut self.days, days, format!("{days} days"));
                    }
                });
            if ui.button("Refresh usage").clicked() {
                self.requested_query = None;
                self.snapshot = None;
                self.error = None;
            }
        });

        let query = self.query(backend_scope, state);
        if self.requested_query.as_ref() != Some(&query) {
            self.requested_query = Some(query.clone());
            self.loading_query = Some(query.clone());
            self.snapshot = None;
            self.error = None;
            effects.push(HomeEffect::LoadUsage(query.clone()));
        }

        if self.loading_query.as_ref() == Some(&query) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Reading local usage history…");
            });
        } else if let Some(error) = &self.error {
            ui.colored_label(egui::Color32::LIGHT_RED, error);
            if ui.button("Retry usage read").clicked() {
                self.requested_query = None;
            }
        } else if let Some(snapshot) = self.snapshot.as_ref() {
            render_usage(ui, snapshot, self.days);
        } else {
            ui.label("Usage data is not available yet.");
        }

        ui.add_space(18.0);
        ui.separator();
        ui.heading("Latest conversations");
        let recent = recent_threads(state);
        if recent.is_empty() {
            ui.label("No conversations have been loaded yet.");
        } else {
            for (
                workspace_id,
                workspace_name,
                thread_id,
                thread_title,
                updated_at,
                processing,
                unread,
            ) in recent
            {
                let title = if thread_title.trim().is_empty() {
                    "Untitled conversation"
                } else {
                    thread_title
                };
                let mut label = format!("{title} · {workspace_name}");
                if processing {
                    label.push_str(" · working");
                }
                if unread {
                    label.push_str(" · unread");
                }
                if updated_at > 0 {
                    label.push_str(&format!(" · {}", format_timestamp(updated_at)));
                }
                if ui.selectable_label(false, label).clicked() {
                    effects.push(HomeEffect::SelectConversation {
                        workspace_id: workspace_id.to_owned(),
                        thread_id: thread_id.to_owned(),
                    });
                }
            }
        }
        effects
    }

    pub(crate) fn complete_usage(
        &mut self,
        query: &HomeUsageQuery,
        result: Result<LocalUsageSnapshot, String>,
    ) -> bool {
        if self.backend_scope.as_ref() != Some(&query.backend_scope)
            || self.query_from_selection(
                query.days,
                query.workspace_id.as_deref(),
                query.workspace_path.as_deref(),
            ) != *query
            || self.loading_query.as_ref() != Some(query)
        {
            return false;
        }
        self.loading_query = None;
        match result {
            Ok(snapshot) => {
                self.snapshot = Some(snapshot);
                self.error = None;
            }
            Err(error) => {
                self.snapshot = None;
                self.error = Some(error);
            }
        }
        true
    }

    fn sync_context(
        &mut self,
        backend_scope: &BackendScope,
        current_workspace_id: Option<&str>,
        state: &ControllerState,
    ) {
        if self.backend_scope.as_ref() != Some(backend_scope) {
            self.backend_scope = Some(backend_scope.clone());
            self.workspace_filter = current_workspace_id
                .filter(|id| {
                    state
                        .workspaces
                        .iter()
                        .any(|workspace| workspace.id.as_str() == *id)
                })
                .map(str::to_owned);
            self.requested_query = None;
            self.loading_query = None;
            self.snapshot = None;
            self.error = None;
        }
        let next_current_workspace_id = current_workspace_id
            .filter(|id| {
                state
                    .workspaces
                    .iter()
                    .any(|workspace| workspace.id.as_str() == *id)
            })
            .map(str::to_owned);
        if self.current_workspace_id != next_current_workspace_id && self.workspace_filter.is_some()
        {
            self.workspace_filter = next_current_workspace_id.clone();
            self.requested_query = None;
            self.loading_query = None;
            self.snapshot = None;
            self.error = None;
        }
        self.current_workspace_id = next_current_workspace_id;
        if self.workspace_filter.as_deref().is_some_and(|id| {
            !state
                .workspaces
                .iter()
                .any(|workspace| workspace.id.as_str() == id)
        }) {
            self.workspace_filter = None;
            self.requested_query = None;
            self.loading_query = None;
            self.snapshot = None;
            self.error = None;
        }
    }

    fn query(&self, scope: &BackendScope, state: &ControllerState) -> HomeUsageQuery {
        let workspace = self.workspace_filter.as_deref().and_then(|id| {
            state
                .workspaces
                .iter()
                .find(|workspace| workspace.id.as_str() == id)
        });
        let mut query = self.query_from_selection(
            self.days,
            workspace.map(|workspace| workspace.id.as_str()),
            workspace.map(|workspace| workspace.path.as_str()),
        );
        query.backend_scope = scope.clone();
        query
    }

    fn query_from_selection(
        &self,
        days: u32,
        workspace_id: Option<&str>,
        workspace_path: Option<&str>,
    ) -> HomeUsageQuery {
        HomeUsageQuery {
            backend_scope: self.backend_scope.clone().unwrap_or_else(|| BackendScope {
                endpoint_id: String::new(),
                connection_epoch: 0,
                store_id: None,
            }),
            days,
            workspace_id: workspace_id.map(str::to_owned),
            workspace_path: workspace_path.map(str::to_owned),
        }
    }
}

fn render_usage(ui: &mut egui::Ui, snapshot: &LocalUsageSnapshot, days: u32) {
    let total: i64 = snapshot.days.iter().map(|day| day.total_tokens).sum();
    ui.columns(3, |columns| {
        metric(&mut columns[0], "Total tokens", format_count(total));
        metric(
            &mut columns[1],
            "Input · cached",
            format!(
                "{} · {} cached",
                format_count(snapshot.days.iter().map(|day| day.input_tokens).sum()),
                format_count(
                    snapshot
                        .days
                        .iter()
                        .map(|day| day.cached_input_tokens)
                        .sum()
                )
            ),
        );
        metric(
            &mut columns[2],
            "Output tokens",
            format_count(snapshot.days.iter().map(|day| day.output_tokens).sum()),
        );
    });
    ui.label(format!(
        "{} days · average {} tokens per day · cache hit {:.1}%",
        days,
        format_count(snapshot.totals.average_daily_tokens),
        snapshot.totals.cache_hit_rate_percent
    ));
    if let (Some(day), tokens) = (
        snapshot.totals.peak_day.as_deref(),
        snapshot.totals.peak_day_tokens,
    ) {
        if tokens > 0 {
            ui.label(format!("Peak day: {day} · {} tokens", format_count(tokens)));
        }
    }

    ui.add_space(8.0);
    ui.strong("Daily usage");
    if !has_recorded_usage(&snapshot.days) {
        ui.label("No recorded usage in this range.");
    } else {
        egui::ScrollArea::vertical()
            .id_salt("home-daily-usage")
            .max_height(180.0)
            .show(ui, |ui| {
                let max = snapshot
                    .days
                    .iter()
                    .map(|day| day.total_tokens)
                    .max()
                    .unwrap_or(0)
                    .max(1);
                for day in snapshot.days.iter().rev() {
                    let ratio = (day.total_tokens.max(0) as f32 / max as f32).clamp(0.0, 1.0);
                    ui.add(egui::ProgressBar::new(ratio).text(format!(
                        "{} · {} tokens",
                        day.day,
                        format_count(day.total_tokens)
                    )));
                }
            });
    }
    ui.add_space(8.0);
    ui.strong("Models");
    if snapshot.top_models.is_empty() {
        ui.label("No model usage recorded in this range.");
    } else {
        for model in &snapshot.top_models {
            ui.label(format!(
                "{} · {} tokens · {:.1}%",
                model.model,
                format_count(model.tokens),
                model.share_percent
            ));
        }
    }
}

fn has_recorded_usage(days: &[crate::types::LocalUsageDay]) -> bool {
    days.iter().any(|day| day.total_tokens > 0)
}

fn metric(ui: &mut egui::Ui, name: &str, value: String) {
    ui.group(|ui| {
        ui.label(name);
        ui.heading(value);
    });
}

fn format_count(value: i64) -> String {
    let digits = value.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            formatted.push(',');
        }
        formatted.push(character);
    }
    formatted
}

fn recent_threads(state: &ControllerState) -> Vec<(&str, &str, &str, &str, i64, bool, bool)> {
    let mut rows = state
        .workspaces
        .iter()
        .flat_map(|workspace| {
            state
                .threads_by_workspace
                .get(&workspace.id)
                .into_iter()
                .flatten()
                .filter(|thread| !thread.hidden)
                .map(move |thread| {
                    (
                        workspace.id.as_str(),
                        workspace.name.as_str(),
                        thread.id.as_str(),
                        thread.title.as_str(),
                        thread.updated_at,
                        thread.processing,
                        thread.unread,
                    )
                })
        })
        .collect::<Vec<_>>();
    sort_recent_rows(&mut rows);
    rows
}

fn sort_recent_rows(rows: &mut Vec<(&str, &str, &str, &str, i64, bool, bool)>) {
    rows.sort_by(|a, b| b.4.cmp(&a.4));
    rows.truncate(8);
}

fn format_timestamp(timestamp_ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(timestamp_ms)
        .map(|timestamp| timestamp.format("%b %-d %H:%M").to_string())
        .unwrap_or_else(|| "time unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend(epoch: u64) -> BackendScope {
        BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: epoch,
            store_id: None,
        }
    }

    #[test]
    fn usage_query_is_scoped_to_backend_generation_and_workspace() {
        let mut panel = HomePanel::new();
        panel.backend_scope = Some(backend(1));
        let first = panel.query_from_selection(30, Some("w1"), Some("/tmp/project"));
        assert_eq!(first.days, 30);
        assert_eq!(first.workspace_path.as_deref(), Some("/tmp/project"));
        panel.backend_scope = Some(backend(2));
        let second = panel.query_from_selection(30, Some("w1"), Some("/tmp/project"));
        assert_ne!(first, second);
    }

    #[test]
    fn stale_usage_reply_is_ignored_after_scope_switch() {
        let mut panel = HomePanel::new();
        let state = ControllerState::default();
        panel.sync_context(&backend(1), None, &state);
        let query = panel.query(&backend(1), &state);
        panel.requested_query = Some(query.clone());
        panel.loading_query = Some(query.clone());
        panel.sync_context(&backend(2), None, &state);
        let snapshot: LocalUsageSnapshot = serde_json::from_value(serde_json::json!({
            "updatedAt": 1,
            "days": [],
            "totals": {
                "last7DaysTokens": 0,
                "last30DaysTokens": 0,
                "averageDailyTokens": 0,
                "cacheHitRatePercent": 0.0,
                "peakDay": null,
                "peakDayTokens": 0
            },
            "topModels": []
        }))
        .unwrap();
        assert!(!panel.complete_usage(&query, Ok(snapshot)));
        assert!(panel.snapshot.is_none());
    }

    #[test]
    fn latest_conversations_are_sorted_without_requesting_codex_data() {
        let mut recent = vec![
            ("w", "Project", "old", "Old", 1, false, false),
            ("w", "Project", "new", "New", 2, true, false),
        ];
        sort_recent_rows(&mut recent);
        assert_eq!(recent[0].2, "new");
        assert_eq!(recent[1].2, "old");
        assert!(HomePanel::default().requested_query.is_none());
    }

    #[test]
    fn empty_daily_usage_does_not_expand_into_a_stack_of_zero_bars() {
        let days = (0..30)
            .map(|index| crate::types::LocalUsageDay {
                day: format!("2026-10-{index:02}"),
                input_tokens: 0,
                cached_input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                agent_time_ms: 0,
                agent_runs: 0,
            })
            .collect::<Vec<_>>();
        assert!(!has_recorded_usage(&days));
        let mut with_usage = days;
        with_usage[17].total_tokens = 1;
        assert!(has_recorded_usage(&with_usage));
    }

    #[test]
    fn tray_usage_summary_requires_a_current_loaded_range_and_uses_that_range_total() {
        let mut panel = HomePanel::new();
        assert_eq!(panel.tray_usage_summary(), None);
        let query = HomeUsageQuery {
            backend_scope: backend(1),
            days: 30,
            workspace_id: None,
            workspace_path: None,
        };
        panel.backend_scope = Some(query.backend_scope.clone());
        panel.requested_query = Some(query.clone());
        panel.loading_query = Some(query.clone());
        assert!(panel.complete_usage(
            &query,
            Ok(serde_json::from_value(serde_json::json!({
                "updatedAt": 10,
                "days": [
                    {"day":"2026-10-19","inputTokens":12,"cachedInputTokens":2,"outputTokens":8,"totalTokens":20},
                    {"day":"2026-10-20","inputTokens":18,"cachedInputTokens":3,"outputTokens":12,"totalTokens":30}
                ],
                "totals": {
                    "last7DaysTokens": 999,
                    "last30DaysTokens": 888,
                    "averageDailyTokens": 42,
                    "cacheHitRatePercent": 7.5,
                    "peakDay": null,
                    "peakDayTokens": 0
                },
                "topModels": []
            })).unwrap())
        ));
        assert_eq!(
            panel.tray_usage_summary(),
            Some("30-day total: 50 tokens".into())
        );

        panel.days = 7;
        assert_eq!(panel.tray_usage_summary(), None);
    }
}

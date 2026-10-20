use super::NativeApp;

/// Bounded measurements supplied by eframe for completed frames. CPU time
/// includes application UI and rendering work and excludes the vsync wait.
#[derive(Default)]
pub(super) struct FrameMetrics {
    samples_ms: std::collections::VecDeque<f64>,
}

impl FrameMetrics {
    pub(super) fn record(&mut self, cpu_seconds: Option<f32>) {
        if let Some(seconds) = cpu_seconds.filter(|v| v.is_finite() && *v >= 0.0) {
            if self.samples_ms.len() == 600 {
                self.samples_ms.pop_front();
            }
            self.samples_ms.push_back(f64::from(seconds) * 1000.0);
        }
    }

    fn summary(&self) -> Option<(usize, f64, f64)> {
        let mut values = self.samples_ms.iter().copied().collect::<Vec<_>>();
        if values.is_empty() {
            return None;
        }
        values.sort_by(f64::total_cmp);
        let p95 = values[((values.len() * 95).div_ceil(100)).saturating_sub(1)];
        Some((values.len(), p95, *values.last().unwrap()))
    }
}

impl NativeApp {
    /// Draw the structural native diagnostics snapshot. The panel intentionally
    /// has no export/copy action and the snapshot omits transcript and prompt
    /// contents.
    pub(super) fn show_diagnostics(&mut self, ui: &mut egui::Ui) {
        if !self.debug_open {
            return;
        }

        let context = ui.ctx().clone();
        let snapshot = self.controller.diagnostics();
        let formatted =
            serde_json::to_string_pretty(&snapshot).unwrap_or_else(|_| snapshot.to_string());
        let mut open = self.debug_open;
        egui::Window::new("Native diagnostics")
            .open(&mut open)
            .default_size([640.0, 480.0])
            .show(&context, |ui| {
                ui.label("Structural state only. Conversation and prompt bodies are omitted.");
                ui.label(format!("Renderer: {}", self.graphics_info));
                if let Some((count, p95, max)) = self.frame_metrics.summary() {
                    ui.label(format!("Last {count} frames: p95 CPU {p95:.2} ms; maximum {max:.2} ms (excludes vsync)."));
                }
                ui.separator();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.monospace(formatted);
                    });
            });
        self.debug_open = open;
    }
}

#[cfg(test)]
mod tests {
    use super::FrameMetrics;

    #[test]
    fn frame_metrics_are_bounded_and_ignore_invalid_samples() {
        let mut metrics = FrameMetrics::default();
        metrics.record(None);
        metrics.record(Some(f32::NAN));
        metrics.record(Some(-1.0));
        assert!(metrics.summary().is_none());
        for _ in 0..650 {
            metrics.record(Some(0.010));
        }
        metrics.record(Some(0.100));
        let (count, p95, max) = metrics.summary().unwrap();
        assert_eq!(count, 600);
        assert!((p95 - 10.0).abs() < 0.001);
        assert!((max - 100.0).abs() < 0.001);
    }
}

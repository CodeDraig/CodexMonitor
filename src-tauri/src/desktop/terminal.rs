//! Native egui terminal panel backed by portable-pty and Alacritty's VT model.

use std::{
    io::Read,
    path::Path,
    sync::{mpsc, Arc},
    thread,
    time::{Duration, Instant},
};

use crate::shared::terminal_core::{
    TerminalColor, TerminalGridSnapshot, TerminalModel, TerminalSession,
};
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Sense, Vec2};

enum ReaderMessage {
    Bytes(Vec<u8>),
    End,
    Error(String),
}

const READER_QUEUE_MESSAGES: usize = 32;
const READER_DRAIN_MAX_BYTES: usize = 64 * 1024;
const READER_DRAIN_MAX_TIME: Duration = Duration::from_millis(4);
const WRITER_CLOSE_ACK_TIMEOUT: Duration = Duration::from_millis(500);
const TERMINAL_REAP_TIMEOUT: Duration = Duration::from_millis(500);

enum PtyAction {
    Write(Vec<u8>),
    Resize(u16, u16),
    Close(mpsc::Sender<Result<(), String>>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CellPoint {
    row: usize,
    col: usize,
}

/// The terminal is deliberately local-only until the daemon exposes PTY RPCs.
/// A remote caller gets the same explicit state instead of a local-shell fallback.
pub struct TerminalPanel {
    workspace_id: String,
    terminal_id: String,
    session: Option<Arc<TerminalSession>>,
    pty_tx: Option<mpsc::Sender<PtyAction>>,
    model: TerminalModel,
    rx: Option<mpsc::Receiver<ReaderMessage>>,
    pending_reader_bytes: Vec<u8>,
    pending_reader_offset: usize,
    error: Option<String>,
    selection: Option<(CellPoint, CellPoint)>,
    dragging_from: Option<CellPoint>,
    last_size: (u16, u16),
    closed_by_child: bool,
    remote: bool,
}

impl TerminalPanel {
    pub fn new(workspace_id: impl Into<String>, terminal_id: impl Into<String>) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            terminal_id: terminal_id.into(),
            session: None,
            pty_tx: None,
            model: TerminalModel::new(80, 24, 10_000),
            rx: None,
            pending_reader_bytes: Vec::new(),
            pending_reader_offset: 0,
            error: None,
            selection: None,
            dragging_from: None,
            last_size: (80, 24),
            closed_by_child: false,
            remote: false,
        }
    }

    pub fn remote_unavailable_reason() -> &'static str {
        "Terminal sessions are currently available only for local workspaces."
    }

    pub fn new_remote(workspace_id: impl Into<String>, terminal_id: impl Into<String>) -> Self {
        let mut panel = Self::new(workspace_id, terminal_id);
        panel.remote = true;
        panel
    }

    pub fn open(
        &mut self,
        cwd: &Path,
        cols: u16,
        rows: u16,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<(), String> {
        if self.session.is_some() {
            return Ok(());
        }
        if self.remote {
            return Err(Self::remote_unavailable_reason().to_owned());
        }
        if !cwd.is_dir() {
            return Err(format!(
                "Terminal workspace directory does not exist: {}",
                cwd.display()
            ));
        }
        let shell = if cfg!(windows) {
            std::env::var("COMSPEC").unwrap_or_else(|_| "powershell.exe".into())
        } else {
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into())
        };
        #[cfg(windows)]
        let shell_args: Vec<&str> = if shell.to_ascii_lowercase().contains("powershell")
            || shell.to_ascii_lowercase().contains("pwsh")
        {
            vec!["-NoLogo", "-NoExit"]
        } else if shell.to_ascii_lowercase().contains("cmd") {
            vec!["/K"]
        } else {
            Vec::new()
        };
        #[cfg(not(windows))]
        let shell_args = vec!["-i"];
        let locale = std::env::var("LC_ALL")
            .or_else(|_| std::env::var("LANG"))
            .ok()
            .filter(|value| {
                value.to_ascii_lowercase().contains("utf-8")
                    || value.to_ascii_lowercase().contains("utf8")
            })
            .unwrap_or_else(|| "en_US.UTF-8".to_owned());
        let (session, reader) = TerminalSession::spawn(
            self.terminal_id.clone(),
            cwd,
            &shell,
            &shell_args,
            cols,
            rows,
            &locale,
        )?;
        let pty_tx = match start_writer(Arc::clone(&session), self.terminal_id.as_str()) {
            Ok(sender) => sender,
            Err(error) => {
                let _ = session.terminate_and_reap(TERMINAL_REAP_TIMEOUT);
                return Err(error);
            }
        };
        // Backpressure the PTY reader if the UI falls behind. The pipe and this
        // bounded queue then retain the stream instead of growing RAM without
        // limit while input is paused or the user is away.
        let (tx, rx) = mpsc::sync_channel(READER_QUEUE_MESSAGES);
        let thread_wake = Arc::clone(&wake);
        if let Err(error) = thread::Builder::new()
            .name(format!("terminal-reader-{}", self.terminal_id))
            .spawn(move || {
                let mut reader = reader;
                let mut buffer = [0u8; 8192];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => {
                            let _ = tx.send(ReaderMessage::End);
                            thread_wake();
                            break;
                        }
                        Ok(count) => {
                            if tx
                                .send(ReaderMessage::Bytes(buffer[..count].to_vec()))
                                .is_err()
                            {
                                break;
                            }
                            thread_wake();
                        }
                        Err(error) => {
                            let _ = tx.send(ReaderMessage::Error(error.to_string()));
                            thread_wake();
                            break;
                        }
                    }
                }
            })
        {
            let _ = close_writer(Some(&session), pty_tx, WRITER_CLOSE_ACK_TIMEOUT);
            return Err(format!("Failed to start terminal reader: {error}"));
        }
        self.model = TerminalModel::new(cols, rows, 10_000);
        self.last_size = (cols.max(2), rows.max(2));
        self.session = Some(session);
        self.pty_tx = Some(pty_tx);
        self.rx = Some(rx);
        self.pending_reader_bytes.clear();
        self.pending_reader_offset = 0;
        self.error = None;
        self.closed_by_child = false;
        Ok(())
    }

    pub fn write(&self, text: &str) -> Result<(), String> {
        self.session
            .as_ref()
            .ok_or_else(|| "Terminal is not open".to_string())?;
        self.pty_tx
            .as_ref()
            .ok_or_else(|| "Terminal writer is not open".to_string())?
            .send(PtyAction::Write(text.as_bytes().to_vec()))
            .map_err(|_| "Terminal writer has stopped".to_string())
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), String> {
        let size = (cols.max(2), rows.max(2));
        if size == self.last_size {
            return Ok(());
        }
        if self.session.is_some() {
            self.pty_tx
                .as_ref()
                .ok_or_else(|| "Terminal writer is not open".to_string())?
                .send(PtyAction::Resize(size.0, size.1))
                .map_err(|_| "Terminal writer has stopped".to_string())?;
        }
        self.model.resize(size.0, size.1);
        self.last_size = size;
        Ok(())
    }

    pub fn close(&mut self) -> Result<(), String> {
        // Keep the session alive until the writer acknowledges its ordered
        // close, or until the bounded synchronous kill/reap fallback finishes.
        let session = self.session.take();
        let result = self.pty_tx.take().map_or_else(
            || close_without_writer(session.as_deref()),
            |tx| close_writer(session.as_deref(), tx, WRITER_CLOSE_ACK_TIMEOUT),
        );
        self.rx = None;
        self.pending_reader_bytes.clear();
        self.pending_reader_offset = 0;
        self.selection = None;
        self.dragging_from = None;
        result
    }

    fn drain_reader(&mut self, ctx: &egui::Context) -> bool {
        let started = Instant::now();
        let mut consumed = 0;
        while consumed < READER_DRAIN_MAX_BYTES && started.elapsed() < READER_DRAIN_MAX_TIME {
            if self.pending_reader_offset == self.pending_reader_bytes.len() {
                self.pending_reader_bytes.clear();
                self.pending_reader_offset = 0;
                let message = self.rx.as_ref().and_then(|rx| match rx.try_recv() {
                    Ok(message) => Some(message),
                    Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => None,
                });
                match message {
                    Some(ReaderMessage::Bytes(bytes)) => {
                        self.pending_reader_bytes = bytes;
                    }
                    Some(ReaderMessage::End) => {
                        self.reader_ended();
                        break;
                    }
                    Some(ReaderMessage::Error(error)) => {
                        self.reader_failed(error);
                        break;
                    }
                    None => break,
                }
            }

            let count = self
                .pending_reader_bytes
                .len()
                .saturating_sub(self.pending_reader_offset)
                .min(READER_DRAIN_MAX_BYTES - consumed);
            if count == 0 {
                break;
            }
            let end = self.pending_reader_offset + count;
            self.model
                .advance(&self.pending_reader_bytes[self.pending_reader_offset..end]);
            self.pending_reader_offset = end;
            consumed += count;
        }

        // Pull at most one message ahead so backlog can be detected without
        // draining the channel or losing its FIFO ordering.
        if self.pending_reader_offset == self.pending_reader_bytes.len() && self.rx.is_some() {
            self.pending_reader_bytes.clear();
            self.pending_reader_offset = 0;
            let message = self.rx.as_ref().and_then(|rx| match rx.try_recv() {
                Ok(message) => Some(message),
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => None,
            });
            match message {
                Some(ReaderMessage::Bytes(bytes)) => self.pending_reader_bytes = bytes,
                Some(ReaderMessage::End) => self.reader_ended(),
                Some(ReaderMessage::Error(error)) => self.reader_failed(error),
                None => {}
            }
        }
        let backlog = self.pending_reader_offset < self.pending_reader_bytes.len();
        if backlog {
            ctx.request_repaint_after(Duration::from_millis(1));
        }
        self.write_terminal_responses();
        backlog
    }

    fn reader_ended(&mut self) {
        self.closed_by_child = true;
        self.session = None;
        self.rx = None;
        self.pty_tx = None;
    }

    fn reader_failed(&mut self, error: String) {
        self.error = Some(error);
        self.reader_ended();
    }

    fn write_terminal_responses(&self) {
        if self.session.is_none() {
            let _ = self.model.take_events();
            return;
        }
        for event in self.model.take_events() {
            match event {
                crate::shared::terminal_core::TerminalEvent::PtyWrite(data) => {
                    let _ = self.write(&data);
                }
                crate::shared::terminal_core::TerminalEvent::ClipboardStore(text) => {
                    if let Ok(mut clipboard) = arboard::Clipboard::new() {
                        let _ = clipboard.set_text(text);
                    }
                }
                crate::shared::terminal_core::TerminalEvent::ClipboardLoad(format) => {
                    let text = arboard::Clipboard::new()
                        .and_then(|mut clipboard| clipboard.get_text())
                        .unwrap_or_default();
                    let _ = self.write(&format(&text));
                }
                crate::shared::terminal_core::TerminalEvent::TextAreaSizeRequest(format) => {
                    let size = alacritty_terminal::event::WindowSize {
                        num_lines: self.last_size.1,
                        num_cols: self.last_size.0,
                        cell_width: 9,
                        cell_height: 18,
                    };
                    let _ = self.write(&format(size));
                }
                crate::shared::terminal_core::TerminalEvent::Title(_)
                | crate::shared::terminal_core::TerminalEvent::Bell => {}
            }
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        if self.remote {
            ui.weak(Self::remote_unavailable_reason());
            return;
        }
        self.drain_reader(ui.ctx());
        if self.session.is_none() && !self.closed_by_child && self.error.is_none() {
            ui.weak(format!("{} · terminal session closed", self.workspace_id));
            return;
        }
        let accessibility_label = format!(
            "Terminal {} in workspace {}",
            self.terminal_id, self.workspace_id
        );
        if let Some(error) = self.error.as_ref() {
            ui.colored_label(Color32::LIGHT_RED, format!("Terminal error: {error}"));
        } else if self.closed_by_child {
            ui.weak("Shell exited");
        }
        let font = FontId::monospace(14.0);
        let cell_size = ui.fonts_mut(|fonts| fonts.glyph_width(&font, 'M'));
        let cell_height = ui.text_style_height(&egui::TextStyle::Monospace) * (14.0 / 14.0);
        let available = ui.available_size().max(Vec2::splat(1.0));
        let cols = (available.x / cell_size.max(1.0)).floor().max(2.0) as u16;
        let rows = (available.y / cell_height.max(1.0)).floor().max(2.0) as u16;
        if let Err(error) = self.resize(cols, rows) {
            self.error = Some(error);
        }
        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(f32::from(cols) * cell_size, f32::from(rows) * cell_height),
            Sense::click_and_drag(),
        );
        let snapshot = self.model.snapshot();
        let screen_text = snapshot_screen_text(&snapshot);
        response.widget_info(|| {
            let mut info = egui::WidgetInfo::labeled(
                egui::accesskit::Role::MultilineTextInput,
                true,
                accessibility_label.clone(),
            );
            info.current_text_value = Some(screen_text.clone());
            info
        });
        // This is a read-only terminal snapshot. The same response remains the
        // focus target used by mouse, keyboard, and AccessKit focus requests.
        response
            .ctx
            .accesskit_node_builder(response.id, |node| node.set_read_only());
        self.paint(ui, rect, &snapshot, cell_size, cell_height, &font);
        self.handle_input(ui, &response, rect, &snapshot, cell_size, cell_height);
        if response.hovered() {
            let scroll = ui.input(|input| input.smooth_scroll_delta.y);
            if scroll.abs() > 0.0 {
                let steps = (scroll / cell_height.max(1.0)).round() as i32;
                if snapshot.alternate_screen && snapshot.alternate_scroll {
                    let key = if steps > 0 { "\x1b[A" } else { "\x1b[B" };
                    for _ in 0..steps.unsigned_abs().min(32) {
                        let _ = self.write(key);
                    }
                } else {
                    self.model.scroll(steps);
                }
                ui.ctx().request_repaint();
            }
        }
    }

    fn paint(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        snapshot: &TerminalGridSnapshot,
        cw: f32,
        ch: f32,
        font: &FontId,
    ) {
        let painter = ui.painter().with_clip_rect(rect);
        painter.rect_filled(rect, 0.0, Color32::from_rgb(17, 20, 25));
        // Paint every cell background before glyphs so double-width glyphs
        // remain intact across their spacer column.
        for (row, line) in snapshot.lines.iter().enumerate().take(snapshot.screen_rows) {
            for (col, cell) in line.cells.iter().enumerate().take(snapshot.columns) {
                let selected = self.is_selected(row, col);
                let bg = if cell.inverse {
                    to_color(cell.foreground, true)
                } else {
                    to_color(cell.background, false)
                };
                let pos = Pos2::new(rect.left() + col as f32 * cw, rect.top() + row as f32 * ch);
                painter.rect_filled(Rect::from_min_size(pos, Vec2::new(cw, ch)), 0.0, bg);
                if selected {
                    painter.rect_filled(
                        Rect::from_min_size(pos, Vec2::new(cw, ch)),
                        0.0,
                        Color32::from_rgb(72, 104, 150),
                    );
                }
            }
        }
        for (row, line) in snapshot.lines.iter().enumerate().take(snapshot.screen_rows) {
            for (col, cell) in line.cells.iter().enumerate().take(snapshot.columns) {
                if cell.spacer {
                    continue;
                }
                let selected = self.is_selected(row, col);
                let mut fg = if cell.inverse {
                    to_color(cell.background, false)
                } else {
                    to_color(cell.foreground, true)
                };
                if selected {
                    fg = Color32::WHITE;
                }
                let pos = Pos2::new(rect.left() + col as f32 * cw, rect.top() + row as f32 * ch);
                painter.text(pos, egui::Align2::LEFT_TOP, &cell.text, font.clone(), fg);
                if cell.underline {
                    painter.line_segment(
                        [
                            Pos2::new(pos.x, pos.y + ch - 1.0),
                            Pos2::new(pos.x + cw, pos.y + ch - 1.0),
                        ],
                        egui::Stroke::new(1.0, fg),
                    );
                }
            }
        }
        if let Some((row, col)) = snapshot.cursor {
            if row < snapshot.screen_rows && col < snapshot.columns {
                let pos = Pos2::new(rect.left() + col as f32 * cw, rect.top() + row as f32 * ch);
                painter.rect_stroke(
                    Rect::from_min_size(pos, Vec2::new(cw, ch)),
                    0.0,
                    egui::Stroke::new(1.0, Color32::LIGHT_GRAY),
                    egui::StrokeKind::Inside,
                );
            }
        }
    }

    fn handle_input(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        rect: Rect,
        snapshot: &TerminalGridSnapshot,
        cw: f32,
        ch: f32,
    ) {
        if response.drag_started() {
            if let Some(pos) = response.interact_pointer_pos() {
                let point = point_at(pos, rect, cw, ch, snapshot);
                self.dragging_from = Some(point);
                self.selection = Some((point, point));
            }
        }
        if response.dragged() {
            if let (Some(start), Some(pos)) = (self.dragging_from, response.interact_pointer_pos())
            {
                self.selection = Some((start, point_at(pos, rect, cw, ch, snapshot)));
            }
        }
        if response.double_clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                let point = point_at(pos, rect, cw, ch, snapshot);
                self.selection = Some((point, point));
            }
        }
        if response.clicked() {
            response.request_focus();
        }
        if !response.has_focus() {
            return;
        }
        let events = ui.input(|input| input.events.clone());
        for event in events {
            match event {
                egui::Event::Text(text) => {
                    let _ = self.write(&text);
                }
                egui::Event::Paste(text) => {
                    let payload = if snapshot.bracketed_paste {
                        format!("\x1b[200~{text}\x1b[201~")
                    } else {
                        text
                    };
                    let _ = self.write(&payload);
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    repeat: _,
                    modifiers,
                    ..
                } => {
                    if modifiers.command && key == egui::Key::C {
                        if let Some(text) = self.selected_text(snapshot) {
                            ui.ctx().copy_text(text);
                        } else if modifiers.ctrl {
                            let _ = self.write("\x03");
                        }
                        continue;
                    }
                    if modifiers.command && key == egui::Key::V {
                        continue;
                    }
                    let sequence = key_sequence(
                        key,
                        snapshot.application_cursor,
                        modifiers.shift,
                        modifiers.ctrl,
                    );
                    if let Some(sequence) = sequence {
                        let _ = self.write(&sequence);
                    }
                }
                _ => {}
            }
        }
    }

    fn selected_text(&self, snapshot: &TerminalGridSnapshot) -> Option<String> {
        let (mut start, mut end) = self.selection?;
        if (start.row, start.col) > (end.row, end.col) {
            std::mem::swap(&mut start, &mut end);
        }
        let mut output = String::new();
        for row in start.row..=end.row.min(snapshot.lines.len().saturating_sub(1)) {
            let line = snapshot.lines.get(row)?;
            let from = if row == start.row { start.col } else { 0 };
            let to = if row == end.row {
                end.col
            } else {
                line.cells.len().saturating_sub(1)
            };
            for cell in line.cells.iter().take(to + 1).skip(from) {
                if cell.spacer {
                    continue;
                }
                output.push_str(&cell.text);
            }
            if row < end.row && !line.wrapped {
                output.push('\n');
            }
        }
        Some(output)
    }

    fn is_selected(&self, row: usize, col: usize) -> bool {
        let Some((mut start, mut end)) = self.selection else {
            return false;
        };
        if (start.row, start.col) > (end.row, end.col) {
            std::mem::swap(&mut start, &mut end);
        }
        (row, col) >= (start.row, start.col) && (row, col) <= (end.row, end.col)
    }
}

impl Drop for TerminalPanel {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn start_writer(
    worker_session: Arc<TerminalSession>,
    terminal_id: &str,
) -> Result<mpsc::Sender<PtyAction>, String> {
    let (pty_tx, pty_rx) = mpsc::channel();
    thread::Builder::new()
        .name(format!("terminal-writer-{terminal_id}"))
        .spawn(move || {
            while let Ok(action) = pty_rx.recv() {
                match action {
                    PtyAction::Write(bytes) => {
                        let _ = worker_session.write(&bytes);
                    }
                    PtyAction::Resize(cols, rows) => {
                        let _ = worker_session.resize(cols, rows);
                    }
                    PtyAction::Close(reply) => {
                        let result = worker_session.terminate_and_reap(TERMINAL_REAP_TIMEOUT);
                        let _ = reply.send(result);
                        break;
                    }
                }
            }
        })
        .map_err(|error| format!("Failed to start terminal writer: {error}"))?;
    Ok(pty_tx)
}

fn close_writer(
    session: Option<&TerminalSession>,
    tx: mpsc::Sender<PtyAction>,
    acknowledgement_timeout: Duration,
) -> Result<(), String> {
    let (reply_tx, reply_rx) = mpsc::channel();
    let queued_close = tx.send(PtyAction::Close(reply_tx));
    let graceful_result = queued_close
        .map_err(|_| "Terminal writer has stopped".to_string())
        .and_then(|()| {
            reply_rx
                .recv_timeout(acknowledgement_timeout)
                .map_err(|error| match error {
                    mpsc::RecvTimeoutError::Timeout => {
                        "Terminal writer did not acknowledge close before the deadline".to_string()
                    }
                    mpsc::RecvTimeoutError::Disconnected => {
                        "Terminal writer stopped before acknowledging close".to_string()
                    }
                })?
        });
    if graceful_result.is_ok() {
        return Ok(());
    }

    match session {
        Some(session) => match session.terminate_and_reap(TERMINAL_REAP_TIMEOUT) {
            Ok(()) => Ok(()),
            Err(error) => Err(format!(
                "{}; shutdown fallback failed: {error}",
                graceful_result.unwrap_err()
            )),
        },
        None => graceful_result,
    }
}

fn close_without_writer(session: Option<&TerminalSession>) -> Result<(), String> {
    session.map_or(Ok(()), |session| {
        session.terminate_and_reap(TERMINAL_REAP_TIMEOUT)
    })
}

fn point_at(pos: Pos2, rect: Rect, cw: f32, ch: f32, snapshot: &TerminalGridSnapshot) -> CellPoint {
    CellPoint {
        row: (((pos.y - rect.top()) / ch.max(1.0)).floor().max(0.0) as usize)
            .min(snapshot.screen_rows.saturating_sub(1)),
        col: (((pos.x - rect.left()) / cw.max(1.0)).floor().max(0.0) as usize)
            .min(snapshot.columns.saturating_sub(1)),
    }
}

fn snapshot_screen_text(snapshot: &TerminalGridSnapshot) -> String {
    snapshot
        .lines
        .iter()
        .take(snapshot.screen_rows)
        .map(|line| {
            line.cells
                .iter()
                .filter(|cell| !cell.spacer)
                .map(|cell| cell.text.as_str())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn key_sequence(key: egui::Key, app_cursor: bool, shift: bool, ctrl: bool) -> Option<String> {
    let sequence = match key {
        egui::Key::Enter => "\r",
        egui::Key::Tab => {
            if shift {
                "\x1b[Z"
            } else {
                "\t"
            }
        }
        egui::Key::Backspace => "\x7f",
        egui::Key::Escape => "\x1b",
        egui::Key::ArrowUp => {
            if app_cursor {
                "\x1bOA"
            } else {
                "\x1b[A"
            }
        }
        egui::Key::ArrowDown => {
            if app_cursor {
                "\x1bOB"
            } else {
                "\x1b[B"
            }
        }
        egui::Key::ArrowRight => {
            if app_cursor {
                "\x1bOC"
            } else {
                "\x1b[C"
            }
        }
        egui::Key::ArrowLeft => {
            if app_cursor {
                "\x1bOD"
            } else {
                "\x1b[D"
            }
        }
        egui::Key::Home => "\x1b[H",
        egui::Key::End => "\x1b[F",
        egui::Key::Delete => "\x1b[3~",
        egui::Key::Insert => "\x1b[2~",
        egui::Key::PageUp => "\x1b[5~",
        egui::Key::PageDown => "\x1b[6~",
        egui::Key::F1 => "\x1bOP",
        egui::Key::F2 => "\x1bOQ",
        egui::Key::F3 => "\x1bOR",
        egui::Key::F4 => "\x1bOS",
        egui::Key::F5 => "\x1b[15~",
        egui::Key::F6 => "\x1b[17~",
        egui::Key::F7 => "\x1b[18~",
        egui::Key::F8 => "\x1b[19~",
        egui::Key::F9 => "\x1b[20~",
        egui::Key::F10 => "\x1b[21~",
        egui::Key::F11 => "\x1b[23~",
        egui::Key::F12 => "\x1b[24~",
        egui::Key::A if ctrl => "\x01",
        egui::Key::B if ctrl => "\x02",
        egui::Key::C if ctrl => "\x03",
        egui::Key::D if ctrl => "\x04",
        egui::Key::E if ctrl => "\x05",
        egui::Key::F if ctrl => "\x06",
        egui::Key::K if ctrl => "\x0b",
        egui::Key::L if ctrl => "\x0c",
        egui::Key::N if ctrl => "\x0e",
        egui::Key::P if ctrl => "\x10",
        egui::Key::U if ctrl => "\x15",
        egui::Key::W if ctrl => "\x17",
        egui::Key::Z if ctrl => "\x1a",
        _ => return None,
    };
    Some(sequence.to_owned())
}

fn to_color(color: TerminalColor, foreground: bool) -> Color32 {
    match color {
        TerminalColor::DefaultForeground => Color32::from_rgb(220, 224, 230),
        TerminalColor::DefaultBackground => Color32::from_rgb(17, 20, 25),
        TerminalColor::Cursor => Color32::WHITE,
        TerminalColor::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        TerminalColor::Indexed(index) => {
            let index = index.min(255) as u8;
            const ANSI: [[u8; 3]; 16] = [
                [0, 0, 0],
                [205, 49, 49],
                [13, 188, 121],
                [229, 229, 16],
                [36, 114, 200],
                [188, 63, 188],
                [17, 168, 205],
                [229, 229, 229],
                [102, 102, 102],
                [241, 76, 76],
                [35, 209, 139],
                [245, 245, 67],
                [59, 142, 234],
                [214, 112, 214],
                [41, 184, 219],
                [255, 255, 255],
            ];
            let rgb = if index < 16 {
                ANSI[index as usize]
            } else if index < 232 {
                let i = index - 16;
                let level = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
                [level(i / 36), level((i / 6) % 6), level(i % 6)]
            } else {
                let gray = 8 + (index.saturating_sub(232)) * 10;
                [gray, gray, gray]
            };
            let _ = foreground;
            Color32::from_rgb(rgb[0], rgb[1], rgb[2])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::accesskit::Role;
    use egui_kittest::kittest::{NodeT as _, Queryable as _};
    use egui_kittest::Harness;

    #[test]
    fn arrow_keys_follow_application_cursor_mode() {
        assert_eq!(
            key_sequence(egui::Key::ArrowUp, false, false, false).as_deref(),
            Some("\x1b[A")
        );
        assert_eq!(
            key_sequence(egui::Key::ArrowUp, true, false, false).as_deref(),
            Some("\x1bOA")
        );
        assert_eq!(
            key_sequence(egui::Key::C, false, false, true).as_deref(),
            Some("\x03")
        );
    }

    #[test]
    fn selection_copy_preserves_unicode_and_soft_wraps() {
        let mut panel = TerminalPanel::new("w", "t");
        panel.model.advance("a€b".as_bytes());
        let snapshot = panel.model.snapshot();
        let row = 0;
        panel.selection = Some((CellPoint { row, col: 0 }, CellPoint { row, col: 2 }));
        assert_eq!(panel.selected_text(&snapshot).as_deref(), Some("a€b"));
    }

    #[test]
    fn terminal_canvas_is_named_focusable_and_exposes_read_only_screen_text() {
        let mut panel = TerminalPanel::new("workspace", "terminal-7");
        panel.closed_by_child = true;
        panel.model.advance(b"terminal output");
        let mut harness = Harness::new_ui(move |ui| panel.show(ui));

        let terminal = harness.get_by_label("Terminal terminal-7 in workspace workspace");
        let node = terminal.accesskit_node();
        assert_eq!(node.role(), Role::MultilineTextInput);
        assert!(node.data().is_read_only());
        assert!(node
            .value()
            .is_some_and(|text| text.starts_with("terminal output")));

        terminal.focus();
        harness.run();
        assert!(harness
            .get_by_label("Terminal terminal-7 in workspace workspace")
            .is_focused());
    }

    #[test]
    fn sustained_pty_stream_is_drained_in_bounded_frames_without_losing_cells() {
        let expected: Vec<u8> = (0..96 * 1024)
            .map(|index| b'a' + (index % 26) as u8)
            .collect();
        let mut panel = TerminalPanel::new("workspace", "terminal-stream");
        panel.model = TerminalModel::new(256, 512, 1024);
        panel.last_size = (256, 512);
        let (tx, rx) = mpsc::sync_channel(4);
        panel.rx = Some(rx);
        let producer_bytes = expected.clone();
        let producer = thread::spawn(move || {
            for chunk in producer_bytes.chunks(2048) {
                assert!(tx.send(ReaderMessage::Bytes(chunk.to_vec())).is_ok());
            }
            assert!(tx.send(ReaderMessage::End).is_ok());
        });

        let context = egui::Context::default();
        let mut frames = 0;
        while !panel.closed_by_child {
            panel.drain_reader(&context);
            frames += 1;
            if frames > 10_000 {
                panic!("PTY stream did not finish draining");
            }
            thread::yield_now();
        }
        producer.join().unwrap();

        assert!(frames > 1, "the byte budget should require multiple frames");
        let snapshot = panel.model.snapshot();
        let actual: Vec<u8> = snapshot
            .lines
            .iter()
            .flat_map(|line| line.cells.iter())
            .filter(|cell| !cell.spacer && cell.text != " ")
            .flat_map(|cell| cell.text.bytes())
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn remote_terminal_cannot_fall_back_to_a_local_shell() {
        let mut panel = TerminalPanel::new_remote("remote", "t");
        let error = panel
            .open(Path::new("/"), 80, 24, Arc::new(|| {}))
            .unwrap_err();
        assert_eq!(error, TerminalPanel::remote_unavailable_reason());
        assert!(panel.session.is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn close_kills_and_reaps_a_live_child_before_returning() {
        let cwd = Path::new("/");
        let (session, _reader) = TerminalSession::spawn(
            "terminal-shutdown-test".to_owned(),
            cwd,
            "/bin/sh",
            &["-c", "exec sleep 30"],
            80,
            24,
            "C.UTF-8",
        )
        .expect("spawn long-running terminal child");
        let pid = session
            .child
            .lock()
            .unwrap()
            .process_id()
            .expect("PTY child should expose its process id");
        let mut panel = TerminalPanel::new("workspace", "terminal-shutdown-test");
        panel.session = Some(Arc::clone(&session));
        panel.pty_tx = Some(start_writer(session, "terminal-shutdown-test").unwrap());

        let started = Instant::now();
        panel.close().expect("close should stop and reap the child");

        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            unsafe { libc::kill(pid as libc::pid_t, 0) },
            -1,
            "the child PID must be gone when close returns"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH),
            "kill(pid, 0) should report that the reaped PID no longer exists"
        );
        assert!(panel.session.is_none());
        assert!(panel.pty_tx.is_none());
    }
}

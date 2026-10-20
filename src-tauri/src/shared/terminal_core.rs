//! Host-independent terminal process and emulator state.
//!
//! The process half is used by the existing Tauri command adapter and by the
//! native desktop. The emulator half is only built for `native-gui`.

use std::{
    io::Write,
    thread,
    time::{Duration, Instant},
};

#[cfg(all(
    any(feature = "tauri-host", feature = "native-gui"),
    not(any(target_os = "ios", target_os = "android"))
))]
use std::{path::Path, sync::Arc};

#[cfg(all(
    any(feature = "tauri-host", feature = "native-gui"),
    not(any(target_os = "ios", target_os = "android"))
))]
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
#[cfg(all(
    any(feature = "tauri-host", feature = "native-gui"),
    not(any(target_os = "ios", target_os = "android"))
))]
use std::sync::Mutex;

/// A PTY session without a dependency on Tauri or any window/event system.
#[cfg(all(
    any(feature = "tauri-host", feature = "native-gui"),
    not(any(target_os = "ios", target_os = "android"))
))]
pub(crate) struct TerminalSession {
    pub(crate) id: String,
    pub(crate) master: Mutex<Box<dyn MasterPty + Send>>,
    pub(crate) writer: Mutex<Box<dyn Write + Send>>,
    pub(crate) child: Mutex<Box<dyn Child + Send>>,
}

#[cfg(all(
    any(feature = "tauri-host", feature = "native-gui"),
    not(any(target_os = "ios", target_os = "android"))
))]
impl TerminalSession {
    /// Spawn the configured shell in a new PTY. Caller owns reader/event
    /// delivery; this type only owns the process and its controlling handles.
    pub(crate) fn spawn(
        id: String,
        cwd: &Path,
        shell: &str,
        shell_args: &[&str],
        cols: u16,
        rows: u16,
        locale: &str,
    ) -> Result<(Arc<Self>, Box<dyn std::io::Read + Send>), String> {
        let system = native_pty_system();
        let size = PtySize {
            rows: rows.max(2),
            cols: cols.max(2),
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = system
            .openpty(size)
            .map_err(|error| format!("Failed to open pty: {error}"))?;

        let mut command = CommandBuilder::new(shell);
        command.cwd(cwd);
        for arg in shell_args {
            command.arg(arg);
        }
        command.env("TERM", "xterm-256color");
        command.env("LANG", locale);
        command.env("LC_ALL", locale);
        command.env("LC_CTYPE", locale);

        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| format!("Failed to spawn shell: {error}"))?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| format!("Failed to open pty reader: {error}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| format!("Failed to open pty writer: {error}"))?;

        Ok((
            Arc::new(Self {
                id,
                master: Mutex::new(pair.master),
                writer: Mutex::new(writer),
                child: Mutex::new(child),
            }),
            reader,
        ))
    }

    pub(crate) fn write(&self, bytes: &[u8]) -> Result<(), String> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| "Terminal writer lock poisoned".to_string())?;
        writer
            .write_all(bytes)
            .and_then(|()| writer.flush())
            .map_err(|error| format!("Failed to write to pty: {error}"))
    }

    pub(crate) fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        let size = PtySize {
            rows: rows.max(2),
            cols: cols.max(2),
            pixel_width: 0,
            pixel_height: 0,
        };
        self.master
            .lock()
            .map_err(|_| "Terminal master lock poisoned".to_string())?
            .resize(size)
            .map_err(|error| format!("Failed to resize pty: {error}"))
    }

    pub(crate) fn kill(&self) -> Result<(), String> {
        self.child
            .lock()
            .map_err(|_| "Terminal child lock poisoned".to_string())?
            .kill()
            .map_err(|error| format!("Failed to stop terminal: {error}"))
    }

    /// Signal the child, then poll until the process has been reaped or the
    /// caller's shutdown budget expires. This keeps app exit independent of a
    /// potentially unbounded `Child::wait` while still collecting normal exits.
    pub(crate) fn terminate_and_reap(&self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        let mut child = loop {
            match self.child.try_lock() {
                Ok(child) => break child,
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err("Terminal child lock poisoned".to_string());
                }
                Err(std::sync::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    return Err(
                        "Terminal child lock did not become available before shutdown deadline"
                            .into(),
                    );
                }
            }
        };
        if child
            .try_wait()
            .map_err(|error| format!("Failed to inspect terminal process: {error}"))?
            .is_some()
        {
            return Ok(());
        }
        child
            .kill()
            .map_err(|error| format!("Failed to stop terminal: {error}"))?;
        loop {
            if child
                .try_wait()
                .map_err(|error| format!("Failed to reap terminal process: {error}"))?
                .is_some()
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("Terminal process did not exit before shutdown deadline".into());
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(all(test, unix, feature = "native-gui"))]
mod pty_tests {
    use super::TerminalSession;
    use std::{io::Read, path::Path};

    #[test]
    fn pty_process_emits_output_and_can_be_stopped() {
        let cwd = Path::new("/");
        let (session, mut reader) = TerminalSession::spawn(
            "core-smoke".into(),
            cwd,
            "/bin/sh",
            &["-c", "printf '__PTY_OK__'; exec sleep 30"],
            80,
            24,
            "C.UTF-8",
        )
        .unwrap();
        let mut output = Vec::new();
        for _ in 0..20 {
            let mut chunk = [0u8; 128];
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(count) => output.extend_from_slice(&chunk[..count]),
            }
            if output.windows(10).any(|window| window == b"__PTY_OK__") {
                break;
            }
        }
        assert!(output.windows(10).any(|window| window == b"__PTY_OK__"));
        session.kill().unwrap();
        let _ = session.child.lock().unwrap().wait();
        drop(reader);
    }
}

#[cfg(feature = "native-gui")]
mod emulator {
    use alacritty_terminal::{
        event::{Event as AlacrittyEvent, EventListener},
        grid::Dimensions,
        term::{
            cell::{Cell, Flags},
            Config, Term,
        },
        vte::ansi::{self, Color as AnsiColor, NamedColor},
    };
    use std::sync::{Arc, Mutex};

    pub enum TerminalEvent {
        PtyWrite(String),
        ClipboardStore(String),
        ClipboardLoad(Arc<dyn Fn(&str) -> String + Send + Sync + 'static>),
        TextAreaSizeRequest(
            Arc<dyn Fn(alacritty_terminal::event::WindowSize) -> String + Send + Sync + 'static>,
        ),
        Title(String),
        Bell,
    }

    #[derive(Clone, Default)]
    struct EventQueue(Arc<Mutex<Vec<AlacrittyEvent>>>);

    impl EventListener for EventQueue {
        fn send_event(&self, event: AlacrittyEvent) {
            if let Ok(mut events) = self.0.lock() {
                events.push(event);
            }
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum TerminalColor {
        DefaultForeground,
        DefaultBackground,
        Cursor,
        Indexed(u16),
        Rgb(u8, u8, u8),
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct TerminalCell {
        pub text: String,
        pub foreground: TerminalColor,
        pub background: TerminalColor,
        pub bold: bool,
        pub italic: bool,
        pub underline: bool,
        pub inverse: bool,
        pub wide: bool,
        pub spacer: bool,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct TerminalLine {
        pub cells: Vec<TerminalCell>,
        pub wrapped: bool,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct TerminalGridSnapshot {
        pub columns: usize,
        pub screen_rows: usize,
        pub history_rows: usize,
        pub display_offset: usize,
        pub cursor: Option<(usize, usize)>,
        pub alternate_screen: bool,
        pub bracketed_paste: bool,
        pub application_cursor: bool,
        pub alternate_scroll: bool,
        pub lines: Vec<TerminalLine>,
    }

    #[derive(Clone, Copy)]
    struct Size {
        columns: usize,
        rows: usize,
    }

    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            self.rows
        }
        fn screen_lines(&self) -> usize {
            self.rows
        }
        fn columns(&self) -> usize {
            self.columns
        }
    }

    /// Stateful ANSI/VT parser and terminal grid. VTE parser state is retained
    /// across calls, so escape sequences and UTF-8 codepoints can cross reads.
    pub struct TerminalModel {
        terminal: Term<EventQueue>,
        parser: ansi::Processor,
        events: EventQueue,
    }

    impl TerminalModel {
        pub fn new(columns: u16, rows: u16, history_rows: usize) -> Self {
            let size = Size {
                columns: usize::from(columns.max(2)),
                rows: usize::from(rows.max(2)),
            };
            let events = EventQueue::default();
            Self {
                terminal: Term::new(
                    Config {
                        scrolling_history: history_rows,
                        ..Config::default()
                    },
                    &size,
                    events.clone(),
                ),
                parser: ansi::Processor::new(),
                events,
            }
        }

        pub fn advance(&mut self, bytes: &[u8]) {
            self.parser.advance(&mut self.terminal, bytes);
        }

        pub fn resize(&mut self, columns: u16, rows: u16) {
            let size = Size {
                columns: usize::from(columns.max(2)),
                rows: usize::from(rows.max(2)),
            };
            self.terminal.resize(size);
        }

        pub fn scroll(&mut self, lines: i32) {
            self.terminal
                .grid_mut()
                .scroll_display(alacritty_terminal::grid::Scroll::Delta(lines));
        }

        pub fn take_events(&self) -> Vec<TerminalEvent> {
            let Ok(mut events) = self.events.0.lock() else {
                return Vec::new();
            };
            events
                .drain(..)
                .filter_map(|event| match event {
                    AlacrittyEvent::PtyWrite(text) => Some(TerminalEvent::PtyWrite(text)),
                    AlacrittyEvent::ClipboardStore(_, text) => {
                        Some(TerminalEvent::ClipboardStore(text))
                    }
                    AlacrittyEvent::ClipboardLoad(_, format) => {
                        Some(TerminalEvent::ClipboardLoad(format))
                    }
                    AlacrittyEvent::TextAreaSizeRequest(format) => {
                        Some(TerminalEvent::TextAreaSizeRequest(format))
                    }
                    AlacrittyEvent::Title(title) => Some(TerminalEvent::Title(title)),
                    AlacrittyEvent::Bell => Some(TerminalEvent::Bell),
                    _ => None,
                })
                .collect()
        }

        pub fn snapshot(&self) -> TerminalGridSnapshot {
            let grid = self.terminal.grid();
            let visible = self.terminal.renderable_content();
            let columns = grid.columns();
            let screen_rows = grid.screen_lines();
            let history_rows = grid.total_lines().saturating_sub(screen_rows);
            let display_offset = grid.display_offset();
            let mut lines: Vec<TerminalLine> = (0..screen_rows)
                .map(|_| TerminalLine {
                    cells: (0..columns)
                        .map(|_| TerminalCell {
                            text: " ".to_owned(),
                            foreground: TerminalColor::DefaultForeground,
                            background: TerminalColor::DefaultBackground,
                            bold: false,
                            italic: false,
                            underline: false,
                            inverse: false,
                            wide: false,
                            spacer: false,
                        })
                        .collect(),
                    wrapped: false,
                })
                .collect();
            for (index, indexed) in visible.display_iter.enumerate() {
                let row_index = index / columns;
                let column = index % columns;
                if row_index >= lines.len() {
                    break;
                }
                let cell: &Cell = indexed.cell;
                let mut text = cell.c.to_string();
                if let Some(extra) = cell.zerowidth() {
                    text.extend(extra.iter());
                }
                lines[row_index].cells[column] = TerminalCell {
                    text,
                    foreground: color(cell.fg),
                    background: color(cell.bg),
                    bold: cell.flags.contains(Flags::BOLD),
                    italic: cell.flags.contains(Flags::ITALIC),
                    underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
                    inverse: cell.flags.contains(Flags::INVERSE),
                    wide: cell.flags.contains(Flags::WIDE_CHAR),
                    spacer: cell.flags.contains(Flags::WIDE_CHAR_SPACER),
                };
                if column == columns.saturating_sub(1) {
                    lines[row_index].wrapped = cell.flags.contains(Flags::WRAPLINE);
                }
            }
            let cursor = if visible
                .mode
                .contains(alacritty_terminal::term::TermMode::SHOW_CURSOR)
            {
                let row = (visible.cursor.point.line.0 + display_offset as i32) as usize;
                (row < screen_rows).then_some((row, visible.cursor.point.column.0))
            } else {
                None
            };
            TerminalGridSnapshot {
                columns,
                screen_rows,
                history_rows,
                display_offset,
                cursor,
                alternate_screen: visible
                    .mode
                    .contains(alacritty_terminal::term::TermMode::ALT_SCREEN),
                bracketed_paste: visible
                    .mode
                    .contains(alacritty_terminal::term::TermMode::BRACKETED_PASTE),
                application_cursor: visible
                    .mode
                    .contains(alacritty_terminal::term::TermMode::APP_CURSOR),
                alternate_scroll: visible
                    .mode
                    .contains(alacritty_terminal::term::TermMode::ALTERNATE_SCROLL),
                lines,
            }
        }
    }

    fn color(color: AnsiColor) -> TerminalColor {
        match color {
            AnsiColor::Named(NamedColor::Foreground) => TerminalColor::DefaultForeground,
            AnsiColor::Named(NamedColor::Background) => TerminalColor::DefaultBackground,
            AnsiColor::Named(NamedColor::Cursor) => TerminalColor::Cursor,
            AnsiColor::Indexed(index) => TerminalColor::Indexed(u16::from(index)),
            AnsiColor::Named(named) => TerminalColor::Indexed(named as u16),
            AnsiColor::Spec(rgb) => TerminalColor::Rgb(rgb.r, rgb.g, rgb.b),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parser_retains_partial_escape_and_utf8_sequences_between_reads() {
            let mut terminal = TerminalModel::new(20, 4, 32);
            terminal.advance(b"\x1b[31mA\xe2");
            terminal.advance(b"\x82\xac\x1b[0m");
            let snapshot = terminal.snapshot();
            let line = snapshot.lines.first().unwrap();
            assert_eq!(line.cells[0].text, "A");
            assert_eq!(line.cells[1].text, "€");
            assert_eq!(line.cells[0].foreground, TerminalColor::Indexed(1));
        }

        #[test]
        fn parser_exposes_alternate_screen_bracketed_paste_and_keyboard_mode() {
            let mut terminal = TerminalModel::new(20, 4, 32);
            terminal.advance(b"\x1b[?1049h\x1b[?2004h\x1b[?1h");
            let snapshot = terminal.snapshot();
            assert!(snapshot.alternate_screen);
            assert!(snapshot.bracketed_paste);
            assert!(snapshot.application_cursor);
        }

        #[test]
        fn parser_preserves_24_bit_colors_and_wide_characters() {
            let mut terminal = TerminalModel::new(20, 4, 32);
            terminal.advance("\x1b[38;2;12;34;56m界".as_bytes());
            let snapshot = terminal.snapshot();
            let line = snapshot.lines.first().unwrap();
            let lead = line.cells.iter().find(|cell| cell.text == "界").unwrap();
            assert_eq!(lead.foreground, TerminalColor::Rgb(12, 34, 56));
            assert!(lead.wide);
            assert!(line.cells.iter().any(|cell| cell.spacer));
        }

        #[test]
        fn parser_surfaces_terminal_query_responses_for_the_pty() {
            let mut terminal = TerminalModel::new(20, 4, 32);
            terminal.advance(b"\x1b[6n");
            assert!(matches!(
                terminal.take_events().as_slice(),
                [TerminalEvent::PtyWrite(response)] if response.starts_with("\x1b[") && response.ends_with('R')
            ));
        }
    }

    pub use TerminalColor as Color;
    pub use TerminalEvent as Event;
    pub use TerminalGridSnapshot as Snapshot;
    pub use TerminalModel as Model;
}

#[cfg(feature = "native-gui")]
pub use emulator::Event as TerminalEvent;
#[cfg(feature = "native-gui")]
pub use emulator::{
    Color as TerminalColor, Model as TerminalModel, Snapshot as TerminalGridSnapshot,
};

//! Optional native tray support. The app shell owns selection and viewport
//! changes; this module only builds a native menu and forwards its events.

use std::sync::{mpsc, Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrayConversation {
    /// The app shell's serialized conversation identity. It is returned
    /// unchanged so the receiver can validate it against current state.
    pub key: String,
    pub title: String,
    pub workspace: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TrayUsageSummary {
    pub short_window: Option<String>,
    pub long_window: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TrayAvailability {
    Available,
    Unavailable { reason: String },
}

impl TrayAvailability {
    pub(crate) fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TrayEvent {
    ShowWindow,
    QuitApplication,
    OpenConversation { key: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowCloseBehavior {
    HideToTray,
    CloseWindow,
}

pub(crate) fn close_behavior(tray_available: bool) -> WindowCloseBehavior {
    if tray_available {
        WindowCloseBehavior::HideToTray
    } else {
        WindowCloseBehavior::CloseWindow
    }
}

pub(crate) struct TrayService {
    availability: TrayAvailability,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    platform: Option<NativeTray>,
    events: mpsc::Receiver<TrayEvent>,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
struct NativeTray {
    _icon: tray_icon::TrayIcon,
    recent_keys: Arc<Mutex<std::collections::HashMap<String, String>>>,
    generation: u64,
}

impl TrayService {
    pub(crate) fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (events_tx, events) = mpsc::channel();
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        let (availability, platform) = match create_native_tray(events_tx.clone(), wake.clone()) {
            Ok(native) => (TrayAvailability::Available, Some(native)),
            Err(error) => (TrayAvailability::Unavailable { reason: error }, None),
        };
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let availability = TrayAvailability::Unavailable {
            reason: "system tray is not implemented for this platform".into(),
        };
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let _ = (events_tx, wake);
        Self {
            availability,
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            platform,
            events,
        }
    }

    pub(crate) fn availability(&self) -> TrayAvailability {
        self.availability.clone()
    }

    pub(crate) fn update_menu(
        &mut self,
        recent: &[TrayConversation],
        usage: &TrayUsageSummary,
    ) -> Result<(), String> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let Some(native) = self.platform.as_mut() {
            native.update_menu(recent, usage)?;
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let _ = (recent, usage);
        Ok(())
    }

    pub(crate) fn drain_events(&self) -> Vec<TrayEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            events.push(event);
        }
        events
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn create_native_tray(
    events: mpsc::Sender<TrayEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<NativeTray, String> {
    use tray_icon::{menu::Menu, Icon, MouseButton, TrayIconBuilder, TrayIconEvent};

    let (menu, recent_keys) = build_menu(&[], &TrayUsageSummary::default(), 0)?;
    let icon_image = image::load_from_memory(include_bytes!("../../../icons/icon.png"))
        .map_err(|error| format!("could not decode tray icon: {error}"))?
        .into_rgba8();
    let (width, height) = icon_image.dimensions();
    let icon = Icon::from_rgba(icon_image.into_raw(), width, height)
        .map_err(|error| format!("could not create tray icon: {error}"))?;
    let tray_icon = TrayIconBuilder::new()
        .with_tooltip("Codex Monitor")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .build()
        .map_err(|error| format!("could not create native tray: {error}"))?;

    let menu_events = events.clone();
    let menu_wake = Arc::clone(&wake);
    let menu_recent_keys = Arc::new(Mutex::new(recent_keys));
    let event_recent_keys = Arc::clone(&menu_recent_keys);
    tray_icon::menu::MenuEvent::set_event_handler(Some(move |event| {
        let event = match event_recent_keys.lock() {
            Ok(recent) => event_to_action(event.id.as_ref(), &recent),
            Err(_) => TrayEvent::ShowWindow,
        };
        let _ = menu_events.send(event);
        menu_wake();
    }));
    let tray_events = events.clone();
    tray_icon::TrayIconEvent::set_event_handler(Some(move |event| {
        let show = match event {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                ..
            }
            | TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } => true,
            _ => false,
        };
        if show {
            let _ = tray_events.send(TrayEvent::ShowWindow);
            wake();
        }
    }));

    Ok(NativeTray {
        _icon: tray_icon,
        recent_keys: menu_recent_keys,
        generation: 0,
    })
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl NativeTray {
    fn update_menu(
        &mut self,
        recent: &[TrayConversation],
        usage: &TrayUsageSummary,
    ) -> Result<(), String> {
        self.generation = self.generation.wrapping_add(1);
        let (menu, recent_keys) = build_menu(recent, usage, self.generation)?;
        self._icon.set_menu(Some(Box::new(menu)));
        *self
            .recent_keys
            .lock()
            .map_err(|_| "tray conversation map lock poisoned".to_string())? = recent_keys;
        Ok(())
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn build_menu(
    recent: &[TrayConversation],
    usage: &TrayUsageSummary,
    generation: u64,
) -> Result<
    (
        tray_icon::menu::Menu,
        std::collections::HashMap<String, String>,
    ),
    String,
> {
    use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};

    let menu = Menu::new();
    let show = MenuItem::with_id("cm-tray-show", "Show Codex Monitor", true, None);
    let usage_short = MenuItem::with_id(
        "cm-tray-usage-short",
        format!(
            "Usage · 5-hour: {}",
            usage.short_window.as_deref().unwrap_or("unavailable")
        ),
        false,
        None,
    );
    let usage_long = MenuItem::with_id(
        "cm-tray-usage-long",
        format!(
            "Usage · weekly: {}",
            usage.long_window.as_deref().unwrap_or("unavailable")
        ),
        false,
        None,
    );
    menu.append(&show)
        .map_err(|error| format!("could not construct tray menu: {error}"))?;
    menu.append(&PredefinedMenuItem::separator())
        .map_err(|error| format!("could not construct tray menu: {error}"))?;
    menu.append(&usage_short)
        .map_err(|error| format!("could not construct tray menu: {error}"))?;
    menu.append(&usage_long)
        .map_err(|error| format!("could not construct tray menu: {error}"))?;
    menu.append(&PredefinedMenuItem::separator())
        .map_err(|error| format!("could not construct tray menu: {error}"))?;

    let mut recent_keys = std::collections::HashMap::new();
    if recent.is_empty() {
        menu.append(&MenuItem::with_id(
            "cm-tray-recent-empty",
            "No recent conversations",
            false,
            None,
        ))
        .map_err(|error| format!("could not construct tray menu: {error}"))?;
    } else {
        for (index, conversation) in recent.iter().take(8).enumerate() {
            let id = format!("cm-tray-recent-{generation}-{index}");
            let label = format!(
                "{} · {}",
                truncate_menu_text(&conversation.title, 64),
                truncate_menu_text(&conversation.workspace, 36)
            );
            let item = MenuItem::with_id(&id, label, true, None);
            menu.append(&item)
                .map_err(|error| format!("could not construct tray menu: {error}"))?;
            recent_keys.insert(id, conversation.key.clone());
        }
    }
    menu.append(&PredefinedMenuItem::separator())
        .map_err(|error| format!("could not construct tray menu: {error}"))?;
    let quit = MenuItem::with_id("cm-tray-quit", "Quit", true, None);
    menu.append(&quit)
        .map_err(|error| format!("could not construct tray menu: {error}"))?;
    Ok((menu, recent_keys))
}

fn event_to_action(id: &str, recent_keys: &std::collections::HashMap<String, String>) -> TrayEvent {
    match id {
        "cm-tray-show" => TrayEvent::ShowWindow,
        "cm-tray-quit" => TrayEvent::QuitApplication,
        _ => recent_keys
            .get(id)
            .map(|key| TrayEvent::OpenConversation { key: key.clone() })
            .unwrap_or(TrayEvent::ShowWindow),
    }
}

fn truncate_menu_text(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let mut result = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_hides_only_when_a_persistent_tray_was_created() {
        assert_eq!(close_behavior(true), WindowCloseBehavior::HideToTray);
        assert_eq!(close_behavior(false), WindowCloseBehavior::CloseWindow);
    }

    #[test]
    fn menu_labels_are_bounded_without_changing_conversation_identity() {
        assert_eq!(truncate_menu_text("short", 8), "short");
        assert_eq!(truncate_menu_text("abcdefgh", 4), "abcd…");
    }

    #[test]
    fn tray_menu_events_route_actions_and_current_conversation_keys() {
        let mut recent = std::collections::HashMap::new();
        recent.insert("cm-tray-recent-4-0".into(), "backend:store:thread".into());
        assert_eq!(
            event_to_action("cm-tray-show", &recent),
            TrayEvent::ShowWindow
        );
        assert_eq!(
            event_to_action("cm-tray-quit", &recent),
            TrayEvent::QuitApplication
        );
        assert_eq!(
            event_to_action("cm-tray-recent-4-0", &recent),
            TrayEvent::OpenConversation {
                key: "backend:store:thread".into()
            }
        );
        assert_eq!(
            event_to_action("cm-tray-recent-stale-0", &recent),
            TrayEvent::ShowWindow
        );
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    #[test]
    fn unsupported_platform_reports_tray_unavailable_and_never_hides_on_close() {
        let tray = TrayService::new(Arc::new(|| {}));
        assert!(matches!(
            tray.availability(),
            TrayAvailability::Unavailable { .. }
        ));
        assert_eq!(
            close_behavior(tray.availability().is_available()),
            WindowCloseBehavior::CloseWindow
        );
    }
}

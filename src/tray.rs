//! The icon in the menu bar (macOS) or notification area (Windows, Linux).
//! It shows whether the tunnel is on, switches connections, brings the window
//! back and quits; the app keeps running in the background with no window.
use std::time::Duration;

use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

use crate::i18n::Strings;

/// What the menu shows. A new value rebuilds it.
#[derive(Clone, PartialEq, Default)]
pub struct State {
    /// "Connected: Almaty", "Off", "Sign in to start".
    pub status: String,
    pub proxies: Vec<(i64, String)>,
    pub active: Option<i64>,
    pub on: bool,
}

pub enum Action {
    Open,
    Quit,
    Connect(i64),
    Disconnect,
}

fn icon(on: bool) -> Icon {
    // macOS: black + alpha, drawn in the menu bar's own colour (a template image).
    #[cfg(target_os = "macos")]
    let (rgba, size): (&[u8], u32) =
        if on { (include_bytes!("../assets/tray-template-on.rgba"), 44) } else { (include_bytes!("../assets/tray-template-off.rgba"), 44) };
    #[cfg(not(target_os = "macos"))]
    let (rgba, size): (&[u8], u32) = if on { (include_bytes!("../assets/tray-on.rgba"), 32) } else { (include_bytes!("../assets/tray-off.rgba"), 32) };
    Icon::from_rgba(rgba.to_vec(), size, size).expect("tray icon size matches its bytes")
}

fn menu(state: &State, s: &Strings) -> Menu {
    let menu = Menu::new();
    let add = |item: &dyn tray_icon::menu::IsMenuItem| {
        let _ = menu.append(item);
    };
    add(&MenuItem::with_id("status", &state.status, false, None));
    add(&PredefinedMenuItem::separator());
    for (id, name) in &state.proxies {
        add(&CheckMenuItem::with_id(format!("proxy:{id}"), name, true, state.active == Some(*id), None));
    }
    if !state.proxies.is_empty() {
        add(&PredefinedMenuItem::separator());
    }
    add(&MenuItem::with_id("disconnect", s.disconnect, state.active.is_some(), None));
    add(&MenuItem::with_id("open", s.open_window, true, None));
    add(&MenuItem::with_id("quit", s.quit, true, None));
    menu
}

fn action(event: MenuEvent) -> Option<Action> {
    match event.id.as_ref() {
        "open" => Some(Action::Open),
        "quit" => Some(Action::Quit),
        "disconnect" => Some(Action::Disconnect),
        id => id.strip_prefix("proxy:")?.parse().ok().map(Action::Connect),
    }
}

/// Menu clicks since the last call, from whichever thread drew the menu.
pub fn actions() -> Vec<Action> {
    std::iter::from_fn(|| MenuEvent::receiver().try_recv().ok()).filter_map(action).collect()
}

// macOS and Windows: the tray lives on the main thread, inside GPUI's own event loop.
#[cfg(not(target_os = "linux"))]
pub struct Tray {
    icon: TrayIcon,
    shown: State,
}

#[cfg(not(target_os = "linux"))]
impl Tray {
    pub fn new(state: &State, s: &Strings) -> Option<Tray> {
        let builder = TrayIconBuilder::new();
        // macOS draws a template icon in the menu bar's own colour.
        let builder = if cfg!(target_os = "macos") { builder.with_icon_templated(icon(state.on)) } else { builder.with_icon(icon(state.on)) };
        let icon = builder
            .with_tooltip("bkn-proxy")
            .with_menu(Box::new(menu(state, s)))
            .build()
            .inspect_err(|e| tracing::warn!("no tray icon: {e}"))
            .ok()?;
        Some(Tray { icon, shown: state.clone() })
    }

    pub fn show(&mut self, state: &State, s: &Strings) {
        if *state == self.shown {
            return;
        }
        if state.on != self.shown.on {
            let _ = if cfg!(target_os = "macos") { self.icon.set_icon_templated(Some(icon(state.on))) } else { self.icon.set_icon(Some(icon(state.on))) };
        }
        self.icon.set_menu(Some(Box::new(menu(state, s))));
        self.shown = state.clone();
    }
}

// Linux: tray-icon needs a GTK main loop, which GPUI doesn't run, so the
// tray gets a thread of its own and is told what to show over a channel.
#[cfg(target_os = "linux")]
pub struct Tray {
    updates: std::sync::mpsc::Sender<(State, &'static Strings)>,
    shown: State,
}

#[cfg(target_os = "linux")]
impl Tray {
    pub fn new(state: &State, s: &'static Strings) -> Option<Tray> {
        let (updates, rx) = std::sync::mpsc::channel::<(State, &'static Strings)>();
        let first = state.clone();
        std::thread::spawn(move || {
            if gtk::init().is_err() {
                tracing::warn!("no tray icon: GTK did not start");
                return;
            }
            let Ok(tray) = TrayIconBuilder::new().with_icon(icon(first.on)).with_tooltip("bkn-proxy").with_menu(Box::new(menu(&first, s))).build() else {
                tracing::warn!("no tray icon");
                return;
            };
            loop {
                while gtk::events_pending() {
                    gtk::main_iteration_do(false);
                }
                match rx.recv_timeout(Duration::from_millis(50)) {
                    Ok((state, s)) => {
                        let _ = tray.set_icon(Some(icon(state.on)));
                        tray.set_menu(Some(Box::new(menu(&state, s))));
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(_) => return, // the app is gone
                }
            }
        });
        Some(Tray { updates, shown: state.clone() })
    }

    pub fn show(&mut self, state: &State, s: &'static Strings) {
        if *state != self.shown {
            let _ = self.updates.send((state.clone(), s));
            self.shown = state.clone();
        }
    }
}

/// How often the app looks for menu clicks.
pub const POLL: Duration = Duration::from_millis(150);

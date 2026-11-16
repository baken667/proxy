#![cfg_attr(windows, windows_subsystem = "windows")]
mod api;
mod i18n;
mod net;
mod signin;
mod tray;
mod tunnel;
mod update;
mod wire;

use std::{
    borrow::Cow,
    collections::VecDeque,
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};

use gpui_kit::{
    component::{
        ActiveTheme, Theme,
        button::{Button, ButtonCustomVariant, ButtonVariants},
        chart::AreaChart,
        h_flex,
        input::{Input, InputEvent, InputState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use i18n::{Lang, Strings, fill};
use reqwest::Method;
use serde_json::{Value, json};
use wire::*;
use tokio::sync::mpsc;
use tunnel::Tunnel;

/// reqwest and the tunnel need tokio; GPUI has its own executor. Work runs
/// here and GPUI tasks await the JoinHandle.
// Two workers: the app only waits on the network. (The helper, which moves
// the traffic, has a runtime of its own sized to the machine.)
static RT: LazyLock<tokio::runtime::Runtime> =
    LazyLock::new(|| tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio runtime"));

/// `host:port` of a proxy URL, without the credentials.
fn host(url: &str) -> &str {
    url.rsplit(['@', '/']).next().unwrap_or_default()
}

/// The saved session: one keychain entry per server, as a token is only good
/// for the server that issued it. It also keeps a dev build (localhost) off
/// the installed app's entry: macOS asks for the keychain password whenever
/// an app reads an entry that a differently signed one wrote.
fn keychain() -> Option<keyring::Entry> {
    keyring::Entry::new("bkn-proxy", &api::base()).ok()
}

/// "bkn-proxy 0.2.2 · a1b2c3d": the version and the commit it was built from.
const BUILD: &str = concat!("bkn-proxy ", env!("CARGO_PKG_VERSION"), " · ", env!("BKN_COMMIT"));

#[derive(PartialEq)]
enum Screen {
    Login,
    Proxies,
    Settings,
}

#[derive(PartialEq)]
enum Link {
    Off,
    Connecting,
    On,
}

struct Shell {
    screen: Screen,
    lang: Lang,
    token: Option<String>,
    me: Option<Me>,
    proxies: Vec<Proxy>,
    /// The proxy the user wants; `link` says how far along we are.
    active: Option<i64>,
    link: Link,
    /// Speeds and totals of the current connection; None while off.
    traffic: Option<Traffic>,
    /// Something went wrong (shown in the signal colour).
    note: SharedString,
    /// Something went right.
    info: SharedString,
    /// The browser sign-in in progress; dropping it does not stop it, aborting does.
    signing_in: Option<tokio::task::AbortHandle>,
    totp_setup: Option<TotpSetup>,
    tunnel: Option<Tunnel>,
    launching: bool,
    /// A repair is waiting for the helper to start.
    pending_repair: bool,
    repairing: bool,
    /// An older version left network settings that can block the internet.
    leftovers: bool,
    tunnel_states: mpsc::UnboundedSender<String>,
    refresh: Option<mpsc::UnboundedSender<()>>,
    /// The proxy whose name is being edited, and the field it is edited in.
    renaming: Option<i64>,
    rename: Entity<InputState>,
    /// A newer version, already downloaded and verified.
    update: Option<update::Update>,
    /// The logo mark, drawn for the light and for the dark theme.
    marks: [Arc<Image>; 2],
    current_password: Entity<InputState>,
    new_password: Entity<InputState>,
    totp_code: Entity<InputState>,
    /// Picked for each connection, so the server can tell connections apart.
    tunnel_id: Option<String>,
    last_report: Option<Instant>,
    tray: Option<tray::Tray>,
    _sync: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    fn new(saved_token: Option<String>, lang: Lang, window: &mut Window, cx: &mut Context<Self>) -> Self {
        apply_theme(window, cx);
        // The wordmark's typeface; everything else is the system font.
        let _ = cx.text_system().add_fonts(vec![Cow::Borrowed(include_bytes!("../assets/JetBrainsMono-Medium.ttf"))]);
        gpui_kit::component::set_locale(lang.code());
        let s = lang.strings();
        let mut input = |placeholder: &'static str, masked: bool| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).masked(masked))
        };
        let rename = input(s.rename_placeholder, false);
        let current_password = input(s.current_password, true);
        let new_password = input(s.new_password, true);
        let totp_code = input(s.code, false);

        let mut _subscriptions: Vec<Subscription> = Vec::new();
        _subscriptions.push(cx.subscribe_in(&rename, window, |this: &mut Self, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.save_rename(window, cx);
            }
        }));
        _subscriptions.push(cx.observe_window_appearance(window, |_, window, cx| {
            apply_theme(window, cx);
            cx.notify();
        }));

        // A newer version arrives here once it is downloaded and verified.
        let (found, mut updates) = mpsc::unbounded_channel();
        RT.spawn(update::watch(found));
        cx.spawn(async move |this, cx| {
            while let Some(update) = updates.recv().await {
                let _ = this.update(cx, |this, cx| {
                    this.update = Some(update);
                    cx.notify();
                });
            }
        })
        .detach();

        // The helper reports `up` / `down [reason]`; the UI just mirrors it.
        let (tunnel_states, mut states) = mpsc::unbounded_channel::<String>();
        cx.spawn(async move |this, cx| {
            while let Some(state) = states.recv().await {
                let _ = this.update(cx, |this, cx| {
                    if let Some(counters) = state.strip_prefix("traffic ") {
                        let mut counters = counters.split(' ').filter_map(|n| n.parse::<u64>().ok());
                        if let (Some(sent), Some(received), Some(traffic)) = (counters.next(), counters.next(), this.traffic.as_mut()) {
                            traffic.tick(sent, received);
                        }
                        this.report(false);
                    } else if state == "repaired" {
                        tracing::info!("network repaired");
                        this.repairing = false;
                        this.leftovers = false;
                        this.info = this.lang.strings().network_repaired.into();
                    } else if state == "up" {
                        tracing::info!("tunnel up");
                        this.link = Link::On;
                        this.traffic = Some(Traffic::new());
                        this.tunnel_id = Some(new_tunnel_id());
                        this.report(true);
                    } else {
                        this.traffic = None;
                        this.tunnel_id = None;
                        this.report(true);
                        tracing::warn!("tunnel {state}");
                        this.active = None;
                        this.link = Link::Off;
                        let reason = state.trim_start_matches("down").trim();
                        this.note = this.lang.strings().message(reason).to_string().into();
                    }
                    cx.notify();
                });
            }
        })
        .detach();

        // Closing the window keeps the app (and the tunnel) running: the
        // tray icon brings it back. macOS hides the app; elsewhere the window
        // is minimized, as there is no hidden window state to return from.
        window.on_window_should_close(cx, |window, cx| {
            if cfg!(target_os = "macos") {
                cx.hide();
            } else {
                window.minimize_window();
            }
            false
        });
        let mut tray_clicks = tray::listen();
        cx.spawn_in(window, async move |this, cx| {
            while let Some(action) = tray_clicks.recv().await {
                if this.update_in(cx, |this, window, cx| this.tray_action(action, window, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        // Network settings an older version may have left behind, found once at
        // start. (Not in tests: they run the UI on a deterministic scheduler.)
        #[cfg(not(test))]
        cx.spawn(async move |this, cx| {
            if let Ok(found) = RT.spawn_blocking(net::leftovers).await {
                if !found.is_empty() {
                    tracing::warn!(?found, "network settings left by an older version");
                }
                let _ = this.update(cx, |this, cx| {
                    this.leftovers = !found.is_empty();
                    cx.notify();
                });
            }
        })
        .detach();

        let mut this = Shell {
            screen: Screen::Login,
            lang,
            token: None,
            me: None,
            proxies: vec![],
            active: None,
            link: Link::Off,
            traffic: None,
            note: "".into(),
            info: "".into(),
            signing_in: None,
            totp_setup: None,
            tunnel: None,
            launching: false,
            pending_repair: false,
            repairing: false,
            leftovers: false,
            tunnel_states,
            refresh: None,
            renaming: None,
            rename,
            update: None,
            marks: [false, true].map(|dark| Arc::new(Image::from_bytes(ImageFormat::Svg, mark_svg(palette(dark))))),
            current_password,
            new_password,
            totp_code,
            tunnel_id: None,
            last_report: None,
            tray: None,
            _sync: None,
            _subscriptions,
        };
        if let Some(token) = saved_token {
            this.signed_in(token, cx);
        }
        this
    }

    // ------------------------------------------------------------ plumbing

    /// Runs an API call off the UI thread, then `done` (or the error path) on it.
    fn call<T: serde::de::DeserializeOwned + Send + 'static>(
        &mut self,
        method: Method,
        path: impl Into<String>,
        body: Option<Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
    ) {
        self.note = "".into();
        self.info = "".into();
        let (path, token) = (path.into(), self.token.clone());
        cx.spawn_in(window, async move |this, cx| {
            let result = RT.spawn(async move { api::call::<T>(method, &path, token.as_deref(), body).await }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(Ok(value)) => done(this, value, window, cx),
                    Ok(Err(error)) => this.fail(error, cx),
                    Err(error) => this.note = error.to_string().into(),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn fail(&mut self, error: api::Error, cx: &mut Context<Self>) {
        tracing::warn!(code = %error.0, "api error");
        match error.0.as_str() {
            // Session revoked: user disabled or deleted, password reset, logged out elsewhere.
            "unauthorized" if self.token.is_some() => self.signed_out(cx),
            _ => {}
        }
        self.note = self.lang.strings().message(&error.0).to_string().into();
    }

    fn value(input: &Entity<InputState>, cx: &App) -> String {
        input.read(cx).value().to_string()
    }

    fn clear(inputs: &[&Entity<InputState>], window: &mut Window, cx: &mut Context<Self>) {
        for input in inputs {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    fn set_lang(&mut self, lang: Lang, window: &mut Window, cx: &mut Context<Self>) {
        self.lang = lang;
        gpui_kit::component::set_locale(lang.code()); // the library's own texts, e.g. the input context menu
        let s = lang.strings();
        let placeholders = [
            (&self.rename, s.rename_placeholder),
            (&self.current_password, s.current_password),
            (&self.new_password, s.new_password),
            (&self.totp_code, s.code),
        ];
        for (input, text) in placeholders {
            input.update(cx, |input, cx| input.set_placeholder(text, window, cx));
        }
        cx.notify();
    }

    // ---------------------------------------------------------------- auth

    /// Opens the browser's sign-in page and waits for it to hand back a token.
    fn sign_in(&mut self, cx: &mut Context<Self>) {
        self.note = "".into();
        let strings = self.lang.strings();
        let task = RT.spawn(signin::sign_in(strings));
        self.signing_in = Some(task.abort_handle());
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.signing_in = None;
                match result {
                    Ok(Ok(token)) => {
                        if let Some(Err(error)) = keychain().map(|k| k.set_password(&token)) {
                            this.note = fill(this.lang.strings().session_not_saved, &error.to_string()).into();
                        }
                        this.signed_in(token, cx);
                        cx.activate(true); // back from the browser
                    }
                    Ok(Err(error)) => {
                        tracing::warn!("browser sign-in failed: {error:#}");
                        this.note = this.lang.strings().message(&error.to_string()).to_string().into();
                    }
                    Err(_) => {} // cancelled
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn cancel_sign_in(&mut self, cx: &mut Context<Self>) {
        if let Some(task) = self.signing_in.take() {
            task.abort();
        }
        cx.notify();
    }

    fn signed_in(&mut self, token: String, cx: &mut Context<Self>) {
        tracing::info!("signed in, syncing");
        self.screen = Screen::Proxies;
        self.token = Some(token.clone());

        // `watch` pokes the channel on connect and on every server event;
        // each poke refetches. Dropping `_sync` ends both sides.
        let (refresh, mut pokes) = mpsc::unbounded_channel();
        RT.spawn(api::watch(token.clone(), refresh.clone()));
        self.refresh = Some(refresh);
        self._sync = Some(cx.spawn(async move |this, cx| {
            while pokes.recv().await.is_some() {
                let result = RT.spawn(api::snapshot(token.clone())).await;
                let updated = this.update(cx, |this, cx| {
                    match result {
                        Ok(Ok((me, proxies))) => {
                            if this.me.is_none() {
                                this.note = "".into(); // whatever kept the session from being confirmed is over
                            }
                            this.apply(me, proxies)
                        }
                        Ok(Err(error)) => this.fail(error, cx),
                        Err(_) => {}
                    }
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        }));
    }

    fn log_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Best effort: the local session is dropped either way.
        self.call(Method::POST, "/auth/logout", None, window, cx, |_, _: (), _, _| {});
        self.signed_out(cx);
    }

    fn signed_out(&mut self, cx: &mut Context<Self>) {
        tracing::info!("signed out");
        self.disconnect();
        if let Some(k) = keychain() {
            let _ = k.delete_credential();
        }
        self.token = None;
        self.me = None;
        self.proxies.clear();
        self.totp_setup = None;
        self.refresh = None;
        self._sync = None;
        self.screen = Screen::Login;
        cx.notify();
    }

    /// New server state. If the admin revoked or changed the proxy in use,
    /// the tunnel follows immediately.
    fn apply(&mut self, me: Me, proxies: Vec<Proxy>) {
        if let Some(id) = self.active {
            let old = self.proxies.iter().find(|p| p.id == id);
            match proxies.iter().find(|p| p.id == id) {
                None => {
                    tracing::warn!(proxy = id, "active proxy was revoked");
                    self.disconnect();
                    self.note = self.lang.strings().proxy_revoked.into();
                }
                Some(new) if old.is_some_and(|old| old.url != new.url) => {
                    if let Some(tunnel) = &self.tunnel {
                        self.link = Link::Connecting;
                        tunnel.connect(&new.url);
                    }
                }
                _ => {}
            }
        }
        tracing::debug!(proxies = proxies.len(), "synced");
        self.me = Some(me);
        self.proxies = proxies;
    }

    fn poke(&self) {
        if let Some(refresh) = &self.refresh {
            let _ = refresh.send(());
        }
    }

    // -------------------------------------------------------------- rename

    fn start_rename(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.proxies.iter().find(|proxy| proxy.id == id).map(|proxy| proxy.name.clone()) else {
            return;
        };
        self.renaming = Some(id);
        self.note = "".into();
        self.rename.update(cx, |input, cx| {
            input.set_value(name, window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    /// The name is the user's own: the server keeps it per account, so it
    /// follows them to their other devices and nobody else sees it.
    fn save_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.renaming else { return };
        let body = json!({ "name": Self::value(&self.rename, cx) });
        self.call(Method::PUT, format!("/proxies/{id}/name"), Some(body), window, cx, |this, _: (), _, _| {
            this.renaming = None;
            this.poke();
        });
    }

    // -------------------------------------------------------------- tunnel

    fn connect(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(url) = self.proxies.iter().find(|p| p.id == id).map(|p| p.url.clone()) else {
            return;
        };
        tracing::info!(proxy = id, host = host(&url), "connecting");
        self.active = Some(id);
        self.link = Link::Connecting;
        self.note = "".into();
        if let Some(tunnel) = self.tunnel.as_ref().filter(|t| t.alive()) {
            tunnel.connect(&url); // helper already running: instant switch
            return;
        }
        self.ensure_helper(cx);
    }

    /// Undoes network settings an older version left behind. Needs the
    /// helper, so the first time it asks for the administrator password.
    fn repair_network(&mut self, cx: &mut Context<Self>) {
        self.note = "".into();
        self.info = "".into();
        self.repairing = true;
        match self.tunnel.as_ref().filter(|t| t.alive()) {
            Some(tunnel) => tunnel.repair(),
            None => {
                self.pending_repair = true;
                self.ensure_helper(cx);
            }
        }
        cx.notify();
    }

    /// Starts the elevated helper unless it is starting already. When it is
    /// up it connects to the active proxy and runs a waiting repair.
    fn ensure_helper(&mut self, cx: &mut Context<Self>) {
        if self.launching {
            return;
        }
        self.launching = true;
        let states = self.tunnel_states.clone();
        cx.spawn(async move |this, cx| {
            let result = RT.spawn(Tunnel::launch(states)).await;
            let _ = this.update(cx, |this, cx| {
                this.launching = false;
                match result {
                    Ok(Ok(tunnel)) => {
                        let wanted = this.active.and_then(|id| this.proxies.iter().find(|p| p.id == id));
                        if let Some(proxy) = wanted {
                            tunnel.connect(&proxy.url);
                        }
                        if std::mem::take(&mut this.pending_repair) {
                            tunnel.repair();
                        }
                        this.tunnel = Some(tunnel);
                    }
                    failed => {
                        this.active = None;
                        this.link = Link::Off;
                        this.pending_repair = false;
                        this.repairing = false;
                        this.note = match failed {
                            // The outermost message is a code or, failing that, the system's reason.
                            Ok(Err(error)) => this.lang.strings().message(&error.to_string()).to_string().into(),
                            _ => this.lang.strings().tunnel_failed.into(),
                        };
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn disconnect(&mut self) {
        if self.active.is_some() {
            tracing::info!("disconnecting");
        }
        if let Some(tunnel) = &self.tunnel {
            tunnel.disconnect();
        }
        self.active = None;
        self.link = Link::Off;
        self.traffic = None;
        if self.tunnel_id.take().is_some() {
            self.report(true);
        }
    }

    /// Tells the server what this device's tunnel is doing: on every change,
    /// and every few seconds while connected. Only the proxy and byte counts.
    fn report(&mut self, now: bool) {
        let Some(token) = self.token.clone() else { return };
        if !now && self.last_report.is_some_and(|at| at.elapsed() < Duration::from_secs(5)) {
            return;
        }
        self.last_report = Some(Instant::now());
        let tunnel = match (&self.tunnel_id, self.active, &self.traffic) {
            (Some(id), Some(proxy_id), Some(traffic)) => json!({ "id": id, "proxy_id": proxy_id, "sent": traffic.sent, "received": traffic.received }),
            _ => Value::Null,
        };
        RT.spawn(async move {
            if let Err(error) = api::call::<()>(Method::POST, "/status", Some(&token), Some(json!({ "tunnel": tunnel }))).await {
                tracing::debug!("status report failed: {}", error.0);
            }
        });
    }

    // ---------------------------------------------------------------- tray

    fn tray_state(&self) -> tray::State {
        let s = self.lang.strings();
        let active = self.active.and_then(|id| self.proxies.iter().find(|p| p.id == id));
        let status = match (&self.link, active) {
            _ if self.token.is_none() => s.sign_in.to_string(),
            (Link::On, Some(p)) => format!("{}: {}", s.connected, p.name),
            (Link::Connecting, Some(p)) => format!("{}: {}", s.connecting, p.name),
            _ => s.off.to_string(),
        };
        tray::State {
            status,
            proxies: self.proxies.iter().map(|p| (p.id, p.name.clone())).collect(),
            active: self.active,
            on: self.link == Link::On,
        }
    }

    fn tray_action(&mut self, action: tray::Action, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            tray::Action::Open => {
                cx.activate(true);
                window.activate_window();
            }
            tray::Action::Quit => cx.quit(),
            tray::Action::Disconnect => self.disconnect(),
            tray::Action::Connect(id) if self.active == Some(id) => self.disconnect(),
            tray::Action::Connect(id) => self.connect(id, cx),
        }
        cx.notify();
    }

    // ------------------------------------------------------------- screens

    /// A line that asks for attention. Signal colour, and always worded as a
    /// problem so it is never colour alone.
    fn note(&self, p: Palette) -> Option<Div> {
        if !self.note.is_empty() {
            Some(div().text_size(px(15.)).text_color(p.signal).child(self.note.clone()))
        } else if !self.info.is_empty() {
            Some(label(self.info.clone(), p))
        } else {
            None
        }
    }

    fn mark(&self, p: Palette, size: f32) -> Img {
        img(self.marks[p.dark as usize].clone()).size(px(size))
    }

    fn login_screen(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let s = self.lang.strings();
        let waiting = self.signing_in.is_some();
        v_flex()
            .size_full()
            .justify_center()
            .gap(px(10.))
            .child(h_flex().items_center().gap(px(14.)).child(self.mark(p, 72.)).child(wordmark(p, 30.)))
            .child(label(s.login_hint, p))
            .child(label(s.sign_in_hint, p).mb(px(10.)))
            .child(if waiting {
                v_flex()
                    .gap(px(10.))
                    .child(div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).child(s.waiting_browser))
                    .child(button("cancel-sign-in", s.cancel, Kind::Default, false, cx).w_full().on_click(cx.listener(|this, _, _, cx| this.cancel_sign_in(cx))))
            } else {
                v_flex().child(button("login", s.sign_in, Kind::Solid, false, cx).w_full().on_click(cx.listener(|this, _, _, cx| this.sign_in(cx))))
            })
            .children(self.note(p))
    }

    /// There is a saved token, but the server has not answered for it yet, or
    /// can't be reached. Nothing of the account is shown until it has.
    fn checking_screen(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let s = self.lang.strings();
        v_flex()
            .size_full()
            .justify_center()
            .gap(px(10.))
            .child(h_flex().items_center().gap(px(14.)).child(self.mark(p, 72.)).child(wordmark(p, 30.)))
            .child(div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).child(s.checking_session))
            .children(self.note(p))
            .child(div().mt(px(10.)).child(
                button("logout", s.sign_out, Kind::Default, false, cx).w_full().on_click(cx.listener(|this, _, window, cx| this.log_out(window, cx))),
            ))
    }

    fn proxies_screen(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let s = self.lang.strings();
        let active = self.active.and_then(|id| self.proxies.iter().find(|proxy| proxy.id == id));
        // The one loud word: what is happening to this device's traffic.
        let (state, phrase) = match (&self.link, active) {
            (Link::On, Some(proxy)) => (s.connected, proxy.name.clone()),
            (Link::Connecting, Some(proxy)) => (s.connecting, proxy.name.clone()),
            _ => (s.off, s.direct.to_string()),
        };
        let email = self.me.as_ref().map(|me| me.email.clone()).unwrap_or_default();

        let rows = self.proxies.iter().map(|proxy| {
            let id = proxy.id;
            if self.renaming == Some(id) {
                return v_flex()
                    .gap(px(8.))
                    .py(px(9.))
                    .child(field(&self.rename, p))
                    .child(label(s.rename_hint, p))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(
                                button("rename-save", s.save, Kind::Solid, true, cx)
                                    .on_click(cx.listener(|this, _, window, cx| this.save_rename(window, cx))),
                            )
                            .child(button("rename-cancel", s.cancel, Kind::Quiet, true, cx).on_click(cx.listener(|this, _, _, cx| {
                                this.renaming = None;
                                cx.notify();
                            }))),
                    );
            }
            let is_active = self.active == Some(id);
            h_flex()
                .items_center()
                .gap(px(4.))
                .py(px(9.))
                .child(
                    v_flex()
                        .flex_1()
                        .overflow_hidden()
                        .child(div().when(is_active, |el| el.font_weight(FontWeight::SEMIBOLD)).child(proxy.name.clone()))
                        // Host only: the URL may carry the proxy password.
                        .child(label(host(&proxy.url).to_string(), p)),
                )
                .child(
                    button(SharedString::from(format!("rename-{id}")), s.rename, Kind::Quiet, true, cx)
                        .on_click(cx.listener(move |this, _, window, cx| this.start_rename(id, window, cx))),
                )
                .child(if is_active {
                    div()
                        .text_size(px(15.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(if self.link == Link::On { s.connected } else { s.connecting })
                        .into_any_element()
                } else {
                    button(SharedString::from(format!("connect-{id}")), s.connect, Kind::Default, true, cx)
                        .on_click(cx.listener(move |this, _, _, cx| this.connect(id, cx)))
                        .into_any_element()
                })
        });

        v_flex()
            .size_full()
            .child(
                h_flex()
                    .items_start()
                    .justify_between()
                    .gap(px(16.))
                    .pb(px(8.))
                    .child(
                        h_flex()
                            .flex_1()
                            .overflow_hidden()
                            .items_center()
                            .gap(px(10.))
                            .child(self.mark(p, 44.))
                            .child(v_flex().child(wordmark(p, 18.)).child(label(email, p))),
                    )
                    .child(button("settings", s.settings, Kind::Quiet, true, cx).mr(px(-14.)).on_click(cx.listener(|this, _, _, cx| {
                        this.screen = Screen::Settings;
                        this.note = "".into();
                        cx.notify();
                    }))),
            )
            .child(
                v_flex()
                    .id("pane")
                    .flex_1()
                    .overflow_y_scroll()
                    .pb(px(40.))
                    .children(self.update.as_ref().map(|update| {
                        h_flex()
                            .items_center()
                            .gap(px(12.))
                            .pt(px(8.))
                            .child(div().flex_1().child(label(fill(s.update_ready, &update.version), p)))
                            .child(button("install", s.install, Kind::Solid, true, cx).on_click(cx.listener(|this, _, _, _| {
                                if let Some(update) = &this.update {
                                    update::open(&update.installer);
                                }
                            })))
                    }))
                    .child(
                        v_flex()
                            .pt(px(28.))
                            .pb(px(8.))
                            .child(figure(state))
                            .child(div().mt(px(14.)).text_size(px(20.)).line_height(relative(1.3)).font_weight(FontWeight::SEMIBOLD).child(phrase))
                            .children(self.traffic.as_ref().map(|traffic| label(fill(s.for_time, &clock(traffic.since.elapsed().as_secs())), p)))
                            .children(self.note(p).map(|note| note.mt(px(6.)))),
                    )
                    .children(self.traffic.as_ref().map(|traffic| traffic_figure(traffic, p, s)))
                    .when(self.active.is_some(), |el| {
                        el.child(div().mt(px(20.)).child(
                            button("disconnect", s.disconnect, Kind::Default, false, cx).w_full().on_click(cx.listener(|this, _, _, cx| {
                                this.disconnect();
                                cx.notify();
                            })),
                        ))
                    })
                    .child(
                        block(s.proxies, self.proxies.len().to_string(), p)
                            .when(self.proxies.is_empty(), |el| el.child(label(s.none_yet, p)))
                            .children(rows),
                    ),
            )
    }

    fn settings_screen(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let s = self.lang.strings();
        let (email, totp_enabled) = self.me.as_ref().map(|me| (me.email.clone(), me.totp_enabled)).unwrap_or_default();

        let two_factor = block(s.two_factor, if totp_enabled { s.two_factor_on } else { s.two_factor_off }, p).child(if totp_enabled {
            v_flex()
                .gap(px(10.))
                .child(label(s.turn_off_hint, p))
                .child(field(&self.totp_code, p))
                .child(
                    button("totp-off", s.turn_off, Kind::Default, false, cx)
                        .w_full()
                        .on_click(cx.listener(|this, _, window, cx| this.totp_switch("disable", window, cx))),
                )
        } else if let Some(setup) = &self.totp_setup {
            let secret = setup.secret.clone();
            let url = setup.otpauth_url.clone();
            v_flex()
                .gap(px(10.))
                .child(label(s.set_up_hint, p))
                .child(div().font_family("Menlo").text_size(px(15.)).child(secret.clone()))
                .child(
                    h_flex()
                        .gap(px(8.))
                        .child(button("copy-key", s.copy_key, Kind::Default, true, cx).on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(secret.clone()));
                        }))
                        .child(button("copy-url", s.copy_link, Kind::Default, true, cx).on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
                        })),
                )
                .child(field(&self.totp_code, p))
                .child(
                    button("totp-on", s.turn_on, Kind::Solid, false, cx)
                        .w_full()
                        .on_click(cx.listener(|this, _, window, cx| this.totp_switch("enable", window, cx))),
                )
        } else {
            v_flex().child(button("totp-setup", s.set_up, Kind::Default, false, cx).w_full().on_click(cx.listener(|this, _, window, cx| {
                this.call(Method::POST, "/me/totp/setup", None, window, cx, |this, setup: TotpSetup, _, _| this.totp_setup = Some(setup));
            })))
        });

        v_flex()
            .size_full()
            .child(
                h_flex().pb(px(8.)).child(button("back", s.back, Kind::Quiet, true, cx).ml(px(-14.)).on_click(cx.listener(|this, _, _, cx| {
                    this.screen = Screen::Proxies;
                    this.note = "".into();
                    this.info = "".into();
                    cx.notify();
                }))),
            )
            .child(
                v_flex()
                    .id("settings-pane")
                    .flex_1()
                    .overflow_y_scroll()
                    .pb(px(40.))
                    .children(self.note(p))
                    .child(
                        block(s.account, "", p).mt(px(20.)).child(pair(s.signed_in_as, email, p)).child(div().mt(px(16.)).child(
                            button("logout", s.sign_out, Kind::Default, false, cx).w_full().on_click(cx.listener(|this, _, window, cx| this.log_out(window, cx))),
                        )),
                    )
                    .child(two_factor)
                    .child(
                        block(s.passkeys, "", p).child(
                            v_flex().gap(px(10.)).child(label(s.passkeys_hint, p)).child(
                                button("passkeys", s.manage_in_browser, Kind::Default, false, cx)
                                    .w_full()
                                    .on_click(|_, _, _| update::open_external(format!("{}/account", api::base()))),
                            ),
                        ),
                    )
                    .child(
                        block(s.password, "", p).child(
                            v_flex().gap(px(10.)).child(field(&self.current_password, p)).child(field(&self.new_password, p)).child(
                                button("password", s.change_password, Kind::Default, false, cx).w_full().on_click(cx.listener(|this, _, window, cx| {
                                    let body = json!({ "current": Self::value(&this.current_password, cx), "new": Self::value(&this.new_password, cx) });
                                    this.call(Method::POST, "/me/password", Some(body), window, cx, |this, _: (), window, cx| {
                                        Self::clear(&[&this.current_password, &this.new_password], window, cx);
                                        this.info = this.lang.strings().password_changed.into();
                                    });
                                })),
                            ),
                        ),
                    )
                    .child(
                        block(s.network, "", p).child(
                            v_flex().gap(px(10.)).child(label(s.network_hint, p)).child(
                                button("repair", if self.repairing { s.repairing } else { s.repair_network }, Kind::Default, false, cx)
                                    .w_full()
                                    .on_click(cx.listener(|this, _, _, cx| this.repair_network(cx))),
                            ),
                        ),
                    )
                    .child(block(s.language, "", p).child(h_flex().gap(px(8.)).children(Lang::ALL.map(|lang| {
                        // The current language is the ink-filled one.
                        let kind = if lang == self.lang { Kind::Solid } else { Kind::Default };
                        button(lang.code(), lang.strings().name, kind, true, cx).on_click(cx.listener(move |this, _, window, cx| {
                            lang.save();
                            this.set_lang(lang, window, cx);
                        }))
                    }))))
                    .child(label(BUILD, p).mt(px(52.))),
            )
    }

    fn totp_switch(&mut self, action: &str, window: &mut Window, cx: &mut Context<Self>) {
        let body = json!({ "code": Self::value(&self.totp_code, cx) });
        self.call(Method::POST, format!("/me/totp/{action}"), Some(body), window, cx, |this, _: (), window, cx| {
            Self::clear(&[&this.totp_code], window, cx);
            this.totp_setup = None;
            this.poke(); // refetch /me for the new 2FA state
        });
    }
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.tray_state();
        if let Some(tray) = self.tray.as_mut() {
            tray.show(&state, self.lang.strings());
        }
        let p = palette(cx.theme().is_dark());
        let body = match self.screen {
            Screen::Login => self.login_screen(p, cx),
            // A saved token is not a session until the server has answered for it.
            _ if self.me.is_none() => self.checking_screen(p, cx),
            Screen::Proxies => self.proxies_screen(p, cx),
            Screen::Settings => self.settings_screen(p, cx),
        };
        let s = self.lang.strings();
        let banner = (self.leftovers && self.screen != Screen::Settings).then(|| {
            v_flex()
                .items_start()
                .gap(px(8.))
                .pb(px(16.))
                .child(div().w_full().text_size(px(15.)).text_color(p.signal).child(s.leftovers_found))
                .child(
                    button("repair-banner", if self.repairing { s.repairing } else { s.repair_network }, Kind::Solid, true, cx)
                        .on_click(cx.listener(|this, _, _, cx| this.repair_network(cx))),
                )
        });
        v_flex()
            .size_full()
            .px(px(24.))
            .pt(px(20.))
            .pb(px(12.))
            .bg(p.ground)
            .text_color(p.ink)
            .text_size(px(17.))
            .line_height(relative(1.45))
            .children(banner)
            .child(body.flex_1())
    }
}

// ------------------------------------------------------------------ traffic

fn new_tunnel_id() -> String {
    format!("t-{:016x}{:016x}", rand::random::<u64>(), rand::random::<u64>())
}


/// What the graph covers, in ticks of one second.
const WINDOW: usize = 60;

/// The current connection's traffic, built from the helper's once-a-second
/// `traffic <sent> <received>` lines.
struct Traffic {
    /// The helper's counters at the previous tick. They count from when the
    /// helper started, not from this connection, so only differences are used.
    last: Option<(u64, u64)>,
    sent: u64,
    received: u64,
    /// (upload, download) in bytes per second, oldest first; always WINDOW long.
    rates: VecDeque<(u64, u64)>,
    since: Instant,
}

impl Traffic {
    fn new() -> Self {
        Traffic { last: None, sent: 0, received: 0, rates: vec![(0, 0); WINDOW].into(), since: Instant::now() }
    }

    // ponytail: a tick is taken to be exactly one second, so a delta is a
    // rate. Timestamp the ticks if the helper's timer ever proves too loose.
    fn tick(&mut self, sent: u64, received: u64) {
        if let Some((last_sent, last_received)) = self.last {
            // saturating: counters that went backwards mean a restarted helper.
            let (up, down) = (sent.saturating_sub(last_sent), received.saturating_sub(last_received));
            self.sent += up;
            self.received += down;
            self.rates.pop_front();
            self.rates.push_back((up, down));
        }
        self.last = Some((sent, received));
    }
}

/// 1536000 -> "1.5 MB" (or "1.5 МБ"). Decimal units, as network speeds are quoted.
fn bytes(n: u64, s: &Strings) -> String {
    let (mut value, mut unit) = (n as f64, 0);
    // 999.5, not 1000: what would print as "1000 KB" becomes "1.0 MB".
    while value >= 999.5 && unit < 4 {
        value /= 1000.;
        unit += 1;
    }
    match unit {
        0 => format!("{n} {}", s.units[0]),
        _ if value < 9.95 => format!("{value:.1} {}", s.units[unit]),
        _ => format!("{value:.0} {}", s.units[unit]),
    }
}

/// 75 -> "1:15", 3675 -> "1:01:15".
fn clock(seconds: u64) -> String {
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

// ------------------------------------------------------------------- design
//
// The system from Codwheel's DESIGN.md: one flat ground, ink, a label grey and
// a single signal colour that means "look here". No cards, borders or shadows;
// structure is space and weight. A fill sits only behind what is pressed or
// typed into.

#[derive(Clone, Copy)]
struct Palette {
    ground: Hsla,
    ground_2: Hsla,
    ink: Hsla,
    ink_2: Hsla,
    signal: Hsla,
    dark: bool,
}

fn palette(dark: bool) -> Palette {
    let c = |hex: u32| Hsla::from(rgb(hex));
    if dark {
        Palette { ground: c(0x1e1e1d), ground_2: c(0x2b2b29), ink: c(0xf1f1ee), ink_2: c(0xa2a29d), signal: c(0xff6a2b), dark }
    } else {
        Palette { ground: c(0xe8e8e6), ground_2: c(0xdcdcd9), ink: c(0x191918), ink_2: c(0x5c5c59), signal: c(0xb53604), dark }
    }
}

/// Follows the system light/dark setting, then puts our colours where the
/// component library reads them (input text, placeholder, caret, selection).
fn apply_theme(window: &mut Window, cx: &mut App) {
    Theme::sync_system_appearance(Some(window), cx);
    apply_palette(cx);
}

fn apply_palette(cx: &mut App) {
    let p = palette(cx.theme().is_dark());
    let theme = Theme::global_mut(cx);
    theme.background = p.ground;
    theme.foreground = p.ink;
    theme.muted_foreground = p.ink_2;
    theme.caret = p.signal;
    theme.selection = p.ink.opacity(0.2);
    theme.ring = p.ink;
}

#[derive(Clone, Copy)]
enum Kind {
    /// Pressed grey: neutral actions.
    Default,
    /// Ink: the one main action of a screen.
    Solid,
    /// No fill.
    Quiet,
}

/// 52px tall with a 16px radius, or `small`: 44px with 12px. No hover style.
/// A quiet button keeps its padding as hit area; callers pull it back with a
/// negative margin where its text should line up with the gutter.
fn button(id: impl Into<ElementId>, text: &'static str, kind: Kind, small: bool, cx: &App) -> Button {
    let p = palette(cx.theme().is_dark());
    let (fill, ink) = match kind {
        Kind::Default => (p.ground_2, p.ink),
        Kind::Solid => (p.ink, p.ground),
        Kind::Quiet => (transparent_black(), if small { p.ink_2 } else { p.ink }),
    };
    let (height, pad, radius, size, weight) = if small { (44., 14., 12., 15., FontWeight::MEDIUM) } else { (52., 20., 16., 17., FontWeight::SEMIBOLD) };
    Button::new(id)
        .custom(ButtonCustomVariant::new(cx).color(fill).foreground(ink).hover(fill).active(fill))
        .rounded(px(radius))
        .label(text)
        // The library draws a custom variant's resting state as a 20% tint; we want the flat fill.
        .bg(fill)
        .h(px(height))
        .px(px(pad))
        .text_size(px(size))
        .font_weight(weight)
}

fn field(input: &Entity<InputState>, p: Palette) -> Div {
    div().h(px(52.)).px(px(6.)).flex().items_center().rounded(px(16.)).bg(p.ground_2).child(Input::new(input).appearance(false))
}

/// The logo mark, "the route": a line that changes course and arrives at a
/// point. Geometry from the brand sheet (64 x 64, 9-wide stroke).
fn mark_svg(p: Palette) -> Vec<u8> {
    let hex = |color: Hsla| {
        let c = color.to_rgb();
        format!("#{:02x}{:02x}{:02x}", (c.r * 255.).round() as u8, (c.g * 255.).round() as u8, (c.b * 255.).round() as u8)
    };
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256" viewBox="0 0 64 64"><path d="M4 20 H24 L40 44 H45" fill="none" stroke="{}" stroke-width="9" stroke-linejoin="round"/><circle cx="55" cy="44" r="6.5" fill="{}"/></svg>"#,
        hex(p.ink),
        hex(p.signal)
    )
    .into_bytes()
}

/// "bkn-proxy" in the logo's typeface, its hyphen in the signal colour.
fn wordmark(p: Palette, size: f32) -> Div {
    h_flex()
        .font_family("JetBrains Mono")
        .font_weight(FontWeight::MEDIUM)
        .text_size(px(size))
        .line_height(relative(1.2))
        .child("bkn")
        .child(div().text_color(p.signal).child("-"))
        .child("proxy")
}

/// 15px label grey: labels, hints, statuses.
fn label(text: impl Into<SharedString>, p: Palette) -> Div {
    div().text_size(px(15.)).text_color(p.ink_2).child(text.into())
}

/// Display size, for the one loud word of a screen: 56px, stepping down for a
/// longer word (Russian ones run wide) so it never breaks in the middle.
// ponytail: sized by letter count for the 372px column of the minimum window
// width, not by measuring the text. Measure it if a language outgrows this.
fn figure(text: &'static str) -> Div {
    let size = (372. / (text.chars().count() as f32 * 0.7)).clamp(40., 56.);
    div().text_size(px(size)).line_height(relative(0.98)).font_weight(FontWeight::EXTRA_BOLD).child(text)
}

/// A section: 52px above, a 17px semibold title, an optional grey figure beside it.
fn block(title: &'static str, aside: impl Into<SharedString>, p: Palette) -> Div {
    v_flex().mt(px(52.)).child(
        h_flex().justify_between().mb(px(14.)).child(div().font_weight(FontWeight::SEMIBOLD).child(title)).child(label(aside, p)),
    )
}

/// The last minute of the connection: two speeds with their totals, and a graph
/// of both. Download is full ink and upload a lighter step of the same ink
/// (DESIGN.md, "The Ink Strength Rule": categories never get a second hue).
fn traffic_figure(traffic: &Traffic, p: Palette, s: &'static Strings) -> Div {
    let (up, down) = traffic.rates.back().copied().unwrap_or_default();
    let upload_ink = p.ink.opacity(0.48);
    let series = |name: &'static str, ink: Hsla, rate: u64, total: u64| {
        v_flex()
            .flex_1()
            .child(h_flex().items_center().gap(px(8.)).child(div().size(px(10.)).rounded(px(2.)).bg(ink)).child(label(name, p)))
            .child(div().text_size(px(20.)).line_height(relative(1.3)).font_weight(FontWeight::SEMIBOLD).child(fill(s.per_second, &bytes(rate, s))))
            .child(label(fill(s.in_total, &bytes(total, s)), p))
    };
    let points: Vec<(usize, f64, f64)> = traffic.rates.iter().enumerate().map(|(i, &(up, down))| (i, up as f64, down as f64)).collect();
    // A floor on the scale, so an idle link is a flat line and not amplified noise.
    let peak = traffic.rates.iter().map(|&(up, down)| up.max(down)).max().unwrap_or(0).max(10_000) as f64;
    v_flex()
        .mt(px(28.))
        .gap(px(14.))
        .child(h_flex().gap(px(24.)).child(series(s.download, p.ink, down, traffic.received)).child(series(s.upload, upload_ink, up, traffic.sent)))
        .child(
            div().h(px(72.)).w_full().child(
                AreaChart::new(points)
                    .id("traffic")
                    .x(|point| point.0.to_string())
                    .y(|point| point.2)
                    .stroke(p.ink)
                    .fill(p.ink.opacity(0.13))
                    .y(|point| point.1)
                    .stroke(upload_ink)
                    .fill(transparent_black())
                    .y_domain(0., peak)
                    .linear()
                    .x_axis(false)
                    .y_axis(false)
                    .grid(false)
                    .interactive(false)
                    .appear(false),
            ),
        )
        .child(h_flex().justify_between().child(label(s.last_minute, p)).child(label(fill(s.peak, &fill(s.per_second, &bytes(peak as u64, s))), p)))
}

/// A grey label over a heavier value.
fn pair(name: &'static str, value: String, p: Palette) -> Div {
    v_flex().child(label(name, p)).child(div().text_size(px(20.)).line_height(relative(1.3)).font_weight(FontWeight::SEMIBOLD).child(value))
}

actions!(bkn_proxy, [Quit]);

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let [_, mode, port, token_file] = args.as_slice()
        && mode == "helper"
    {
        if let Err(error) = tunnel::helper_main(port, token_file) {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
        return;
    }

    // To stderr; RUST_LOG picks the level (default info). Proxy URLs carry
    // passwords, so log lines name a proxy by `host()` only.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    // Logged before the keychain read: macOS may hold that read behind a
    // permission prompt (it does after every rebuild of an unsigned binary,
    // and for an entry that a differently signed build wrote).
    tracing::info!("starting, reading the saved session from the keychain");
    let saved_token = keychain().and_then(|k| k.get_password().ok());
    tracing::info!(saved_session = saved_token.is_some(), "keychain read");
    let lang = Lang::load();
    let application = gpui_kit::application();
    // macOS: a click on the Dock icon brings the hidden window back.
    application.on_reopen(|cx| cx.activate(true));
    application.run(move |cx| {
        gpui_kit::init(cx);
        // Closing the window leaves the app running in the tray (see Shell::new).
        // Quit is in the tray menu, or Cmd+Q on macOS; quitting closes the
        // helper's connection, which tears the tunnel down.
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.set_menus([Menu { name: "bkn-proxy".into(), items: vec![MenuItem::action("Quit bkn-proxy", Quit)], disabled: false }]);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(420.), px(640.)), cx))),
            // A fixed size: the screens are laid out for this column; long content scrolls.
            is_resizable: false,
            titlebar: Some(TitlebarOptions { title: Some("bkn-proxy".into()), ..Default::default() }),
            ..Default::default()
        };
        let (_, shell) =
            gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Shell::new(saved_token, lang, window, cx))).expect("failed to open window");
        // The tray icon is made here, on the main thread, which macOS requires.
        shell.update(cx, |shell, _| shell.tray = tray::Tray::new(&shell.tray_state(), shell.lang.strings()));
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    // No `use super::*`: the gpui_kit glob would shadow Rust's #[test].
    use super::{Lang, Link, Screen, Shell, Traffic, WINDOW, apply_palette, bytes, clock};
    use gpui_kit::{
        AppContext, Bounds, HeadlessAppContext, TestAppContext, WindowBounds, WindowOptions,
        component::{Theme, ThemeMode},
        px, size,
        test::TestWindowExt,
    };
    use crate::wire::{Me, Proxy};

    /// Headless: every screen renders, navigation works, and a proxy revoked
    /// by the admin drops the tunnel. No network, no keychain.
    #[gpui_kit::test]
    fn screens_and_revocation(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (window, shell) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| cx.new(|cx| Shell::new(None, Lang::En, window, cx))).unwrap()
        });
        let me = || Me { email: "a@b.c".into(), totp_enabled: false };
        let proxy = |id| Proxy { id, name: format!("p{id}"), url: format!("socks5://u:secret@10.0.0.{id}:1080") };

        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("login").is_some());

            // A saved token the server has not confirmed: nothing of the account yet.
            shell.update(cx, |shell, cx| {
                shell.screen = Screen::Settings;
                cx.notify();
            });
            window.render_frame(cx);
            assert!(window.try_find("logout").is_some() && window.try_find("totp-setup").is_none() && window.try_find("password").is_none());

            shell.update(cx, |shell, cx| {
                shell.screen = Screen::Proxies;
                shell.apply(me(), vec![proxy(1), proxy(2)]);
                cx.notify();
            });
            window.render_frame(cx);
            assert!(window.try_find("connect-1").is_some());
            assert!(window.try_find("add").is_none(), "users no longer add proxies of their own");

            // Renaming swaps the row for a field; cancelling puts the row back.
            window.click("rename-2", cx);
            assert!(window.try_find("rename-save").is_some() && window.try_find("connect-2").is_none());
            window.click("rename-cancel", cx);
            assert!(window.try_find("connect-2").is_some());

            window.click("settings", cx);
            assert!(window.try_find("totp-setup").is_some());
            assert!(window.try_find("en").is_some() && window.try_find("ru").is_some(), "language switch");
            window.click("back", cx);
            assert!(window.try_find("settings").is_some());

            shell.update(cx, |shell, _| {
                shell.active = Some(1);
                shell.link = Link::On;
                shell.apply(me(), vec![proxy(2)]); // admin revoked proxy 1
                assert_eq!(shell.active, None);
                assert!(shell.link == Link::Off);
            });
        })
        .unwrap();
    }

    #[test]
    fn traffic_rates_and_formatting() {
        let mut traffic = Traffic::new();
        traffic.tick(5_000, 9_000); // first tick is only a baseline: the helper counts from its own start
        assert_eq!((traffic.sent, traffic.received), (0, 0));
        traffic.tick(6_000, 12_000);
        traffic.tick(6_000, 12_000); // an idle second
        assert_eq!((traffic.sent, traffic.received), (1_000, 3_000));
        assert_eq!(traffic.rates.len(), WINDOW);
        assert_eq!(traffic.rates.iter().rev().take(3).copied().collect::<Vec<_>>(), [(0, 0), (1_000, 3_000), (0, 0)]);
        traffic.tick(10, 20); // helper restarted: counters fell, nothing is added
        assert_eq!((traffic.sent, traffic.received), (1_000, 3_000));

        let (en, ru) = (Lang::En.strings(), Lang::Ru.strings());
        assert_eq!(bytes(0, en), "0 B");
        assert_eq!(bytes(999, en), "999 B");
        assert_eq!(bytes(1_536, en), "1.5 KB");
        assert_eq!(bytes(999_600, en), "1.0 MB");
        assert_eq!(bytes(12_400_000, en), "12 MB");
        assert_eq!(bytes(3_200_000_000, ru), "3.2 ГБ");
        assert_eq!((clock(5), clock(75), clock(3_675)), ("0:05".into(), "1:15".into(), "1:01:15".into()));
    }

    /// Renders every screen offscreen, light and dark, into $BKN_SHOTS as PNGs.
    /// Skipped when the variable is unset:
    ///   BKN_SHOTS=/tmp/shots cargo test -p bkn-proxy shots
    #[cfg(target_os = "macos")]
    #[test]
    fn shots() {
        let Ok(dir) = std::env::var("BKN_SHOTS") else { return };
        std::fs::create_dir_all(&dir).unwrap();
        let mut cx = HeadlessAppContext::with_platform(
            // Not `current_platform()`: on macOS that insists on the main thread.
            std::sync::Arc::new(gpui_apple::AppleTextSystem::new()),
            std::sync::Arc::new(gpui_kit::assets::Assets),
            gpui_kit::platform::current_headless_renderer,
        );
        cx.update(gpui_kit::init);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: Default::default(), size: size(px(420.), px(980.)) })),
            focus: false,
            show: false,
            ..Default::default()
        };
        let (window, shell) =
            cx.update(|cx| gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Shell::new(None, Lang::En, window, cx)))).unwrap();
        let proxy = |id, name: &str| Proxy { id, name: name.into(), url: format!("socks5://u:secret@10.0.0.{id}:1080") };

        for (mode, lang) in [(ThemeMode::Light, Lang::En), (ThemeMode::Dark, Lang::En), (ThemeMode::Light, Lang::Ru)] {
            let mut shot = |name: &str, set: &dyn Fn(&mut Shell)| {
                cx.update_window(window, |_, window, cx| {
                    Theme::change(mode, None, cx);
                    apply_palette(cx);
                    shell.update(cx, |shell, cx| {
                        shell.set_lang(lang, window, cx);
                        set(shell);
                    });
                    window.render_frame(cx);
                })
                .unwrap();
                // Images (the logo mark) are decoded off the frame: let that finish, then draw again.
                cx.run_until_parked();
                cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
                let file = format!("{dir}/{name}-{}-{}.png", mode.name(), lang.code());
                cx.capture_screenshot(window).unwrap().save(file).unwrap();
            };
            shot("1-login", &|shell| {
                shell.screen = Screen::Login;
                shell.signing_in = None;
                shell.note = "".into();
            });
            shot("2-login-waiting", &|shell| {
                shell.signing_in = Some(super::RT.spawn(std::future::pending::<()>()).abort_handle());
            });
            shot("3-off", &|shell| {
                shell.signing_in = None;
                shell.leftovers = false;
                shell.screen = Screen::Proxies;
                shell.note = "".into();
                shell.active = None;
                shell.link = Link::Off;
                shell.me = Some(Me { email: "test@example.com".into(), totp_enabled: false });
                shell.renaming = None;
                shell.update = None;
                shell.proxies = vec![proxy(1, "Frankfurt"), proxy(2, "Amsterdam"), proxy(3, "Home server")];
            });
            shot("4-connected", &|shell| {
                shell.active = Some(1);
                shell.link = Link::On;
                // A made-up minute: a download that ramps up, with a trickle of upload.
                let mut traffic = Traffic::new();
                let (mut sent, mut received) = (0u64, 0u64);
                for second in 0..=WINDOW as u64 {
                    traffic.tick(sent, received);
                    received += 400_000 + (second * 37 % 11) * 90_000 + if second > 35 { 1_400_000 } else { 0 };
                    sent += 60_000 + (second * 13 % 7) * 25_000;
                }
                shell.traffic = Some(traffic);
            });
            shot("5-settings", &|shell| shell.screen = Screen::Settings);
            shot("7-leftovers", &|shell| {
                shell.screen = Screen::Proxies;
                shell.leftovers = true;
                shell.renaming = None;
                shell.update = None;
            });
            shot("6-rename-and-update", &|shell| {
                shell.screen = Screen::Proxies;
                shell.active = None;
                shell.link = Link::Off;
                shell.traffic = None;
                shell.renaming = Some(2);
                shell.update = Some(crate::update::Update { version: "0.2.0".into(), installer: "bkn-proxy-0.2.0.dmg".into() });
            });
            shot("8-checking", &|shell| {
                shell.me = None;
                shell.leftovers = false;
                shell.renaming = None;
                shell.note = lang.strings().offline.into();
            });
        }
    }
}

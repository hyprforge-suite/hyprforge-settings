mod ipc;
mod look;
mod module;
mod modules;
mod singleton;

use hyprforge_ui::theme::{app_theme, spacing, surface, FontScale, text_dim};
use hyprforge_ui::widgets::{primary_button, scaled_text, secondary_button};
use crate::module::SettingsModule;
use iced::keyboard::{self, key, Key};
use iced::widget::{column, container, operation, row, text_input, Id};
use iced::{window, Background, Element, Length, Size, Subscription, Task, Theme};
use modules::appearance::AppearanceModule;
use modules::bluetooth::{BluetoothModule, LazyBlueZBackend};
use modules::desktop::DesktopModule;
use modules::session::SessionModule;
use modules::system::SystemModule;
use modules::displays::DisplaysModule;
use modules::input::InputModule;
use modules::network::{LazyNetworkManagerBackend, NetworkModule};
use modules::power::{
    LazyLogindBackend, LazyPowerProfilesDaemonBackend, LazyUPowerBackend, PowerModule,
};
use modules::shortcuts::ShortcutsModule;
use modules::tray::TrayModule;
use modules::window_rules::WindowRulesModule;

/// Opens the app on one screen: `hyprforge-settings --screen network`.
///
/// Deep-linking, the way `gnome-control-center wifi` does it, so a
/// desktop entry or a notification can point at the page it is about
/// rather than at the front door.
///
/// It is also what makes a screen reviewable. Proving a page *looks*
/// right means opening it and taking a picture, and without this there
/// is no way to reach one from outside the app — Ctrl+1..3 cover three
/// of the nine screens, and injecting a click needs tooling that is not
/// on every machine. A screenshot is how the `web-colors` bug was found;
/// this is what makes taking one repeatable.
fn screen_from_cli(name: &str) -> Option<(Screen, Option<modules::desktop::Tab>)> {
    use modules::desktop::Tab;
    let screen = match name.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "monitors" | "displays" => (Screen::Monitors, None),
        "window-rules" | "windowrules" | "rules" => (Screen::WindowRules, None),
        "shortcuts" | "keybinds" => (Screen::Shortcuts, None),
        "input" | "keyboard" => (Screen::Input, None),
        "network" | "wifi" | "wi-fi" => (Screen::Network, None),
        "bluetooth" | "bt" => (Screen::Bluetooth, None),
        // This is the name the tray's keep-awake menu is meant to link
        // to next — see the module doc comment on `modules::power` and
        // the `--screen idle` stand-in it replaces.
        "power" => (Screen::Power, None),
        "tray" => (Screen::Tray, None),
        "appearance" | "theme" => (Screen::Appearance, None),
        "desktop" => (Screen::Desktop, None),
        // These name a *tab*. A setting that lives on one is not
        // reachable by naming its screen alone, and landing someone on
        // Wallpaper when they asked for night light is the same miss as
        // not deep-linking at all. The tray icons are why: each opens the
        // page carrying its own setting.
        "wallpaper" => (Screen::Desktop, Some(Tab::Wallpaper)),
        "night-light" | "nightlight" => (Screen::Desktop, Some(Tab::NightLight)),
        "idle" | "keep-awake" => (Screen::Desktop, Some(Tab::Idle)),
        "screen-sharing" | "screensharing" => (Screen::Desktop, Some(Tab::ScreenSharing)),
        "session" | "autostart" => (Screen::Session, None),
        "system" => (Screen::System, None),
        _ => return None,
    };
    Some(screen)
}

/// The same validity check `--screen` uses, wrapped as a plain `fn(&str)
/// -> bool` so `ipc::handle_line` can validate a `show-screen` request
/// without knowing what a `Screen` is — one source of truth for "which
/// names are screens" shared between the command line and the control
/// socket.
pub(crate) fn screen_name_is_known(name: &str) -> bool {
    screen_from_cli(name).is_some()
}

/// Every name [`screen_from_cli`] accepts, for the usage message. The
/// aliases are deliberately left out: one canonical name per screen is
/// what a help text is for.
const SCREEN_NAMES: &[&str] = &[
    "monitors",
    "window-rules",
    "shortcuts",
    "input",
    "network",
    "bluetooth",
    "power",
    "tray",
    "appearance",
    "desktop",
    "wallpaper",
    "night-light",
    "idle",
    "screen-sharing",
    "session",
    "system",
];

const SIDEBAR_WIDTH: f32 = 240.0;
const CONTENT_MAX_WIDTH: f32 = 880.0;

/// This window's application id (X11 `WM_CLASS` / Wayland `app_id`).
/// Without setting `platform_specific.application_id` explicitly, iced's
/// default is an empty string (see `iced_core::window::settings::linux`),
/// which leaves nothing for Hyprland to select this window by — and
/// `focus_self` below needs exactly that to bring an already-running
/// window to the front for a second invocation that handed its request
/// off instead of opening its own. Matches the `.desktop` file's own
/// basename, per iced's own suggested convention.
const APP_ID: &str = "hyprforge-settings";

/// Set once from `--screen` before iced starts. A static because
/// `iced::daemon` builds the app from a function taking no arguments.
static INITIAL_SCREEN: std::sync::OnceLock<Screen> = std::sync::OnceLock::new();

/// The tab to open the Desktop screen on, when `--screen` named one.
///
/// Some settings live on a tab rather than a screen, and landing a user
/// on the Desktop screen's first tab when they asked for night light is
/// the same kind of miss as not deep-linking at all. The tray's icons
/// are the reason this exists: each one opens the page its own setting
/// is on.
static INITIAL_DESKTOP_TAB: std::sync::OnceLock<modules::desktop::Tab> =
    std::sync::OnceLock::new();

fn main() -> iced::Result {
    // `from_default_env()` alone defaults to ERROR, and these crates emit
    // no `error!` at all — so with RUST_LOG unset, which is how a GUI
    // launched from a menu always runs, every `warn!` in the app went
    // nowhere. That included the one saying the greeter's theme could
    // not be exported, which was itself the fix for a bug whose whole
    // symptom was silence. `hyprforge-displayd` already got this right;
    // this makes the GUI agree. RUST_LOG still overrides.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    // Resolve the shared look before the first frame. This is the one
    // place that joins the two halves: hyprforge-appearance knows what
    // the user set, hyprforge-ui knows how to draw it, and neither
    // depends on the other — the app they are both part of does the
    // introduction.
    hyprforge_ui::theme::init(hyprforge_appearance::look::resolve());

    // `daemon` rather than `application` because the revert countdown needs
    // its own window: a change that blanks a screen may well leave the
    // settings window itself invisible, so the prompt to undo it can't live
    // inside that window.
    // Hand-rolled rather than clap: one optional flag does not justify
    // pulling an argument parser into a GUI's dependency graph, and the
    // whole surface is visible here.
    let mut args = std::env::args().skip(1);
    // The raw string, not the parsed `Screen` — this is what travels over
    // the control socket if another instance turns out to already be
    // running (see below). The running instance validates it again
    // independently through the same `screen_from_cli`, so there is one
    // source of truth either way; this process validating it too, up
    // front, is what lets a typo be reported here even when nothing is
    // running yet to hand it off to.
    let mut requested_screen: Option<String> = None;
    while let Some(arg) = args.next() {
        let value = match arg.as_str() {
            "--screen" => args.next(),
            other => other.strip_prefix("--screen=").map(str::to_string),
        };
        let Some(value) = value else {
            eprintln!("usage: hyprforge-settings [--screen <{}>]", SCREEN_NAMES.join("|"));
            std::process::exit(2);
        };
        match screen_from_cli(&value) {
            Some((screen, tab)) => {
                INITIAL_SCREEN.set(screen).ok().unwrap_or(());
                if let Some(tab) = tab {
                    INITIAL_DESKTOP_TAB.set(tab).ok().unwrap_or(());
                }
                requested_screen = Some(value);
            }
            None => {
                // Naming a screen that does not exist is a typo worth
                // reporting, not a silent fall back to Monitors.
                eprintln!("hyprforge-settings: no screen called {value:?}");
                eprintln!("  known screens: {}", SCREEN_NAMES.join(", "));
                std::process::exit(2);
            }
        }
    }

    // One Settings window, ever: a second invocation (the tray spawns
    // `hyprforge-settings --screen <name>` fresh on every click) must
    // hand its request off to whichever instance is already running
    // rather than opening a second window somewhere else. See
    // `singleton.rs` for why this is an `flock`, not a PID file or a
    // `pgrep` on a name over 15 characters.
    //
    // Kept alive for as long as this `main` runs (nothing here ever drops
    // it early) — the lock covers the process's entire lifetime, not just
    // this check.
    let _singleton_lock = match singleton::acquire(&singleton::lock_path()) {
        Ok(Some(lock)) => Some(lock),
        Ok(None) => {
            // Another hyprforge-settings is already running. Handing this
            // off and exiting quietly and successfully is the whole
            // point — a tray click finding Settings already open is not
            // an error, and opening a second window here is exactly the
            // bug this exists to fix.
            let result = match &requested_screen {
                Some(name) => ipc::request_show_screen(name),
                None => ipc::request_focus(),
            };
            if let Err(e) = result {
                // Nothing to do about it from here: the request just
                // silently doesn't reach the running window this time.
                // Never printed as an alarming failure — this process is
                // still exiting 0, because a menu click is not an error.
                tracing::debug!(
                    error = %e,
                    "could not hand off to the already-running hyprforge-settings"
                );
            }
            return Ok(());
        }
        Err(e) => {
            // A broken lock (an unwritable runtime directory, say) must
            // never lock the app out entirely — start normally, same as
            // if this were the only instance.
            tracing::warn!(
                error = %e,
                "could not check whether hyprforge-settings is already running; starting normally"
            );
            None
        }
    };

    iced::daemon(App::new, App::update, App::view)
        .title(App::title)
        .theme(App::theme)
        .subscription(App::subscription)
        .run()
}

/// Settings for the main window, opened on boot.
fn main_window_settings() -> window::Settings {
    window::Settings {
        size: Size::new(1000.0, 700.0),
        min_size: Some(Size::new(760.0, 520.0)),
        position: window::Position::Centered,
        // See `APP_ID`'s doc: without this, Hyprland has nothing to
        // select this window by, and `focus_self` (used when a second
        // invocation hands its `--screen` off instead of opening its
        // own window) has nothing to target.
        platform_specific: window::settings::PlatformSpecific {
            application_id: APP_ID.to_string(),
            ..window::settings::PlatformSpecific::default()
        },
        ..window::Settings::default()
    }
}

/// The revert prompt's window title.
///
/// Also the handle Hyprland's dispatchers use to find it (see
/// [`pin_revert_popup`]), so it must stay stable, unique, and free of regex
/// metacharacters. The window is undecorated, so this is an identifier
/// rather than a visible caption.
const REVERT_POPUP_TITLE: &str = "Hyprforge Keep Display Settings";

fn revert_popup_settings() -> window::Settings {
    window::Settings {
        size: Size::new(420.0, 190.0),
        position: window::Position::Centered,
        resizable: false,
        decorations: false,
        // Honoured on X11/Windows/macOS. Wayland has no always-on-top
        // protocol, so this is a no-op there and `pin_revert_popup` does
        // the real work — it's set anyway so the intent survives a port.
        level: window::Level::AlwaysOnTop,
        ..window::Settings::default()
    }
}

/// Floats and pins the popup via Hyprland's IPC.
///
/// A Wayland client cannot ask to be kept above other windows — there's no
/// protocol for it — so `window::Level::AlwaysOnTop` does nothing here.
/// Hyprland can do it on our behalf: `float` lifts it out of the tiling
/// layout, and `pin` keeps it visible on every workspace. Without this the
/// prompt can end up tiled behind, or on a workspace the user isn't looking
/// at, which defeats the entire point of a countdown you must answer.
///
/// Best-effort by design: if the dispatch fails the prompt is still a real
/// window the user can reach, and the daemon reverts on its own regardless.
async fn pin_revert_popup() {
    // Give the compositor a moment to map the window before addressing it.
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;

    // Lua long-bracket strings pass their contents through verbatim, with no
    // escape processing, so a selector can never break the dispatch's own
    // parse — the same reason the window-rules codegen quotes regexes this
    // way. (A `\?` inside a normal Lua "..." literal is a hard parse error.)
    let selector = format!("[[title:^{REVERT_POPUP_TITLE}$]]");
    for dispatcher in ["float", "pin"] {
        let call =
            format!("hl.dsp.window.{dispatcher}({{ action = \"set\", window = {selector} }})");
        let dispatched = tokio::time::timeout(
            hyprforge_core::command::TIMEOUT,
            tokio::process::Command::new("hyprctl").arg("dispatch").arg(&call).output(),
        )
        .await;
        match dispatched.unwrap_or_else(|_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "hyprctl did not answer",
            ))
        }) {
            Ok(out) => {
                // hyprctl exits 0 even when the Lua call itself errored, so
                // the response body is what actually reports success.
                let body = String::from_utf8_lossy(&out.stdout);
                if !body.trim().eq_ignore_ascii_case("ok") {
                    tracing::warn!(
                        dispatcher,
                        response = %body.trim(),
                        "hyprctl rejected the dispatch; revert prompt may not stay on top"
                    );
                }
            }
            Err(e) => {
                // Not running Hyprland, or hyprctl isn't installed. The
                // prompt is still a real window the user can reach, and the
                // daemon reverts on its own either way.
                tracing::debug!(error = %e, "could not run hyprctl to pin the revert prompt");
                return;
            }
        }
    }
}

/// Focus is `hl.dsp.focus({ window = selector })`, and the verb was
/// checked against the running compositor rather than inferred from the
/// `float`/`pin` calls above it. The obvious guess —
/// `hl.dsp.window.focuswindow`, matching the shorthand `pin_revert_popup`
/// uses and Hyprland's own dispatcher name — does not exist here: the
/// compositor answers "attempt to call a nil value (field
/// 'focuswindow')". `hyprforge-shortcuts`'s catalogue agrees, carrying
/// this one as a bare `focus`.
///
/// A selector matching nothing comes back as "hl.focus: window not
/// found", which is what distinguishes "the right name" from "a name
/// that parses" — and is how this one was confirmed. A wrong verb would
/// cost only a logged warning, never a broken screen switch, which is
/// exactly why it was worth checking rather than trusting.
async fn focus_self() {
    let selector = format!("[[class:^{APP_ID}$]]");
    // `hl.dsp.focus`, top level — not `hl.dsp.window.focuswindow`, which
    // does not exist: the compositor answers "attempt to call a nil
    // value (field 'focuswindow')". Checked against the running
    // compositor rather than guessed, the way CLAUDE.md says every claim
    // about somebody else's interface has to be, and it agrees with
    // `hyprforge-shortcuts`'s catalogue, which also has this dispatcher
    // as a bare `focus`. A selector that matches nothing comes back as
    // "hl.focus: window not found", which is how this was confirmed to
    // be the right name rather than merely a name that parses.
    let call = format!("hl.dsp.focus({{ window = {selector} }})");
    let dispatched = tokio::time::timeout(
        hyprforge_core::command::TIMEOUT,
        tokio::process::Command::new("hyprctl").arg("dispatch").arg(&call).output(),
    )
    .await;
    match dispatched.unwrap_or_else(|_| {
        Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "hyprctl did not answer"))
    }) {
        Ok(out) => {
            let body = String::from_utf8_lossy(&out.stdout);
            if !body.trim().eq_ignore_ascii_case("ok") {
                tracing::warn!(
                    response = %body.trim(),
                    "hyprctl rejected the focus dispatch; the settings window may not have been raised"
                );
            }
        }
        Err(e) => {
            tracing::debug!(error = %e, "could not run hyprctl to focus the settings window");
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Monitors,
    WindowRules,
    Shortcuts,
    Input,
    Network,
    Bluetooth,
    Power,
    Tray,
    Appearance,
    Desktop,
    Session,
    System,
}

impl Screen {
    fn title(self) -> &'static str {
        match self {
            Screen::Monitors => "Monitors",
            Screen::WindowRules => "Window Rules",
            Screen::Shortcuts => "Shortcuts",
            Screen::Input => "Input",
            Screen::Network => "Network",
            Screen::Bluetooth => "Bluetooth",
            Screen::Power => "Power",
            Screen::Tray => "Tray",
            Screen::Appearance => "Appearance",
            Screen::Desktop => "Desktop",
            Screen::Session => "Session",
            Screen::System => "System",
        }
    }
}

/// A top-level sidebar grouping. Monitors and Window Rules share the
/// "Displays" category (both are about how windows/outputs are arranged);
/// Shortcuts gets its own category since keybindings are a different kind
/// of setting entirely — later categories (Network, Bluetooth, ...) each
/// get their own entry here too, rather than flattening everything into
/// one nav list.
struct NavCategory {
    label: &'static str,
    screens: &'static [Screen],
}

const NAV: &[NavCategory] = &[
    NavCategory {
        label: "Displays",
        screens: &[Screen::Monitors, Screen::WindowRules],
    },
    NavCategory {
        label: "Shortcuts",
        screens: &[Screen::Shortcuts],
    },
    NavCategory {
        label: "Input",
        screens: &[Screen::Input],
    },
    NavCategory {
        label: "Network",
        screens: &[Screen::Network],
    },
    NavCategory {
        label: "Bluetooth",
        screens: &[Screen::Bluetooth],
    },
    NavCategory {
        label: "Power",
        screens: &[Screen::Power],
    },
    NavCategory {
        label: "Tray",
        screens: &[Screen::Tray],
    },
    NavCategory {
        label: "Appearance",
        screens: &[Screen::Appearance, Screen::Desktop],
    },
    NavCategory {
        label: "Session",
        screens: &[Screen::Session, Screen::System],
    },
];

#[derive(Debug, Clone)]
enum Message {
    Navigate(Screen),
    SearchChanged(String),
    FocusSearch,
    ClearOrCancel,
    RefreshActive,
    Displays(modules::displays::Message),
    WindowRules(modules::window_rules::Message),
    Shortcuts(modules::shortcuts::Message),
    Input(modules::input::Message),
    Network(modules::network::Message),
    Bluetooth(modules::bluetooth::Message),
    Power(modules::power::Message),
    Tray(modules::tray::Message),
    Appearance(modules::appearance::Message),
    Desktop(modules::desktop::Message),
    Session(modules::session::Message),
    System(modules::catalog_screen::Message),
    WindowOpened(window::Id),
    WindowClosed(window::Id),
    RevertPopupOpened(window::Id),
    /// Something arrived on the control socket — see `ipc.rs`. A second
    /// `hyprforge-settings` invocation found this one already running and
    /// handed its request off instead of opening its own window.
    ExternalRequest(ipc::Signal),
    Noop,
}

struct App {
    screen: Screen,
    displays: DisplaysModule,
    window_rules: WindowRulesModule,
    shortcuts: ShortcutsModule,
    input: InputModule,
    network: NetworkModule<LazyNetworkManagerBackend>,
    bluetooth: BluetoothModule<LazyBlueZBackend>,
    power: PowerModule<LazyLogindBackend, LazyUPowerBackend, LazyPowerProfilesDaemonBackend>,
    tray: TrayModule,
    appearance: AppearanceModule,
    desktop: DesktopModule,
    session: SessionModule,
    system: SystemModule,
    search_query: String,
    search_id: Id,
    font_scale: FontScale,
    main_window: Option<window::Id>,
    /// The pinned countdown prompt, while a display change is provisional.
    revert_popup: Option<window::Id>,
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let (displays, displays_task) = DisplaysModule::new();
        let (window_rules, window_rules_task) = WindowRulesModule::new();
        let (shortcuts, shortcuts_task) = ShortcutsModule::new();
        let (input, input_task) = InputModule::new();
        let (network, network_task) =
            NetworkModule::new(std::sync::Arc::new(LazyNetworkManagerBackend::new()));
        let (bluetooth, bluetooth_task) =
            BluetoothModule::new(std::sync::Arc::new(LazyBlueZBackend::new()));
        let (power, power_task) = PowerModule::new(
            std::sync::Arc::new(LazyLogindBackend::new()),
            std::sync::Arc::new(LazyUPowerBackend::new()),
            std::sync::Arc::new(LazyPowerProfilesDaemonBackend::new()),
        );
        let (tray, tray_task) = TrayModule::new();
        let (appearance, appearance_task) = AppearanceModule::new();
        let (mut desktop, desktop_task) = DesktopModule::new();
        if let Some(tab) = INITIAL_DESKTOP_TAB.get() {
            desktop.open_on(*tab);
        }
        let (session, session_task) = SessionModule::new();
        let (system, system_task) = SystemModule::new();
        (
            App {
                screen: INITIAL_SCREEN.get().copied().unwrap_or(Screen::Monitors),
                displays,
                window_rules,
                shortcuts,
                input,
                network,
                bluetooth,
                power,
                tray,
                appearance,
                desktop,
                session,
                system,
                search_query: String::new(),
                search_id: Id::unique(),
                font_scale: FontScale(hyprforge_ui::theme::active().font_scale),
                main_window: None,
                revert_popup: None,
            },
            Task::batch([
                window::open(main_window_settings()).1.map(Message::WindowOpened),
                displays_task.map(Message::Displays),
                window_rules_task.map(Message::WindowRules),
                shortcuts_task.map(Message::Shortcuts),
                input_task.map(Message::Input),
                network_task.map(Message::Network),
                bluetooth_task.map(Message::Bluetooth),
                power_task.map(Message::Power),
                tray_task.map(Message::Tray),
                appearance_task.map(Message::Appearance),
                desktop_task.map(Message::Desktop),
                session_task.map(Message::Session),
                system_task.map(Message::System),
            ]),
        )
    }

    fn theme(&self, _window: window::Id) -> Theme {
        app_theme()
    }

    fn title(&self, window: window::Id) -> String {
        if Some(window) == self.revert_popup {
            REVERT_POPUP_TITLE.to_string()
        } else {
            "Hyprforge Settings".to_string()
        }
    }

    /// Opens or closes the countdown prompt to match the module's state.
    ///
    /// Driven from the module rather than duplicated: `revert_seconds_left`
    /// is set by the daemon's `RevertPending` signal and cleared by
    /// `RevertResolved`, so the window's lifetime tracks the daemon's own
    /// notion of "a change is provisional" instead of a second timer that
    /// could drift out of sync with it.
    fn sync_revert_popup(&mut self) -> Task<Message> {
        match (self.displays.revert_seconds_left(), self.revert_popup) {
            (Some(_), None) => {
                // Record the id now, not on `RevertPopupOpened`. `window::open`
                // hands it back before the window exists, and this runs on
                // every message from the module — including the `RevertTick`
                // that arrives every second while a countdown is live. Waiting
                // for the open to round-trip leaves those ticks looking at a
                // popup that is still `None`, so each one opens another window,
                // and every window whose id isn't the one recorded here falls
                // through `view` to the full settings UI, drawn at the prompt's
                // 420x190.
                let (id, open) = window::open(revert_popup_settings());
                self.revert_popup = Some(id);
                open.map(Message::RevertPopupOpened)
            }
            (None, Some(id)) => {
                self.revert_popup = None;
                window::close(id)
            }
            _ => Task::none(),
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Navigate(screen) => {
                self.screen = screen;
                Task::none()
            }
            Message::SearchChanged(query) => {
                self.search_query = query;
                Task::none()
            }
            Message::FocusSearch => operation::focus(self.search_id.clone()),
            Message::ClearOrCancel => {
                if !self.search_query.is_empty() {
                    self.search_query.clear();
                    Task::none()
                } else {
                    // Harmless no-ops when nothing's open: RenameCancelled
                    // clears a rename that may not be in progress,
                    // DraftCancel/CancelSetup close forms/dialogs that may
                    // already be closed. Cheaper and more robust than
                    // tracking "is anything open" at the App level.
                    Task::batch([
                        self.displays
                            .update(modules::displays::Message::RenameCancelled)
                            .map(Message::Displays),
                        self.displays
                            .update(modules::displays::Message::DeleteCancel)
                            .map(Message::Displays),
                        self.displays
                            .update(modules::displays::Message::ImportClose)
                            .map(Message::Displays),
                        self.window_rules
                            .update(modules::window_rules::Message::DraftCancel)
                            .map(Message::WindowRules),
                        self.window_rules
                            .update(modules::window_rules::Message::ImportCancel)
                            .map(Message::WindowRules),
                        self.shortcuts
                            .update(modules::shortcuts::Message::DraftCancel)
                            .map(Message::Shortcuts),
                        self.shortcuts
                            .update(modules::shortcuts::Message::ImportCancel)
                            .map(Message::Shortcuts),
                    ])
                }
            }
            Message::RefreshActive => match self.screen {
                Screen::Monitors => self
                    .displays
                    .update(modules::displays::Message::Refresh)
                    .map(Message::Displays),
                // Window Rules and Shortcuts have no external state to
                // refresh — each is the sole writer of its own TOML, so
                // it's always already current.
                Screen::WindowRules => Task::none(),
                Screen::Shortcuts => Task::none(),
                Screen::Input => Task::none(),
                Screen::Network => self
                    .network
                    .update(modules::network::Message::Refresh)
                    .map(Message::Network),
                Screen::Bluetooth => self
                    .bluetooth
                    .update(modules::bluetooth::Message::Refresh)
                    .map(Message::Bluetooth),
                Screen::Power => {
                    self.power.update(modules::power::Message::Refresh).map(Message::Power)
                }
                Screen::Tray => {
                    self.tray.update(modules::tray::Message::Refresh).map(Message::Tray)
                }
                Screen::Appearance => Task::none(),
                Screen::Desktop => Task::none(),
                Screen::Session => Task::none(),
                Screen::System => Task::none(),
            },
            Message::Displays(msg) => {
                let task = self.displays.update(msg).map(Message::Displays);
                Task::batch([task, self.sync_revert_popup()])
            }
            Message::Input(msg) => self.input.update(msg).map(Message::Input),
            Message::Network(msg) => self.network.update(msg).map(Message::Network),
            Message::Bluetooth(msg) => self.bluetooth.update(msg).map(Message::Bluetooth),
            Message::Power(msg) => self.power.update(msg).map(Message::Power),
            Message::Tray(msg) => self.tray.update(msg).map(Message::Tray),
            Message::Appearance(msg) => self.appearance.update(msg).map(Message::Appearance),
            Message::Desktop(msg) => self.desktop.update(msg).map(Message::Desktop),
            Message::Session(msg) => self.session.update(msg).map(Message::Session),
            Message::System(msg) => self.system.update(msg).map(Message::System),
            Message::WindowOpened(id) => {
                if self.main_window.is_none() {
                    self.main_window = Some(id);
                }
                Task::none()
            }
            Message::RevertPopupOpened(id) => {
                self.revert_popup = Some(id);
                // Ask Hyprland to float + pin it; see `pin_revert_popup`.
                Task::perform(pin_revert_popup(), |()| Message::Noop)
            }
            Message::WindowClosed(id) => {
                if Some(id) == self.revert_popup {
                    self.revert_popup = None;
                    // Closing the prompt is not an answer. The daemon keeps
                    // its own countdown and still reverts, so the change
                    // can't be silently kept by dismissing the window.
                    return Task::none();
                }
                if Some(id) == self.main_window {
                    return iced::exit();
                }
                Task::none()
            }
            Message::ExternalRequest(signal) => {
                if let ipc::Signal::ShowScreen(name) = signal {
                    // Validated once already by the socket server (see
                    // `ipc.rs`'s `handle_line`) against the same
                    // `screen_name_is_known` this resolves through, so
                    // `None` here should not happen in practice — but a
                    // request that somehow named nothing real still just
                    // falls through to "focus only" rather than panicking
                    // or navigating nowhere.
                    if let Some((screen, tab)) = screen_from_cli(&name) {
                        self.screen = screen;
                        if let Some(tab) = tab {
                            self.desktop.open_on(tab);
                        }
                    }
                }
                Task::perform(focus_self(), |()| Message::Noop)
            }
            Message::Noop => Task::none(),
            Message::WindowRules(msg) => self.window_rules.update(msg).map(Message::WindowRules),
            Message::Shortcuts(msg) => self.shortcuts.update(msg).map(Message::Shortcuts),
        }
    }

    fn view(&self, window: window::Id) -> Element<'_, Message> {
        if Some(window) == self.revert_popup {
            return self.revert_popup_view();
        }
        self.settings_view()
    }

    /// The countdown prompt. Deliberately tiny and self-contained: it may
    /// be the only thing legible on a screen that a bad mode change just
    /// scrambled, so it states what happened, how long is left, and the two
    /// ways out — nothing else.
    fn revert_popup_view(&self) -> Element<'_, Message> {
        let scale = self.font_scale;
        let left = self.displays.revert_seconds_left().unwrap_or(0);
        container(
            column![
                scaled_text("Keep these display settings?", 17.0, scale),
                scaled_text(
                    format!("Reverting in {left}s if you don't choose."),
                    13.0,
                    scale,
                )
                .color(text_dim()),
                row![
                    secondary_button("Revert now")
                        .on_press(Message::Displays(modules::displays::Message::RevertLayoutNow)),
                    primary_button("Keep changes")
                        .on_press(Message::Displays(modules::displays::Message::KeepLayout)),
                ]
                .spacing(spacing::SM),
            ]
            .spacing(spacing::MD)
            .padding(spacing::LG),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .center(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(Background::Color(surface::card())),
            border: iced::Border {
                radius: 10.0.into(),
                width: 1.0,
                color: surface::card_border(),
            },
            ..container::Style::default()
        })
        .into()
    }

    fn settings_view(&self) -> Element<'_, Message> {
        let scale = self.font_scale;

        let sidebar_button = |label: String, screen: Screen, active: bool| {
            let btn = if active {
                primary_button(label)
            } else {
                secondary_button(label)
            };
            btn.width(Length::Fill)
                .padding([10, 12])
                .on_press(Message::Navigate(screen))
        };

        let icon_for = |screen: Screen| match screen {
            Screen::Monitors => self.displays.icon(),
            Screen::WindowRules => self.window_rules.icon(),
            Screen::Shortcuts => self.shortcuts.icon(),
            Screen::Input => self.input.icon(),
            Screen::Network => self.network.icon(),
            Screen::Bluetooth => self.bluetooth.icon(),
            Screen::Power => self.power.icon(),
            Screen::Tray => self.tray.icon(),
            Screen::Appearance => self.appearance.icon(),
            Screen::Desktop => self.desktop.icon(),
            Screen::Session => self.session.icon(),
            Screen::System => self.system.icon(),
        };

        let query = self.search_query.to_lowercase();
        let mut nav = column![].spacing(spacing::MD);
        let mut any_visible = false;
        for category in NAV {
            let visible_screens: Vec<Screen> = category
                .screens
                .iter()
                .copied()
                .filter(|s| query.is_empty() || s.title().to_lowercase().contains(&query))
                .collect();
            if visible_screens.is_empty() {
                continue;
            }
            any_visible = true;

            let mut sub_items = column![].spacing(spacing::XS);
            for screen in visible_screens {
                let label = format!("{}  {}", icon_for(screen), screen.title());
                sub_items = sub_items.push(sidebar_button(label, screen, self.screen == screen));
            }

            // The category header is itself a button to its first/default
            // sub-item, not just a static label — "Displays" takes you to
            // Monitors the same way clicking "Monitors" does.
            let header = iced::widget::button(
                scaled_text(category.label.to_uppercase(), 11.0, scale).color(text_dim()),
            )
            .style(|_theme: &Theme, _status| iced::widget::button::Style::default())
            .padding(0)
            .on_press(Message::Navigate(category.screens[0]));

            nav = nav.push(
                column![
                    header,
                    container(sub_items).padding(iced::Padding {
                        left: 4.0,
                        ..iced::Padding::default()
                    }),
                ]
                .spacing(spacing::XS),
            );
        }
        if !any_visible {
            nav = nav.push(scaled_text("No matches", 13.0, scale).color(text_dim()));
        }

        let sidebar = container(
            column![
                scaled_text("Hyprforge", 20.0, scale),
                text_input("Search…", &self.search_query)
                    .id(self.search_id.clone())
                    .on_input(Message::SearchChanged)
                    .padding(8),
                nav,
            ]
            .spacing(spacing::MD)
            .padding(spacing::MD)
            .width(Length::Fixed(SIDEBAR_WIDTH)),
        )
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(Background::Color(surface::sidebar())),
            ..container::Style::default()
        });

        let content: Element<'_, Message> = match self.screen {
            Screen::Monitors => self.displays.view(scale).map(Message::Displays),
            Screen::WindowRules => self.window_rules.view(scale).map(Message::WindowRules),
            Screen::Shortcuts => self.shortcuts.view(scale).map(Message::Shortcuts),
            Screen::Input => self.input.view(scale).map(Message::Input),
            Screen::Network => self.network.view(scale).map(Message::Network),
            Screen::Bluetooth => self.bluetooth.view(scale).map(Message::Bluetooth),
            Screen::Power => self.power.view(scale).map(Message::Power),
            Screen::Tray => self.tray.view(scale).map(Message::Tray),
            Screen::Appearance => self.appearance.view(scale).map(Message::Appearance),
            Screen::Desktop => self.desktop.view(scale).map(Message::Desktop),
            Screen::Session => self.session.view(scale).map(Message::Session),
            Screen::System => self.system.view(scale).map(Message::System),
        };
        // The Monitors editor (canvas + full property panel + policy/swap
        // sections) routinely exceeds window height — without scrolling,
        // everything past the window edge was just clipped and silently
        // invisible, not merely off-screen.
        //
        // Order matters here: the scrollable has to be the *outermost* of
        // these three, so its scrollbar tracks the edge of the content pane.
        // Nesting it inside the max-width/centering containers instead pins
        // the bar to the right edge of the centered column, which on a wide
        // window reads as a scrollbar floating in the middle of the screen.
        let content = container(content)
            .max_width(CONTENT_MAX_WIDTH)
            .width(Length::Fill);
        // Centred in the pane, GNOME-style. Left-aligning a capped column in
        // a very wide window dumps all the slack on one side, which reads as
        // a broken layout; splitting it evenly reads as deliberate margin.
        let content = container(content).width(Length::Fill).center_x(Length::Fill);
        let content = iced::widget::scrollable(content)
            .width(Length::Fill)
            .height(Length::Fill);

        container(row![sidebar, content])
            .style(|_theme: &Theme| container::Style {
                background: Some(Background::Color(surface::root())),
                ..container::Style::default()
            })
            .into()
    }

    /// The one keyboard grammar used across the whole app (vision pillar
    /// #9 — pick once, document it, never diverge per-module):
    ///
    /// - `Ctrl+1` / `Ctrl+2` / `Ctrl+3` — switch to Monitors / Window Rules /
    ///   Shortcuts
    /// - `Ctrl+F` — focus the sidebar search box
    /// - `Ctrl+R` — refresh the active module
    /// - `Escape` — clear the search box if it has text, else cancel
    ///   whatever draft/dialog is open in the active module
    fn subscription(&self) -> Subscription<Message> {
        // While the Shortcuts module is recording a chord, every key on the
        // keyboard belongs to it — including Ctrl+F and Escape. Binding
        // Ctrl+F would otherwise navigate to the search box instead of being
        // recorded, and there'd be no way to bind it at all.
        if self.shortcuts.is_capturing() {
            return Subscription::batch([
                self.displays.subscription().map(Message::Displays),
                self.shortcuts.subscription().map(Message::Shortcuts),
                self.network.subscription().map(Message::Network),
                self.bluetooth.subscription().map(Message::Bluetooth),
                self.power.subscription().map(Message::Power),
                window::close_events().map(Message::WindowClosed),
                Subscription::run(ipc_stream),
            ]);
        }

        let shortcuts = keyboard::listen().filter_map(|event| {
            let keyboard::Event::KeyPressed { key, modifiers, .. } = event else {
                return None;
            };
            if !modifiers.control() {
                return match key {
                    Key::Named(key::Named::Escape) => Some(Message::ClearOrCancel),
                    _ => None,
                };
            }
            match key.as_ref() {
                Key::Character("1") => Some(Message::Navigate(Screen::Monitors)),
                Key::Character("2") => Some(Message::Navigate(Screen::WindowRules)),
                Key::Character("3") => Some(Message::Navigate(Screen::Shortcuts)),
                Key::Character("f") => Some(Message::FocusSearch),
                Key::Character("r") => Some(Message::RefreshActive),
                _ => None,
            }
        });

        Subscription::batch([
            self.displays.subscription().map(Message::Displays),
            self.shortcuts.subscription().map(Message::Shortcuts),
            self.network.subscription().map(Message::Network),
            self.bluetooth.subscription().map(Message::Bluetooth),
            self.power.subscription().map(Message::Power),
            shortcuts,
            window::close_events().map(Message::WindowClosed),
            Subscription::run(ipc_stream),
        ])
    }
}

/// Runs the control socket for as long as the app runs, and turns
/// whatever arrives on it into a `Message`. `ipc.rs` never mentions
/// `Message` or `Screen` — this is the one place that closes the loop, the
/// same split `displays.rs`'s `signal_stream` makes for D-Bus signals.
///
/// If the socket dies (a bind error — in practice, only possible if the
/// singleton lock in `main` somehow let two instances through at once),
/// this retries after a few seconds rather than silently leaving the app
/// with no way for a second invocation to ever reach it again.
fn ipc_stream() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(16, |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
        use iced::futures::SinkExt;

        loop {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ipc::Signal>();
            let server = tokio::spawn(ipc::run(tx));

            while let Some(signal) = rx.recv().await {
                let _ = output.send(Message::ExternalRequest(signal)).await;
            }

            match server.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!(error = %e, "settings control socket stopped"),
                Err(e) => tracing::warn!(error = %e, "settings control socket task panicked"),
            }
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
    })
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    use modules::desktop::Tab;

    /// Every tray icon opens the page its own setting is on. The two
    /// that live on a tab are the reason `--screen` can name one: before
    /// this, clicking night light landed on Wallpaper, and clicking keep
    /// awake did nothing at all because it had no mapping.
    #[test]
    fn each_tray_icon_opens_the_page_carrying_its_own_setting() {
        for (icon, expected_screen, expected_tab) in [
            ("network", Screen::Network, None),
            ("bluetooth", Screen::Bluetooth, None),
            ("night-light", Screen::Desktop, Some(Tab::NightLight)),
            ("idle", Screen::Desktop, Some(Tab::Idle)),
        ] {
            assert_eq!(
                screen_from_cli(icon),
                Some((expected_screen, expected_tab)),
                "{icon} does not open where its setting lives"
            );
        }
    }

    /// Naming the screen still works and leaves the tab alone, so
    /// `--screen desktop` opens wherever that screen opens.
    #[test]
    fn naming_a_screen_rather_than_a_tab_does_not_choose_one() {
        assert_eq!(screen_from_cli("desktop"), Some((Screen::Desktop, None)));
    }

    /// `--screen power` is the name `hyprforge-trayd`'s keep-awake menu is
    /// meant to link to next, replacing its `--screen idle` stand-in — see
    /// the module doc comment on `modules::power`.
    #[test]
    fn screen_power_opens_the_power_screen() {
        assert_eq!(screen_from_cli("power"), Some((Screen::Power, None)));
    }

    /// `--screen tray` is the Tray screen's own name — for hand deep-linking
    /// and for `hyprforge-settings --screen tray` from a desktop entry.
    /// Nothing currently sends this from `hyprforge-trayd`'s own menus
    /// (each icon's settings row still goes to the screen carrying that
    /// icon's own setting — Network, Bluetooth, or Power), so this is
    /// reachable only by asking for the Tray screen by name.
    #[test]
    fn screen_tray_opens_the_tray_screen() {
        assert_eq!(screen_from_cli("tray"), Some((Screen::Tray, None)));
    }

    /// A name in the help text that the parser rejects is a promise the
    /// program does not keep.
    #[test]
    fn every_name_the_usage_message_lists_actually_parses() {
        for name in SCREEN_NAMES {
            assert!(
                screen_from_cli(name).is_some(),
                "{name} is offered in --help but is not accepted"
            );
        }
    }

    #[test]
    fn an_unknown_screen_is_refused_rather_than_falling_back() {
        assert_eq!(screen_from_cli("bogus"), None);
        assert_eq!(screen_from_cli(""), None);
    }
}

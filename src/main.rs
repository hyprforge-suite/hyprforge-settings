mod ipc;
mod look;
mod module;
mod modules;
mod search;
mod setup_cli;
mod singleton;

use hyprforge_ui::density;
use hyprforge_ui::theme::{app_theme, spacing, surface, text, text_dim, FontScale};
use hyprforge_ui::widgets::{
    chip, config_line, countdown_ring, hint_text, page_header, pending_bar, primary_button, scaled_text, search_field, secondary_button,
    section_label, selectable_row_style, status_dot, Tint,
};
use crate::module::{NavBadge, Pending, SearchEntry, SettingsModule};
use iced::keyboard::{self, key, Key};
use iced::widget::{column, container, operation, row, Id, Space};
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
    LazyPowerProfilesDaemonBackend, LazyUPowerBackend, PowerModule,
};
use modules::shortcuts::ShortcutsModule;
use modules::default_apps::DefaultAppsModule;
use modules::setup::SetupModule;
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
/// of the nineteen pages, and injecting a click needs tooling that is not
/// on every machine. A screenshot is how the `web-colors` bug was found;
/// this is what makes taking one repeatable.
fn screen_from_cli(name: &str) -> Option<Screen> {
    let screen = match name.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "displays" | "monitors" => Screen::Monitors,
        "power" | "battery" => Screen::Power,
        "keyboard-mouse" | "input" | "keyboard" | "mouse" => Screen::Input,
        "default-apps" | "defaults" | "applications" => Screen::DefaultApps,
        "network" | "wifi" | "wi-fi" => Screen::Network,
        "bluetooth" | "bt" => Screen::Bluetooth,
        "windows" | "workspaces" | "windows-workspaces" => Screen::WindowsWorkspaces,
        "keybinds" | "shortcuts" => Screen::Shortcuts,
        "animations" => Screen::Animations,
        "window-rules" | "windowrules" | "rules" => Screen::WindowRules,
        // `keep-awake` is the tray's name for it, and has been since
        // before idle was a page of its own.
        "idle" | "keep-awake" | "idle-lock" => Screen::Idle,
        "session" | "autostart" => Screen::Session,
        "advanced" | "system" => Screen::System,
        "appearance" | "theme" => Screen::Appearance,
        // `desktop` was the screen these four pages were tabs of, and it
        // always opened on its first — so it still opens there.
        "wallpaper" | "desktop" => Screen::Wallpaper,
        "night-light" | "nightlight" => Screen::NightLight,
        "screen-sharing" | "screensharing" => Screen::Sharing,
        "tray" => Screen::Tray,
        // What `--setup` hands a running window when it finds one open,
        // so the wording matches the flag rather than the page title.
        "setup" | "set-up" | "get-started" => Screen::Setup,
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
    "setup",
    "displays",
    "power",
    "keyboard-mouse",
    "default-apps",
    "network",
    "bluetooth",
    "windows",
    "keybinds",
    "animations",
    "window-rules",
    "idle",
    "session",
    "advanced",
    "appearance",
    "wallpaper",
    "night-light",
    "screen-sharing",
    "tray",
];

const SIDEBAR_WIDTH: f32 = 240.0;

/// The most pages the palette lists, and the most rows in each of its
/// other two groups. Enough to find what you meant; past this the query
/// wants another letter rather than the palette wanting a scrollbar.
const PALETTE_PAGES: usize = 4;
const PALETTE_ROWS: usize = 6;

/// How wide the palette grows at most — wide enough for a label and its
/// page side by side, narrow enough to read as a panel over the page
/// rather than a second page.
const PALETTE_WIDTH: f32 = 640.0;

/// What the palette found, in the order Enter would take it.
struct Palette {
    pages: Vec<Screen>,
    settings: Vec<(Screen, SearchEntry<Message>)>,
    keys: Vec<(Screen, SearchEntry<Message>)>,
}

impl Palette {
    /// What Enter does: the first page, else the first setting, else the
    /// first key — the same order the palette draws them in, so the
    /// highlighted row is always the one that gets taken.
    fn first(&self) -> Option<Message> {
        if let Some(screen) = self.pages.first() {
            return Some(Message::Navigate(*screen));
        }
        self.settings
            .first()
            .or(self.keys.first())
            .map(|(screen, entry)| Message::Reveal(*screen, entry.reveal.clone()))
    }
}

/// A sidebar page mark's box at 100%. The mark itself fills most of it;
/// at the view marks' smaller size an outline beside a 13px label read
/// as a speck.
const NAV_MARK_BASE: f32 = 19.0;
const CONTENT_MAX_WIDTH: f32 = 880.0;

/// A page's content, capped at a readable width and centred in the
/// pane, GNOME-style. Left-aligning a capped column in a very wide window
/// dumps all the slack on one side, which reads as a broken layout;
/// splitting it evenly reads as deliberate margin.
fn centred(content: Element<'_, Message>) -> Element<'_, Message> {
    let capped = container(content).max_width(CONTENT_MAX_WIDTH).width(Length::Fill);
    container(capped).width(Length::Fill).center_x(Length::Fill).into()
}

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

/// Set from `--search`: text to open the window with in the search
/// field, palette showing.
///
/// For the same reason `--screen` exists — making a state reviewable by
/// screenshot. The palette only exists while there is a query, and
/// without this there is no way to raise it from outside the app on a
/// machine with no input injector. It only applies to a window this
/// process opens; a second invocation hands its `--screen` to the
/// running one and says nothing about a search.
static INITIAL_SEARCH: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn main() -> iced::Result {
    // Keep awake's holder: this binary run only to hold the inhibit, so
    // it outlives the window that turned it on — see
    // `hyprforge_power::keep_awake`. First, before the single-instance
    // lock, which would otherwise hand the holder off to a running
    // Settings window and exit.
    if std::env::args().nth(1).as_deref() == Some(hyprforge_power::keep_awake::HOLDER_ARG) {
        let held = tokio::runtime::Runtime::new()
            .map_err(|e| e.to_string())
            .and_then(|rt| rt.block_on(hyprforge_power::keep_awake::hold()).map_err(|e| e.to_string()));
        if let Err(e) = held {
            eprintln!("hyprforge-settings: keep awake could not take hold: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // `--setup`: the Set up page without a window, for a terminal and for
    // the installer — see `setup_cli`. Before the theme and the window,
    // neither of which it needs, and before the singleton handoff below,
    // which it makes for itself and only for the modes that write.
    if std::env::args().nth(1).as_deref() == Some("--setup") {
        let rest: Vec<String> = std::env::args().skip(2).collect();
        let mode = match setup_cli::parse(&rest) {
            Ok(mode) => mode,
            Err(message) => {
                eprintln!("hyprforge-settings: {message}");
                eprintln!("{}", setup_cli::USAGE);
                std::process::exit(2);
            }
        };
        let stdin = std::io::stdin();
        let status = setup_cli::run(
            &mode,
            &hyprforge_setup::Env::from_environment(),
            &hyprforge_setup::RealSystem,
            &singleton::lock_path(),
            &|| ipc::request_show_screen("setup").map_err(|e| e.to_string()),
            setup_cli::Io {
                input: &mut stdin.lock(),
                out: &mut std::io::stdout(),
                err: &mut std::io::stderr(),
            },
        );
        std::process::exit(status);
    }
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
        if arg == "--search" {
            let Some(query) = args.next() else {
                eprintln!("usage: hyprforge-settings [--screen <name>] [--search <text>]");
                std::process::exit(2);
            };
            INITIAL_SEARCH.set(query).ok().unwrap_or(());
            continue;
        }
        let value = match arg.as_str() {
            "--screen" => args.next(),
            other => other.strip_prefix("--screen=").map(str::to_string),
        };
        let Some(value) = value else {
            eprintln!(
                "usage: hyprforge-settings [--screen <{}>] [--search <text>]",
                SCREEN_NAMES.join("|")
            );
            std::process::exit(2);
        };
        match screen_from_cli(&value) {
            Some(screen) => {
                INITIAL_SCREEN.set(screen).ok().unwrap_or(());
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

/// Whether the first Set up check may still choose the opening page:
/// only when `--screen` did not. A deep link is somebody asking for a
/// page — the tray's network icon, a notification — and opening Set up
/// over it would answer a question nobody asked.
fn first_launch_waits(initial: Option<Screen>) -> bool {
    initial.is_none()
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

/// The countdown ring's side at 100%.
const REVERT_RING_BASE: f32 = 56.0;

/// What the countdown prompt says under its question.
///
/// "Automatically" and "if you can't see this" are the point: the prompt
/// exists because the change may have blanked the screen it is on, and
/// someone who can read it needs to know that doing nothing is safe.
fn revert_sentence(left: u32) -> String {
    match left {
        1 => "Reverting automatically in 1 second if you can't see this.".into(),
        n => format!("Reverting automatically in {n} seconds if you can't see this."),
    }
}

fn revert_popup_settings() -> window::Settings {
    window::Settings {
        size: Size::new(460.0, 180.0),
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

/// A page in the sidebar.
///
/// Not the same thing as a module: Appearance's three tabs and Desktop's
/// four are pages of their own, each backed by the one module that holds
/// their state — see [`Screen::host`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Monitors,
    Power,
    Input,
    DefaultApps,
    Network,
    Bluetooth,
    WindowsWorkspaces,
    Shortcuts,
    Animations,
    WindowRules,
    Idle,
    Session,
    System,
    Appearance,
    Wallpaper,
    NightLight,
    Sharing,
    Tray,
    Setup,
}

impl Screen {
    fn title(self) -> &'static str {
        match self {
            Screen::Monitors => "Displays",
            Screen::Power => "Power & battery",
            Screen::Input => "Keyboard & mouse",
            Screen::DefaultApps => "Default apps",
            Screen::Network => "Network",
            Screen::Bluetooth => "Bluetooth",
            Screen::WindowsWorkspaces => "Windows & workspaces",
            Screen::Shortcuts => "Keybinds",
            Screen::Animations => "Animations",
            Screen::WindowRules => "Window rules",
            Screen::Idle => "Idle & lock",
            // Not the mockup's "Autostart & environment": the page also
            // holds gestures and permissions, and a name listing two of
            // its four tabs hides the other two.
            Screen::Session => "Session",
            Screen::System => "Advanced",
            Screen::Appearance => "Appearance",
            Screen::Wallpaper => "Wallpaper",
            Screen::NightLight => "Night light",
            Screen::Sharing => "Screen sharing",
            Screen::Tray => "Tray",
            Screen::Setup => "Set up",
        }
    }

    /// The mark beside this page in the sidebar.
    fn glyph(self) -> hyprforge_ui::glyph::Page {
        use hyprforge_ui::glyph::Page;
        match self {
            Screen::Monitors => Page::Display,
            Screen::Power => Page::Power,
            Screen::Input => Page::Keyboard,
            Screen::DefaultApps => Page::Apps,
            Screen::Network => Page::Network,
            Screen::Bluetooth => Page::Bluetooth,
            Screen::WindowsWorkspaces => Page::Windows,
            Screen::Shortcuts => Page::Keybinds,
            Screen::Animations => Page::Animation,
            Screen::WindowRules => Page::WindowRules,
            Screen::Idle => Page::Idle,
            Screen::Session => Page::Session,
            Screen::System => Page::Advanced,
            Screen::Appearance => Page::Appearance,
            Screen::Wallpaper => Page::Wallpaper,
            Screen::NightLight => Page::NightLight,
            Screen::Sharing => Page::ScreenSharing,
            Screen::Tray => Page::Tray,
            Screen::Setup => Page::Setup,
        }
    }

    /// The module that holds this page's state and draws its body.
    fn host(self) -> Host {
        match self {
            Screen::Monitors => Host::Displays,
            Screen::Power => Host::Power,
            Screen::Input => Host::Input,
            Screen::DefaultApps => Host::DefaultApps,
            Screen::Network => Host::Network,
            Screen::Bluetooth => Host::Bluetooth,
            Screen::Shortcuts => Host::Shortcuts,
            Screen::WindowRules => Host::WindowRules,
            Screen::Session => Host::Session,
            Screen::System => Host::System,
            Screen::Tray => Host::Tray,
            Screen::Setup => Host::Setup,
            Screen::Appearance | Screen::WindowsWorkspaces | Screen::Animations => Host::Appearance,
            Screen::Wallpaper | Screen::NightLight | Screen::Idle | Screen::Sharing => {
                Host::Desktop
            }
        }
    }

    /// Which of Appearance's tabs this page is, if it is one.
    fn appearance_tab(self) -> Option<modules::appearance::Tab> {
        use modules::appearance::Tab;
        match self {
            Screen::Appearance => Some(Tab::Theme),
            Screen::WindowsWorkspaces => Some(Tab::Windows),
            Screen::Animations => Some(Tab::Animations),
            _ => None,
        }
    }

    /// Which of Desktop's tabs this page is, if it is one.
    fn desktop_tab(self) -> Option<modules::desktop::Tab> {
        use modules::desktop::Tab;
        match self {
            Screen::Wallpaper => Some(Tab::Wallpaper),
            Screen::NightLight => Some(Tab::NightLight),
            Screen::Idle => Some(Tab::Idle),
            Screen::Sharing => Some(Tab::ScreenSharing),
            _ => None,
        }
    }
}

/// One of the modules the app hosts. Every per-module dispatch in the
/// shell matches on this rather than on [`Screen`], so a page added over
/// an existing module's tab needs no new arm anywhere but [`Screen`]'s
/// own methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Host {
    Displays,
    WindowRules,
    Shortcuts,
    Input,
    Network,
    Bluetooth,
    Power,
    Tray,
    Appearance,
    Desktop,
    DefaultApps,
    Session,
    System,
    Setup,
}

/// A group of pages in the sidebar — the mockup's four.
struct NavCategory {
    label: &'static str,
    screens: &'static [Screen],
}

/// The sidebar, in the mockup's grouping: the machine, what it connects
/// to, the compositor, and the person using it.
///
/// Pages the mockup does not draw sit in the nearest group — Default apps
/// with the machine, Tray and the desktop daemons' pages with the person.
/// The mockup's Sound, Users and Notifications have no page here: an
/// entry that opened onto nothing would be a promise the app does not
/// keep.
const NAV: &[NavCategory] = &[
    // First, above the machine: it is where a fresh install starts, and
    // its count beside it says whether anything is still left — which is
    // the way back here for whoever dismissed it on first launch.
    NavCategory { label: "Get started", screens: &[Screen::Setup] },
    NavCategory {
        label: "System",
        screens: &[Screen::Monitors, Screen::Power, Screen::Input, Screen::DefaultApps],
    },
    NavCategory {
        label: "Connectivity",
        screens: &[Screen::Network, Screen::Bluetooth],
    },
    NavCategory {
        label: "Hyprland",
        screens: &[
            Screen::WindowsWorkspaces,
            Screen::Shortcuts,
            Screen::Animations,
            Screen::WindowRules,
            Screen::Idle,
            Screen::Session,
            Screen::System,
        ],
    },
    NavCategory {
        label: "Personal",
        screens: &[
            Screen::Appearance,
            Screen::Wallpaper,
            Screen::NightLight,
            Screen::Sharing,
            Screen::Tray,
        ],
    },
];

#[derive(Debug, Clone)]
enum Message {
    Navigate(Screen),
    SearchChanged(String),
    /// Enter in the search field: take the palette's first result.
    SearchSubmit,
    /// A setting picked in the palette: go to its page, then send the
    /// page what it needs to bring that setting into view.
    Reveal(Screen, Vec<Message>),
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
    DefaultApps(modules::default_apps::Message),
    Session(modules::session::Message),
    System(modules::catalog_screen::Message),
    Setup(modules::setup::Message),
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
    power: PowerModule<hyprforge_power::DetachedBackend, LazyUPowerBackend, LazyPowerProfilesDaemonBackend>,
    tray: TrayModule,
    appearance: AppearanceModule,
    desktop: DesktopModule,
    default_apps: DefaultAppsModule,
    session: SessionModule,
    system: SystemModule,
    setup: SetupModule,
    /// True until the first Set up check has had its say over which page
    /// the window shows — see [`modules::setup::should_open_on_setup`].
    /// False from the start when `--screen` named a page, and cleared the
    /// moment anyone goes anywhere, so a late check never moves a window
    /// somebody is already using.
    first_launch_waiting: bool,
    /// When the app started, for [`modules::setup::FIRST_LAUNCH_GRACE`].
    started: std::time::Instant,
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
            // Keep awake is held by a process of its own, so closing this
            // window no longer turns it off — `hyprforge_power::keep_awake`.
            std::sync::Arc::new(hyprforge_power::DetachedBackend::this_binary().unwrap_or_else(|_| {
                hyprforge_power::DetachedBackend::new("hyprforge-settings".into())
            })),
            std::sync::Arc::new(LazyUPowerBackend::new()),
            std::sync::Arc::new(LazyPowerProfilesDaemonBackend::new()),
        );
        let (tray, tray_task) = TrayModule::new();
        let screen = INITIAL_SCREEN.get().copied().unwrap_or(Screen::Monitors);
        let (mut appearance, appearance_task) = AppearanceModule::new();
        let (mut desktop, desktop_task) = DesktopModule::new();
        // The same as `open_tabs`, before there is an `App` to call it on.
        if let Some(tab) = screen.appearance_tab() {
            let _ = appearance.update(modules::appearance::Message::TabSelected(tab));
        }
        if let Some(tab) = screen.desktop_tab() {
            let _ = desktop.update(modules::desktop::Message::TabSelected(tab));
        }
        let (default_apps, default_apps_task) = DefaultAppsModule::new();
        let (session, session_task) = SessionModule::new();
        let (system, system_task) = SystemModule::new();
        // The first check runs on the blocking pool and the window opens
        // without it; `Message::Setup` decides on arrival whether it
        // still may move the window here.
        let (setup, setup_task) = SetupModule::new();
        (
            App {
                screen,
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
                default_apps,
                session,
                system,
                setup,
                first_launch_waiting: first_launch_waits(INITIAL_SCREEN.get().copied()),
                started: std::time::Instant::now(),
                search_query: INITIAL_SEARCH.get().cloned().unwrap_or_default(),
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
                default_apps_task.map(Message::DefaultApps),
                session_task.map(Message::Session),
                system_task.map(Message::System),
                setup_task.map(Message::Setup),
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
                self.first_launch_waiting = false;
                self.screen = screen;
                self.open_tabs();
                // Going somewhere closes the palette, whether it was the
                // palette that sent you or the sidebar.
                self.search_query.clear();
                Task::none()
            }
            Message::SearchSubmit => match self.palette().first() {
                Some(pick) => self.update(pick),
                None => Task::none(),
            },
            Message::Reveal(screen, messages) => {
                self.first_launch_waiting = false;
                self.screen = screen;
                self.open_tabs();
                self.search_query.clear();
                let mut tasks = Vec::with_capacity(messages.len());
                for message in messages {
                    tasks.push(self.update(message));
                }
                Task::batch(tasks)
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
            Message::RefreshActive => match self.screen.host() {
                Host::Displays => self
                    .displays
                    .update(modules::displays::Message::Refresh)
                    .map(Message::Displays),
                // Window Rules and Shortcuts have no external state to
                // refresh — each is the sole writer of its own TOML, so
                // it's always already current.
                Host::WindowRules => Task::none(),
                Host::Shortcuts => Task::none(),
                Host::Input => Task::none(),
                Host::Network => self
                    .network
                    .update(modules::network::Message::Refresh)
                    .map(Message::Network),
                Host::Bluetooth => self
                    .bluetooth
                    .update(modules::bluetooth::Message::Refresh)
                    .map(Message::Bluetooth),
                Host::Power => {
                    self.power.update(modules::power::Message::Refresh).map(Message::Power)
                }
                Host::Tray => {
                    self.tray.update(modules::tray::Message::Refresh).map(Message::Tray)
                }
                // An application may have been installed since this
                // screen was last looked at.
                Host::DefaultApps => self
                    .default_apps
                    .update(modules::default_apps::Message::Refresh)
                    .map(Message::DefaultApps),
                Host::Appearance => Task::none(),
                Host::Desktop => Task::none(),
                Host::Session => Task::none(),
                Host::System => Task::none(),
                Host::Setup => {
                    self.setup.update(modules::setup::Message::Check).map(Message::Setup)
                }
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
            Message::DefaultApps(msg) => {
                self.default_apps.update(msg).map(Message::DefaultApps)
            }
            Message::Appearance(msg) => self.appearance.update(msg).map(Message::Appearance),
            Message::Desktop(msg) => self.desktop.update(msg).map(Message::Desktop),
            Message::Session(msg) => self.session.update(msg).map(Message::Session),
            Message::System(msg) => self.system.update(msg).map(Message::System),
            Message::Setup(msg) => self.setup_message(msg),
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
                    if let Some(screen) = screen_from_cli(&name) {
                        self.first_launch_waiting = false;
                        self.screen = screen;
                        self.open_tabs();
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
        let total = self.displays.revert_seconds_total().unwrap_or(left);
        // The mockup's 1f: the countdown as a ring beside the question,
        // so how much time is left reads at a glance and not only as a
        // number, then the two ways out on their own row.
        let question = column![
            scaled_text("Keep these display settings?", 16.0, scale)
                .font(iced::Font { weight: iced::font::Weight::Semibold, ..iced::Font::DEFAULT })
                .color(text()),
            scaled_text(revert_sentence(left), density::META_TEXT_BASE, scale).color(text_dim()),
        ]
        .spacing(spacing::XS);
        container(
            column![
                row![countdown_ring(left, total, scale.apply(REVERT_RING_BASE)), question]
                    .spacing(spacing::MD)
                    .align_y(iced::Alignment::Center),
                row![
                    Space::new().width(Length::Fill),
                    secondary_button("Revert")
                        .on_press(Message::Displays(modules::displays::Message::RevertLayoutNow)),
                    primary_button("Keep changes")
                        .on_press(Message::Displays(modules::displays::Message::KeepLayout)),
                ]
                .spacing(spacing::SM),
            ]
            .spacing(spacing::LG)
            .padding(spacing::LG),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .center(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(Background::Color(surface::card())),
            border: iced::Border {
                radius: density::outer_radius().into(),
                width: 1.0,
                color: surface::card_border(),
            },
            ..container::Style::default()
        })
        .into()
    }

    /// This page's mono subtitle, from its module.
    fn subtitle_for(&self, screen: Screen) -> Option<String> {
        match screen.host() {
            Host::Displays => self.displays.subtitle(),
            Host::WindowRules => self.window_rules.subtitle(),
            Host::Shortcuts => self.shortcuts.subtitle(),
            Host::Input => self.input.subtitle(),
            Host::Network => self.network.subtitle(),
            Host::Bluetooth => self.bluetooth.subtitle(),
            Host::Power => self.power.subtitle(),
            Host::Tray => self.tray.subtitle(),
            Host::DefaultApps => self.default_apps.subtitle(),
            Host::Appearance => self.appearance.subtitle(),
            Host::Desktop => self.desktop.subtitle(),
            Host::Session => self.session.subtitle(),
            Host::System => self.system.subtitle(),
            Host::Setup => self.setup.subtitle(),
        }
    }

    /// This page's sidebar badge, from its module.
    fn badge_for(&self, screen: Screen) -> Option<NavBadge> {
        match screen.host() {
            Host::Displays => self.displays.nav_badge(),
            Host::WindowRules => self.window_rules.nav_badge(),
            Host::Shortcuts => self.shortcuts.nav_badge(),
            Host::Input => self.input.nav_badge(),
            Host::Network => self.network.nav_badge(),
            Host::Bluetooth => self.bluetooth.nav_badge(),
            Host::Power => self.power.nav_badge(),
            Host::Tray => self.tray.nav_badge(),
            Host::DefaultApps => self.default_apps.nav_badge(),
            Host::Appearance => self.appearance.nav_badge(),
            Host::Desktop => self.desktop.nav_badge(),
            Host::Session => self.session.nav_badge(),
            Host::System => self.system.nav_badge(),
            Host::Setup => self.setup.nav_badge(),
        }
    }

    /// The active page's whole-page buttons, mapped into the app's messages.
    fn header_actions(&self, scale: FontScale) -> Option<Element<'_, Message>> {
        match self.screen.host() {
            Host::Displays => self.displays.header_actions(scale).map(|e| e.map(Message::Displays)),
            Host::WindowRules => {
                self.window_rules.header_actions(scale).map(|e| e.map(Message::WindowRules))
            }
            Host::Shortcuts => self.shortcuts.header_actions(scale).map(|e| e.map(Message::Shortcuts)),
            Host::Input => self.input.header_actions(scale).map(|e| e.map(Message::Input)),
            Host::Network => self.network.header_actions(scale).map(|e| e.map(Message::Network)),
            Host::Bluetooth => self.bluetooth.header_actions(scale).map(|e| e.map(Message::Bluetooth)),
            Host::Power => self.power.header_actions(scale).map(|e| e.map(Message::Power)),
            Host::Tray => self.tray.header_actions(scale).map(|e| e.map(Message::Tray)),
            Host::DefaultApps => {
                self.default_apps.header_actions(scale).map(|e| e.map(Message::DefaultApps))
            }
            Host::Appearance => self.appearance.header_actions(scale).map(|e| e.map(Message::Appearance)),
            Host::Desktop => self.desktop.header_actions(scale).map(|e| e.map(Message::Desktop)),
            Host::Session => self.session.header_actions(scale).map(|e| e.map(Message::Session)),
            Host::System => self.system.header_actions(scale).map(|e| e.map(Message::System)),
            Host::Setup => self.setup.header_actions(scale).map(|e| e.map(Message::Setup)),
        }
    }

    /// The active page's unapplied changes, mapped into the app's messages.
    fn pending(&self) -> Option<Pending<Message>> {
        match self.screen.host() {
            Host::Displays => self.displays.pending().map(|p| p.map(Message::Displays)),
            Host::WindowRules => self.window_rules.pending().map(|p| p.map(Message::WindowRules)),
            Host::Shortcuts => self.shortcuts.pending().map(|p| p.map(Message::Shortcuts)),
            Host::Input => self.input.pending().map(|p| p.map(Message::Input)),
            Host::Network => self.network.pending().map(|p| p.map(Message::Network)),
            Host::Bluetooth => self.bluetooth.pending().map(|p| p.map(Message::Bluetooth)),
            Host::Power => self.power.pending().map(|p| p.map(Message::Power)),
            Host::Tray => self.tray.pending().map(|p| p.map(Message::Tray)),
            Host::DefaultApps => self.default_apps.pending().map(|p| p.map(Message::DefaultApps)),
            Host::Appearance => self.appearance.pending().map(|p| p.map(Message::Appearance)),
            Host::Desktop => self.desktop.pending().map(|p| p.map(Message::Desktop)),
            Host::Session => self.session.pending().map(|p| p.map(Message::Session)),
            Host::System => self.system.pending().map(|p| p.map(Message::System)),
            Host::Setup => self.setup.pending().map(|p| p.map(Message::Setup)),
        }
    }

    /// A Set up message, with what has to happen around it.
    ///
    /// Two things only the shell can do. A finished job may have changed
    /// a file another page holds a copy of, and that page is reloaded
    /// *before* anything else runs — its next save writes the whole file
    /// from memory and would drop what setup added (see
    /// `modules::setup`'s module doc). And the first check decides, once,
    /// whether a first launch moves the window here.
    fn setup_message(&mut self, msg: modules::setup::Message) -> Task<Message> {
        let reloads = modules::setup::reloads(&msg);
        let mut tasks = Vec::new();
        if reloads.shortcuts {
            self.shortcuts.reload_store();
        }
        if reloads.window_rules {
            self.window_rules.reload_store();
        }
        if reloads.idle {
            self.desktop.reload_idle();
        }
        if reloads.session {
            self.session.reload_store();
        }
        if reloads.default_apps {
            // Already a reload: it re-reads the database off the UI thread.
            tasks.push(
                self.default_apps
                    .update(modules::default_apps::Message::Refresh)
                    .map(Message::DefaultApps),
            );
        }
        if let modules::setup::Message::Checked(report) = &msg {
            if std::mem::take(&mut self.first_launch_waiting)
                && modules::setup::should_open_on_setup(true, self.started.elapsed(), report)
            {
                self.screen = Screen::Setup;
                self.open_tabs();
            }
        }
        tasks.push(self.setup.update(msg).map(Message::Setup));
        Task::batch(tasks)
    }

    /// Points Appearance and Desktop at the tab the current page is.
    ///
    /// Called wherever the page changes. Their tabs are pages of their
    /// own now, and the module still draws whichever tab it was last
    /// told — so a page that forgot to say which would show its sibling.
    fn open_tabs(&mut self) {
        // Through the modules' own `TabSelected`, which only sets the tab
        // and returns no task — the same message their tab rows sent
        // before the tabs became pages.
        if let Some(tab) = self.screen.appearance_tab() {
            let _ = self.appearance.update(modules::appearance::Message::TabSelected(tab));
        }
        if let Some(tab) = self.screen.desktop_tab() {
            let _ = self.desktop.update(modules::desktop::Message::TabSelected(tab));
        }
    }

    /// Every setting any page offers the palette, tagged with its page.
    fn search_entries(&self) -> Vec<(Screen, SearchEntry<Message>)> {
        let tag = |screen: Screen| move |e: SearchEntry<Message>| (screen, e);
        let mut all: Vec<(Screen, SearchEntry<Message>)> = Vec::new();
        all.extend(
            self.input.search_entries().into_iter().map(|e| e.map(Message::Input)).map(tag(Screen::Input)),
        );
        all.extend(
            self.appearance
                .search_entries()
                .into_iter()
                .map(|e| e.map(Message::Appearance))
                .map(tag(Screen::WindowsWorkspaces)),
        );
        all.extend(
            self.system.search_entries().into_iter().map(|e| e.map(Message::System)).map(tag(Screen::System)),
        );
        all.extend(
            self.setup.search_entries().into_iter().map(|e| e.map(Message::Setup)).map(tag(Screen::Setup)),
        );
        all
    }

    /// What the palette shows for the current query.
    fn palette(&self) -> Palette {
        let query = &self.search_query;
        let screens: Vec<Screen> = NAV.iter().flat_map(|c| c.screens.iter().copied()).collect();
        let entries = self.search_entries();
        Palette {
            pages: search::best(query, screens.iter(), |s| s.title(), PALETTE_PAGES)
                .into_iter()
                .copied()
                .collect(),
            settings: search::best(query, entries.iter(), |(_, e)| e.label, PALETTE_ROWS)
                .into_iter()
                .cloned()
                .collect(),
            keys: search::best(query, entries.iter(), |(_, e)| e.key, PALETTE_ROWS)
                .into_iter()
                .cloned()
                .collect(),
        }
    }

    /// The search palette, over the page while the search field has text.
    ///
    /// The first result is drawn selected because it is what Enter
    /// takes — the highlight is the answer to "what happens if I press
    /// Enter now", which a palette otherwise leaves you to guess.
    fn palette_view(&self, scale: FontScale) -> Element<'_, Message> {
        let palette = self.palette();
        let total = palette.pages.len() + palette.settings.len() + palette.keys.len();
        let mut first = true;
        let mut take_first = || std::mem::replace(&mut first, false);

        let row_button = |content: Element<'static, Message>, selected: bool, on_press: Message| {
            iced::widget::button(content)
                .width(Length::Fill)
                .padding([spacing::SM - 2.0, spacing::SM + 2.0])
                .style(move |t: &Theme, status| selectable_row_style(t, status, selected))
                .on_press(on_press)
        };

        let mut body = column![row![
            section_label("Search", scale),
            Space::new().width(Length::Fill),
            config_line(
                match total {
                    1 => "1 match".to_string(),
                    n => format!("{n} matches"),
                },
                scale
            ),
        ]
        .align_y(iced::Alignment::Center)]
        .spacing(2.0);

        if !palette.pages.is_empty() {
            body = body.push(container(section_label("Pages", scale)).padding([spacing::SM, 0.0]));
            for screen in palette.pages {
                let selected = take_first();
                let mark = if selected { text() } else { text_dim() };
                let line = row![
                    hyprforge_ui::glyph::page(screen.glyph(), scale.apply(NAV_MARK_BASE), mark),
                    scaled_text(screen.title(), density::ROW_TEXT_BASE * 0.9, scale).color(text()),
                ]
                .spacing(spacing::SM + 2.0)
                .align_y(iced::Alignment::Center);
                body = body.push(row_button(line.into(), selected, Message::Navigate(screen)));
            }
        }
        if !palette.settings.is_empty() {
            body = body.push(container(section_label("Settings", scale)).padding([spacing::SM, 0.0]));
            for (screen, entry) in palette.settings {
                let selected = take_first();
                // The page name in the text colour on the selected row,
                // where dim text on the accent fill could not be read.
                let crumb = hint_text(screen.title(), scale);
                let crumb = if selected { crumb.color(text()) } else { crumb };
                let line = row![
                    scaled_text(entry.label, density::ROW_TEXT_BASE * 0.9, scale).color(text()),
                    Space::new().width(Length::Fill),
                    crumb,
                ]
                .spacing(spacing::SM)
                .align_y(iced::Alignment::Center);
                body = body.push(row_button(line.into(), selected, Message::Reveal(screen, entry.reveal)));
            }
        }
        if !palette.keys.is_empty() {
            body = body.push(container(section_label("Config keys", scale)).padding([spacing::SM, 0.0]));
            for (screen, entry) in palette.keys {
                let selected = take_first();
                // The stored value in the plain text colour, not the
                // mockup's green-for-true and red-for-false: a setting
                // being off is not an error, and red is reserved for one.
                let mut line = row![
                    config_line(entry.key, scale).color(text()),
                    Space::new().width(Length::Fill),
                ]
                .spacing(spacing::SM)
                .align_y(iced::Alignment::Center);
                let value = match entry.value {
                    Some(v) => config_line(v, scale),
                    None => hint_text("not set here", scale),
                };
                line = line.push(if selected { value.color(text()) } else { value });
                body = body.push(row_button(line.into(), selected, Message::Reveal(screen, entry.reveal)));
            }
        }
        if total == 0 {
            body = body.push(
                container(
                    scaled_text(
                        format!("Nothing matches \u{201c}{}\u{201d}.", self.search_query.trim()),
                        density::META_TEXT_BASE,
                        scale,
                    )
                    .color(text_dim()),
                )
                .padding([spacing::SM, 0.0]),
            );
        }
        body = body.push(
            container(config_line("⏎ go to the first result   esc dismiss", scale))
                .padding(iced::Padding { top: spacing::SM, ..iced::Padding::default() }),
        );

        let card = container(body)
            .padding(spacing::MD)
            .max_width(PALETTE_WIDTH)
            .width(Length::Fill)
            .style(|_t: &Theme| container::Style {
                background: Some(Background::Color(surface::card())),
                border: iced::Border {
                    radius: density::outer_radius().into(),
                    width: 1.0,
                    color: surface::card_border(),
                },
                shadow: iced::Shadow {
                    color: iced::Color { a: 0.45, ..iced::Color::BLACK },
                    offset: iced::Vector::new(0.0, 8.0),
                    blur_radius: 28.0,
                },
                ..container::Style::default()
            });
        container(card)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::alignment::Horizontal::Center)
            .padding([spacing::SM, spacing::LG])
            .into()
    }

    /// The main window: the mockup's `1b` shell.
    ///
    /// A header bar across the top holding the app's mark and the search
    /// field; a grouped sidebar of drawn marks under it; and the page,
    /// which is a header the shell draws — title, subtitle, whole-page
    /// buttons — over the page's own body in the window's one scroll
    /// area.
    fn settings_view(&self) -> Element<'_, Message> {
        let scale = self.font_scale;

        let pending = self.pending();
        let header = self.header_bar(pending.is_some(), scale);
        let sidebar = self.sidebar(scale);

        let content: Element<'_, Message> = match self.screen.host() {
            Host::Displays => self.displays.view(scale).map(Message::Displays),
            Host::WindowRules => self.window_rules.view(scale).map(Message::WindowRules),
            Host::Shortcuts => self.shortcuts.view(scale).map(Message::Shortcuts),
            Host::Input => self.input.view(scale).map(Message::Input),
            Host::Network => self.network.view(scale).map(Message::Network),
            Host::Bluetooth => self.bluetooth.view(scale).map(Message::Bluetooth),
            Host::Power => self.power.view(scale).map(Message::Power),
            Host::Tray => self.tray.view(scale).map(Message::Tray),
            Host::DefaultApps => self.default_apps.view(scale).map(Message::DefaultApps),
            Host::Appearance => self.appearance.view(scale).map(Message::Appearance),
            Host::Desktop => self.desktop.view(scale).map(Message::Desktop),
            Host::Session => self.session.view(scale).map(Message::Session),
            Host::System => self.system.view(scale).map(Message::System),
            Host::Setup => self.setup.view(scale).map(Message::Setup),
        };

        // Title row: the page's name and its subtitle, with whatever acts
        // on the whole page at the right. Outside the scroll area, so the
        // name of the page you are on never scrolls away from you.
        let mut title_row = row![page_header(self.screen.title(), self.subtitle_for(self.screen), scale)]
            .spacing(spacing::SM)
            .align_y(iced::Alignment::Center);
        if let Some(actions) = self.header_actions(scale) {
            title_row = title_row.push(Space::new().width(Length::Fill)).push(actions);
        }
        let title_row = container(title_row).padding(iced::Padding {
            top: spacing::LG,
            left: spacing::LG,
            right: spacing::LG,
            bottom: 0.0,
        });

        // The Monitors editor (canvas + full property panel + policy/swap
        // sections) routinely exceeds window height — without scrolling,
        // everything past the window edge was just clipped and silently
        // invisible, not merely off-screen.
        //
        // Order matters here: the scrollable has to be the *outermost* of
        // these, so its scrollbar tracks the edge of the content pane.
        // Nesting it inside the max-width/centering containers instead pins
        // the bar to the right edge of the centered column, which on a wide
        // window reads as a scrollbar floating in the middle of the screen.
        //
        // And it is the only one. Five pages used to wrap themselves in a
        // scrollable of their own inside this one, so a long page scrolled
        // twice, with two scrollbars, depending on where the pointer was.
        let body = iced::widget::scrollable(centred(content))
            .width(Length::Fill)
            .height(Length::Fill);

        let mut page = column![centred(title_row.into()), body].width(Length::Fill);
        // Pinned under the scroll area rather than inside it: changes
        // waiting to be written are the one thing on a page that must
        // never be scrolled out of sight.
        if let Some(p) = pending {
            page = page.push(pending_bar(p.summary, p.preview, p.discard, p.apply, scale));
        }

        // The palette floats over the page while there is a query, so the
        // page it would take you away from stays visible behind it.
        let page: Element<'_, Message> = match self.search_query.trim().is_empty() {
            true => page.into(),
            false => iced::widget::stack![page, self.palette_view(scale)].into(),
        };

        container(column![header, row![sidebar, page].height(Length::Fill)])
            .style(|_theme: &Theme| container::Style {
                background: Some(Background::Color(surface::root())),
                ..container::Style::default()
            })
            .into()
    }

    /// The bar across the top: the app's mark over the sidebar, the
    /// search field over the page, and whether this page has anything
    /// waiting to be written.
    fn header_bar(&self, has_pending: bool, scale: FontScale) -> Element<'_, Message> {
        let mark_side = scale.apply(20.0);
        let mark = container(Space::new())
            .width(Length::Fixed(mark_side))
            .height(Length::Fixed(mark_side))
            .style(move |_t: &Theme| container::Style {
                // The accent, filled: the one piece of chrome that is
                // purple without being selected, because it *is* the
                // app's identity, and the mockup's mark is the accent too.
                background: Some(Background::Color(Tint::Accent.iced())),
                border: iced::Border {
                    radius: (mark_side * 0.3).into(),
                    ..iced::Border::default()
                },
                ..container::Style::default()
            });
        let brand = row![
            mark,
            scaled_text("Settings", density::ROW_TEXT_BASE, scale)
                .font(iced::Font { weight: iced::font::Weight::Semibold, ..iced::Font::DEFAULT })
                .color(text()),
        ]
        .spacing(spacing::SM + spacing::XS)
        .align_y(iced::Alignment::Center);

        let search = search_field(
            "Search any setting or config key",
            &self.search_query,
            Message::SearchChanged,
            Some(Message::SearchSubmit),
            Some(self.search_id.clone()),
            scale,
        )
        .width(Length::Fill);

        container(
            row![
                container(brand).width(Length::Fixed(SIDEBAR_WIDTH - spacing::MD)),
                search,
                // "live" in the success colour when what the page shows is
                // what the files say; "pending" in the warning colour while
                // anything is held back. The mockup's `● live`, and the
                // reason it is a state colour: it is a state.
                match has_pending {
                    false => chip("● live", Tint::Success, scale),
                    true => chip("● pending", Tint::Warning, scale),
                },
            ]
            .spacing(spacing::MD)
            .align_y(iced::Alignment::Center),
        )
        .height(Length::Fixed(density::bar_height(scale) + spacing::SM))
        .center_y(Length::Fixed(density::bar_height(scale) + spacing::SM))
        .padding([0.0, spacing::MD])
        .width(Length::Fill)
        .style(|_t: &Theme| container::Style {
            background: Some(Background::Color(surface::sidebar())),
            border: iced::Border {
                width: 1.0,
                color: surface::card_border(),
                ..iced::Border::default()
            },
            ..container::Style::default()
        })
        .into()
    }

    /// The grouped list of pages down the left.
    ///
    /// Never filtered by the search field: the palette answers searches,
    /// and a sidebar that emptied itself while you typed would take away
    /// the one map of the app that stays put.
    fn sidebar(&self, scale: FontScale) -> Element<'_, Message> {
        let mut nav = column![].spacing(spacing::MD);
        for category in NAV {
            let mut group = column![container(section_label(category.label, scale))
                .padding([spacing::XS, spacing::SM + 2.0])]
            .spacing(1.0);
            for screen in category.screens {
                group = group.push(self.nav_item(*screen, scale));
            }
            nav = nav.push(group);
        }

        container(
            iced::widget::scrollable(container(nav).padding([spacing::MD, spacing::SM]))
                .height(Length::Fill),
        )
        .width(Length::Fixed(SIDEBAR_WIDTH))
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(Background::Color(surface::sidebar())),
            ..container::Style::default()
        })
        .into()
    }

    /// One page's entry in the sidebar: its mark, its name, and whatever
    /// badge its module offers.
    fn nav_item(&self, screen: Screen, scale: FontScale) -> Element<'_, Message> {
        let selected = self.screen == screen;
        // The text colour on the current page, dim elsewhere. Not the
        // accent: the row's own fill already says "selected" in it, and
        // an accent mark on an accent fill disappeared into it.
        let mark_color = if selected { text() } else { text_dim() };
        let mut line = row![
            hyprforge_ui::glyph::page(screen.glyph(), scale.apply(NAV_MARK_BASE), mark_color),
            scaled_text(screen.title(), density::ROW_TEXT_BASE * 0.9, scale).color(text()),
            Space::new().width(Length::Fill),
        ]
        .spacing(spacing::SM + 2.0)
        .align_y(iced::Alignment::Center);
        match self.badge_for(screen) {
            Some(NavBadge::Text(t)) => {
                line = line.push(config_line(t, scale));
            }
            Some(NavBadge::Dot(tint)) => {
                line = line.push(status_dot(tint, scale));
            }
            None => {}
        }
        // Centred in a fill-height container: a button's content sits at
        // its top, and at a fixed row height that put every label above
        // the middle of its own highlight.
        iced::widget::button(container(line).center_y(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fixed(density::row_height(scale) + spacing::XS))
            .padding([0.0, spacing::SM + 2.0])
            .style(move |t: &Theme, status| selectable_row_style(t, status, selected))
            .on_press(Message::Navigate(screen))
            .into()
    }

    /// The one keyboard grammar used across the whole app (vision pillar
    /// #9 — pick once, document it, never diverge per-module):
    ///
    /// - `Ctrl+1` / `Ctrl+2` / `Ctrl+3` — switch to Monitors / Window Rules /
    ///   Shortcuts
    /// - `Ctrl+F` / `Ctrl+K` — focus the search field
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
                // Ctrl+K is the mockup's, and the one most palettes use;
                // Ctrl+F stays because it has always worked here.
                Key::Character("f") | Key::Character("k") => Some(Message::FocusSearch),
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

    /// Every tray icon opens the page its own setting is on. Night light
    /// and keep-awake used to be the reason `--screen` could name a tab:
    /// before that, clicking night light landed on Wallpaper, and keep
    /// awake did nothing at all. They are pages of their own now, and
    /// the names have to keep landing on them.
    #[test]
    fn each_tray_icon_opens_the_page_carrying_its_own_setting() {
        for (icon, expected) in [
            ("network", Screen::Network),
            ("bluetooth", Screen::Bluetooth),
            ("night-light", Screen::NightLight),
            ("idle", Screen::Idle),
            ("keep-awake", Screen::Idle),
            ("power", Screen::Power),
            ("tray", Screen::Tray),
        ] {
            assert_eq!(screen_from_cli(icon), Some(expected), "{icon} does not open where its setting lives");
        }
    }

    /// Every name `--screen` accepted before the sidebar was regrouped
    /// still opens a page — the one its setting now lives on. A desktop
    /// entry, a tray build or a script written against the old names
    /// must not start failing because the pages moved.
    #[test]
    fn every_old_screen_name_still_opens_a_screen() {
        for (old, expected) in [
            ("monitors", Screen::Monitors),
            ("window-rules", Screen::WindowRules),
            ("shortcuts", Screen::Shortcuts),
            ("input", Screen::Input),
            ("network", Screen::Network),
            ("bluetooth", Screen::Bluetooth),
            ("power", Screen::Power),
            ("tray", Screen::Tray),
            ("appearance", Screen::Appearance),
            // Desktop opened on its first tab, Wallpaper.
            ("desktop", Screen::Wallpaper),
            ("default-apps", Screen::DefaultApps),
            ("wallpaper", Screen::Wallpaper),
            ("night-light", Screen::NightLight),
            ("idle", Screen::Idle),
            ("screen-sharing", Screen::Sharing),
            ("session", Screen::Session),
            ("system", Screen::System),
        ] {
            assert_eq!(screen_from_cli(old), Some(expected), "--screen {old}");
        }
    }

    /// A name in the help text that the parser rejects is a promise the
    /// program does not keep.
    #[test]
    fn every_name_the_usage_message_lists_actually_parses() {
        for name in SCREEN_NAMES {
            assert!(screen_from_cli(name).is_some(), "{name} is offered in --help but is not accepted");
        }
    }

    /// Every page in the sidebar can be opened by name, or it could not be
    /// screenshotted — and that is how every page here gets checked.
    #[test]
    fn every_sidebar_page_has_a_name_that_opens_it() {
        for screen in NAV.iter().flat_map(|c| c.screens) {
            assert!(
                SCREEN_NAMES.iter().any(|n| screen_from_cli(n) == Some(*screen)),
                "{screen:?} has no --screen name"
            );
        }
    }

    /// A page that shares a module with others must say which tab it is,
    /// or it draws whichever tab that module last showed.
    #[test]
    fn every_page_over_a_shared_module_names_its_tab() {
        for screen in NAV.iter().flat_map(|c| c.screens) {
            match screen.host() {
                Host::Appearance => assert!(screen.appearance_tab().is_some(), "{screen:?}"),
                Host::Desktop => assert!(screen.desktop_tab().is_some(), "{screen:?}"),
                _ => {}
            }
        }
    }

    /// Every page appears in the sidebar exactly once.
    #[test]
    fn every_page_is_in_the_sidebar_once() {
        let listed: Vec<Screen> = NAV.iter().flat_map(|c| c.screens.iter().copied()).collect();
        for (i, a) in listed.iter().enumerate() {
            assert!(!listed[i + 1..].contains(a), "{a:?} is listed twice");
        }
        assert_eq!(listed.len(), 19, "a page was added or dropped without updating this count");
    }

    /// `--setup` hands a running window `show-screen setup`, and that has
    /// to land on the page rather than be refused as unknown.
    #[test]
    fn the_name_setup_hands_off_with_opens_the_set_up_page() {
        assert_eq!(screen_from_cli("setup"), Some(Screen::Setup));
        assert!(screen_name_is_known("setup"));
    }

    /// A launch that named a page is never waiting on the first check.
    #[test]
    fn a_requested_screen_means_the_first_check_cannot_move_the_window() {
        assert!(first_launch_waits(None));
        assert!(!first_launch_waits(Some(Screen::Network)));
        assert!(!first_launch_waits(Some(Screen::Setup)), "already there; nothing to decide");
    }

    #[test]
    fn an_unknown_screen_is_refused_rather_than_falling_back() {
        assert_eq!(screen_from_cli("bogus"), None);
        assert_eq!(screen_from_cli(""), None);
    }
}

#[cfg(test)]
mod palette_tests {
    use super::*;

    fn entry(label: &'static str, key: &'static str) -> SearchEntry<Message> {
        SearchEntry { label, key, value: None, reveal: vec![Message::Noop] }
    }

    /// Enter takes the row drawn highlighted, and that is the first page,
    /// then the first setting, then the first key — the order the palette
    /// draws its groups in. If the two orders ever disagreed, Enter would
    /// go somewhere other than where the highlight said.
    #[test]
    fn enter_takes_the_row_the_palette_highlights() {
        let with_page = Palette {
            pages: vec![Screen::Network],
            settings: vec![(Screen::Input, entry("Natural scroll", "input:natural_scroll"))],
            keys: vec![],
        };
        assert!(matches!(with_page.first(), Some(Message::Navigate(Screen::Network))));

        let settings_first = Palette {
            pages: vec![],
            settings: vec![(Screen::Input, entry("Natural scroll", "input:natural_scroll"))],
            keys: vec![(Screen::System, entry("Other", "misc:other"))],
        };
        assert!(matches!(settings_first.first(), Some(Message::Reveal(Screen::Input, _))));

        let keys_only = Palette {
            pages: vec![],
            settings: vec![],
            keys: vec![(Screen::System, entry("Other", "misc:other"))],
        };
        assert!(matches!(keys_only.first(), Some(Message::Reveal(Screen::System, _))));

        let empty = Palette { pages: vec![], settings: vec![], keys: vec![] };
        assert!(empty.first().is_none());
    }
}

#[cfg(test)]
mod revert_tests {
    use super::*;

    /// Doing nothing is the safe answer, and the sentence has to say so to
    /// someone who can barely read the screen it is on.
    #[test]
    fn the_prompt_says_doing_nothing_reverts_and_counts_properly() {
        assert_eq!(revert_sentence(1), "Reverting automatically in 1 second if you can't see this.");
        assert_eq!(revert_sentence(9), "Reverting automatically in 9 seconds if you can't see this.");
    }
}

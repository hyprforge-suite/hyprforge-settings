//! The Network screen: wired interfaces, and Wi-Fi — the radio, the
//! network list, joining, and saved networks.
//!
//! Everything that talks to NetworkManager lives in `hyprforge-network`
//! already — this module is the screen on top of it, and nothing more.
//! It is generic over [`NetworkBackend`] for the same reason
//! `hyprforge-network` itself is split into a trait and a real
//! implementation: the tests below drive a [`MockBackend`], and
//! `main.rs` drives [`NetworkManagerBackend`], and the screen's logic —
//! which row is clickable, what a wrong password does to the state — is
//! identical either way.
//!
//! # The one rule this module exists to keep
//!
//! A typed Wi-Fi passphrase never becomes a plain `String` field or a
//! `Message` variant that could be logged. It is held as [`Psk`], whose
//! `Debug` renders a count, from the moment a keystroke lands to the
//! moment it crosses into `hyprforge_network::NetworkBackend::connect`.

use hyprforge_network::backend::{for_display, NetworkBackend, SavedNetwork, Status};
use hyprforge_network::{
    AccessPoint, NetworkError, Psk, RadioState, Security, WiredState, WiredStatus,
};
use hyprforge_tray::Prefs as TrayPrefs;
use hyprforge_ui::theme::{self, spacing, FontScale, BASE_TEXT_SIZE};
use hyprforge_ui::widgets::{
    chip, config_line, hero_card, hint_text, inset_input_style, meta_text, primary_button,
    scaled_text, secondary_button, section_label, setting_list, setting_row, toggle, Tint,
};
use iced::widget::{column, row, text_input};
use iced::{Alignment, Element, Length, Subscription, Task};
use std::sync::Arc;
use std::time::Duration;

use crate::module::SettingsModule;

/// How often the screen re-polls status and access points while it's open,
/// so a network that appeared or a signal that changed shows up without
/// the user having to hit Refresh. Independent of [`ScanPressed`], which
/// asks the radio to actively scan — this just re-reads what it already
/// knows.
///
/// [`ScanPressed`]: Message::ScanPressed
const POLL_INTERVAL: Duration = Duration::from_secs(10);

/// A load-time failure, reduced to what the screen needs: text to show,
/// and whether it's the one kind of failure ([`NetworkError::Unavailable`])
/// that must never be confused with an empty list.
///
/// `NetworkError` itself isn't `Clone` (nor should `Message` need it to
/// be, since it carries nothing sensitive) — this is the boundary where a
/// borrowed error becomes an owned value a `Message` can carry.
#[derive(Debug, Clone)]
pub struct LoadError {
    message: String,
    unavailable: bool,
    /// [`NetworkError::NoWifiDevice`]: a machine with no Wi-Fi adapter,
    /// which is a kind of machine and not a failure — a desktop on a
    /// cable. It used to reach the warning banner on every refresh.
    no_wifi_device: bool,
}

impl From<NetworkError> for LoadError {
    fn from(e: NetworkError) -> Self {
        LoadError {
            unavailable: matches!(e, NetworkError::Unavailable),
            no_wifi_device: matches!(e, NetworkError::NoWifiDevice),
            message: e.to_string(),
        }
    }
}

/// The result of one refresh: four independent calls, four independent
/// outcomes. A failure in `access_points` must not discard a successful
/// `status`, and vice versa — the screen shows as much true information
/// as it has, not the least common denominator of four calls.
#[derive(Debug, Clone)]
pub struct Loaded {
    status: Result<Status, LoadError>,
    access_points: Result<Vec<AccessPoint>, LoadError>,
    saved: Result<Vec<SavedNetwork>, LoadError>,
    wired: Result<Vec<WiredStatus>, LoadError>,
}

async fn load<B: NetworkBackend + ?Sized>(backend: Arc<B>) -> Loaded {
    Loaded {
        status: backend.status().await.map_err(LoadError::from),
        access_points: backend.access_points().await.map_err(LoadError::from),
        saved: backend.saved_networks().await.map_err(LoadError::from),
        wired: backend.wired().await.map_err(LoadError::from),
    }
}

/// What a wired row's button does, if it has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WiredAction {
    Connect,
    Disconnect,
}

/// The one control a wired port gets.
///
/// An unplugged port gets none — nothing on screen plugs a cable in, and
/// a Connect that can only fail is a button that lies. A port already
/// connecting gets none either, rather than a second request racing the
/// first.
fn wired_action(state: WiredState) -> Option<WiredAction> {
    match state {
        WiredState::Connected => Some(WiredAction::Disconnect),
        WiredState::Disconnected => Some(WiredAction::Connect),
        WiredState::Connecting | WiredState::CableUnplugged => None,
    }
}

/// A wired port's second line: its state, then whatever NetworkManager
/// adds to it.
fn wired_detail(port: &WiredStatus) -> String {
    let mut parts = vec![port.interface.clone()];
    parts.push(
        match port.state {
            WiredState::Connected => "Connected",
            WiredState::Connecting => "Connecting…",
            WiredState::CableUnplugged => "Cable unplugged",
            WiredState::Disconnected => "Cable plugged in, not connected",
        }
        .to_string(),
    );
    if let Some(name) = &port.connection {
        parts.push(name.clone());
    }
    if let Some(speed) = port.speed_mbps {
        parts.push(format!("{speed} Mb/s"));
    }
    parts.join(" \u{b7} ")
}

/// Whether the radio row gets a control that can actually do something.
///
/// `RadioState::HardwareOff` means a rocker switch or an Fn key is what
/// has to move — no D-Bus call changes that — so this is `false` there and
/// nowhere else. Pulled out as its own function, rather than left as an
/// inline match arm in `radio_row`, so the property "hardware-off gets no
/// toggle" is something a test can assert directly instead of only being
/// implied by which arm of a view function happens to construct a
/// `checkbox`.
fn radio_offers_toggle(radio: Option<RadioState>) -> bool {
    matches!(radio, Some(RadioState::On) | Some(RadioState::Off))
}

/// A network being joined: the row that was clicked, plus whatever has
/// been typed toward its passphrase.
///
/// `passphrase` is a [`Psk`] and not a `String` for the same reason
/// `hyprforge-network::secret` exists at all — a draft is exactly as
/// sensitive as the value that gets sent, and holding it as plain text
/// "just until it's submitted" is the mistake that type was built to make
/// unreachable.
struct JoinDraft {
    ap: AccessPoint,
    passphrase: Psk,
    /// Set once a connect attempt is in flight, so the dialog can't be
    /// submitted twice while NetworkManager is mid-handshake.
    connecting: bool,
    error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    Loaded(Loaded),
    ScanPressed,
    ScanRequested(Result<(), LoadError>),
    RadioToggled(bool),
    RadioSet(Result<(), LoadError>),
    /// Index into the currently-shown, already-deduplicated access point
    /// list — never a raw D-Bus path, which the screen never sees.
    RowClicked(usize),
    /// Carries a [`Psk`] rather than the `String` the text field hands
    /// over, because `Message` derives `Debug` and this variant is
    /// produced on every keystroke. As a `String` it rendered as
    /// `PassphraseChanged("correct-horse-battery")` — the password, in a
    /// debug dump, which is the rule in CLAUDE.md and the one the lock
    /// screen's hand-written `Debug for State` already exists to keep.
    ///
    /// Nothing in the app logs a message today. This is typed rather than
    /// remembered so that it stays true when something does.
    PassphraseChanged(Psk),
    JoinConfirm,
    JoinCancel,
    Connected(Result<(), LoadError>),
    ForgetPressed(String),
    Forgotten(String, Result<(), LoadError>),
    /// A wired port's button, by interface name — the handle
    /// `hyprforge-network` addresses ports by.
    WiredConnect(String),
    WiredDisconnect(String),
    WiredChanged(Result<(), LoadError>),
    /// Whether `hyprforge-trayd` should show the network icon.
    ///
    /// Only reachable from a loaded [`TrayPrefs`] — see `tray_row` — so
    /// this never has to guess a value for the field it does not own
    /// (`bluetooth`). Guarded again in `update` anyway, same pattern as
    /// [`Message::RadioToggled`] against a hardware-off radio.
    TrayToggled(bool),
}

pub struct NetworkModule<B: NetworkBackend + 'static> {
    backend: Arc<B>,
    /// `true` until the first [`Message::Loaded`] lands, so the screen can
    /// say "loading" instead of "no networks" while the first round trip
    /// is still in flight.
    loading: bool,
    /// Set only from [`NetworkError::Unavailable`], and rendered as its
    /// own state rather than folded into `access_points` being empty —
    /// the whole distinction `hyprforge-network` was written to keep.
    unavailable: Option<String>,
    /// Anything else that went wrong: a scan that failed, a forget that
    /// didn't take. Cleared on the next successful action of the same
    /// kind, not on every refresh, so it doesn't flash away before it's
    /// been read.
    error: Option<String>,
    status: Option<Status>,
    /// Already deduplicated and sorted by [`for_display`] — this module
    /// does not re-implement that.
    access_points: Vec<AccessPoint>,
    saved: Vec<SavedNetwork>,
    /// Every Ethernet port NetworkManager manages; empty on most laptops.
    wired: Vec<WiredStatus>,
    /// The port a Connect or Disconnect is in flight for, so its button
    /// can say so and not be pressed twice.
    wired_busy: Option<String>,
    /// `false` on a machine with no Wi-Fi adapter — see
    /// [`LoadError::no_wifi_device`]. Starts `true`: a Wi-Fi section that
    /// disappears once the first load says so is better than one that
    /// appears late on nearly every machine.
    wifi_present: bool,
    scanning: bool,
    joining: Option<JoinDraft>,
    /// The on-disk `tray.toml`, loaded once at construction.
    ///
    /// `Err` means the file exists and would not parse — the rule in
    /// CLAUDE.md about never collapsing "could not read" into "nothing
    /// configured". Kept as the *whole* [`TrayPrefs`], not just this
    /// screen's `network` bit, so toggling here can write back
    /// `bluetooth` unchanged instead of a default that would silently
    /// switch the other icon off. While this is `Err`, `tray_row` shows
    /// no checkbox at all, and [`Message::TrayToggled`] has nothing to
    /// flip — there is no last-known-good value to start from, and
    /// guessing one is exactly the write this field exists to prevent.
    tray_prefs: Result<TrayPrefs, String>,
}

impl<B: NetworkBackend + 'static> NetworkModule<B> {
    /// Builds the module around an already-usable backend and kicks off
    /// the first load.
    ///
    /// A `NetworkManagerBackend` connects asynchronously and can fail
    /// (no bus, no NetworkManager) — `main.rs` resolves that before
    /// calling here, or hands over a backend that resolves it lazily on
    /// first call, so this constructor never has to be fallible itself.
    pub fn new(backend: Arc<B>) -> (Self, Task<Message>) {
        // A missing file is first run and loads as defaults; a file that
        // exists and will not parse is reported here, in the same banner
        // every other load failure on this screen uses, rather than
        // silently treated as "both icons shown".
        let (tray_prefs, tray_error) = match hyprforge_tray::prefs::load() {
            Ok(prefs) => (Ok(prefs), None),
            Err(e) => (Err(e.to_string()), Some(e.to_string())),
        };
        let module = NetworkModule {
            backend,
            loading: true,
            unavailable: None,
            error: tray_error,
            status: None,
            access_points: Vec::new(),
            saved: Vec::new(),
            wired: Vec::new(),
            wired_busy: None,
            wifi_present: true,
            scanning: false,
            joining: None,
            tray_prefs,
        };
        let task = Task::perform(load(Arc::clone(&module.backend)), Message::Loaded);
        (module, task)
    }

    fn refresh_task(&self) -> Task<Message> {
        Task::perform(load(Arc::clone(&self.backend)), Message::Loaded)
    }
}

impl<B: NetworkBackend + 'static> SettingsModule for NetworkModule<B> {
    type Message = Message;

    fn subtitle(&self) -> Option<String> {
        Some("NetworkManager".into())
    }

    fn header_actions(&self, _scale: FontScale) -> Option<Element<'_, Message>> {
        Some(
            row![
                secondary_button(if self.scanning { "Scanning…" } else { "Scan" })
                    .on_press_maybe((!self.scanning).then_some(Message::ScanPressed)),
                secondary_button("Refresh").on_press(Message::Refresh),
            ]
            .spacing(spacing::SM)
            .into(),
        )
    }

    /// A dot while on a wireless network, so the sidebar says "online"
    /// from any page.
    fn nav_badge(&self) -> Option<crate::module::NavBadge> {
        self.status
            .as_ref()
            .and_then(|s| s.connected_to.as_ref())
            .map(|_| crate::module::NavBadge::Dot(hyprforge_ui::widgets::Tint::Success))
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Refresh => {
                self.loading = self.status.is_none() && self.unavailable.is_none();
                self.refresh_task()
            }
            Message::Loaded(result) => {
                self.loading = false;
                // Any of the three calls can be the one that notices
                // NetworkManager is gone — the mock fails all three at
                // once, but the real backend fails whichever call it was
                // mid-way through. Whichever it is, it wins: an empty
                // network list must never stand in for this.
                let unavailable_msg = [
                    result.status.as_ref().err(),
                    result.access_points.as_ref().err(),
                    result.saved.as_ref().err(),
                    result.wired.as_ref().err(),
                ]
                .into_iter()
                .flatten()
                .find(|e| e.unavailable)
                .map(|e| e.message.clone());

                if let Some(msg) = unavailable_msg {
                    self.unavailable = Some(msg);
                    self.status = None;
                    self.access_points.clear();
                    self.saved.clear();
                    self.wired.clear();
                    return Task::none();
                }
                self.unavailable = None;
                match result.status {
                    Ok(status) => self.status = Some(status),
                    Err(e) => self.error = Some(e.message),
                }
                match result.access_points {
                    Ok(points) => {
                        self.wifi_present = true;
                        self.access_points = for_display(points);
                    }
                    // A state, not an error: the Wi-Fi section says it
                    // quietly instead of the banner saying it every ten
                    // seconds.
                    Err(e) if e.no_wifi_device => {
                        self.wifi_present = false;
                        self.access_points.clear();
                    }
                    Err(e) => self.error = Some(e.message),
                }
                match result.wired {
                    Ok(wired) => self.wired = wired,
                    Err(e) => self.error = Some(e.message),
                }
                match result.saved {
                    Ok(saved) => self.saved = saved,
                    Err(e) => self.error = Some(e.message),
                }
                Task::none()
            }
            Message::ScanPressed => {
                self.scanning = true;
                let backend = Arc::clone(&self.backend);
                Task::perform(
                    async move { backend.request_scan().await.map_err(LoadError::from) },
                    Message::ScanRequested,
                )
            }
            Message::ScanRequested(Ok(())) => {
                self.scanning = false;
                self.refresh_task()
            }
            Message::ScanRequested(Err(e)) => {
                self.scanning = false;
                self.error = Some(e.message);
                Task::none()
            }
            Message::RadioToggled(on) => {
                // A hardware-off radio never reaches this arm because the
                // view renders no toggle for that state — see `radio_row`
                // — but the check stays here too, so a stray message can
                // never turn a dead control into a dispatched D-Bus call.
                if matches!(self.status.as_ref().map(|s| s.radio), Some(RadioState::HardwareOff)) {
                    return Task::none();
                }
                let backend = Arc::clone(&self.backend);
                Task::perform(
                    async move { backend.set_radio(on).await.map_err(LoadError::from) },
                    Message::RadioSet,
                )
            }
            Message::RadioSet(Ok(())) => self.refresh_task(),
            Message::RadioSet(Err(e)) => {
                self.error = Some(e.message);
                Task::none()
            }
            Message::RowClicked(index) => {
                let Some(ap) = self.access_points.get(index).cloned() else {
                    return Task::none();
                };
                // Enterprise networks are listed, not joinable — the row
                // that would send this has no click handler wired to it
                // in `view`, but a stray message still must not start a
                // connection attempt that can only fail.
                if ap.security.unsupported_reason().is_some() {
                    return Task::none();
                }
                self.error = None;

                // A network NetworkManager has already saved rejoins with
                // the secret it already holds. Asking for the password
                // again to reconnect to your own home network is the
                // commonest thing this screen does, and typing it every
                // time is not a small annoyance — it is the difference
                // between the screen being usable and being a form.
                if let Some(saved) = self
                    .saved
                    .iter()
                    .find(|s| s.ssid == ap.ssid)
                    .map(|s| s.id.clone())
                {
                    let backend = self.backend.clone();
                    return Task::perform(
                        async move { backend.connect_saved(&saved).await.map_err(LoadError::from) },
                        Message::Connected,
                    );
                }

                if ap.security.needs_passphrase() {
                    self.joining = Some(JoinDraft {
                        ap,
                        passphrase: Psk::new(String::new()),
                        connecting: false,
                        error: None,
                    });
                    Task::none()
                } else {
                    // Open and OWE networks need nothing typed — the whole
                    // point of not needing a passphrase — so this connects
                    // immediately rather than opening a dialog with
                    // nothing in it.
                    let backend = Arc::clone(&self.backend);
                    Task::perform(
                        async move { backend.connect(&ap, None).await.map_err(LoadError::from) },
                        Message::Connected,
                    )
                }
            }
            Message::PassphraseChanged(passphrase) => {
                if let Some(join) = &mut self.joining {
                    join.passphrase = passphrase;
                    join.error = None;
                }
                Task::none()
            }
            Message::JoinConfirm => {
                let Some(join) = &mut self.joining else {
                    return Task::none();
                };
                // A passphrase too short or too long for WPA is refused
                // here rather than sent: NetworkManager reports that the
                // same way it reports a wrong password, several seconds
                // later, and the two must not look identical to the user
                // when one of them was avoidable on sight.
                if join.ap.security.needs_passphrase() && !join.passphrase.is_plausible_wpa() {
                    join.error = Some(
                        "That password doesn't look right for this network — WPA needs \
                         8 to 63 characters, or a 64-character key."
                            .to_string(),
                    );
                    return Task::none();
                }
                join.connecting = true;
                join.error = None;
                let backend = Arc::clone(&self.backend);
                let ap = join.ap.clone();
                let psk = join.ap.security.needs_passphrase().then(|| join.passphrase.clone());
                Task::perform(
                    async move { backend.connect(&ap, psk.as_ref()).await.map_err(LoadError::from) },
                    Message::Connected,
                )
            }
            Message::JoinCancel => {
                self.joining = None;
                Task::none()
            }
            Message::Connected(Ok(())) => {
                self.joining = None;
                self.error = None;
                self.refresh_task()
            }
            Message::Connected(Err(e)) => {
                // The join dialog, if one is open, keeps its own error —
                // that is what tells the user their password (not the
                // whole screen) is what's wrong, and leaves the dialog
                // open on the network they were trying to join rather
                // than bouncing them back to the list.
                if let Some(join) = &mut self.joining {
                    join.connecting = false;
                    join.error = Some(e.message);
                } else {
                    self.error = Some(e.message);
                }
                Task::none()
            }
            Message::ForgetPressed(id) => {
                let backend = Arc::clone(&self.backend);
                let for_result = id.clone();
                Task::perform(
                    async move { backend.forget(&id).await.map_err(LoadError::from) },
                    move |result| Message::Forgotten(for_result.clone(), result),
                )
            }
            Message::Forgotten(id, Ok(())) => {
                self.saved.retain(|s| s.id != id);
                Task::none()
            }
            Message::Forgotten(_, Err(e)) => {
                self.error = Some(e.message);
                Task::none()
            }
            // One at a time: a second click while the first is in flight
            // would race it, and the button already says busy.
            Message::WiredConnect(_) | Message::WiredDisconnect(_) if self.wired_busy.is_some() => {
                Task::none()
            }
            Message::WiredConnect(interface) => {
                self.error = None;
                self.wired_busy = Some(interface.clone());
                let backend = Arc::clone(&self.backend);
                Task::perform(
                    async move { backend.wired_connect(&interface).await.map_err(LoadError::from) },
                    Message::WiredChanged,
                )
            }
            Message::WiredDisconnect(interface) => {
                self.error = None;
                self.wired_busy = Some(interface.clone());
                let backend = Arc::clone(&self.backend);
                Task::perform(
                    async move { backend.wired_disconnect(&interface).await.map_err(LoadError::from) },
                    Message::WiredChanged,
                )
            }
            Message::WiredChanged(result) => {
                self.wired_busy = None;
                if let Err(e) = result {
                    self.error = Some(e.message);
                }
                self.refresh_task()
            }
            Message::TrayToggled(shown) => {
                // Read-modify-write, not save-what-`new`-loaded: the Tray
                // screen (or Bluetooth, or a hand edit) may have changed
                // `tray.toml` since this screen's own `tray_prefs` was
                // last read, and a save built from this screen's stale
                // copy would silently undo that write. `prefs::update`
                // reloads immediately before applying just this one
                // field, which is correct regardless of who wrote last —
                // see its own doc comment. It also refuses and reports
                // rather than overwriting if the file has gone unreadable
                // since, the same rule `tray_prefs` already applies to a
                // plain load.
                match hyprforge_tray::prefs::update(|p| p.network = shown) {
                    Ok(updated) => self.tray_prefs = Ok(updated),
                    Err(e) => self.error = Some(e.to_string()),
                }
                Task::none()
            }
        }
    }

    fn view(&self, scale: FontScale) -> Element<'_, Message> {
        let mut content = column![].spacing(spacing::LG);

        if let Some(msg) = &self.error {
            content = content.push(scaled_text(msg.clone(), 13.0, scale).color(theme::warning()));
        }

        // Unavailable is a dead end, not a section among sections: there is
        // nothing else useful to show underneath "NetworkManager isn't
        // running", so the rest of the screen doesn't render at all.
        if let Some(msg) = &self.unavailable {
            // The tray toggle stays reachable here. It is the one control
            // on this screen that does not depend on the daemon being up
            // — and it is wanted most when the daemon is down, because
            // that is exactly when the tray icon is sitting there showing
            // an error nobody can currently do anything about. Hiding the
            // switch that turns it off is its own small dead end.
            content = content.push(scaled_text(msg.clone(), BASE_TEXT_SIZE, scale));
            content = content.push(group("Tray", vec![self.tray_row(0, scale)], scale));
            return padded(content);
        }

        if self.loading {
            content = content.push(meta_text("Loading…", BASE_TEXT_SIZE, scale));
            return padded(content);
        }

        if let Some(hero) = self.connected_card(scale) {
            content = content.push(hero);
        }

        if !self.wired.is_empty() {
            content = content.push(self.wired_section(scale));
        }

        if !self.wifi_present {
            content = content.push(meta_text("This machine has no Wi-Fi adapter.", BASE_TEXT_SIZE, scale));
            content = content.push(group("Tray", vec![self.tray_row(0, scale)], scale));
            return padded(content);
        }

        content = content.push(group("Wi-Fi", vec![self.radio_row(scale)], scale));

        let radio_on = matches!(self.status.as_ref().map(|s| s.radio), Some(RadioState::On));
        if radio_on {
            content = content.push(self.networks_section(scale));
            if !self.saved.is_empty() {
                content = content.push(self.saved_section(scale));
            }
        }

        if let Some(join) = &self.joining {
            content = content.push(self.join_dialog(join, scale));
        }

        // Its own group rather than a row under Wi-Fi: the tray icon
        // covers the cable too, and a switch inside the Wi-Fi group read
        // as switching a Wi-Fi-only icon.
        content = content.push(group("Tray", vec![self.tray_row(0, scale)], scale));

        padded(content)
    }

    fn subscription(&self) -> Subscription<Message> {
        if self.unavailable.is_some() {
            return Subscription::none();
        }
        iced::time::every(POLL_INTERVAL).map(|_| Message::Refresh)
    }
}

impl<B: NetworkBackend + 'static> NetworkModule<B> {
    /// The Wi-Fi on/off row.
    ///
    /// `RadioState::HardwareOff` gets no toggle at all: a rocker switch or
    /// an Fn key is what has to move, and a control that dispatches a
    /// D-Bus call which can't possibly change anything is worse than no
    /// control — it invites clicking it and getting no explanation why
    /// nothing happened.
    /// The network you are on, leading the page — the mockup's hero card.
    ///
    /// Only for Wi-Fi: a cable is its own row under Wired, where its
    /// Connect and Disconnect already live. Absent when not connected.
    fn connected_card(&self, scale: FontScale) -> Option<Element<'_, Message>> {
        let ssid = self.status.as_ref()?.connected_to.as_ref()?;
        let ap = self.access_points.iter().find(|ap| &ap.ssid == ssid);
        let detail = match ap {
            Some(ap) => format!(
                "Connected \u{b7} {} \u{b7} {} \u{b7} {}%",
                security_label(ap.security),
                ap.band(),
                ap.strength
            ),
            None => "Connected".to_string(),
        };
        let mark = hyprforge_ui::glyph::page(
            hyprforge_ui::glyph::Page::Network,
            scale.apply(22.0),
            Tint::Accent.iced(),
        );
        Some(
            hero_card(
                Tint::Accent,
                row![
                    mark,
                    column![
                        scaled_text(ssid.to_display_string(), 15.0, scale)
                            .font(iced::Font { weight: iced::font::Weight::Semibold, ..iced::Font::DEFAULT }),
                        config_line(detail, scale),
                    ]
                    .spacing(2.0)
                    .width(Length::Fill),
                ]
                .spacing(spacing::MD)
                .align_y(Alignment::Center),
            )
            .into(),
        )
    }

    fn radio_row(&self, scale: FontScale) -> Element<'_, Message> {
        let radio = self.status.as_ref().map(|s| s.radio);
        debug_assert_eq!(
            matches!(radio, Some(RadioState::HardwareOff)),
            !radio_offers_toggle(radio),
            "the toggle arm and the no-toggle arm below must stay in sync with this helper",
        );
        match radio {
            Some(RadioState::HardwareOff) => setting_row(
                0,
                "Wi-Fi is off",
                Some(
                    hint_text(
                        "A physical switch or Fn key is turning the radio off. \
                         Hyprforge can't turn it back on from here.",
                        scale,
                    )
                    .into(),
                ),
                iced::widget::Space::new(),
                scale,
            ),
            Some(state) => setting_row(
                0,
                "Wi-Fi",
                None,
                toggle(state == RadioState::On, scale).on_toggle(Message::RadioToggled),
                scale,
            ),
            None => setting_row(0, "Wi-Fi", Some(hint_text("Status unknown.", scale).into()), iced::widget::Space::new(), scale),
        }
    }

    /// One row per Ethernet port: its name and state, and the one button
    /// [`wired_action`] allows it.
    fn wired_section(&self, scale: FontScale) -> Element<'_, Message> {
        let mut rows = Vec::new();
        for (i, port) in self.wired.iter().enumerate() {
            let busy = self.wired_busy.as_deref() == Some(port.interface.as_str());
            let action: Element<'_, Message> = match wired_action(port.state) {
                Some(WiredAction::Connect) => secondary_button(if busy { "Connecting…" } else { "Connect" })
                    .on_press_maybe(
                        self.wired_busy
                            .is_none()
                            .then(|| Message::WiredConnect(port.interface.clone())),
                    )
                    .into(),
                Some(WiredAction::Disconnect) => {
                    secondary_button(if busy { "Disconnecting…" } else { "Disconnect" })
                        .on_press_maybe(
                            self.wired_busy
                                .is_none()
                                .then(|| Message::WiredDisconnect(port.interface.clone())),
                        )
                        .into()
                }
                None => row![].into(),
            };
            rows.push(setting_row(
                i,
                "Ethernet",
                Some(config_line(wired_detail(port), scale).into()),
                action,
                scale,
            ));
        }
        group("Wired", rows, scale)
    }

    /// The "show in tray" row.
    ///
    /// Unlike `radio_row`, this doesn't depend on live NetworkManager
    /// status — the preference lives entirely in `tray_prefs`, loaded
    /// once in `new`. While that load failed, there is no known value to
    /// show a switch for, so this says so instead of guessing a state.
    fn tray_row(&self, index: usize, scale: FontScale) -> Element<'_, Message> {
        match &self.tray_prefs {
            Ok(prefs) => setting_row(
                index,
                "Show in tray",
                None,
                toggle(prefs.network, scale).on_toggle(Message::TrayToggled),
                scale,
            ),
            Err(_) => setting_row(
                index,
                "Show in tray",
                Some(hint_text("Unavailable — see the error above.", scale).into()),
                iced::widget::Space::new(),
                scale,
            ),
        }
    }

    /// Every network in range but the one you are on — that one leads the
    /// page in its own card.
    fn networks_section(&self, scale: FontScale) -> Element<'_, Message> {
        let connected_ssid = self.status.as_ref().and_then(|s| s.connected_to.as_ref());
        let rows: Vec<Element<'_, Message>> = self
            .access_points
            .iter()
            .enumerate()
            .filter(|(_, ap)| connected_ssid != Some(&ap.ssid))
            .enumerate()
            .map(|(stripe, (i, ap))| self.network_row(stripe, i, ap, scale))
            .collect();
        if rows.is_empty() {
            return column![
                section_label("Available", scale),
                meta_text("No other networks found nearby yet.", BASE_TEXT_SIZE, scale),
            ]
            .spacing(spacing::SM)
            .into();
        }
        group("Available", rows, scale)
    }

    fn network_row<'a>(
        &'a self,
        stripe: usize,
        index: usize,
        ap: &'a AccessPoint,
        scale: FontScale,
    ) -> Element<'a, Message> {
        let saved = self.saved.iter().any(|s| s.ssid == ap.ssid);
        // Facts as chips: band and security, and "saved" when a profile
        // exists. An open network's chip is the warning colour — the
        // mockup's orange "open", and a fair thing to be told before
        // joining one.
        let security_tint = match ap.security {
            Security::Open => Tint::Warning,
            _ => Tint::Dim,
        };
        let mut facts = row![
            chip(ap.band(), Tint::Dim, scale),
            chip(security_label(ap.security), security_tint, scale),
        ]
        .spacing(spacing::XS);
        if saved {
            facts = facts.push(chip("saved", Tint::Dim, scale));
        }

        // Enterprise is listed but never gets a Join button — the reason
        // it can't be joined from here is the row's hint instead, so a
        // user scanning the list sees why without having to click first
        // and be told.
        let (hint, action): (Element<'a, Message>, Element<'a, Message>) =
            match ap.security.unsupported_reason() {
                Some(reason) => (hint_text(reason, scale).into(), iced::widget::Space::new().into()),
                None => (
                    facts.into(),
                    secondary_button("Join").on_press(Message::RowClicked(index)).into(),
                ),
            };

        let bars = hyprforge_ui::glyph::signal(
            ap.strength,
            scale.apply(16.0),
            hyprforge_ui::theme::text(),
            hyprforge_ui::theme::surface::card_border(),
        );
        setting_row(
            stripe,
            ap.ssid.to_display_string(),
            Some(row![bars, hint].spacing(spacing::SM).align_y(Alignment::Center).into()),
            action,
            scale,
        )
    }

    fn saved_section(&self, scale: FontScale) -> Element<'_, Message> {
        let rows = self
            .saved
            .iter()
            .enumerate()
            .map(|(i, saved)| {
                setting_row(
                    i,
                    saved.ssid.to_display_string(),
                    None,
                    secondary_button("Forget").on_press(Message::ForgetPressed(saved.id.clone())),
                    scale,
                )
            })
            .collect();
        group("Saved networks", rows, scale)
    }

    fn join_dialog<'a>(&'a self, join: &'a JoinDraft, scale: FontScale) -> Element<'a, Message> {
        let mut body = column![scaled_text(
            format!("Join {}", join.ap.ssid.to_display_string()),
            15.0,
            scale,
        )
        .font(iced::Font { weight: iced::font::Weight::Semibold, ..iced::Font::DEFAULT })]
        .spacing(spacing::SM);

        if let Some(err) = &join.error {
            body = body.push(scaled_text(err.clone(), 13.0, scale).color(theme::warning()));
        }

        body = body.push(
            text_input("Password", join.passphrase.expose())
                .secure(true)
                .on_input(|typed| Message::PassphraseChanged(Psk::new(typed)))
                .on_submit(Message::JoinConfirm)
                .padding(spacing::SM)
                .style(inset_input_style),
        );

        let confirm = if join.connecting {
            secondary_button("Connecting…")
        } else {
            primary_button("Connect").on_press(Message::JoinConfirm)
        };
        body = body.push(
            row![
                iced::widget::Space::new().width(Length::Fill),
                secondary_button("Cancel").on_press(Message::JoinCancel),
                confirm,
            ]
            .spacing(spacing::SM),
        );

        hero_card(Tint::Accent, body).into()
    }
}

/// A section label over a stack of striped rows — this page's groups.
fn group<'a>(label: &str, rows: Vec<Element<'a, Message>>, scale: FontScale) -> Element<'a, Message> {
    column![section_label(label, scale), setting_list(rows)].spacing(spacing::SM).into()
}

/// The page's content, padded like every other page so its first group
/// sits a gap below the title the shell draws.
fn padded(content: iced::widget::Column<'_, Message>) -> Element<'_, Message> {
    iced::widget::container(content).padding(spacing::LG).width(Length::Fill).into()
}

/// A network's security, as its chip says it.
fn security_label(security: Security) -> &'static str {
    match security {
        Security::Open => "open",
        Security::Owe => "enhanced open",
        Security::Wep => "WEP",
        Security::Wpa2Personal => "WPA2",
        Security::Wpa3Personal => "WPA3",
        Security::Enterprise => "802.1X",
    }
}

/// The real backend, connected lazily.
///
/// `NetworkManagerBackend::connect()` is async and fallible (no system
/// bus, no NetworkManager), but `App::new` — like every module's — builds
/// its screens synchronously and hands back a `Task` for anything that
/// has to wait. Wrapping the connection behind [`NetworkBackend`] itself
/// means `NetworkModule` never needs an `Option` for "not connected yet":
/// connecting is just what the first call does, the same way every other
/// bounded D-Bus round trip in this app already works. A machine with no
/// NetworkManager gets exactly one failed connect attempt, cached, and
/// every call after that returns the same [`NetworkError::Unavailable`]
/// without dialing the bus again.
pub struct LazyNetworkManagerBackend {
    /// Only a *successful* connection is cached.
    ///
    /// This held a `OnceCell<Result<..>>` first, which cached the failure
    /// too — and the failure is the one the user is told how to fix.
    /// `NetworkError::Unavailable` says "start it with `systemctl start
    /// NetworkManager`", so opening Settings before the service is up
    /// meant following the app's own instruction and watching the screen
    /// go on saying the same thing until Settings was restarted. The
    /// ten-second refresh was running the whole time and could not help.
    ///
    /// A dead end whose exit is printed on it is worse than one without,
    /// which is what pillar 3 is about.
    inner: tokio::sync::Mutex<Option<Arc<hyprforge_network::NetworkManagerBackend>>>,
}

impl LazyNetworkManagerBackend {
    pub fn new() -> Self {
        LazyNetworkManagerBackend {
            inner: tokio::sync::Mutex::new(None),
        }
    }

    /// The lock is held across the connect so that a burst of calls —
    /// the refresh tick fires status, access points and saved networks
    /// together — opens one bus connection rather than three.
    async fn get(&self) -> Result<Arc<hyprforge_network::NetworkManagerBackend>, NetworkError> {
        let mut slot = self.inner.lock().await;
        if let Some(backend) = slot.as_ref() {
            return Ok(backend.clone());
        }
        let backend = Arc::new(hyprforge_network::NetworkManagerBackend::connect().await?);
        *slot = Some(backend.clone());
        Ok(backend)
    }
}

impl Default for LazyNetworkManagerBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl NetworkBackend for LazyNetworkManagerBackend {
    async fn status(&self) -> Result<Status, NetworkError> {
        self.get().await?.status().await
    }

    async fn access_points(&self) -> Result<Vec<AccessPoint>, NetworkError> {
        self.get().await?.access_points().await
    }

    async fn wired(&self) -> Result<Vec<hyprforge_network::WiredStatus>, NetworkError> {
        self.get().await?.wired().await
    }

    async fn wired_connect(&self, interface: &str) -> Result<(), NetworkError> {
        self.get().await?.wired_connect(interface).await
    }

    async fn wired_disconnect(&self, interface: &str) -> Result<(), NetworkError> {
        self.get().await?.wired_disconnect(interface).await
    }

    async fn request_scan(&self) -> Result<(), NetworkError> {
        self.get().await?.request_scan().await
    }

    async fn saved_networks(&self) -> Result<Vec<SavedNetwork>, NetworkError> {
        self.get().await?.saved_networks().await
    }

    async fn connect(&self, ap: &AccessPoint, psk: Option<&Psk>) -> Result<(), NetworkError> {
        self.get().await?.connect(ap, psk).await
    }

    async fn connect_saved(&self, id: &str) -> Result<(), NetworkError> {
        self.get().await?.connect_saved(id).await
    }

    async fn disconnect(&self) -> Result<(), NetworkError> {
        self.get().await?.disconnect().await
    }

    async fn forget(&self, id: &str) -> Result<(), NetworkError> {
        self.get().await?.forget(id).await
    }

    async fn set_radio(&self, on: bool) -> Result<(), NetworkError> {
        self.get().await?.set_radio(on).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyprforge_network::backend::mock::MockBackend;
    use hyprforge_network::Ssid;

    fn ap(ssid: &str, security: Security, strength: u8) -> AccessPoint {
        AccessPoint {
            ssid: Ssid::new(ssid),
            bssid: "aa:bb:cc:dd:ee:ff".to_string(),
            strength,
            frequency_mhz: 2412,
            security,
        }
    }

    fn loaded(status: Status, points: Vec<AccessPoint>, saved: Vec<SavedNetwork>) -> Loaded {
        Loaded { status: Ok(status), access_points: Ok(points), saved: Ok(saved), wired: Ok(vec![]) }
    }

    fn status(radio: RadioState, connected_to: Option<&str>) -> Status {
        Status { radio, connected_to: connected_to.map(Ssid::new) }
    }

    /// Builds a module with a fresh `MockBackend`, returning both — the
    /// backend is kept so a test can inspect what the module actually
    /// called it with (`connect_calls`, `forgotten`, ...), which is the
    /// one thing a synchronous `update()` call can't show by itself.
    fn module() -> (NetworkModule<MockBackend>, Arc<MockBackend>) {
        let backend = Arc::new(MockBackend::new());
        let (module, _task) = NetworkModule::new(Arc::clone(&backend));
        (module, backend)
    }

    /// Isolated config home and greeter export dir — see
    /// [`crate::modules::with_temp_env`].
    fn with_temp_config<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        crate::modules::with_temp_env(f)
    }

    // --- tray preferences -------------------------------------------------

    /// A missing `tray.toml` is first run: both icons read as shown, and
    /// nothing about it is reported as an error — the same distinction
    /// `hyprforge_tray::prefs::load` documents, one layer up.
    #[test]
    fn a_missing_tray_toml_is_first_run_and_reads_as_shown() {
        with_temp_config(|_dir| {
            let (m, _backend) = module();
            let prefs = m.tray_prefs.as_ref().expect("a missing file is defaults, not an error");
            assert!(prefs.network);
            assert!(prefs.bluetooth);
            assert!(m.error.is_none(), "a first run is not a load failure");
        });
    }

    /// The rule this feature is most likely to break: a `tray.toml` that
    /// exists and will not parse must be reported in the screen's error
    /// banner, and a subsequent toggle must not then overwrite it with a
    /// freshly-guessed default — that would destroy whatever the user (or
    /// a hand edit) actually wrote.
    #[test]
    fn an_unreadable_tray_toml_is_reported_and_not_overwritten_by_a_toggle() {
        with_temp_config(|dir| {
            let tray_toml = dir.join("hyprforge").join("tray.toml");
            std::fs::create_dir_all(tray_toml.parent().unwrap()).unwrap();
            std::fs::write(&tray_toml, "network = yes please\n").unwrap();

            let (mut m, _backend) = module();
            assert!(m.tray_prefs.is_err(), "a malformed file must not be treated as defaults");
            assert!(
                m.error.as_ref().is_some_and(|e| e.contains("tray.toml")),
                "the failure must reach the screen's own error banner, got {:?}",
                m.error
            );

            let before = std::fs::read_to_string(&tray_toml).unwrap();
            let _ = m.update(Message::TrayToggled(false));
            let after = std::fs::read_to_string(&tray_toml).unwrap();
            assert_eq!(before, after, "a toggle must never overwrite a file it could not read");
        });
    }

    /// Toggling the icon this screen owns must leave the other icon's
    /// setting exactly as it was — never re-derived as a default, or
    /// switching Wi-Fi off would silently also turn Bluetooth back on (or
    /// off) depending on which way the default falls.
    #[test]
    fn toggling_the_tray_setting_on_one_screen_preserves_the_others_setting() {
        use crate::modules::bluetooth::{BluetoothModule, Message as BtMessage};
        use hyprforge_bluetooth::backend::mock::MockBackend as BtMockBackend;

        with_temp_config(|_dir| {
            // Bluetooth switches its own icon off first.
            let bt_backend = Arc::new(BtMockBackend::new());
            let (mut bt, _bt_backend) = BluetoothModule::new(Arc::clone(&bt_backend));
            let _ = bt.update(BtMessage::TrayToggled(false));

            // Network, built afterwards, must see that write and then
            // leave it alone when it toggles its own icon.
            let (mut net, _backend) = module();
            assert!(
                net.tray_prefs.as_ref().is_ok_and(|p| !p.bluetooth),
                "network's own load must see bluetooth's write"
            );
            let _ = net.update(Message::TrayToggled(false));

            let prefs = hyprforge_tray::prefs::load().unwrap();
            assert!(!prefs.network, "the icon this screen owns was switched off");
            assert!(!prefs.bluetooth, "the other icon's setting must survive untouched");
        });
    }

    /// Reconnecting to your own network is the commonest thing this
    /// screen does, and NetworkManager already holds the secret. Asking
    /// for it again turns a click into a form — and it is the one thing
    /// a tray menu cannot do at all, which is how this was noticed.
    #[test]
    fn clicking_a_saved_network_rejoins_it_without_asking_for_the_password() {
        let (mut m, backend) = module();
        let home = ap("home", Security::Wpa2Personal, 70);
        let _ = m.update(Message::Loaded(loaded(
            status(RadioState::On, None),
            vec![home],
            vec![SavedNetwork {
                ssid: Ssid::new("home"),
                id: "conn-1".to_string(),
                autoconnect: true,
            }],
        )));

        let _ = m.update(Message::RowClicked(0));
        assert!(
            m.joining.is_none(),
            "a saved network must not open the passphrase dialog"
        );
        let _ = backend;
    }

    fn port(interface: &str, state: WiredState) -> WiredStatus {
        WiredStatus {
            interface: interface.to_string(),
            state,
            connection: (state == WiredState::Connected).then(|| "Wired connection 1".to_string()),
            speed_mbps: (state == WiredState::Connected).then_some(1000),
        }
    }

    fn loaded_with_wired(wired: Vec<WiredStatus>) -> Loaded {
        Loaded { wired: Ok(wired), ..loaded(status(RadioState::On, None), vec![], vec![]) }
    }

    /// No button for an unplugged port — no click plugs a cable in — and
    /// none for one already connecting, so a second request can't race
    /// the first.
    #[test]
    fn only_a_port_something_can_be_done_about_gets_a_button() {
        assert_eq!(wired_action(WiredState::Connected), Some(WiredAction::Disconnect));
        assert_eq!(wired_action(WiredState::Disconnected), Some(WiredAction::Connect));
        assert_eq!(wired_action(WiredState::CableUnplugged), None);
        assert_eq!(wired_action(WiredState::Connecting), None);
    }

    #[test]
    fn a_connected_ports_line_names_its_connection_and_speed() {
        assert_eq!(
            wired_detail(&port("enp3s0", WiredState::Connected)),
            "enp3s0 \u{b7} Connected \u{b7} Wired connection 1 \u{b7} 1000 Mb/s"
        );
        assert_eq!(
            wired_detail(&port("enp3s0", WiredState::CableUnplugged)),
            "enp3s0 \u{b7} Cable unplugged"
        );
    }

    #[test]
    fn wired_ports_are_shown_in_every_state() {
        let (mut m, _backend) = module();
        let _ = m.update(Message::Loaded(loaded_with_wired(vec![
            port("enp3s0", WiredState::Connected),
            port("enp4s0", WiredState::Disconnected),
            port("enx0011", WiredState::CableUnplugged),
            port("enx0022", WiredState::Connecting),
        ])));
        assert_eq!(m.wired.len(), 4);
        let _ = m.view(FontScale::default());
    }

    /// A desktop on a cable is a kind of machine, not a failure: no
    /// banner every ten seconds, and no Wi-Fi toggle for a radio that
    /// isn't there — just a quiet line saying so.
    #[test]
    fn a_machine_with_no_wifi_adapter_is_a_state_not_a_warning() {
        let (mut m, _backend) = module();
        let _ = m.update(Message::Loaded(Loaded {
            access_points: Err(LoadError::from(NetworkError::NoWifiDevice)),
            ..loaded_with_wired(vec![port("enp3s0", WiredState::Connected)])
        }));
        assert!(!m.wifi_present);
        assert!(m.error.is_none(), "{:?}", m.error);
        assert!(m.unavailable.is_none());
        let _ = m.view(FontScale::default());
    }

    /// Whichever call notices NetworkManager is gone wins — the wired one
    /// included — so an outage never renders as "no wired ports".
    #[test]
    fn networkmanager_going_away_mid_refresh_is_noticed_by_the_wired_read_too() {
        let (mut m, _backend) = module();
        let _ = m.update(Message::Loaded(Loaded {
            wired: Err(LoadError::from(NetworkError::Unavailable)),
            ..loaded(status(RadioState::On, None), vec![], vec![])
        }));
        assert!(m.unavailable.is_some());
    }

    #[test]
    fn a_second_wired_click_while_one_is_in_flight_does_nothing() {
        let (mut m, _backend) = module();
        let _ = m.update(Message::Loaded(loaded_with_wired(vec![
            port("enp3s0", WiredState::Disconnected),
            port("enp4s0", WiredState::Connected),
        ])));
        let _ = m.update(Message::WiredConnect("enp3s0".to_string()));
        assert_eq!(m.wired_busy.as_deref(), Some("enp3s0"));
        let _ = m.update(Message::WiredDisconnect("enp4s0".to_string()));
        assert_eq!(m.wired_busy.as_deref(), Some("enp3s0"), "the second click was not taken");
        let _ = m.view(FontScale::default());
    }

    #[test]
    fn a_failed_wired_change_says_why_and_frees_the_buttons() {
        let (mut m, _backend) = module();
        let _ = m.update(Message::WiredConnect("enp3s0".to_string()));
        let _ = m.update(Message::WiredChanged(Err(LoadError::from(NetworkError::Refused(
            "The cable is unplugged.".to_string(),
        )))));
        assert!(m.wired_busy.is_none());
        assert_eq!(m.error.as_deref(), Some("The cable is unplugged."));
    }

    /// The tray toggle does not depend on NetworkManager, and is wanted
    /// most when NetworkManager is down — that is when the icon is
    /// sitting in the bar showing an error. The unavailable branch
    /// returns early, so it is easy to lose; this is what notices.
    #[test]
    fn the_tray_toggle_is_still_reachable_when_networkmanager_is_down() {
        let _guard = crate::modules::CONFIG_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (mut m, _backend) = module();
        let _ = m.update(Message::Loaded(Loaded {
            status: Err(LoadError::from(NetworkError::Unavailable)),
            access_points: Err(LoadError::from(NetworkError::Unavailable)),
            saved: Err(LoadError::from(NetworkError::Unavailable)),
            wired: Err(LoadError::from(NetworkError::Unavailable)),
        }));
        assert!(m.unavailable.is_some(), "precondition: the screen is on its dead-end path");
        // `view` is what renders the row; it must not panic and must be
        // built from the unavailable branch rather than the normal one.
        let _ = m.view(FontScale::default());
        assert!(
            m.tray_prefs.is_ok(),
            "the toggle's own state is readable regardless of the daemon"
        );
    }

    /// The distinction `hyprforge-network` exists to keep, one layer up:
    /// a daemon that isn't running must not render as an empty network
    /// list on this screen either.
    #[test]
    fn an_unavailable_networkmanager_shows_its_message_rather_than_an_empty_list() {
        let (mut m, _backend) = module();
        let _ = m.update(Message::Loaded(Loaded {
            status: Err(LoadError::from(NetworkError::Unavailable)),
            access_points: Err(LoadError::from(NetworkError::Unavailable)),
            saved: Err(LoadError::from(NetworkError::Unavailable)),
            wired: Err(LoadError::from(NetworkError::Unavailable)),
        }));
        assert!(m.unavailable.as_ref().is_some_and(|msg| msg.contains("isn't running")));
        assert!(m.access_points.is_empty(), "no networks to show while the daemon is gone");
        let _ = m.view(FontScale::default());
    }

    /// The property `radio_row` is built around: a rocker switch or an Fn
    /// key is what has to move, and no control here can do that.
    #[test]
    fn a_hardware_off_radio_does_not_offer_a_working_toggle() {
        assert!(!radio_offers_toggle(Some(RadioState::HardwareOff)));
        assert!(radio_offers_toggle(Some(RadioState::On)));
        assert!(radio_offers_toggle(Some(RadioState::Off)));

        let (mut m, _backend) = module();
        let _ = m.update(Message::Loaded(loaded(
            status(RadioState::HardwareOff, None),
            Vec::new(),
            Vec::new(),
        )));
        // Toggling it must be a no-op even if some stray message reaches
        // update — the view not rendering a control is not the only
        // guard.
        let task = m.update(Message::RadioToggled(true));
        assert_eq!(task.units(), 0, "a hardware-off radio must dispatch nothing");
        let _ = m.view(FontScale::default());
    }

    /// Enterprise networks are listed — a missing network is a bug report
    /// — but this screen can't join them, and has to say why rather than
    /// pretend the row is like any other.
    #[test]
    fn an_enterprise_network_is_listed_but_cannot_be_joined_and_says_why() {
        let (mut m, _backend) = module();
        let corp = ap("corp-wifi", Security::Enterprise, 80);
        assert!(corp.security.unsupported_reason().is_some());
        let _ = m.update(Message::Loaded(loaded(
            status(RadioState::On, None),
            vec![corp],
            Vec::new(),
        )));
        assert_eq!(m.access_points.len(), 1, "still listed");

        let _ = m.update(Message::RowClicked(0));
        assert!(m.joining.is_none(), "clicking it must not open a passphrase dialog it can't use");
        let _ = m.view(FontScale::default());
    }

    /// Open and OWE networks need nothing typed, so clicking one must
    /// connect immediately rather than opening an empty passphrase box.
    #[test]
    fn an_open_network_connects_without_asking_for_a_passphrase() {
        let (mut m, _backend) = module();
        let cafe = ap("cafe", Security::Open, 60);
        let _ = m.update(Message::Loaded(loaded(status(RadioState::On, None), vec![cafe], Vec::new())));

        let _ = m.update(Message::RowClicked(0));
        assert!(m.joining.is_none(), "an open network is never routed through the join dialog");
    }

    /// NetworkManager reports a too-short or too-long WPA passphrase the
    /// same way it reports a wrong one, several seconds later — so this
    /// has to be caught before the call is even made, or the two failures
    /// look identical to whoever's watching the screen.
    /// The passphrase is typed, so it falls under the same rule as the
    /// lock screen's: never a log, never a panic message, never a debug
    /// dump. `Message` derives `Debug` and this variant is built on every
    /// keystroke, so a `String` here put the password one `{:?}` away
    /// from the journal — and in an agent session, from the transcript.
    ///
    /// Pinned rather than reviewed, because the failure is silent: a
    /// `String` compiles, every other test passes, and nothing looks
    /// wrong until someone adds a `tracing::debug!(?message)`.
    #[test]
    fn a_typed_passphrase_never_reaches_a_debug_rendering_of_its_message() {
        let secret = "correct-horse-battery";
        let message = Message::PassphraseChanged(Psk::new(secret));
        let rendered = format!("{message:?}");
        assert!(
            !rendered.contains(secret),
            "the passphrase reached a Debug rendering of Message: {rendered}"
        );
        assert!(rendered.contains("chars"), "expected a redacted count, got {rendered}");
    }

    #[test]
    fn a_secured_network_asks_for_a_passphrase_and_refuses_an_implausible_one_before_the_backend_is_called() {
        let (mut m, backend) = module();
        let home = ap("home", Security::Wpa2Personal, 70);
        let _ = m.update(Message::Loaded(loaded(status(RadioState::On, None), vec![home], Vec::new())));

        let _ = m.update(Message::RowClicked(0));
        assert!(m.joining.is_some(), "a secured network opens the passphrase dialog");

        let _ = m.update(Message::PassphraseChanged(Psk::new("short".to_string())));
        let _ = m.update(Message::JoinConfirm);

        assert!(
            m.joining.as_ref().unwrap().error.is_some(),
            "an implausible password is refused on the spot"
        );
        assert!(
            backend.connect_calls.lock().unwrap().is_empty(),
            "the backend must never see a password the screen already knows is wrong"
        );
    }

    /// A rejected password is a fact about the password, not about the
    /// connection — the screen must keep saying "not connected" and keep
    /// the dialog open so the user can try again, not flip to Connected
    /// and then silently be wrong.
    #[test]
    fn a_wrong_passphrase_reports_a_bad_password_and_does_not_leave_the_screen_claiming_to_be_connected() {
        let (mut m, _backend) = module();
        let home = ap("home", Security::Wpa2Personal, 70);
        let _ = m.update(Message::Loaded(loaded(status(RadioState::On, None), vec![home], Vec::new())));
        let _ = m.update(Message::RowClicked(0));
        let _ = m.update(Message::PassphraseChanged(Psk::new("a-plausible-passphrase".to_string())));

        let bad_password: NetworkError = NetworkError::BadPassphrase;
        let _ = m.update(Message::Connected(Err(LoadError::from(bad_password))));

        let join = m.joining.as_ref().expect("the dialog stays open so the user can retry");
        assert!(join.error.as_ref().is_some_and(|e| e.contains("password")));
        assert!(!join.connecting, "no attempt is in flight after a failure");
        assert_eq!(
            m.status.as_ref().and_then(|s| s.connected_to.as_ref()),
            None,
            "a rejected password must never leave the screen looking connected"
        );
    }

    /// A successful join closes the dialog and clears any banner from a
    /// previous attempt — the whole reason the dialog exists is to get out
    /// of the way once it has worked.
    #[test]
    fn a_successful_join_closes_the_dialog() {
        let (mut m, _backend) = module();
        let home = ap("home", Security::Wpa2Personal, 70);
        let _ = m.update(Message::Loaded(loaded(status(RadioState::On, None), vec![home], Vec::new())));
        let _ = m.update(Message::RowClicked(0));
        m.error = Some("stale error from a previous attempt".to_string());

        let _ = m.update(Message::Connected(Ok(())));
        assert!(m.joining.is_none());
        assert!(m.error.is_none());
    }

    /// Forgetting has to be reflected immediately — waiting for the next
    /// poll to notice would leave a network the user just told the app to
    /// forget sitting in the list for up to ten seconds.
    #[test]
    fn forgetting_a_saved_network_removes_it_from_the_list() {
        let (mut m, _backend) = module();
        let saved = SavedNetwork { ssid: Ssid::new("home"), id: "conn-1".to_string(), autoconnect: true };
        let _ = m.update(Message::Loaded(loaded(
            status(RadioState::On, None),
            Vec::new(),
            vec![saved],
        )));
        assert_eq!(m.saved.len(), 1);

        let _ = m.update(Message::Forgotten("conn-1".to_string(), Ok(())));
        assert!(m.saved.is_empty());
    }

    /// A failed forget must not silently vanish the network either — the
    /// user needs to know it's still there and why.
    #[test]
    fn a_failed_forget_leaves_the_saved_network_in_place_and_reports_why() {
        let (mut m, _backend) = module();
        let saved = SavedNetwork { ssid: Ssid::new("home"), id: "conn-1".to_string(), autoconnect: true };
        let _ = m.update(Message::Loaded(loaded(
            status(RadioState::On, None),
            Vec::new(),
            vec![saved],
        )));

        let _ = m.update(Message::Forgotten(
            "conn-1".to_string(),
            Err(LoadError::from(NetworkError::Refused("busy".to_string()))),
        ));
        assert_eq!(m.saved.len(), 1, "still there");
        assert!(m.error.is_some());
    }

    /// Every state the screen can be in has to build without panicking:
    /// loading, unavailable, a hardware-off radio, a populated list with
    /// every security kind, and the join dialog on top of it.
    #[test]
    fn the_screen_builds_in_every_state() {
        let (mut m, _backend) = module();
        let scale = FontScale::default();
        let _ = m.view(scale); // loading

        let _ = m.update(Message::Loaded(Loaded {
            status: Err(LoadError::from(NetworkError::Unavailable)),
            access_points: Err(LoadError::from(NetworkError::Unavailable)),
            saved: Err(LoadError::from(NetworkError::Unavailable)),
            wired: Err(LoadError::from(NetworkError::Unavailable)),
        }));
        let _ = m.view(scale); // unavailable

        let (mut m, _backend) = module();
        let _ = m.update(Message::Loaded(loaded(
            status(RadioState::HardwareOff, None),
            Vec::new(),
            Vec::new(),
        )));
        let _ = m.view(scale); // hardware-off radio

        let (mut m, _backend) = module();
        let points = vec![
            ap("open-cafe", Security::Open, 40),
            ap("home", Security::Wpa2Personal, 70),
            ap("corp-wifi", Security::Enterprise, 55),
            ap("guest", Security::Owe, 65),
            // Secured and *not* saved, which is the only case that still
            // opens the passphrase dialog — a saved one rejoins with the
            // secret NetworkManager already has.
            ap("neighbour", Security::Wpa2Personal, 30),
        ];
        let saved = vec![SavedNetwork {
            ssid: Ssid::new("home"),
            id: "conn-1".to_string(),
            autoconnect: true,
        }];
        let _ = m.update(Message::Loaded(Loaded {
            status: Ok(status(RadioState::On, Some("home"))),
            access_points: Ok(points),
            saved: Ok(saved),
            wired: Ok(vec![]),
        }));
        let _ = m.view(scale); // populated, one connected, one saved

        m.error = Some("a scan failed".to_string());
        let _ = m.view(scale); // error banner over a populated list

        // `for_display` reorders by strength, so find the row rather than
        // assuming an index — and it must be the *unsaved* secured one,
        // since a saved network no longer prompts.
        let unsaved_index = m
            .access_points
            .iter()
            .position(|ap| ap.ssid == Ssid::new("neighbour"))
            .unwrap();
        let _ = m.update(Message::RowClicked(unsaved_index));
        assert!(m.joining.is_some());
        let _ = m.view(scale); // join dialog open

        let _ = m.update(Message::PassphraseChanged(Psk::new("short".to_string())));
        let _ = m.update(Message::JoinConfirm);
        let _ = m.view(scale); // join dialog with its own error
    }
}

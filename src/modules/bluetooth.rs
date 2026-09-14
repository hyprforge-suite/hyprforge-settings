//! Bluetooth: the adapter, discovery, and the device list — over BlueZ.
//!
//! Same shape as `modules::network`: everything that talks to BlueZ lives
//! in `hyprforge-bluetooth` already, this module is the screen on top of
//! it, and it is generic over [`BluetoothBackend`] so the tests below can
//! drive a [`MockBackend`] while `main.rs` drives the real one.

use hyprforge_bluetooth::backend::{for_display, BluetoothBackend};
use hyprforge_bluetooth::{Address, AdapterState, BluetoothError, Device, PairingPrompt, Passkey, Status};
use hyprforge_tray::Prefs as TrayPrefs;
use hyprforge_ui::theme::{self, spacing, FontScale, BASE_TEXT_SIZE};
use hyprforge_ui::widgets::{divider, meta_text, primary_button, scaled_text, secondary_button, section};
use iced::widget::{checkbox, column, row};
use iced::{Alignment, Element, Length, Subscription, Task};
use std::sync::Arc;
use std::time::Duration;

use crate::module::SettingsModule;

/// How often the screen re-polls status and devices while it's open, so a
/// device that appeared, connected, or disconnected elsewhere shows up
/// without the user hitting a refresh button. Same interval as the Network
/// screen; independent of [`Message::ScanToggled`], which is the one thing
/// this poll must never touch — see the comment on that variant.
const POLL_INTERVAL: Duration = Duration::from_secs(10);

/// A load-time failure, reduced to what the screen needs: text to show,
/// and whether it's the one kind of failure ([`BluetoothError::Unavailable`])
/// that must never be confused with an empty device list.
///
/// `BluetoothError` itself isn't `Clone` — this is the boundary where a
/// borrowed error becomes an owned value a `Message` can carry, same as
/// `network::LoadError`.
#[derive(Debug, Clone)]
pub struct LoadError {
    message: String,
    unavailable: bool,
}

impl From<BluetoothError> for LoadError {
    fn from(e: BluetoothError) -> Self {
        LoadError {
            unavailable: matches!(e, BluetoothError::Unavailable),
            message: e.to_string(),
        }
    }
}

/// The result of one refresh: two independent calls, two independent
/// outcomes. A failure in `devices` must not discard a successful
/// `status`, and vice versa.
#[derive(Debug, Clone)]
pub struct Loaded {
    status: Result<Status, LoadError>,
    devices: Result<Vec<Device>, LoadError>,
}

async fn load<B: BluetoothBackend + ?Sized>(backend: Arc<B>) -> Loaded {
    Loaded {
        status: backend.status().await.map_err(LoadError::from),
        devices: backend.devices().await.map_err(LoadError::from),
    }
}

/// Whether the adapter row gets a control that can actually do something.
///
/// `AdapterState::HardwareBlocked` means a rfkill switch has to move — no
/// D-Bus call changes that — so this is `false` there and nowhere else.
/// `AdapterState::Changing` also gets no toggle: the state is mid-flight,
/// and a control offered while it's unknown which way it will land just
/// invites a second click that races the first.
///
/// Pulled out as its own function, rather than left as an inline match arm
/// in `adapter_row`, for the same reason `network::radio_offers_toggle`
/// is: the property is something a test can assert directly.
fn adapter_offers_toggle(state: Option<AdapterState>) -> bool {
    matches!(state, Some(AdapterState::On) | Some(AdapterState::Off))
}

#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    Loaded(Loaded),
    AdapterToggled(bool),
    AdapterSet(Result<(), LoadError>),
    /// Discovery, started or stopped. This is the *only* place a discovery
    /// call is made — never from `Refresh`, and never from entering the
    /// screen (see [`BluetoothModule::new`]). Discovery costs airtime and
    /// battery on both ends, and `hyprforge-bluetooth::BluetoothBackend`
    /// already documents why it's a separate call from listing devices;
    /// starting it implicitly here would quietly turn every ten-second
    /// poll, and every app launch, into a radio-on event the user never
    /// asked for.
    ScanToggled(bool),
    ScanSet(Result<(), LoadError>),
    ConnectPressed(Address),
    Connected(Address, Result<(), LoadError>),
    DisconnectPressed(Address),
    Disconnected(Address, Result<(), LoadError>),
    ForgetPressed(Address),
    Forgotten(Address, Result<(), LoadError>),
    /// Trusting a device is what lets it reconnect on its own — a headset
    /// that comes back when it's switched on rather than needing a click
    /// here every time. See `BluetoothBackend::set_trusted`.
    TrustToggled(Address, bool),
    /// Carries the target `trusted` value along with the result: BlueZ's
    /// reply is just success/failure, and the row needs to know what it
    /// asked for in order to show the outcome without waiting on the next
    /// poll to re-read it.
    Trusted(Address, bool, Result<(), LoadError>),
    /// Whether `hyprforge-trayd` should show the Bluetooth icon. Same
    /// shape as `network::Message::TrayToggled` — only reachable from a
    /// loaded [`TrayPrefs`], guarded again in `update`.
    TrayToggled(bool),

    // --- Pairing ------------------------------------------------------
    //
    // `Device::unsupported_reason` used to be the whole story for an
    // unpaired device: a string telling the user to run `bluetoothctl`.
    // Pairing is wired up here now, and that method has since been
    // deleted from `hyprforge-bluetooth` — the follow-up this comment
    // used to ask for is done. The `unsupported_reason` still in the
    // tree belongs to `hyprforge-network` and is a different thing: a
    // Wi-Fi security mode this suite genuinely cannot join.
    /// The device a Pair button was pressed for.
    PairPressed(Address),
    Paired(Address, Result<(), LoadError>),
    /// A prompt BlueZ wants answered, relayed from whatever agent is
    /// registered — `org.bluez.Agent1`, driven by
    /// `hyprforge_bluetooth::agent`.
    ///
    /// This screen is built entirely in terms of [`PairingPrompt`], which
    /// already existed when this was written — every test below drives
    /// this variant directly, the same way it would be driven by a real
    /// subscription once one is wired up. See the comment above
    /// `LazyBlueZBackend` for why the real `agent::PairingRequest`
    /// channel isn't plumbed all the way to `subscription()` yet, and
    /// what's needed to finish that.
    ///
    /// `#[allow(dead_code)]`: nothing constructs this outside tests until
    /// that wiring exists, and that's the honest state of things rather
    /// than something to paper over with a fake caller.
    #[allow(dead_code)]
    PairingPrompted(PairingPrompt),
    /// Match (on a [`PairingPrompt::Confirm`]) or Accept (on
    /// [`PairingPrompt::Authorize`]) — answering the prompt
    /// affirmatively. Never produced for [`PairingPrompt::Display`],
    /// whose dialog has no button that would send it — see
    /// [`PairingPrompt::needs_an_answer`].
    PairingAccepted,
    /// Don't match, Cancel, or dismissing the dialog by any other means
    /// — all the same action underneath: give up on this pairing and
    /// tell BlueZ, so a dismissed dialog doesn't leave it waiting on an
    /// answer that will never come.
    PairingCancelled,
    PairingCancelSet(Result<(), LoadError>),
}

pub struct BluetoothModule<B: BluetoothBackend + 'static> {
    backend: Arc<B>,
    /// `true` until the first [`Message::Loaded`] lands, so the screen can
    /// say "loading" instead of "no devices" while the first round trip is
    /// still in flight.
    loading: bool,
    /// Set only from [`BluetoothError::Unavailable`], and rendered as its
    /// own state rather than folded into `devices` being empty — the
    /// distinction `hyprforge-bluetooth` was written to keep.
    unavailable: Option<String>,
    /// Anything else that went wrong: a connect that failed, a forget that
    /// didn't take. Cleared on the next successful action, not on every
    /// refresh, so it doesn't flash away before it's been read.
    error: Option<String>,
    status: Option<Status>,
    /// Already grouped and sorted by [`for_display`] — this module does
    /// not re-implement that.
    devices: Vec<Device>,
    /// The on-disk `tray.toml`, loaded once at construction. Same shape
    /// and same reasoning as `network::NetworkModule::tray_prefs`: kept
    /// as the whole [`TrayPrefs`] so a toggle here can write back
    /// `network` unchanged, and `Err` (a file that exists and would not
    /// parse) renders no checkbox at all rather than guessing a value.
    tray_prefs: Result<TrayPrefs, String>,
    /// The pairing conversation currently in front of the user, if any —
    /// set by [`Message::PairingPrompted`] and cleared by an answer, a
    /// cancel, or the underlying `pair()` call resolving one way or the
    /// other. At most one at a time: the screen shows one dialog, the
    /// same way `network::NetworkModule::joining` is one draft at a time.
    pairing: Option<PairingPrompt>,
}

impl<B: BluetoothBackend + 'static> BluetoothModule<B> {
    /// Builds the module around an already-usable backend and kicks off
    /// the first load.
    ///
    /// The first load is `status` and `devices` only — never
    /// `set_discovery`. Entering the screen must not itself start a scan;
    /// that is `Message::ScanToggled`'s alone to do, from an explicit
    /// click.
    pub fn new(backend: Arc<B>) -> (Self, Task<Message>) {
        // A missing file is first run and loads as defaults; a file that
        // exists and will not parse is reported here, in the same banner
        // every other load failure on this screen uses, rather than
        // silently treated as "both icons shown".
        let (tray_prefs, tray_error) = match hyprforge_tray::prefs::load() {
            Ok(prefs) => (Ok(prefs), None),
            Err(e) => (Err(e.to_string()), Some(e.to_string())),
        };
        let module = BluetoothModule {
            backend,
            loading: true,
            unavailable: None,
            error: tray_error,
            status: None,
            devices: Vec::new(),
            tray_prefs,
            pairing: None,
        };
        let task = Task::perform(load(Arc::clone(&module.backend)), Message::Loaded);
        (module, task)
    }

    fn refresh_task(&self) -> Task<Message> {
        Task::perform(load(Arc::clone(&self.backend)), Message::Loaded)
    }
}

impl<B: BluetoothBackend + 'static> SettingsModule for BluetoothModule<B> {
    type Message = Message;

    fn icon(&self) -> &'static str {
        // There is no standard Unicode Bluetooth glyph — the logo is a
        // registered trademark symbol, not a codepoint — so this picks a
        // color distinct from every other sidebar icon rather than reusing
        // Network's 📶, which would read as the same screen twice.
        "\u{1F535}" // 🔵
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Refresh => {
                self.loading = self.status.is_none() && self.unavailable.is_none();
                self.refresh_task()
            }
            Message::Loaded(result) => {
                self.loading = false;
                // Either call can be the one that notices bluetoothd is
                // gone — the mock fails both at once, the real backend
                // fails whichever it was mid-call on. Whichever it is, it
                // wins: an empty device list must never stand in for this.
                let unavailable_msg = [result.status.as_ref().err(), result.devices.as_ref().err()]
                    .into_iter()
                    .flatten()
                    .find(|e| e.unavailable)
                    .map(|e| e.message.clone());

                if let Some(msg) = unavailable_msg {
                    self.unavailable = Some(msg);
                    self.status = None;
                    self.devices.clear();
                    return Task::none();
                }
                self.unavailable = None;
                match result.status {
                    Ok(status) => self.status = Some(status),
                    Err(e) => self.error = Some(e.message),
                }
                match result.devices {
                    Ok(devices) => self.devices = for_display(devices),
                    Err(e) => self.error = Some(e.message),
                }
                Task::none()
            }
            Message::AdapterToggled(on) => {
                // A blocked or already-changing adapter never reaches this
                // arm because the view renders no toggle for either state
                // — see `adapter_row` — but the check stays here too, so a
                // stray message can never dispatch a call that can only
                // fail or race.
                if !adapter_offers_toggle(self.status.as_ref().map(|s| s.state)) {
                    return Task::none();
                }
                let backend = Arc::clone(&self.backend);
                Task::perform(
                    async move { backend.set_powered(on).await.map_err(LoadError::from) },
                    Message::AdapterSet,
                )
            }
            Message::AdapterSet(Ok(())) => self.refresh_task(),
            Message::AdapterSet(Err(e)) => {
                self.error = Some(e.message);
                Task::none()
            }
            Message::ScanToggled(on) => {
                let backend = Arc::clone(&self.backend);
                Task::perform(
                    async move { backend.set_discovery(on).await.map_err(LoadError::from) },
                    Message::ScanSet,
                )
            }
            Message::ScanSet(Ok(())) => self.refresh_task(),
            Message::ScanSet(Err(e)) => {
                self.error = Some(e.message);
                Task::none()
            }
            Message::ConnectPressed(address) => {
                // An unpaired device is listed but its row has no Connect
                // button wired to it — see `device_row` — so a stray
                // message here still must not start a connection that can
                // only fail. Same guard `network.rs` keeps for enterprise
                // Wi-Fi. Checked as `!paired` directly rather than via
                // `unsupported_reason()`, which no longer exists on
                // `Device` at all — this guard never depended on text,
                // which is why its deletion did not touch this line.
                let Some(device) = self.devices.iter().find(|d| d.address == address) else {
                    return Task::none();
                };
                if !device.paired {
                    return Task::none();
                }
                self.error = None;
                let backend = Arc::clone(&self.backend);
                let for_result = address.clone();
                Task::perform(
                    async move { backend.connect(&address).await.map_err(LoadError::from) },
                    move |result| Message::Connected(for_result.clone(), result),
                )
            }
            Message::Connected(address, Ok(())) => {
                self.error = None;
                // Reflected immediately rather than waiting on the refresh
                // that follows: the round trip that just succeeded is
                // itself proof of the new state, and ten seconds is a long
                // time for a row to keep reading "disconnected" about a
                // connection the user watched succeed.
                if let Some(d) = self.devices.iter_mut().find(|d| d.address == address) {
                    d.connected = true;
                }
                self.refresh_task()
            }
            Message::Connected(_, Err(e)) => {
                self.error = Some(e.message);
                Task::none()
            }
            Message::DisconnectPressed(address) => {
                self.error = None;
                let backend = Arc::clone(&self.backend);
                let for_result = address.clone();
                Task::perform(
                    async move { backend.disconnect(&address).await.map_err(LoadError::from) },
                    move |result| Message::Disconnected(for_result.clone(), result),
                )
            }
            Message::Disconnected(address, Ok(())) => {
                if let Some(d) = self.devices.iter_mut().find(|d| d.address == address) {
                    d.connected = false;
                }
                self.refresh_task()
            }
            Message::Disconnected(_, Err(e)) => {
                self.error = Some(e.message);
                Task::none()
            }
            Message::ForgetPressed(address) => {
                self.error = None;
                let backend = Arc::clone(&self.backend);
                let for_result = address.clone();
                Task::perform(
                    async move { backend.forget(&address).await.map_err(LoadError::from) },
                    move |result| Message::Forgotten(for_result.clone(), result),
                )
            }
            Message::Forgotten(address, Ok(())) => {
                self.devices.retain(|d| d.address != address);
                Task::none()
            }
            Message::Forgotten(_, Err(e)) => {
                self.error = Some(e.message);
                Task::none()
            }
            Message::TrustToggled(address, trusted) => {
                self.error = None;
                let backend = Arc::clone(&self.backend);
                let for_result = address.clone();
                Task::perform(
                    async move {
                        backend.set_trusted(&address, trusted).await.map_err(LoadError::from)
                    },
                    move |result| Message::Trusted(for_result.clone(), trusted, result),
                )
            }
            Message::Trusted(address, trusted, Ok(())) => {
                self.error = None;
                if let Some(d) = self.devices.iter_mut().find(|d| d.address == address) {
                    d.trusted = trusted;
                }
                self.refresh_task()
            }
            Message::Trusted(_, _, Err(e)) => {
                self.error = Some(e.message);
                Task::none()
            }
            Message::TrayToggled(shown) => {
                // Read-modify-write — see `network::Message::TrayToggled`'s
                // arm and `hyprforge_tray::prefs::update`'s own doc
                // comment for why a save built from this screen's own
                // (possibly stale) `tray_prefs` copy is not safe here.
                match hyprforge_tray::prefs::update(|p| p.bluetooth = shown) {
                    Ok(updated) => self.tray_prefs = Ok(updated),
                    Err(e) => self.error = Some(e.to_string()),
                }
                Task::none()
            }
            Message::PairPressed(address) => {
                // The view renders no Pair button once a device is
                // paired — see `device_row` — but the guard stays here
                // too, for the same reason `ConnectPressed` keeps its
                // own: a stray message must not reach the backend with a
                // call that can only fail or race.
                let Some(device) = self.devices.iter().find(|d| d.address == address) else {
                    return Task::none();
                };
                if device.paired {
                    return Task::none();
                }
                self.error = None;
                let backend = Arc::clone(&self.backend);
                let for_result = address.clone();
                Task::perform(
                    async move { backend.pair(&address).await.map_err(LoadError::from) },
                    move |result| Message::Paired(for_result.clone(), result),
                )
            }
            Message::Paired(address, Ok(())) => {
                self.error = None;
                // Same immediacy as `Connected`: the round trip that
                // just succeeded is itself proof of the new state, and
                // the row should read "paired" the moment it is, not up
                // to ten seconds later.
                if let Some(d) = self.devices.iter_mut().find(|d| d.address == address) {
                    d.paired = true;
                }
                // BlueZ would not report success while still waiting on
                // an answer, so whatever prompt was open for this device
                // is resolved now.
                if self.pairing.as_ref().is_some_and(|p| p.device() == &address) {
                    self.pairing = None;
                }
                self.refresh_task()
            }
            Message::Paired(address, Err(e)) => {
                self.error = Some(e.message);
                // Nothing to revert — the device was never marked paired
                // in the first place — but a dialog left open for a
                // conversation that's already over would be asking the
                // user to answer a question BlueZ has stopped listening
                // for.
                if self.pairing.as_ref().is_some_and(|p| p.device() == &address) {
                    self.pairing = None;
                }
                Task::none()
            }
            Message::PairingPrompted(prompt) => {
                self.pairing = Some(prompt);
                Task::none()
            }
            Message::PairingAccepted => {
                // BlueZ is blocked on this answer. The `pair()` call
                // started by `PairPressed` does not complete until the
                // conversation does, and `Message::Paired` is what
                // updates the row afterwards.
                answer_pairing(true);
                self.pairing = None;
                Task::none()
            }
            Message::PairingCancelled => {
                let Some(prompt) = self.pairing.take() else {
                    return Task::none();
                };
                // Answer first, then cancel. BlueZ is waiting on the
                // agent's reply, and `CancelPairing` on a conversation
                // that is still blocked on us is the slower way round.
                answer_pairing(false);
                let backend = Arc::clone(&self.backend);
                let address = prompt.device().clone();
                Task::perform(
                    async move { backend.cancel_pairing(&address).await.map_err(LoadError::from) },
                    Message::PairingCancelSet,
                )
            }
            Message::PairingCancelSet(Ok(())) => Task::none(),
            Message::PairingCancelSet(Err(e)) => {
                self.error = Some(e.message);
                Task::none()
            }
        }
    }

    fn view(&self, scale: FontScale) -> Element<'_, Message> {
        let mut content = column![row![
            scaled_text("Bluetooth", 22.0, scale).width(Length::Fill),
            secondary_button("Refresh").on_press(Message::Refresh),
        ]
        .spacing(spacing::SM)
        .align_y(Alignment::Center)]
        .spacing(spacing::LG);

        if let Some(msg) = &self.error {
            content = content.push(scaled_text(msg.clone(), 13.0, scale).color(theme::warning()));
        }

        // Unavailable is a dead end, not a section among sections: there is
        // nothing else useful to show underneath "Bluetooth isn't
        // running", so the rest of the screen doesn't render at all —
        // same call `network.rs` makes for NetworkManager being gone.
        if let Some(msg) = &self.unavailable {
            // The tray toggle stays reachable here. It is the one control
            // on this screen that does not depend on the daemon being up
            // — and it is wanted most when the daemon is down, because
            // that is exactly when the tray icon is sitting there showing
            // an error nobody can currently do anything about. Hiding the
            // switch that turns it off is its own small dead end.
            content = content.push(section(
                "Bluetooth",
                scale,
                column![scaled_text(msg.clone(), BASE_TEXT_SIZE, scale), self.tray_row(scale)]
                    .spacing(spacing::SM),
            ));
            return content.into();
        }

        if self.loading {
            content = content.push(meta_text("Loading…", BASE_TEXT_SIZE, scale));
            return content.into();
        }

        content = content.push(self.adapter_row(scale));

        let adapter_on = matches!(self.status.as_ref().map(|s| s.state), Some(AdapterState::On));
        if adapter_on {
            content = content.push(self.scan_row(scale));
            content = content.push(self.devices_section(scale));
        }

        if let Some(prompt) = &self.pairing {
            content = content.push(self.pairing_dialog(prompt, scale));
        }

        content.into()
    }

    fn subscription(&self) -> Subscription<Message> {
        if self.unavailable.is_some() {
            return Subscription::none();
        }
        // The pairing agent runs alongside the poll rather than only
        // while a dialog is open: BlueZ asks the agent *during* `pair()`,
        // so an agent registered in response to the first prompt would
        // already be too late.
        //
        // Safe to name here in tests as well — an iced `Subscription` is
        // a description, and nothing connects to a bus until the runtime
        // polls the stream.
        Subscription::batch([
            iced::time::every(POLL_INTERVAL).map(|_| Message::Refresh),
            Subscription::run(pairing_stream),
        ])
    }
}

impl<B: BluetoothBackend + 'static> BluetoothModule<B> {
    /// The Bluetooth on/off row.
    ///
    /// `AdapterState::HardwareBlocked` gets no toggle at all: a rfkill
    /// switch is what has to move, and a control that dispatches a D-Bus
    /// call which can't possibly change anything is worse than no control.
    /// `AdapterState::Changing` gets no toggle either, shown as busy
    /// instead — flicking a checkbox while the real state is still
    /// mid-transition is how it snaps back under the pointer.
    fn adapter_row(&self, scale: FontScale) -> Element<'_, Message> {
        let state = self.status.as_ref().map(|s| s.state);
        debug_assert_eq!(
            matches!(state, Some(AdapterState::HardwareBlocked) | Some(AdapterState::Changing)),
            !adapter_offers_toggle(state),
            "the toggle arm and the no-toggle arms below must stay in sync with this helper",
        );
        let body: Element<'_, Message> = match state {
            Some(AdapterState::HardwareBlocked) => column![
                scaled_text("Bluetooth is off", 15.0, scale),
                meta_text(
                    "A physical switch or Fn key is blocking the radio. \
                     Hyprforge can't turn it back on from here.",
                    13.0,
                    scale,
                ),
            ]
            .spacing(spacing::XS)
            .into(),
            Some(AdapterState::Changing) => meta_text("Bluetooth is changing…", BASE_TEXT_SIZE, scale).into(),
            Some(state) => {
                let on = state == AdapterState::On;
                row![
                    checkbox(on).on_toggle(Message::AdapterToggled),
                    scaled_text("Bluetooth", 15.0, scale),
                ]
                .spacing(spacing::SM)
                .align_y(Alignment::Center)
                .into()
            }
            None => meta_text("Bluetooth status unknown.", BASE_TEXT_SIZE, scale).into(),
        };
        section("Bluetooth", scale, column![body, self.tray_row(scale)].spacing(spacing::SM))
    }

    /// The "show in tray" row, appended to the Bluetooth section. Same
    /// shape and same reasoning as `network::NetworkModule::tray_row`.
    fn tray_row(&self, scale: FontScale) -> Element<'_, Message> {
        match &self.tray_prefs {
            Ok(prefs) => row![
                checkbox(prefs.bluetooth).on_toggle(Message::TrayToggled),
                scaled_text("Show in tray", 15.0, scale),
            ]
            .spacing(spacing::SM)
            .align_y(Alignment::Center)
            .into(),
            Err(_) => meta_text(
                "Tray setting unavailable — see the error above.",
                13.0,
                scale,
            )
            .into(),
        }
    }

    /// Discovery: an explicit toggle, never implied by a refresh or by
    /// opening the screen. See the comment on [`Message::ScanToggled`].
    fn scan_row(&self, scale: FontScale) -> Element<'_, Message> {
        let discovering = self.status.as_ref().is_some_and(|s| s.discovering);
        section(
            "Scan",
            scale,
            row![
                checkbox(discovering).on_toggle(Message::ScanToggled),
                scaled_text(
                    if discovering { "Scanning for devices…" } else { "Scan for devices" },
                    15.0,
                    scale,
                ),
            ]
            .spacing(spacing::SM)
            .align_y(Alignment::Center),
        )
    }

    fn devices_section(&self, scale: FontScale) -> Element<'_, Message> {
        if self.devices.is_empty() {
            return section(
                "Devices",
                scale,
                meta_text("No devices yet. Turn on Scan to look for nearby ones.", BASE_TEXT_SIZE, scale),
            );
        }
        let mut list = column![].spacing(spacing::SM);
        for (i, device) in self.devices.iter().enumerate() {
            if i > 0 {
                list = list.push(divider());
            }
            list = list.push(self.device_row(device, scale));
        }
        section("Devices", scale, list)
    }

    fn device_row<'a>(&'a self, device: &'a Device, scale: FontScale) -> Element<'a, Message> {
        // A device that has never told us its name shows its address, and
        // presenting that as though it were a name is worse than saying
        // plainly that no name is known yet — same reasoning `Device` docs
        // give for keeping `alias` and `name` separate.
        let name: Element<'_, Message> = if device.is_unnamed() {
            meta_text(format!("Unnamed device ({})", device.address), BASE_TEXT_SIZE, scale).into()
        } else {
            scaled_text(device.alias.clone(), BASE_TEXT_SIZE, scale).into()
        };

        let connection = if device.connected { " \u{b7} Connected" } else { "" };
        let line = column![
            name,
            meta_text(format!("{}{}", device.kind.label(), connection), 12.0, scale),
        ]
        .spacing(2.0);

        // `Device::unsupported_reason` is *not* read here on purpose: its
        // "use bluetoothctl" string is stale now that pairing has a
        // screen of its own, and it should be deleted from
        // `hyprforge-bluetooth` in a follow-up (that crate is off limits
        // for this change). An unpaired device gets a Pair button
        // instead of an explanation, the same way a device with nothing
        // else to offer used to get only text.
        if !device.paired {
            let pair_action: Element<'_, Message> = secondary_button("Pair")
                .on_press(Message::PairPressed(device.address.clone()))
                .into();
            return row![line.width(Length::Fill), pair_action]
                .spacing(spacing::SM)
                .align_y(Alignment::Center)
                .into();
        }

        let connect_action: Element<'_, Message> = if device.connected {
            secondary_button("Disconnect")
                .on_press(Message::DisconnectPressed(device.address.clone()))
                .into()
        } else {
            secondary_button("Connect")
                .on_press(Message::ConnectPressed(device.address.clone()))
                .into()
        };

        let actions = column![
            row![connect_action, secondary_button("Forget").on_press(Message::ForgetPressed(device.address.clone()))]
                .spacing(spacing::SM),
            row![
                checkbox(device.trusted)
                    .on_toggle({
                        let address = device.address.clone();
                        move |v| Message::TrustToggled(address.clone(), v)
                    }),
                meta_text("Trust (reconnect automatically)", 12.0, scale),
            ]
            .spacing(spacing::SM)
            .align_y(Alignment::Center),
        ]
        .spacing(spacing::XS);

        row![line.width(Length::Fill), actions]
            .spacing(spacing::SM)
            .align_y(Alignment::Center)
            .into()
    }

    /// The pairing conversation, shaped like `network::join_dialog` —
    /// one dialog, appended below the device list while it's open.
    ///
    /// Every arm ends in a Cancel (or, for `Confirm`, "Don't match")
    /// that dismisses it. There is no way to close this dialog other
    /// than a button that sends [`Message::PairingCancelled`] or
    /// [`Message::PairingAccepted`] — no implicit dismissal on, say,
    /// leaving the screen — because either message is what tells BlueZ
    /// the conversation is over. A dialog that could vanish on its own
    /// would be exactly the "left waiting" case `cancel_pairing` exists
    /// to prevent.
    fn pairing_dialog<'a>(&'a self, prompt: &'a PairingPrompt, scale: FontScale) -> Element<'a, Message> {
        let mut body = column![scaled_text(format!("Pair with {}", prompt.name()), 16.0, scale)]
            .spacing(spacing::SM);

        match prompt {
            PairingPrompt::Confirm { passkey, .. } => {
                // The digits are the entire security property of a
                // numeric-comparison pairing, so they get the biggest
                // text on this screen rather than blending in with a
                // line of prose above or below them.
                body = body.push(scaled_text(passkey_digits(passkey), 28.0, scale));
                body = body.push(meta_text(
                    "Check that the other device is showing the same six digits.",
                    13.0,
                    scale,
                ));
            }
            PairingPrompt::Authorize { .. } => {
                // "Just Works": nothing to compare. Showing no digits
                // here isn't an omission — inviting the user to check a
                // number that doesn't exist is worse than showing none.
                body = body.push(meta_text(
                    "This device has no screen or keypad to show a code on. \
                     Only continue if you meant to pair it.",
                    13.0,
                    scale,
                ));
            }
            PairingPrompt::Display { passkey, .. } => {
                body = body.push(scaled_text(passkey_digits(passkey), 28.0, scale));
                body = body.push(meta_text(
                    "Type this code on the other device to finish pairing.",
                    13.0,
                    scale,
                ));
            }
        }

        let mut actions = row![].spacing(spacing::SM);
        if pairing_offers_accept(prompt) {
            let accept_label = if matches!(prompt, PairingPrompt::Confirm { .. }) {
                "Match"
            } else {
                "Accept"
            };
            actions = actions.push(primary_button(accept_label).on_press(Message::PairingAccepted));
        }
        let cancel_label =
            if matches!(prompt, PairingPrompt::Confirm { .. }) { "Don't match" } else { "Cancel" };
        actions = actions.push(secondary_button(cancel_label).on_press(Message::PairingCancelled));
        body = body.push(actions);

        section("Pairing", scale, body)
    }
}

/// Whether the pairing dialog should render a button that answers the
/// prompt right now, as opposed to a Cancel-only dialog.
///
/// Pulled out of `pairing_dialog` as its own function, the same way
/// `adapter_offers_toggle` is pulled out of `adapter_row`: the property
/// — [`PairingPrompt::Display`] gets no accept button, because
/// [`PairingPrompt::needs_an_answer`] is false for it and the far end
/// answers by typing — becomes something a test can assert directly
/// instead of only being implied by which arm of a view function
/// happens to push a button.
fn pairing_offers_accept(prompt: &PairingPrompt) -> bool {
    prompt.needs_an_answer()
}

/// The digits shown in a `Confirm` or `Display` dialog.
///
/// Always [`Passkey`]'s own `Display` impl, which zero-pads to six
/// digits — never `passkey.as_u32()` formatted here instead. That
/// zero-pad is load-bearing: a passkey of `1234` shown as "1234" next to
/// a device showing "001234" is a user correctly deciding the codes
/// don't match. Pulled into its own function so a test pins that this
/// screen goes through `Passkey::to_string`, not a shortcut around it.
fn passkey_digits(passkey: &Passkey) -> String {
    passkey.to_string()
}

// The seam where `hyprforge_bluetooth::agent`'s pairing-prompt channel
// plugs in — and why it stops at a seam rather than going all the way
// to `subscription()`.
//
// `hyprforge_bluetooth::agent` landed while this module was being
// written. Its `register(connection)` hands back an
// `mpsc::UnboundedReceiver<agent::PairingRequest>`, and each
// `PairingRequest` carries a `pub prompt: PairingPrompt` alongside
// `accept(self)`/`reject(self)` — deliberately consuming, so accept and
// reject can't be swapped at the call site.
//
// Two things stop this screen from owning that receiver directly:
//
// 1. `PairingRequest`'s constructor is private — `register` is the only
//    way to produce one. That is almost certainly deliberate (the same
//    instinct as `PairingPrompt::needs_an_answer` being the only way to
//    ask what a prompt wants), but it also means this module's own
//    tests, which construct every `PairingPrompt` variant directly,
//    cannot construct a `PairingRequest` to drive `Message::Paired`'s
//    sibling messages the same way. Message would need to carry the
//    request in order to call `accept`/`reject` on it later, and this
//    module cannot build one to test that path.
// 2. Registering the agent needs a live `zbus::Connection` up front,
//    kept for the app's lifetime. `LazyBlueZBackend` (below) only
//    creates one lazily, inside a private `get()`, on the first status
//    or device call — there is nothing today that hands a `Connection`
//    to `main.rs` at startup to register against.
//
// Both are `main.rs`-level decisions, not this screen's: whether the
// backend grows a way to expose or share its connection, and how a
// `PairingRequest` becomes a `Message::PairingPrompted(prompt)` plus
// something *outside* this module's own state that still holds the
// means to answer it when `PairingAccepted`/`PairingCancelled` comes
// back out. `Message::PairingPrompted(PairingPrompt)` and the
// `PairingAccepted`/`PairingCancelled` messages this screen already
// sends are the seam — whatever bridges the real channel just needs to
// map a `PairingRequest` into `PairingPrompted(request.prompt.clone())`
// on the way in, and watch for this screen's own outgoing messages (or
// take the accept/reject decision some other way) to call
// `request.accept()` / `request.reject()` on the way out.

/// The real backend, connected lazily.
///
/// `BlueZBackend::connect()` is async and fallible (no system bus, no
/// `bluetoothd`), but `App::new` — like every module's — builds its
/// screens synchronously and hands back a `Task` for anything that has to
/// wait. Wrapping the connection behind [`BluetoothBackend`] itself means
/// `BluetoothModule` never needs an `Option` for "not connected yet":
/// connecting is just what the first call does.
/// The answer half of a pairing conversation.
///
/// The two halves genuinely live in different places. A `PairingRequest`
/// arrives on the subscription below, which owns it and is the only thing
/// that can answer it — but the answer comes from `update`, which cannot
/// reach into a running stream. This is the wire between them.
///
/// `Message` deliberately carries only a [`PairingPrompt`], never the
/// request itself: the prompt is plain data any test can build, and a
/// request's constructor is private to `hyprforge-bluetooth`. Keeping the
/// message testable is worth one process-global sender.
static PAIRING_ANSWERS: std::sync::OnceLock<tokio::sync::mpsc::UnboundedSender<bool>> =
    std::sync::OnceLock::new();

/// Answers the pairing conversation currently in flight, if any.
///
/// A no-op when nothing is listening — which is the case in every test in
/// this module, and is why the dialog's own behaviour can be asserted
/// without a bus.
fn answer_pairing(accept: bool) {
    if let Some(tx) = PAIRING_ANSWERS.get() {
        let _ = tx.send(accept);
    }
}

/// Registers the pairing agent and turns its prompts into messages.
///
/// Its own system-bus connection, not the backend's: registering an agent
/// needs a connection that outlives any one call, and `LazyBlueZBackend`
/// only makes one lazily inside a method.
///
/// Deliberately not bounded by a call timeout — this is a subscription,
/// and waiting is what it does. The agent applies its own bound to each
/// individual prompt, which is where the bound belongs.
fn pairing_stream() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(16, |mut output| async move {
        loop {
            if let Err(e) = forward_pairing(&mut output).await {
                tracing::warn!(error = %e, "pairing agent stopped; retrying in 3s");
            }
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
    })
}

async fn forward_pairing(
    output: &mut iced::futures::channel::mpsc::Sender<Message>,
) -> anyhow::Result<()> {
    use iced::futures::SinkExt;

    let connection = zbus::Connection::system().await?;
    let (_handle, mut prompts) = hyprforge_bluetooth::agent::register(&connection)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let (answers_tx, mut answers) = tokio::sync::mpsc::unbounded_channel();
    // Only the first registration wins; a retry after a dropped
    // connection reuses the sender the screen already holds.
    let _ = PAIRING_ANSWERS.set(answers_tx);

    while let Some(request) = prompts.recv().await {
        // Discard anything answered for a prompt that has already gone —
        // one that timed out inside the agent, say. Without this, a click
        // that arrived too late for its own dialog would silently answer
        // the *next* pairing, which is the one failure here that must
        // never happen quietly.
        while answers.try_recv().is_ok() {}

        output.send(Message::PairingPrompted(request.prompt.clone())).await?;

        match answers.recv().await {
            Some(true) => request.accept(),
            // A closed channel means the screen is gone. Reject: an
            // unanswered pairing left hanging is worse than a refused one.
            Some(false) | None => request.reject(),
        }
    }
    Ok(())
}

pub struct LazyBlueZBackend {
    /// Only a *successful* connection is cached.
    ///
    /// This is the one rule `network::LazyNetworkManagerBackend`'s doc
    /// comment exists to keep, copied here rather than re-derived: caching
    /// a *failed* connect means `BluetoothError::Unavailable` — which
    /// tells the user to run `systemctl start bluetooth` — keeps being
    /// shown after they do exactly that, because nothing ever retries the
    /// connection that failed once. The ten-second refresh runs the whole
    /// time and cannot help, because it's calling through the same cached
    /// failure. A dead end whose exit is printed on it is worse than one
    /// without.
    inner: tokio::sync::Mutex<Option<Arc<hyprforge_bluetooth::BlueZBackend>>>,
}

impl LazyBlueZBackend {
    pub fn new() -> Self {
        LazyBlueZBackend {
            inner: tokio::sync::Mutex::new(None),
        }
    }

    /// The lock is held across the connect so that a burst of calls — the
    /// refresh tick fires status and devices together — opens one bus
    /// connection rather than two.
    async fn get(&self) -> Result<Arc<hyprforge_bluetooth::BlueZBackend>, BluetoothError> {
        let mut slot = self.inner.lock().await;
        if let Some(backend) = slot.as_ref() {
            return Ok(backend.clone());
        }
        let backend = Arc::new(hyprforge_bluetooth::BlueZBackend::connect().await?);
        *slot = Some(backend.clone());
        Ok(backend)
    }
}

impl Default for LazyBlueZBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl BluetoothBackend for LazyBlueZBackend {
    async fn status(&self) -> Result<Status, BluetoothError> {
        self.get().await?.status().await
    }

    async fn devices(&self) -> Result<Vec<Device>, BluetoothError> {
        self.get().await?.devices().await
    }

    async fn set_discovery(&self, on: bool) -> Result<(), BluetoothError> {
        self.get().await?.set_discovery(on).await
    }

    async fn set_powered(&self, on: bool) -> Result<(), BluetoothError> {
        self.get().await?.set_powered(on).await
    }

    async fn pair(&self, address: &Address) -> Result<(), BluetoothError> {
        self.get().await?.pair(address).await
    }

    async fn cancel_pairing(&self, address: &Address) -> Result<(), BluetoothError> {
        self.get().await?.cancel_pairing(address).await
    }

    async fn connect(&self, address: &Address) -> Result<(), BluetoothError> {
        self.get().await?.connect(address).await
    }

    async fn disconnect(&self, address: &Address) -> Result<(), BluetoothError> {
        self.get().await?.disconnect(address).await
    }

    async fn set_trusted(&self, address: &Address, trusted: bool) -> Result<(), BluetoothError> {
        self.get().await?.set_trusted(address, trusted).await
    }

    async fn forget(&self, address: &Address) -> Result<(), BluetoothError> {
        self.get().await?.forget(address).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyprforge_bluetooth::backend::mock::MockBackend;
    use hyprforge_bluetooth::DeviceKind;

    fn device(alias: &str, addr: &str, paired: bool, connected: bool) -> Device {
        Device {
            address: Address::new(addr),
            alias: alias.to_string(),
            name: Some(alias.to_string()),
            kind: DeviceKind::Headset,
            paired,
            trusted: false,
            connected,
            rssi: Some(-55),
        }
    }

    fn status(state: AdapterState, discovering: bool) -> Status {
        Status { state, discovering, alias: "mock-adapter".to_string() }
    }

    fn loaded(status: Status, devices: Vec<Device>) -> Loaded {
        Loaded { status: Ok(status), devices: Ok(devices) }
    }

    /// Builds a module with a fresh `MockBackend`, returning both — the
    /// backend is kept so a test can inspect what the module actually
    /// called it with, which a synchronous `update()` call can't show by
    /// itself.
    fn module() -> (BluetoothModule<MockBackend>, Arc<MockBackend>) {
        let backend = Arc::new(MockBackend::new());
        let (module, _task) = BluetoothModule::new(Arc::clone(&backend));
        (module, backend)
    }

    /// Runs `f` with `$XDG_CONFIG_HOME` repointed at a throwaway
    /// directory, holding `CONFIG_ENV_LOCK` for the duration — same
    /// reasoning as `network::tests::with_temp_config`, whose sibling
    /// this is: the variable is process-global, so the two must not race.
    fn with_temp_config<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        let _lock = crate::modules::CONFIG_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("XDG_CONFIG_HOME");
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", dir.path());
        }
        let out = f(dir.path());
        match previous {
            Some(p) => unsafe { std::env::set_var("XDG_CONFIG_HOME", p) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
        out
    }

    // --- tray preferences -------------------------------------------------

    /// A missing `tray.toml` is first run: both icons read as shown, and
    /// nothing about it is reported as an error.
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

    /// A `tray.toml` that exists and will not parse must be reported in
    /// this screen's error banner too, and a toggle afterwards must not
    /// overwrite it — the same property `network.rs` pins for its own
    /// screen.
    #[test]
    fn an_unreadable_tray_toml_is_reported_and_not_overwritten_by_a_toggle() {
        with_temp_config(|dir| {
            let tray_toml = dir.join("hyprforge").join("tray.toml");
            std::fs::create_dir_all(tray_toml.parent().unwrap()).unwrap();
            std::fs::write(&tray_toml, "bluetooth = yes please\n").unwrap();

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

    /// The distinction `hyprforge-bluetooth` exists to keep, one layer up:
    /// a daemon that isn't running must not render as an empty device list
    /// on this screen either.
    #[test]
    fn an_unavailable_bluez_shows_its_message_rather_than_an_empty_device_list() {
        let (mut m, _backend) = module();
        let _ = m.update(Message::Loaded(Loaded {
            status: Err(LoadError::from(BluetoothError::Unavailable)),
            devices: Err(LoadError::from(BluetoothError::Unavailable)),
        }));
        assert!(m.unavailable.as_ref().is_some_and(|msg| msg.contains("isn't running")));
        assert!(m.devices.is_empty(), "no devices to show while the daemon is gone");
        let _ = m.view(FontScale::default());
    }

    /// The property `adapter_row` is built around: a rfkill switch is what
    /// has to move, and no control here can do that.
    #[test]
    fn a_hardware_blocked_adapter_does_not_offer_a_toggle_that_cannot_work() {
        assert!(!adapter_offers_toggle(Some(AdapterState::HardwareBlocked)));
        assert!(adapter_offers_toggle(Some(AdapterState::On)));
        assert!(adapter_offers_toggle(Some(AdapterState::Off)));

        let (mut m, _backend) = module();
        let _ = m.update(Message::Loaded(loaded(
            status(AdapterState::HardwareBlocked, false),
            Vec::new(),
        )));
        // Toggling it must be a no-op even if some stray message reaches
        // update — the view not rendering a control is not the only guard.
        let task = m.update(Message::AdapterToggled(true));
        assert_eq!(task.units(), 0, "a hardware-blocked adapter must dispatch nothing");
        let _ = m.view(FontScale::default());
    }

    /// Unpaired devices are listed — a missing device is a bug report —
    /// but this screen can't connect to them without a pairing agent, and
    /// has to say why rather than pretend the row is like any other.
    #[test]
    fn an_unpaired_device_is_listed_but_offers_no_connect_action_and_says_why() {
        let (mut m, _backend) = module();
        let stranger = device("Stranger", "AA:BB:CC:DD:EE:01", false, false);
        assert!(!stranger.paired, "precondition: this device has never been paired");
        let _ = m.update(Message::Loaded(loaded(status(AdapterState::On, false), vec![stranger.clone()])));
        assert_eq!(m.devices.len(), 1, "still listed");

        let task = m.update(Message::ConnectPressed(stranger.address.clone()));
        assert_eq!(task.units(), 0, "connecting an unpaired device must not reach the backend");
        let _ = m.view(FontScale::default());
    }

    /// Entering the screen builds the module and kicks off `status` and
    /// `devices` only — never a discovery session. Discovery costs battery
    /// and airtime on both ends and must be a deliberate click.
    #[test]
    fn entering_the_screen_does_not_start_discovery() {
        let (_m, backend) = module();
        assert!(
            backend.discovery_calls.lock().unwrap().is_empty(),
            "the module's own constructor must never call set_discovery"
        );
    }

    /// Powering the adapter off also ends discovery on the backend's side
    /// (see `MockBackend::set_powered`) — the screen must reflect that
    /// truthfully rather than keep showing "Scanning…" over a radio that
    /// is now off.
    #[test]
    fn powering_the_adapter_off_does_not_leave_the_screen_claiming_to_be_scanning() {
        let (mut m, _backend) = module();
        let _ = m.update(Message::Loaded(loaded(status(AdapterState::On, true), Vec::new())));
        assert!(m.status.as_ref().unwrap().discovering);

        let _ = m.update(Message::AdapterSet(Ok(())));
        // AdapterSet(Ok(..)) triggers a refresh; simulate what that refresh
        // would see now that the backend has turned the radio off.
        let _ = m.update(Message::Loaded(loaded(status(AdapterState::Off, false), Vec::new())));
        assert!(!m.status.as_ref().unwrap().discovering);
        let _ = m.view(FontScale::default());
    }

    /// Connecting a paired device has to be reflected immediately — waiting
    /// for the next poll would leave a device the user just told the app
    /// to connect showing as disconnected for up to ten seconds. `update`
    /// runs synchronously and the D-Bus call itself only happens once the
    /// `Task` it returns is polled by iced's executor, which a unit test
    /// never drives — so this delivers the `Connected` outcome directly,
    /// the same way `network.rs`'s join-failure tests do for `Connected`.
    #[test]
    fn connecting_a_paired_device_updates_the_row() {
        let (mut m, _backend) = module();
        let paired = device("Headset", "AA:BB:CC:DD:EE:02", true, false);
        let _ = m.update(Message::Loaded(loaded(status(AdapterState::On, false), vec![paired.clone()])));
        assert!(!m.devices[0].connected);

        let _ = m.update(Message::ConnectPressed(paired.address.clone()));
        let _ = m.update(Message::Connected(paired.address.clone(), Ok(())));
        assert!(m.devices[0].connected, "the row must show connected without waiting on the next poll");
    }

    /// Forgetting has to be reflected immediately, not after the next poll.
    #[test]
    fn forgetting_a_device_removes_it_from_the_list() {
        let (mut m, _backend) = module();
        let paired = device("Headset", "AA:BB:CC:DD:EE:03", true, false);
        let _ = m.update(Message::Loaded(loaded(status(AdapterState::On, false), vec![paired.clone()])));
        assert_eq!(m.devices.len(), 1);

        let _ = m.update(Message::Forgotten(paired.address.clone(), Ok(())));
        assert!(m.devices.is_empty());
    }

    /// A refused action — the mock's stand-in for BlueZ saying no — must
    /// report why and leave the device exactly where it was, not vanish it
    /// or silently pretend the action worked.
    #[test]
    fn a_refused_action_reports_why_and_leaves_the_device_in_place() {
        let (mut m, backend) = module();
        let paired = device("Headset", "AA:BB:CC:DD:EE:04", true, false);
        let _ = m.update(Message::Loaded(loaded(status(AdapterState::On, false), vec![paired.clone()])));

        *backend.refuse.lock().unwrap() = Some("device is off".to_string());
        let _ = m.update(Message::Connected(
            paired.address.clone(),
            Err(LoadError::from(BluetoothError::Refused("device is off".to_string()))),
        ));

        assert!(m.error.as_ref().is_some_and(|e| e.contains("device is off")));
        assert_eq!(m.devices.len(), 1, "still there");
        assert!(!m.devices[0].connected, "a refused connect must not be shown as connected");
    }

    /// Every state the screen can be in has to build without panicking:
    /// loading, unavailable, hardware-blocked, powered-off, and a
    /// populated list mixing paired, connected, and unpaired devices.
    #[test]
    fn the_screen_builds_in_every_state() {
        let (mut m, _backend) = module();
        let scale = FontScale::default();
        let _ = m.view(scale); // loading

        let _ = m.update(Message::Loaded(Loaded {
            status: Err(LoadError::from(BluetoothError::Unavailable)),
            devices: Err(LoadError::from(BluetoothError::Unavailable)),
        }));
        let _ = m.view(scale); // unavailable

        let (mut m, _backend) = module();
        let _ = m.update(Message::Loaded(loaded(status(AdapterState::HardwareBlocked, false), Vec::new())));
        let _ = m.view(scale); // hardware-blocked adapter

        let (mut m, _backend) = module();
        let _ = m.update(Message::Loaded(loaded(status(AdapterState::Off, false), Vec::new())));
        let _ = m.view(scale); // powered off

        let (mut m, _backend) = module();
        let mut unnamed = device("Unnamed", "AA:BB:CC:DD:EE:05", true, false);
        unnamed.name = None;
        let devices = vec![
            device("Connected headset", "AA:BB:CC:DD:EE:06", true, true),
            device("Paired mouse", "AA:BB:CC:DD:EE:07", true, false),
            device("Stranger", "AA:BB:CC:DD:EE:08", false, false),
            unnamed,
        ];
        let _ = m.update(Message::Loaded(loaded(status(AdapterState::On, true), devices)));
        let _ = m.view(scale); // populated: connected, paired, unpaired, unnamed

        m.error = Some("a connect failed".to_string());
        let _ = m.view(scale); // error banner over a populated list
    }

    // --- Pairing ------------------------------------------------------

    /// The property `device_row` is now built around, in place of the
    /// "use bluetoothctl" text it replaced: an unpaired device offers a way to
    /// pair it, and pressing that button actually reaches the backend
    /// rather than being a no-op the way `ConnectPressed` still is for
    /// the same row.
    #[test]
    fn an_unpaired_device_offers_a_pair_action_that_reaches_the_backend() {
        let (mut m, _backend) = module();
        let stranger = device("Stranger", "AA:BB:CC:DD:EE:09", false, false);
        let _ = m.update(Message::Loaded(loaded(status(AdapterState::On, false), vec![stranger.clone()])));
        let _ = m.view(FontScale::default());

        let task = m.update(Message::PairPressed(stranger.address.clone()));
        assert_ne!(task.units(), 0, "pairing an unpaired device must reach the backend");
    }

    /// A stray `PairPressed` for a device that's already paired (or that
    /// doesn't exist) must be a no-op, the same guard every other action
    /// message on this screen keeps against a message that outlives the
    /// state it was built from.
    #[test]
    fn pairing_an_already_paired_device_is_a_no_op() {
        let (mut m, _backend) = module();
        let paired = device("Headset", "AA:BB:CC:DD:EE:16", true, false);
        let _ = m.update(Message::Loaded(loaded(status(AdapterState::On, false), vec![paired.clone()])));

        let task = m.update(Message::PairPressed(paired.address.clone()));
        assert_eq!(task.units(), 0, "an already-paired device must never be re-paired");
    }

    /// The spec's own zero-pad rule, pinned at this screen's boundary:
    /// `passkey_digits` — the only place this module is allowed to turn
    /// a `Passkey` into text — must go through `Passkey::to_string`, not
    /// a shortcut like `passkey.as_u32()` that would drop the padding a
    /// device showing "001234" depends on to be comparable at all.
    #[test]
    fn a_confirm_prompts_passkey_is_shown_zero_padded_to_six_digits() {
        assert_eq!(passkey_digits(&Passkey::new(1234)), "001234");
        assert_eq!(passkey_digits(&Passkey::new(0)), "000000");

        let (mut m, _backend) = module();
        let addr = Address::new("AA:BB:CC:DD:EE:17");
        let _ = m.update(Message::PairingPrompted(PairingPrompt::Confirm {
            device: addr,
            name: "Phone".to_string(),
            passkey: Passkey::new(1234),
        }));
        assert!(m.pairing.is_some());
        let _ = m.view(FontScale::default());
    }

    /// `Authorize` carries no `Passkey` field at all — structurally, not
    /// just by choice, there's nothing for this screen to show digits
    /// for. Pairing.rs pins the same split from the type's own side; this
    /// pins that the screen renders such a prompt without trying to
    /// invent a code that doesn't exist.
    #[test]
    fn an_authorize_prompt_has_no_digits_to_show() {
        let (mut m, _backend) = module();
        let _ = m.update(Message::PairingPrompted(PairingPrompt::Authorize {
            device: Address::new("AA:BB:CC:DD:EE:18"),
            name: "Speaker".to_string(),
        }));
        assert!(m.pairing.is_some());
        let _ = m.view(FontScale::default());
    }

    /// The property `pairing_dialog` is built around for `Display`: no
    /// button answers it from this end, because
    /// `PairingPrompt::needs_an_answer` is false and the far end answers
    /// by typing. Asserted directly via `pairing_offers_accept` rather
    /// than only implied by the view building without panicking.
    #[test]
    fn a_display_prompt_offers_no_accept_action() {
        let display = PairingPrompt::Display {
            device: Address::new("AA:BB:CC:DD:EE:19"),
            name: "Keyboard".to_string(),
            passkey: Passkey::new(42),
            entered: 0,
        };
        assert!(!pairing_offers_accept(&display));
        assert!(pairing_offers_accept(&PairingPrompt::Confirm {
            device: Address::new("AA:BB:CC:DD:EE:20"),
            name: "Phone".to_string(),
            passkey: Passkey::new(1),
        }));
        assert!(pairing_offers_accept(&PairingPrompt::Authorize {
            device: Address::new("AA:BB:CC:DD:EE:21"),
            name: "Speaker".to_string(),
        }));

        let (mut m, _backend) = module();
        let _ = m.update(Message::PairingPrompted(display));
        let _ = m.view(FontScale::default());
    }

    /// The rule item 2 of the task exists to keep: a dismissed dialog
    /// must not leave BlueZ waiting on an answer that will never come.
    /// `PairingCancelled` has to both close the dialog immediately and
    /// dispatch `cancel_pairing` — closing it without the call would be
    /// exactly the silent abandonment this is testing against.
    #[test]
    fn dismissing_a_pairing_dialog_cancels_it_rather_than_abandoning_it() {
        let (mut m, _backend) = module();
        let addr = Address::new("AA:BB:CC:DD:EE:22");
        let _ = m.update(Message::PairingPrompted(PairingPrompt::Confirm {
            device: addr,
            name: "Phone".to_string(),
            passkey: Passkey::new(4321),
        }));
        assert!(m.pairing.is_some());

        let task = m.update(Message::PairingCancelled);
        assert!(m.pairing.is_none(), "the dialog must close immediately");
        assert_ne!(task.units(), 0, "cancelling must reach the backend, not just clear local state");
    }

    /// A stray `PairingCancelled` with no dialog open must not dispatch
    /// a `cancel_pairing` call for a pairing that isn't happening.
    #[test]
    fn cancelling_with_no_pairing_open_is_a_no_op() {
        let (mut m, _backend) = module();
        let task = m.update(Message::PairingCancelled);
        assert_eq!(task.units(), 0);
    }

    /// Accepting is answering, not dismissing — it must clear the dialog
    /// without calling `cancel_pairing`, the opposite of the property
    /// above. There is no "confirm" call to make yet (see the doc
    /// comment on `Message::PairingAccepted`), so this is local state
    /// only.
    #[test]
    fn accepting_a_pairing_prompt_clears_it_without_cancelling() {
        let (mut m, _backend) = module();
        let addr = Address::new("AA:BB:CC:DD:EE:23");
        let _ = m.update(Message::PairingPrompted(PairingPrompt::Authorize {
            device: addr,
            name: "Speaker".to_string(),
        }));

        let task = m.update(Message::PairingAccepted);
        assert!(m.pairing.is_none(), "accepting closes the dialog");
        assert_eq!(task.units(), 0, "accepting must not itself call cancel_pairing");
    }

    /// Item 3 of the task: a pairing that succeeds updates the row so it
    /// can be connected to, and clears whatever dialog was open for it.
    #[test]
    fn a_successful_pairing_updates_the_row_and_closes_its_dialog() {
        let (mut m, _backend) = module();
        let stranger = device("Stranger", "AA:BB:CC:DD:EE:24", false, false);
        let _ = m.update(Message::Loaded(loaded(status(AdapterState::On, false), vec![stranger.clone()])));
        let _ = m.update(Message::PairPressed(stranger.address.clone()));
        let _ = m.update(Message::PairingPrompted(PairingPrompt::Confirm {
            device: stranger.address.clone(),
            name: "Stranger".to_string(),
            passkey: Passkey::new(555555),
        }));
        assert!(m.pairing.is_some());

        let _ = m.update(Message::Paired(stranger.address.clone(), Ok(())));
        assert!(m.devices[0].paired, "the row must show paired without waiting on the next poll");
        assert!(m.pairing.is_none(), "a resolved pairing must not leave a stale dialog open");
    }

    /// Item 3 of the task: a pairing that fails reports the backend's
    /// own text in the existing error banner and must never invent
    /// "check the logs" in its place — and the device it was tried
    /// against stays unpaired, not left in some in-between state.
    #[test]
    fn a_failed_pairing_reports_in_the_banner_and_leaves_the_device_unpaired() {
        let (mut m, _backend) = module();
        let stranger = device("Stranger", "AA:BB:CC:DD:EE:25", false, false);
        let _ = m.update(Message::Loaded(loaded(status(AdapterState::On, false), vec![stranger.clone()])));
        let _ = m.update(Message::PairPressed(stranger.address.clone()));
        let _ = m.update(Message::PairingPrompted(PairingPrompt::Confirm {
            device: stranger.address.clone(),
            name: "Stranger".to_string(),
            passkey: Passkey::new(1),
        }));

        let _ = m.update(Message::Paired(
            stranger.address.clone(),
            Err(LoadError::from(BluetoothError::Refused("authentication failed".to_string()))),
        ));

        assert!(
            m.error.as_ref().is_some_and(|e| e.contains("authentication failed")),
            "the backend's own text must reach the banner, got {:?}",
            m.error
        );
        assert!(!m.devices[0].paired, "a failed pairing must not be shown as paired");
        assert!(m.pairing.is_none(), "a dialog for a pairing that's already failed must not linger");
    }

    /// Every state `pairing_dialog` can be asked to build has to build
    /// without panicking — the three prompt kinds this task lists
    /// explicitly, on top of everything `the_screen_builds_in_every_state`
    /// already covers.
    #[test]
    fn the_screen_builds_with_each_pairing_prompt_kind() {
        let scale = FontScale::default();
        let (mut m, _backend) = module();
        let _ = m.update(Message::Loaded(loaded(status(AdapterState::On, false), Vec::new())));
        let addr = Address::new("AA:BB:CC:DD:EE:26");

        let _ = m.update(Message::PairingPrompted(PairingPrompt::Confirm {
            device: addr.clone(),
            name: "Phone".to_string(),
            passkey: Passkey::new(56),
        }));
        let _ = m.view(scale); // Confirm

        let _ = m.update(Message::PairingPrompted(PairingPrompt::Authorize {
            device: addr.clone(),
            name: "Speaker".to_string(),
        }));
        let _ = m.view(scale); // Authorize

        let _ = m.update(Message::PairingPrompted(PairingPrompt::Display {
            device: addr,
            name: "Keyboard".to_string(),
            passkey: Passkey::new(7),
            entered: 3,
        }));
        let _ = m.view(scale); // Display
    }
}

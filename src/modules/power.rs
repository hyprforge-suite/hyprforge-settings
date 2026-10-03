//! Keep awake, battery, and power profile — over `hyprforge-power`'s three
//! independent backends (`systemd-logind`, UPower, `power-profiles-daemon`).
//!
//! Same shape as `modules::network` and `modules::bluetooth`: everything
//! that talks to the daemons already lives in `hyprforge-power`, this
//! module is only the screen on top of it, and it is generic over the
//! three backend traits so its tests can drive mocks while `main.rs`
//! drives the real ones.
//!
//! Unlike Network and Bluetooth, this screen is not one daemon behind one
//! "unavailable" dead end — it is *three* independent daemons, and
//! `hyprforge-power`'s own doc comments are explicit that one can be down
//! while the others answer fine. So each of the three sections below
//! carries its own [`InhibitDisplayState`] / [`BatteryDisplayState`] /
//! [`ProfileDisplayState`], and a daemon being unreachable only collapses
//! its own section, never the whole screen.

use crate::module::SettingsModule;
use hyprforge_power::backend::{BatteryBackend, InhibitBackend, PowerProfilesBackend};
use hyprforge_power::{
    BatteryError, BatteryInfo, BatteryState, InhibitError, InhibitorInfo, PowerProfile,
    ProfileError, WhatSet,
};
use hyprforge_ui::theme::{self, spacing, FontScale, BASE_TEXT_SIZE};
use hyprforge_ui::widgets::{
    chip, config_line, hero_card, hint_text, meta_text, scaled_text, secondary_button,
    section_label, setting_list, setting_row, toggle, Tint,
};
use iced::widget::{column, row, Space};
use iced::{Alignment, Element, Length, Subscription, Task};
use std::sync::Arc;
use std::time::Duration;

/// How often the screen re-polls all three daemons while it's open, so a
/// profile switched from a shortcut, or a battery percentage ticking
/// down, shows up without a manual refresh. Same interval Network and
/// Bluetooth poll at.
const POLL_INTERVAL: Duration = Duration::from_secs(10);

/// The `who` this screen's own inhibit is filed under — the string
/// `Inhibit`'s caller passes, echoed straight back by
/// `ListInhibitors`. Needed so the "other processes" list below the
/// toggle can filter this screen's own hold out of "everyone else",
/// the same way `hyprforge-trayd`'s `KEEP_AWAKE_WHO` does for its own
/// tray icon (see `other_inhibitors` in `trayd.rs`) — a different string
/// from the tray's, because this and the tray can each hold their own
/// inhibit independently and neither should hide the other's from the
/// user.
const KEEP_AWAKE_WHO: &str = "hyprforge-settings";
const KEEP_AWAKE_WHY: &str = "Keep awake toggle in Settings";

/// A load-time failure, reduced to what the screen needs: text to show,
/// and whether it's the one kind of failure (`Unavailable`, on all three
/// of `InhibitError`/`BatteryError`/`ProfileError`) that must never be
/// confused with a normal reading — an empty inhibitor list, a desktop
/// with no battery, or a specific active profile.
///
/// None of the three source error types is `Clone` — this is the
/// boundary where a borrowed error becomes an owned value a `Message`
/// can carry, same as `network::LoadError` and `bluetooth::LoadError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError {
    message: String,
    unavailable: bool,
}

impl From<InhibitError> for LoadError {
    fn from(e: InhibitError) -> Self {
        LoadError { unavailable: matches!(e, InhibitError::Unavailable), message: e.to_string() }
    }
}

impl From<BatteryError> for LoadError {
    fn from(e: BatteryError) -> Self {
        LoadError { unavailable: matches!(e, BatteryError::Unavailable), message: e.to_string() }
    }
}

impl From<ProfileError> for LoadError {
    fn from(e: ProfileError) -> Self {
        LoadError { unavailable: matches!(e, ProfileError::Unavailable), message: e.to_string() }
    }
}

/// What a successful `InhibitBackend` round trip found.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InhibitLoaded {
    held: Option<WhatSet>,
    /// Already filtered to exclude this screen's own hold — see
    /// `KEEP_AWAKE_WHO`.
    others: Vec<InhibitorInfo>,
}

/// What a successful `PowerProfilesBackend` round trip found.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProfilesLoaded {
    available: Vec<PowerProfile>,
    active: PowerProfile,
}

/// The result of one refresh: three independent calls, three independent
/// outcomes. A failure reading the battery must not discard a successful
/// profile read, and vice versa — the whole reason this screen keeps
/// three separate `Result`s instead of one.
#[derive(Debug, Clone)]
pub struct Loaded {
    inhibit: Result<InhibitLoaded, LoadError>,
    battery: Result<Option<BatteryInfo>, LoadError>,
    profiles: Result<ProfilesLoaded, LoadError>,
}

async fn load_inhibit<I: InhibitBackend + ?Sized>(backend: &I) -> Result<InhibitLoaded, LoadError> {
    let held = backend.held().await?;
    let all = backend.list_inhibitors().await?;
    // Neither this screen's own lock nor keep awake's holder is "someone
    // else": the holder is what this toggle starts, wherever it runs.
    let others = all
        .into_iter()
        .filter(|i| i.who != KEEP_AWAKE_WHO && i.who != hyprforge_power::keep_awake::HOLDER_WHO)
        .collect();
    Ok(InhibitLoaded { held, others })
}

async fn load_profiles<P: PowerProfilesBackend + ?Sized>(
    backend: &P,
) -> Result<ProfilesLoaded, LoadError> {
    let available = backend.profiles().await?;
    let active = backend.active_profile().await?;
    Ok(ProfilesLoaded { available, active })
}

async fn load<I, Bat, P>(inhibit: Arc<I>, battery: Arc<Bat>, profiles: Arc<P>) -> Loaded
where
    I: InhibitBackend,
    Bat: BatteryBackend,
    P: PowerProfilesBackend,
{
    Loaded {
        inhibit: load_inhibit(&*inhibit).await,
        battery: battery.battery().await.map_err(LoadError::from),
        profiles: load_profiles(&*profiles).await,
    }
}

/// The keep-awake section's state. `Unknown` only before the very first
/// load lands.
#[derive(Debug, Clone, PartialEq, Eq)]
enum InhibitDisplayState {
    Unknown,
    Unavailable(String),
    Ready { held: Option<WhatSet>, others: Vec<InhibitorInfo> },
}

/// The battery section's state.
///
/// `Unavailable` and `NoBattery` are kept as two different variants,
/// deliberately never collapsed into one — `hyprforge-power`'s own docs
/// call this out as load-bearing: a desktop with no battery is a normal
/// machine, and a UPower that isn't answering is a problem, and this
/// screen must not draw them the same way.
#[derive(Debug, Clone, PartialEq)]
enum BatteryDisplayState {
    Unknown,
    Unavailable(String),
    NoBattery,
    Present(BatteryInfo),
}

/// The power profile section's state.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProfileDisplayState {
    Unknown,
    Unavailable(String),
    Ready { available: Vec<PowerProfile>, active: PowerProfile },
}

#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    Loaded(Loaded),
    /// The keep-awake checkbox. `true` takes the inhibit, `false`
    /// releases it — both idempotent on the backend side (see
    /// `InhibitBackend::take`/`release`'s own doc comments), so a stray
    /// duplicate here is harmless.
    KeepAwakeToggled(bool),
    KeepAwakeSet(Result<(), LoadError>),
    /// A profile button pressed. The one message on this screen that
    /// writes — see the module doc comment on being careful with it.
    ProfileSelected(PowerProfile),
    /// Carries the profile that was requested alongside the outcome, the
    /// same way `bluetooth::Message::Trusted` carries the value it asked
    /// for: the row needs to know what it asked for to show the outcome
    /// without waiting on the next poll to re-read it.
    ProfileSet(PowerProfile, Result<(), LoadError>),
}

pub struct PowerModule<I, Bat, P>
where
    I: InhibitBackend + 'static,
    Bat: BatteryBackend + 'static,
    P: PowerProfilesBackend + 'static,
{
    inhibit_backend: Arc<I>,
    battery_backend: Arc<Bat>,
    profiles_backend: Arc<P>,
    inhibit: InhibitDisplayState,
    battery: BatteryDisplayState,
    profiles: ProfileDisplayState,
    /// Anything else that went wrong: a toggle that failed, a profile
    /// switch that was refused. Cleared on the next attempted action, not
    /// on every refresh, so it doesn't flash away before it's been read —
    /// same reasoning as `bluetooth::BluetoothModule::error`.
    error: Option<String>,
}

impl<I, Bat, P> PowerModule<I, Bat, P>
where
    I: InhibitBackend + 'static,
    Bat: BatteryBackend + 'static,
    P: PowerProfilesBackend + 'static,
{
    pub fn new(
        inhibit_backend: Arc<I>,
        battery_backend: Arc<Bat>,
        profiles_backend: Arc<P>,
    ) -> (Self, Task<Message>) {
        let task = Task::perform(
            load(
                Arc::clone(&inhibit_backend),
                Arc::clone(&battery_backend),
                Arc::clone(&profiles_backend),
            ),
            Message::Loaded,
        );
        let module = PowerModule {
            inhibit_backend,
            battery_backend,
            profiles_backend,
            inhibit: InhibitDisplayState::Unknown,
            battery: BatteryDisplayState::Unknown,
            profiles: ProfileDisplayState::Unknown,
            error: None,
        };
        (module, task)
    }

    fn refresh_task(&self) -> Task<Message> {
        Task::perform(
            load(
                Arc::clone(&self.inhibit_backend),
                Arc::clone(&self.battery_backend),
                Arc::clone(&self.profiles_backend),
            ),
            Message::Loaded,
        )
    }
}

impl<I, Bat, P> SettingsModule for PowerModule<I, Bat, P>
where
    I: InhibitBackend + 'static,
    Bat: BatteryBackend + 'static,
    P: PowerProfilesBackend + 'static,
{
    type Message = Message;

    fn subtitle(&self) -> Option<String> {
        Some("UPower · power-profiles-daemon".into())
    }

    fn header_actions(&self, _scale: FontScale) -> Option<Element<'_, Message>> {
        Some(secondary_button("Refresh").on_press(Message::Refresh).into())
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Refresh => self.refresh_task(),
            Message::Loaded(loaded) => {
                // Each of the three results updates only its own section.
                // A daemon that failed with something other than
                // `Unavailable` (a `Refused`/`TimedOut`) leaves that
                // section's last-known state on screen and surfaces the
                // message in the shared error banner instead — the same
                // "don't discard a good reading over a transient hiccup"
                // choice `bluetooth.rs` and `network.rs` make for their
                // own single daemon.
                match loaded.inhibit {
                    Ok(i) => {
                        self.inhibit = InhibitDisplayState::Ready { held: i.held, others: i.others }
                    }
                    Err(e) if e.unavailable => self.inhibit = InhibitDisplayState::Unavailable(e.message),
                    Err(e) => self.error = Some(e.message),
                }
                match loaded.battery {
                    Ok(Some(info)) => self.battery = BatteryDisplayState::Present(info),
                    Ok(None) => self.battery = BatteryDisplayState::NoBattery,
                    Err(e) if e.unavailable => self.battery = BatteryDisplayState::Unavailable(e.message),
                    Err(e) => self.error = Some(e.message),
                }
                match loaded.profiles {
                    Ok(p) => {
                        self.profiles =
                            ProfileDisplayState::Ready { available: p.available, active: p.active }
                    }
                    Err(e) if e.unavailable => self.profiles = ProfileDisplayState::Unavailable(e.message),
                    Err(e) => self.error = Some(e.message),
                }
                Task::none()
            }
            Message::KeepAwakeToggled(on) => {
                // The view renders no checkbox unless this is
                // `InhibitDisplayState::Ready` — guarded again here so a
                // stray message can't reach the backend from a state
                // where there's nothing sensible to toggle.
                if !matches!(self.inhibit, InhibitDisplayState::Ready { .. }) {
                    return Task::none();
                }
                self.error = None;
                let backend = Arc::clone(&self.inhibit_backend);
                if on {
                    Task::perform(
                        async move {
                            backend
                                .take(WhatSet::keep_awake(), KEEP_AWAKE_WHO, KEEP_AWAKE_WHY)
                                .await
                                .map_err(LoadError::from)
                        },
                        Message::KeepAwakeSet,
                    )
                } else {
                    Task::perform(
                        async move { backend.release().await.map_err(LoadError::from) },
                        Message::KeepAwakeSet,
                    )
                }
            }
            Message::KeepAwakeSet(Ok(())) => self.refresh_task(),
            Message::KeepAwakeSet(Err(e)) => {
                self.error = Some(e.message);
                Task::none()
            }
            Message::ProfileSelected(profile) => {
                // Only reachable for a profile the daemon actually
                // offered, and only when it isn't already the active one
                // — the view only ever presses a button for an offered
                // profile, but a stray message must not reach the
                // backend with a redundant write either.
                let ProfileDisplayState::Ready { available, active } = &self.profiles else {
                    return Task::none();
                };
                if !available.contains(&profile) || *active == profile {
                    return Task::none();
                }
                self.error = None;
                let backend = Arc::clone(&self.profiles_backend);
                Task::perform(
                    async move { backend.set_active_profile(profile).await.map_err(LoadError::from) },
                    move |result| Message::ProfileSet(profile, result),
                )
            }
            Message::ProfileSet(profile, Ok(())) => {
                // Reflected immediately rather than waiting on the next
                // poll — same reasoning as `bluetooth::Message::Connected`.
                if let ProfileDisplayState::Ready { active, .. } = &mut self.profiles {
                    *active = profile;
                }
                self.refresh_task()
            }
            Message::ProfileSet(_, Err(e)) => {
                self.error = Some(e.message);
                Task::none()
            }
        }
    }

    fn view(&self, scale: FontScale) -> Element<'_, Message> {
        let mut content = column![].spacing(spacing::LG);

        if let Some(msg) = &self.error {
            content = content.push(scaled_text(msg.clone(), 13.0, scale).color(theme::warning()));
        }

        // The battery leads, as in the mockup: it is the thing this page
        // is about for most people who open it.
        content = content.push(self.battery_section(scale));
        content = content.push(self.profile_section(scale));
        content = content.push(self.keep_awake_section(scale));

        // Padded like every other page, so its first section sits a
        // gap below the title the shell draws, not flush against it.
        iced::widget::container(content).padding(spacing::LG).width(iced::Length::Fill).into()
    }

    fn subscription(&self) -> Subscription<Message> {
        iced::time::every(POLL_INTERVAL).map(|_| Message::Refresh)
    }
}

impl<I, Bat, P> PowerModule<I, Bat, P>
where
    I: InhibitBackend + 'static,
    Bat: BatteryBackend + 'static,
    P: PowerProfilesBackend + 'static,
{
    /// The keep-awake toggle, and — the tray's own reason for surfacing
    /// this data — who else besides this screen is currently preventing
    /// sleep.
    fn keep_awake_section(&self, scale: FontScale) -> Element<'_, Message> {
        let rows: Vec<Element<'_, Message>> = match &self.inhibit {
            InhibitDisplayState::Unknown => {
                vec![setting_row(0, "Keep this machine awake", Some(hint_text("Loading…", scale).into()), Space::new(), scale)]
            }
            InhibitDisplayState::Unavailable(msg) => vec![setting_row(
                0,
                "Keep this machine awake",
                Some(hint_text(msg.clone(), scale).color(theme::warning()).into()),
                Space::new(),
                scale,
            )],
            InhibitDisplayState::Ready { held, others } => {
                let summary = if others.is_empty() {
                    "Nothing else is preventing sleep.".to_string()
                } else {
                    let plural = if others.len() == 1 { "" } else { "es" };
                    format!("{} other process{plural} also preventing sleep", others.len())
                };
                let mut rows = vec![setting_row(
                    0,
                    "Keep this machine awake",
                    Some(hint_text(summary, scale).into()),
                    toggle(held.is_some(), scale).on_toggle(Message::KeepAwakeToggled),
                    scale,
                )];
                // Each other holder as a row of its own, so "who" and
                // "why" read as a pair rather than as one long line.
                for (i, other) in others.iter().enumerate() {
                    rows.push(setting_row(
                        i + 1,
                        other.who.clone(),
                        Some(hint_text(other.why.clone(), scale).into()),
                        chip("holding", Tint::Dim, scale),
                        scale,
                    ));
                }
                rows
            }
        };
        group("Keep awake", rows, scale)
    }

    /// The battery, leading the page — the mockup's hero card.
    ///
    /// Tinted by what the battery is doing: the success colour while it
    /// charges or is full, the warning colour once it is low, and no
    /// state colour otherwise, because a battery discharging normally is
    /// not a state worth a colour.
    fn battery_section(&self, scale: FontScale) -> Element<'_, Message> {
        match &self.battery {
            BatteryDisplayState::Unknown => meta_text("Loading…", BASE_TEXT_SIZE, scale).into(),
            BatteryDisplayState::Unavailable(msg) => {
                scaled_text(msg.clone(), BASE_TEXT_SIZE, scale).color(theme::warning()).into()
            }
            BatteryDisplayState::NoBattery => {
                meta_text("This machine has no battery.", BASE_TEXT_SIZE, scale).into()
            }
            BatteryDisplayState::Present(info) => {
                let tint = battery_tint(info.state, info.is_low());
                let mut detail = battery_state_label(info.state).to_string();
                if let Some(words) = info.time_remaining_words() {
                    detail = format!("{detail} \u{b7} {words}");
                }
                let mark = hyprforge_ui::glyph::battery(
                    info.percentage as f32 / 100.0,
                    scale.apply(28.0),
                    tint.iced(),
                    tint.iced(),
                );
                hero_card(
                    tint,
                    row![
                        mark,
                        column![
                            scaled_text(format!("{}%", info.percentage), 22.0, scale)
                                .font(iced::Font { weight: iced::font::Weight::Semibold, ..iced::Font::DEFAULT })
                                .color(theme::text()),
                            config_line(detail, scale),
                        ]
                        .spacing(2.0),
                    ]
                    .spacing(spacing::LG)
                    .align_y(Alignment::Center),
                )
                .into()
            }
        }
    }

    /// The one section on this screen that writes: picking a profile
    /// calls `set_active_profile` immediately, so this offers no
    /// confirmation step of its own — the same one-click convention
    /// Network's radio toggle and Bluetooth's adapter toggle already use
    /// for a setting the daemon itself lets you flip straight back.
    ///
    /// Three cards side by side, the mockup's profile picker: the active
    /// one outlined in the accent, because it is the selection.
    fn profile_section(&self, scale: FontScale) -> Element<'_, Message> {
        let body: Element<'_, Message> = match &self.profiles {
            ProfileDisplayState::Unknown => meta_text("Loading…", BASE_TEXT_SIZE, scale).into(),
            ProfileDisplayState::Unavailable(msg) => {
                scaled_text(msg.clone(), BASE_TEXT_SIZE, scale).color(theme::warning()).into()
            }
            ProfileDisplayState::Ready { available, active } => {
                let mut cards = row![].spacing(spacing::SM);
                for profile in available {
                    let selected = profile == active;
                    cards = cards.push(
                        iced::widget::button(
                            column![
                                scaled_text(profile_label(*profile), 14.0, scale)
                                    .font(iced::Font { weight: iced::font::Weight::Semibold, ..iced::Font::DEFAULT })
                                    .color(theme::text()),
                                hint_text(profile_hint(*profile), scale),
                            ]
                            .spacing(2.0),
                        )
                        .width(Length::Fill)
                        .padding(spacing::MD)
                        .on_press(Message::ProfileSelected(*profile))
                        .style(move |t: &iced::Theme, status| profile_card_style(t, status, selected)),
                    );
                }
                cards.into()
            }
        };
        column![section_label("Power profile", scale), body].spacing(spacing::SM).into()
    }
}

/// A section label over a stack of striped rows — this page's groups.
fn group<'a>(label: &str, rows: Vec<Element<'a, Message>>, scale: FontScale) -> Element<'a, Message> {
    column![section_label(label, scale), setting_list(rows)].spacing(spacing::SM).into()
}

/// The battery card's colour — see `battery_section`.
fn battery_tint(state: BatteryState, low: bool) -> Tint {
    match (state, low) {
        (_, true) => Tint::Warning,
        (BatteryState::Charging | BatteryState::FullyCharged | BatteryState::PendingCharge, _) => {
            Tint::Success
        }
        _ => Tint::Dim,
    }
}

/// What each profile does, in a line — what power-profiles-daemon itself
/// changes, not a promise about refresh rates or blur it does not make.
fn profile_hint(profile: PowerProfile) -> &'static str {
    match profile {
        PowerProfile::PowerSaver => "Slower, for longer on battery",
        PowerProfile::Balanced => "The usual default",
        PowerProfile::Performance => "Faster, uses more power",
    }
}

/// A profile card: the stripe fill when idle, the accent outline and a
/// faint accent fill when it is the active profile, and the row step on
/// hover — never the accent on hover, which would look selected.
fn profile_card_style(t: &iced::Theme, status: iced::widget::button::Status, selected: bool) -> iced::widget::button::Style {
    let accent = t.extended_palette().primary.base.color;
    let hovered = matches!(status, iced::widget::button::Status::Hovered);
    let background = match (selected, hovered) {
        (true, _) => iced::Color { a: 0.14, ..accent },
        (false, true) => hyprforge_ui::theme::surface::row(),
        (false, false) => hyprforge_ui::theme::row_tint(),
    };
    iced::widget::button::Style {
        background: Some(iced::Background::Color(background)),
        text_color: theme::text(),
        border: iced::Border {
            radius: hyprforge_ui::density::card_radius().into(),
            width: if selected { 1.5 } else { 0.0 },
            color: if selected { accent } else { iced::Color::TRANSPARENT },
        },
        ..iced::widget::button::Style::default()
    }
}

fn profile_label(profile: PowerProfile) -> &'static str {
    match profile {
        PowerProfile::PowerSaver => "Power saver",
        PowerProfile::Balanced => "Balanced",
        PowerProfile::Performance => "Performance",
    }
}

fn battery_state_label(state: BatteryState) -> &'static str {
    match state {
        BatteryState::Charging => "Charging",
        BatteryState::Discharging => "Discharging",
        BatteryState::Empty => "Empty",
        BatteryState::FullyCharged => "Fully charged",
        BatteryState::PendingCharge => "Plugged in, preparing to charge",
        BatteryState::PendingDischarge => "On battery, finishing up",
        BatteryState::Unknown => "Unknown",
    }
}

/// The real `BatteryBackend`, connected lazily.
///
/// `UPowerBackend::connect()` is async and fallible, but `App::new` — like
/// every module's — builds its screens synchronously and hands back a
/// `Task` for anything that has to wait. Wrapping the connection behind
/// the trait itself means `PowerModule` never needs an `Option` for "not
/// connected yet": connecting is just what the first call does.
///
/// Only a *successful* connection is cached — the same rule
/// `network::LazyNetworkManagerBackend` and `bluetooth::LazyBlueZBackend`
/// keep, and for the same reason: caching a *failed* connect would keep
/// showing the service as unavailable after it comes back. Keep awake's
/// backend, `hyprforge_power::DetachedBackend`, connects the same way.
pub struct LazyUPowerBackend {
    inner: tokio::sync::Mutex<Option<Arc<hyprforge_power::UPowerBackend>>>,
}

impl LazyUPowerBackend {
    pub fn new() -> Self {
        LazyUPowerBackend { inner: tokio::sync::Mutex::new(None) }
    }

    async fn get(&self) -> Result<Arc<hyprforge_power::UPowerBackend>, BatteryError> {
        let mut slot = self.inner.lock().await;
        if let Some(backend) = slot.as_ref() {
            return Ok(backend.clone());
        }
        let backend = Arc::new(hyprforge_power::UPowerBackend::connect().await?);
        *slot = Some(backend.clone());
        Ok(backend)
    }
}

impl Default for LazyUPowerBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl BatteryBackend for LazyUPowerBackend {
    async fn battery(&self) -> Result<Option<BatteryInfo>, BatteryError> {
        self.get().await?.battery().await
    }
}

/// The real `PowerProfilesBackend`, connected lazily. Same shape as
/// [`LazyUPowerBackend`] and for the same reason — see its doc comment.
pub struct LazyPowerProfilesDaemonBackend {
    inner: tokio::sync::Mutex<Option<Arc<hyprforge_power::PowerProfilesDaemonBackend>>>,
}

impl LazyPowerProfilesDaemonBackend {
    pub fn new() -> Self {
        LazyPowerProfilesDaemonBackend { inner: tokio::sync::Mutex::new(None) }
    }

    async fn get(&self) -> Result<Arc<hyprforge_power::PowerProfilesDaemonBackend>, ProfileError> {
        let mut slot = self.inner.lock().await;
        if let Some(backend) = slot.as_ref() {
            return Ok(backend.clone());
        }
        let backend = Arc::new(hyprforge_power::PowerProfilesDaemonBackend::connect().await?);
        *slot = Some(backend.clone());
        Ok(backend)
    }
}

impl Default for LazyPowerProfilesDaemonBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl PowerProfilesBackend for LazyPowerProfilesDaemonBackend {
    async fn profiles(&self) -> Result<Vec<PowerProfile>, ProfileError> {
        self.get().await?.profiles().await
    }

    async fn active_profile(&self) -> Result<PowerProfile, ProfileError> {
        self.get().await?.active_profile().await
    }

    // Never called in any test in this crate against anything but a mock
    // — see the module doc comment and CLAUDE.md's rule about never
    // changing the active profile of the machine this suite is developed
    // on.
    async fn set_active_profile(&self, profile: PowerProfile) -> Result<(), ProfileError> {
        self.get().await?.set_active_profile(profile).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyprforge_power::backend::mock::{BatteryMockBackend, MockBackend, PowerProfilesMockBackend};

    type TestModule = PowerModule<MockBackend, BatteryMockBackend, PowerProfilesMockBackend>;

    /// Builds a module with fresh mocks, returning all three backends
    /// alongside it so a test can drive them directly and inspect what
    /// the module actually called them with — the same shape
    /// `bluetooth::tests::module` uses.
    fn module() -> (TestModule, Arc<MockBackend>, Arc<BatteryMockBackend>, Arc<PowerProfilesMockBackend>) {
        let inhibit = Arc::new(MockBackend::new());
        let battery = Arc::new(BatteryMockBackend::new());
        let profiles = Arc::new(PowerProfilesMockBackend::with_active(PowerProfile::Balanced));
        let (module, _task) = PowerModule::new(
            Arc::clone(&inhibit),
            Arc::clone(&battery),
            Arc::clone(&profiles),
        );
        (module, inhibit, battery, profiles)
    }

    fn loaded_ready(
        held: Option<WhatSet>,
        others: Vec<InhibitorInfo>,
        battery: Option<BatteryInfo>,
        available: Vec<PowerProfile>,
        active: PowerProfile,
    ) -> Loaded {
        Loaded {
            inhibit: Ok(InhibitLoaded { held, others }),
            battery: Ok(battery),
            profiles: Ok(ProfilesLoaded { available, active }),
        }
    }

    fn battery_info(state: BatteryState, percentage: u8) -> BatteryInfo {
        BatteryInfo { percentage, state, time_to_empty: None, time_to_full: None }
    }

    // --- three independently-absent daemons ------------------------------

    /// The property this screen exists to keep: logind being down must
    /// not blank the battery or profile sections, and must not read as
    /// an empty inhibitor list.
    #[test]
    fn logind_being_unavailable_does_not_blank_battery_or_profiles() {
        let (mut m, _i, _b, _p) = module();
        let _ = m.update(Message::Loaded(Loaded {
            inhibit: Err(LoadError::from(InhibitError::Unavailable)),
            battery: Ok(Some(battery_info(BatteryState::FullyCharged, 90))),
            profiles: Ok(ProfilesLoaded {
                available: vec![PowerProfile::Balanced, PowerProfile::Performance],
                active: PowerProfile::Performance,
            }),
        }));
        assert!(matches!(m.inhibit, InhibitDisplayState::Unavailable(ref msg) if msg.contains("isn't answering")));
        assert!(matches!(m.battery, BatteryDisplayState::Present(_)));
        assert!(matches!(m.profiles, ProfileDisplayState::Ready { .. }));
        let _ = m.view(FontScale::default());
    }

    /// The same property, for UPower: a battery read failing must not
    /// disturb the keep-awake or profile sections.
    #[test]
    fn upower_being_unavailable_does_not_blank_keep_awake_or_profiles() {
        let (mut m, _i, _b, _p) = module();
        let _ = m.update(Message::Loaded(Loaded {
            inhibit: Ok(InhibitLoaded { held: Some(WhatSet::keep_awake()), others: Vec::new() }),
            battery: Err(LoadError::from(BatteryError::Unavailable)),
            profiles: Ok(ProfilesLoaded {
                available: vec![PowerProfile::Balanced],
                active: PowerProfile::Balanced,
            }),
        }));
        assert!(matches!(m.inhibit, InhibitDisplayState::Ready { .. }));
        assert!(matches!(m.battery, BatteryDisplayState::Unavailable(ref msg) if msg.contains("isn't answering")));
        assert!(matches!(m.profiles, ProfileDisplayState::Ready { .. }));
        let _ = m.view(FontScale::default());
    }

    /// The same property, for `power-profiles-daemon`: a profile read
    /// failing must not disturb keep-awake or battery.
    #[test]
    fn power_profiles_daemon_being_unavailable_does_not_blank_keep_awake_or_battery() {
        let (mut m, _i, _b, _p) = module();
        let _ = m.update(Message::Loaded(Loaded {
            inhibit: Ok(InhibitLoaded { held: None, others: Vec::new() }),
            battery: Ok(Some(battery_info(BatteryState::Discharging, 55))),
            profiles: Err(LoadError::from(ProfileError::Unavailable)),
        }));
        assert!(matches!(m.inhibit, InhibitDisplayState::Ready { .. }));
        assert!(matches!(m.battery, BatteryDisplayState::Present(_)));
        assert!(matches!(m.profiles, ProfileDisplayState::Unavailable(ref msg) if msg.contains("isn't answering")));
        let _ = m.view(FontScale::default());
    }

    /// All three down at once must still build a screen, not panic.
    #[test]
    fn all_three_daemons_unavailable_at_once_still_builds_a_screen() {
        let (mut m, _i, _b, _p) = module();
        let _ = m.update(Message::Loaded(Loaded {
            inhibit: Err(LoadError::from(InhibitError::Unavailable)),
            battery: Err(LoadError::from(BatteryError::Unavailable)),
            profiles: Err(LoadError::from(ProfileError::Unavailable)),
        }));
        assert!(matches!(m.inhibit, InhibitDisplayState::Unavailable(_)));
        assert!(matches!(m.battery, BatteryDisplayState::Unavailable(_)));
        assert!(matches!(m.profiles, ProfileDisplayState::Unavailable(_)));
        let _ = m.view(FontScale::default());
    }

    // --- no battery vs UPower unavailable --------------------------------

    /// The distinction `hyprforge-power` was built to keep, one layer up:
    /// a desktop with no battery must read completely differently on
    /// screen from UPower not answering.
    #[test]
    fn no_battery_and_upower_unavailable_are_told_apart_on_screen() {
        let (mut desktop, _i, _b, _p) = module();
        let _ = desktop.update(Message::Loaded(loaded_ready(
            None,
            Vec::new(),
            None,
            vec![PowerProfile::Balanced],
            PowerProfile::Balanced,
        )));
        assert_eq!(desktop.battery, BatteryDisplayState::NoBattery);

        let (mut down, _i, _b, _p) = module();
        let _ = down.update(Message::Loaded(Loaded {
            inhibit: Ok(InhibitLoaded { held: None, others: Vec::new() }),
            battery: Err(LoadError::from(BatteryError::Unavailable)),
            profiles: Ok(ProfilesLoaded {
                available: vec![PowerProfile::Balanced],
                active: PowerProfile::Balanced,
            }),
        }));
        assert!(matches!(down.battery, BatteryDisplayState::Unavailable(_)));

        assert_ne!(desktop.battery, down.battery, "a desktop and a dead UPower must never look the same");
    }

    // --- low battery threshold --------------------------------------------

    /// `is_low` only applies while discharging — pinned here too, one
    /// layer above `hyprforge-power`'s own test of the same property, so
    /// this screen is proven not to have reintroduced a second threshold.
    #[test]
    fn low_battery_only_reads_as_low_while_discharging() {
        let low_discharging = battery_info(BatteryState::Discharging, 10);
        assert!(low_discharging.is_low());
        let low_charging = battery_info(BatteryState::Charging, 10);
        assert!(!low_charging.is_low());

        let (mut m, _i, _b, _p) = module();
        let _ = m.update(Message::Loaded(loaded_ready(
            None,
            Vec::new(),
            Some(low_charging),
            vec![PowerProfile::Balanced],
            PowerProfile::Balanced,
        )));
        assert!(matches!(m.battery, BatteryDisplayState::Present(info) if !info.is_low()));
        let _ = m.view(FontScale::default());
    }

    // --- keep-awake round trip --------------------------------------------

    /// Turning the toggle on reaches the backend, and the row reflects it
    /// without waiting on the next ten-second poll.
    #[tokio::test]
    async fn toggling_keep_awake_on_reaches_the_backend_and_is_reflected_on_refresh() {
        let (mut m, inhibit, _b, _p) = module();
        let _ = m.update(Message::Loaded(loaded_ready(
            None,
            Vec::new(),
            None,
            vec![PowerProfile::Balanced],
            PowerProfile::Balanced,
        )));

        let _ = m.update(Message::KeepAwakeToggled(true));
        // `update` returns a `Task`; a unit test doesn't drive iced's
        // executor, so call the backend directly the way
        // `bluetooth.rs`'s equivalent tests do, then simulate the
        // refresh the successful `Task` would have triggered.
        inhibit.take(WhatSet::keep_awake(), KEEP_AWAKE_WHO, KEEP_AWAKE_WHY).await.unwrap();
        let held = inhibit.held().await.unwrap();
        let _ = m.update(Message::Loaded(loaded_ready(
            held,
            Vec::new(),
            None,
            vec![PowerProfile::Balanced],
            PowerProfile::Balanced,
        )));
        assert!(matches!(m.inhibit, InhibitDisplayState::Ready { held: Some(_), .. }));
    }

    /// A toggle sent while the section isn't in `Ready` (still loading,
    /// or logind unavailable) must not dispatch anything.
    #[test]
    fn a_stray_toggle_before_the_first_load_does_nothing() {
        let (mut m, _i, _b, _p) = module();
        assert_eq!(m.inhibit, InhibitDisplayState::Unknown);
        let task = m.update(Message::KeepAwakeToggled(true));
        assert_eq!(task.units(), 0, "nothing to toggle before the first load lands");
    }

    /// This screen's own hold must not appear a second time in "who else
    /// is preventing sleep" — the same property `trayd`'s
    /// `other_inhibitors` keeps, checked here for this screen's own
    /// filter.
    #[tokio::test]
    async fn this_screens_own_hold_is_filtered_out_of_the_other_inhibitors_list() {
        let (_m, inhibit, _b, _p) = module();
        inhibit.take(WhatSet::keep_awake(), KEEP_AWAKE_WHO, KEEP_AWAKE_WHY).await.unwrap();
        *inhibit.others.lock().unwrap() = vec![InhibitorInfo {
            what: "sleep".to_string(),
            who: KEEP_AWAKE_WHO.to_string(),
            why: KEEP_AWAKE_WHY.to_string(),
            mode: "block".to_string(),
            uid: 1000,
            pid: 4242,
        }];
        // `list_inhibitors` on the mock returns exactly `others`, unlike
        // the real backend which returns everyone including ourselves —
        // so this exercises `load_inhibit`'s own filter rather than the
        // mock's behaviour.
        let loaded = load_inhibit(&*inhibit).await.unwrap();
        assert!(loaded.others.is_empty(), "our own who must be filtered out");
    }

    // --- power profile selection (mock only — never the real daemon) -----

    /// Selecting a profile reaches the backend and is reflected
    /// immediately. This is the one call this whole test module ever
    /// makes against `set_active_profile`, and it is always the mock —
    /// see the module-level guarantee below.
    #[tokio::test]
    async fn selecting_a_profile_reaches_the_backend_and_updates_the_row() {
        let (mut m, _i, _b, profiles) = module();
        let _ = m.update(Message::Loaded(loaded_ready(
            None,
            Vec::new(),
            None,
            vec![PowerProfile::PowerSaver, PowerProfile::Balanced, PowerProfile::Performance],
            PowerProfile::Balanced,
        )));

        let _ = m.update(Message::ProfileSelected(PowerProfile::Performance));
        // Same reasoning as the keep-awake test above: call the backend
        // directly to stand in for the `Task` a unit test doesn't drive.
        profiles.set_active_profile(PowerProfile::Performance).await.unwrap();
        let _ = m.update(Message::ProfileSet(PowerProfile::Performance, Ok(())));

        assert!(matches!(
            m.profiles,
            ProfileDisplayState::Ready { active: PowerProfile::Performance, .. }
        ));
        assert_eq!(*profiles.set_calls.lock().unwrap(), vec![PowerProfile::Performance]);
    }

    /// Selecting the already-active profile must not reach the backend
    /// at all — no redundant write.
    #[test]
    fn selecting_the_already_active_profile_does_not_reach_the_backend() {
        let (mut m, _i, _b, profiles) = module();
        let _ = m.update(Message::Loaded(loaded_ready(
            None,
            Vec::new(),
            None,
            vec![PowerProfile::Balanced, PowerProfile::Performance],
            PowerProfile::Balanced,
        )));
        let task = m.update(Message::ProfileSelected(PowerProfile::Balanced));
        assert_eq!(task.units(), 0);
        assert!(profiles.set_calls.lock().unwrap().is_empty());
    }

    /// A profile the daemon never offered must not reach the backend
    /// either, even though nothing in the view would ever press such a
    /// button.
    #[test]
    fn an_unoffered_profile_does_not_reach_the_backend() {
        let (mut m, _i, _b, profiles) = module();
        let _ = m.update(Message::Loaded(loaded_ready(
            None,
            Vec::new(),
            None,
            vec![PowerProfile::Balanced],
            PowerProfile::Balanced,
        )));
        let task = m.update(Message::ProfileSelected(PowerProfile::Performance));
        assert_eq!(task.units(), 0);
        assert!(profiles.set_calls.lock().unwrap().is_empty());
    }

    /// A refused profile switch reports why and leaves the active
    /// profile exactly where it was.
    #[test]
    fn a_refused_profile_switch_reports_why_and_leaves_the_active_profile_in_place() {
        let (mut m, _i, _b, _p) = module();
        let _ = m.update(Message::Loaded(loaded_ready(
            None,
            Vec::new(),
            None,
            vec![PowerProfile::Balanced, PowerProfile::Performance],
            PowerProfile::Balanced,
        )));
        let _ = m.update(Message::ProfileSet(
            PowerProfile::Performance,
            Err(LoadError::from(ProfileError::Refused("policy denied".to_string()))),
        ));
        assert!(m.error.as_ref().is_some_and(|e| e.contains("policy denied")));
        assert!(matches!(
            m.profiles,
            ProfileDisplayState::Ready { active: PowerProfile::Balanced, .. }
        ));
    }

    // --- screen builds in every state --------------------------------------

    #[test]
    fn the_screen_builds_before_the_first_load_lands() {
        let (m, _i, _b, _p) = module();
        let _ = m.view(FontScale::default());
    }
}

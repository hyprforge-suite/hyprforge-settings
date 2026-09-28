//! Which of `hyprforge-trayd`'s six icons show, and where
//! `hyprforge-traymenu` opens its right-click menu relative to the bar.
//!
//! Unlike every other module in this directory, this screen has no D-Bus
//! backend and no daemon to poll: its entire world is one file,
//! `tray.toml`, read and written through `hyprforge_tray::prefs`. That is
//! also what makes it the smallest module here — no trait to be generic
//! over, no mock, no `Arc<dyn Backend>`.
//!
//! # Two other screens already write this file
//!
//! `network.rs` and `bluetooth.rs` each carry their own "Show in tray"
//! checkbox for the one icon they own, and both save through
//! `hyprforge_tray::prefs::update` for the reason documented on that
//! function: this file has more than one writer inside a single
//! `hyprforge-settings` process, and a save built from a copy loaded at
//! construction can silently undo whatever another screen (or this one,
//! left open in another window state) wrote in between. This module goes
//! through the same `update` for every field it owns, for the same
//! reason.

use crate::module::SettingsModule;
use hyprforge_tray::Prefs;
use hyprforge_ui::theme::{self, spacing, FontScale};
use hyprforge_ui::widgets::{
    config_line, hint_text, scaled_text, section_label, setting_list, setting_row, slider_style,
    toggle,
};
use iced::widget::{column, slider};
use iced::{Alignment, Element, Length, Task};

/// The slider's own range for [`Prefs::menu_y_offset`].
///
/// `hyprforge_popup::placement::place_below_bar`'s own tests are the
/// source for what actually occurs: an ordinary waybar-height bar reserves
/// somewhere around 34-50 logical pixels, and the field's own default (32)
/// is chosen to clear that with no configuration at all. This range goes
/// well past a bar reserving nothing (0, the "no bar at all" case one of
/// those tests exercises) up to a bar carrying enough padding or a border
/// thick enough that the reserved area undershoots what it visually
/// occupies by a wide margin — the whole reason this field is
/// user-configurable rather than a constant. It is deliberately not
/// unbounded: a slider that can be dragged to some huge number invites
/// opening the menu off the bottom of the screen, which
/// `place_below_bar`'s own clamp then has to fight to recover from.
const OFFSET_RANGE: std::ops::RangeInclusive<i32> = 0..=120;

pub struct TrayModule {
    /// The on-disk preferences, loaded once at construction — same rule
    /// as `network::NetworkModule::tray_prefs` and for the same reason.
    /// Every write below reloads through `prefs::update` rather than
    /// saving this copy back, so this field exists only to answer "what
    /// is on disk as of the last load or the last write this screen made",
    /// which is exactly what the view needs to render checkboxes and the
    /// slider.
    tray_prefs: Result<Prefs, String>,
    /// The slider's own in-flight value while it's being dragged, kept
    /// apart from `tray_prefs.menu_y_offset` so a drag does not write
    /// `tray.toml` on every pixel of movement — only
    /// [`Message::OffsetReleased`] commits it to disk. Outside of a drag
    /// this is kept equal to `tray_prefs`'s own value (see
    /// [`Message::Refresh`] and every toggle arm, none of which touch
    /// this field).
    offset_draft: i32,
    /// Anything that went wrong on the last load or the last save.
    /// Cleared on the next attempt, not on every refresh, so it doesn't
    /// flash away before it's been read — same reasoning as every other
    /// module's `error` field.
    error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Message {
    /// Re-reads `tray.toml` from disk. Cheap enough (one small local
    /// file) to do synchronously, the same way `WindowRulesModule::new`
    /// loads its own TOML store without a `Task`.
    Refresh,
    NetworkToggled(bool),
    BluetoothToggled(bool),
    KeepAwakeToggled(bool),
    NightLightToggled(bool),
    PowerToggled(bool),
    DisplaysToggled(bool),
    /// Fired continuously while the slider is being dragged. Updates only
    /// [`TrayModule::offset_draft`] — see its own doc comment for why
    /// this does not write anything.
    OffsetChanged(i32),
    /// Fired once, when the drag ends. This is the one point that writes
    /// [`Prefs::menu_y_offset`] to disk.
    OffsetReleased,
    ClickOutsideToggled(bool),
}

impl TrayModule {
    pub fn new() -> (Self, Task<Message>) {
        let (tray_prefs, error) = load();
        let offset_draft = tray_prefs.as_ref().map(|p| p.menu_y_offset).unwrap_or(Prefs::default().menu_y_offset);
        (TrayModule { tray_prefs, offset_draft, error }, Task::none())
    }

    /// Applies `f` to the field this toggle owns, through
    /// `prefs::update` — see the module doc comment. Used by all six
    /// icon toggles; the slider commits through its own arm instead,
    /// since it has a draft value to reconcile.
    fn write(&mut self, f: impl FnOnce(&mut Prefs)) {
        match hyprforge_tray::prefs::update(f) {
            Ok(updated) => {
                self.offset_draft = updated.menu_y_offset;
                self.tray_prefs = Ok(updated);
                self.error = None;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }
}

/// A missing file is first run; a file that exists and will not parse is
/// reported, never silently defaulted — see `prefs::load_from`'s own doc
/// comment and the rule in CLAUDE.md it exists to keep.
fn load() -> (Result<Prefs, String>, Option<String>) {
    match hyprforge_tray::prefs::load() {
        Ok(prefs) => (Ok(prefs), None),
        Err(e) => (Err(e.to_string()), Some(e.to_string())),
    }
}

impl SettingsModule for TrayModule {
    type Message = Message;

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Refresh => {
                let (tray_prefs, error) = load();
                self.offset_draft =
                    tray_prefs.as_ref().map(|p| p.menu_y_offset).unwrap_or(self.offset_draft);
                self.tray_prefs = tray_prefs;
                self.error = error;
                Task::none()
            }
            Message::NetworkToggled(shown) => {
                self.write(|p| p.network = shown);
                Task::none()
            }
            Message::BluetoothToggled(shown) => {
                self.write(|p| p.bluetooth = shown);
                Task::none()
            }
            Message::KeepAwakeToggled(shown) => {
                self.write(|p| p.keep_awake = shown);
                Task::none()
            }
            Message::NightLightToggled(shown) => {
                self.write(|p| p.night_light = shown);
                Task::none()
            }
            Message::PowerToggled(shown) => {
                self.write(|p| p.power = shown);
                Task::none()
            }
            Message::DisplaysToggled(shown) => {
                self.write(|p| p.displays = shown);
                Task::none()
            }
            Message::OffsetChanged(value) => {
                self.offset_draft = value;
                Task::none()
            }
            Message::OffsetReleased => {
                let value = self.offset_draft;
                self.write(|p| p.menu_y_offset = value);
                Task::none()
            }
            Message::ClickOutsideToggled(closes) => {
                self.write(|p| p.menu_closes_on_click_outside = closes);
                Task::none()
            }
        }
    }

    fn view(&self, scale: FontScale) -> Element<'_, Message> {
        let mut content = column![].spacing(spacing::LG);

        if let Some(msg) = &self.error {
            content = content.push(scaled_text(msg.clone(), 13.0, scale).color(theme::warning()));
        }

        content = content.push(self.icons_section(scale));
        content = content.push(self.offset_section(scale));
        content = content.push(self.dismissal_section(scale));

        // Padded like every other page, so its first section sits a
        // gap below the title the shell draws, not flush against it.
        iced::widget::container(content).padding(spacing::LG).width(iced::Length::Fill).into()
    }
}

impl TrayModule {
    /// The six icon toggles, one group — mirrors the "Show in tray"
    /// switch Network and Bluetooth each already carry for their own
    /// icon, gathered here alongside the four that have no such switch
    /// anywhere else (keep-awake, night light, battery and power profile,
    /// and display layouts).
    fn icons_section(&self, scale: FontScale) -> Element<'_, Message> {
        let rows: Vec<Element<'_, Message>> = match &self.tray_prefs {
            Ok(prefs) => [
                (prefs.network, "Network (Wi-Fi and Ethernet)", Message::NetworkToggled as fn(bool) -> Message),
                (prefs.bluetooth, "Bluetooth", Message::BluetoothToggled),
                (prefs.keep_awake, "Keep awake", Message::KeepAwakeToggled),
                (prefs.night_light, "Night light", Message::NightLightToggled),
                (prefs.power, "Battery and power profile", Message::PowerToggled),
                (prefs.displays, "Display layouts", Message::DisplaysToggled),
            ]
            .into_iter()
            .enumerate()
            .map(|(i, (shown, label, message))| icon_row(i, shown, label, message, scale))
            .collect(),
            Err(_) => vec![unavailable_row(scale)],
        };
        group("Tray icons", rows, scale)
    }

    /// The menu offset — the setting the whole screen exists for.
    ///
    /// A bar's *reserved* screen space and the space it *visibly* takes
    /// up are not always the same number — padding, a border around the
    /// bar, both count toward what you see but not toward what Hyprland
    /// reports as reserved. `hyprforge-traymenu` anchors the tray's
    /// right-click menu to the reserved area plus this offset, so on a
    /// bar where that gap is larger than the default clears, the menu can
    /// end up tucked slightly under the bar, or with an odd gap beneath
    /// it, until this is turned up (or down) to match.
    fn offset_section(&self, scale: FontScale) -> Element<'_, Message> {
        let row: Element<'_, Message> = match &self.tray_prefs {
            Ok(_) => setting_row(
                0,
                "Menu offset",
                Some(
                    hint_text(
                        "How far below your bar the right-click menu opens. Raise this if the \
                         menu overlaps the bar or leaves a gap under it; the default clears an \
                         ordinary waybar-height bar with nothing configured.",
                        scale,
                    )
                    .into(),
                ),
                iced::widget::row![
                    slider(OFFSET_RANGE, self.offset_draft, Message::OffsetChanged)
                        .on_release(Message::OffsetReleased)
                        .style(slider_style)
                        .width(Length::Fixed(scale.apply(150.0))),
                    config_line(format!("{} px", self.offset_draft), scale)
                        .color(theme::text()),
                ]
                .spacing(spacing::SM)
                .align_y(Alignment::Center),
                scale,
            ),
            Err(_) => unavailable_row(scale),
        };
        group("Menu position", vec![row], scale)
    }

    /// What a click outside an open tray menu does.
    ///
    /// Worth its own group rather than a row in "Menu position",
    /// because it is not about where the menu is: it changes how the
    /// popup asks the compositor for the keyboard, and that is what
    /// decides whether the click that dismissed it also reaches whatever
    /// it landed on. Turning it off is for driving the menus from the
    /// keyboard, where a stray click closing the menu is a nuisance
    /// rather than a convenience.
    fn dismissal_section(&self, scale: FontScale) -> Element<'_, Message> {
        let row: Element<'_, Message> = match &self.tray_prefs {
            Ok(prefs) => setting_row(
                0,
                "Close the menu when I click somewhere else",
                Some(
                    hint_text(
                        "On, the menu behaves like every other menu on the desktop: clicking \
                         away dismisses it, and that click still reaches whatever you clicked \
                         — including another tray icon. Off, the menu keeps the keyboard until \
                         you choose a row or press Escape.",
                        scale,
                    )
                    .into(),
                ),
                toggle(prefs.menu_closes_on_click_outside, scale).on_toggle(Message::ClickOutsideToggled),
                scale,
            ),
            Err(_) => unavailable_row(scale),
        };
        group("Clicking away", vec![row], scale)
    }
}

/// A section label over a stack of striped rows — this page's groups.
fn group<'a>(label: &str, rows: Vec<Element<'a, Message>>, scale: FontScale) -> Element<'a, Message> {
    column![section_label(label, scale), setting_list(rows)].spacing(spacing::SM).into()
}

/// What a group shows when `tray.toml` could not be read.
fn unavailable_row<'a>(scale: FontScale) -> Element<'a, Message> {
    setting_row(
        0,
        "Tray settings unavailable",
        Some(hint_text("See the error above.", scale).into()),
        iced::widget::Space::new(),
        scale,
    )
}

/// One icon-toggle row, factored out because all six are identical apart
/// from which field and which label.
fn icon_row<'a>(
    index: usize,
    shown: bool,
    label: &'static str,
    message: fn(bool) -> Message,
    scale: FontScale,
) -> Element<'a, Message> {
    setting_row(index, label, None, toggle(shown, scale).on_toggle(message), scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Isolated config home and greeter export dir — see
    /// [`crate::modules::with_temp_env`].
    fn with_temp_config<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        crate::modules::with_temp_env(f)
    }

    #[test]
    fn a_missing_tray_toml_is_first_run_and_reads_as_the_defaults() {
        with_temp_config(|_dir| {
            let (m, _task) = TrayModule::new();
            let prefs = m.tray_prefs.as_ref().expect("a missing file is defaults, not an error");
            assert!(prefs.network);
            assert!(prefs.bluetooth);
            assert!(!prefs.keep_awake);
            assert!(!prefs.night_light);
            assert!(!prefs.power);
            assert!(!prefs.displays);
            assert_eq!(m.offset_draft, 32);
            assert!(m.error.is_none());
            let _ = m.view(FontScale::default());
        });
    }

    #[test]
    fn a_file_that_will_not_parse_is_reported_and_a_toggle_does_not_touch_it() {
        with_temp_config(|dir| {
            let tray_toml = dir.join("hyprforge").join("tray.toml");
            std::fs::create_dir_all(tray_toml.parent().unwrap()).unwrap();
            std::fs::write(&tray_toml, "network = yes please\n").unwrap();

            let (mut m, _task) = TrayModule::new();
            assert!(m.tray_prefs.is_err());
            assert!(m.error.is_some());
            let _ = m.view(FontScale::default());

            let before = std::fs::read_to_string(&tray_toml).unwrap();
            let _ = m.update(Message::NetworkToggled(false));
            let after = std::fs::read_to_string(&tray_toml).unwrap();
            assert_eq!(before, after, "a toggle must never overwrite a file it could not read");
        });
    }

    /// Each toggle flips only its own field, leaving the other three
    /// exactly where they were.
    #[test]
    fn toggling_one_icon_leaves_the_other_three_alone() {
        with_temp_config(|_dir| {
            let (mut m, _task) = TrayModule::new();
            let _ = m.update(Message::KeepAwakeToggled(true));
            let prefs = m.tray_prefs.as_ref().unwrap();
            assert!(prefs.keep_awake);
            assert!(prefs.network, "untouched icon keeps its value");
            assert!(prefs.bluetooth, "untouched icon keeps its value");
            assert!(!prefs.night_light, "untouched icon keeps its value");
            assert!(!prefs.power, "untouched icon keeps its value");
        });
    }

    #[test]
    fn the_displays_toggle_writes_only_its_own_field() {
        with_temp_config(|_dir| {
            let (mut m, _task) = TrayModule::new();
            let _ = m.update(Message::DisplaysToggled(true));
            let on_disk = hyprforge_tray::prefs::load().unwrap();
            assert_eq!(on_disk, Prefs { displays: true, ..Prefs::default() });
        });
    }

    #[test]
    fn the_power_toggle_writes_only_its_own_field() {
        with_temp_config(|_dir| {
            let (mut m, _task) = TrayModule::new();
            let _ = m.update(Message::PowerToggled(true));
            let on_disk = hyprforge_tray::prefs::load().unwrap();
            assert!(on_disk.power);
            assert_eq!(on_disk, Prefs { power: true, ..Prefs::default() });
        });
    }

    /// Dragging the slider must not write anything until it's released —
    /// the whole reason `offset_draft` exists apart from `tray_prefs`.
    #[test]
    fn dragging_the_offset_slider_does_not_write_until_released() {
        with_temp_config(|dir| {
            let tray_toml = dir.join("hyprforge").join("tray.toml");
            let (mut m, _task) = TrayModule::new();
            let _ = m.update(Message::OffsetChanged(75));
            assert_eq!(m.offset_draft, 75);
            assert!(!tray_toml.exists(), "a drag preview must not create or touch the file");
            assert_eq!(
                m.tray_prefs.as_ref().unwrap().menu_y_offset,
                32,
                "the loaded value is untouched until release"
            );

            let _ = m.update(Message::OffsetReleased);
            assert_eq!(m.tray_prefs.as_ref().unwrap().menu_y_offset, 75);
            assert_eq!(hyprforge_tray::prefs::load_from(&tray_toml).unwrap().menu_y_offset, 75);
        });
    }

    /// The two-writer property `hyprforge_tray::prefs::update` exists to
    /// guarantee, pinned one layer up: two modules built against the same
    /// on-disk file, each writing a different field, must not step on one
    /// another — regardless of which one is stale by the time it writes.
    #[test]
    fn two_modules_writing_different_fields_both_survive() {
        with_temp_config(|_dir| {
            // Both load the same starting file, before either has written
            // anything — the shape that actually breaks a save built from
            // a remembered copy instead of a fresh reload.
            let (mut tray_a, _task_a) = TrayModule::new();
            let (mut tray_b, _task_b) = TrayModule::new();

            let _ = tray_a.update(Message::NetworkToggled(false));
            let _ = tray_b.update(Message::OffsetChanged(64));
            let _ = tray_b.update(Message::OffsetReleased);

            let on_disk = hyprforge_tray::prefs::load().unwrap();
            assert!(!on_disk.network, "tray_a's write must survive tray_b's later write");
            assert_eq!(on_disk.menu_y_offset, 64, "tray_b's own write must have landed");
        });
    }

    /// The same property, against `network.rs`'s own writer rather than a
    /// second `TrayModule` — the pairing the brief this screen was built
    /// from called out by name.
    #[test]
    fn a_tray_screen_and_a_network_screen_writing_different_fields_both_survive() {
        use crate::modules::network::{LazyNetworkManagerBackend, NetworkModule};

        with_temp_config(|_dir| {
            let (mut tray, _task) = TrayModule::new();
            let (mut net, _net_task) =
                NetworkModule::new(std::sync::Arc::new(LazyNetworkManagerBackend::new()));

            let _ = tray.update(Message::OffsetChanged(48));
            let _ = tray.update(Message::OffsetReleased);
            let _ = net.update(crate::modules::network::Message::TrayToggled(false));

            let on_disk = hyprforge_tray::prefs::load().unwrap();
            assert_eq!(on_disk.menu_y_offset, 48, "the tray screen's own write must survive");
            assert!(!on_disk.network, "network's write must also land");
        });
    }

    #[test]
    fn refresh_picks_up_a_change_written_by_another_writer() {
        with_temp_config(|_dir| {
            let (mut tray, _task) = TrayModule::new();
            hyprforge_tray::prefs::update(|p| p.night_light = true).unwrap();

            let _ = tray.update(Message::Refresh);
            assert!(tray.tray_prefs.as_ref().unwrap().night_light);
        });
    }
}

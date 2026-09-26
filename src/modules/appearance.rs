//! One screen for how the desktop looks.
//!
//! Three things a user thinks of as one decision, which the system keeps
//! in three places: Hyprland's own appearance settings, its per-animation
//! speeds and curves, and the GTK/icon/cursor/font settings in gsettings
//! that no file here generates. On this machine the window borders and the
//! GTK theme are both Dracula, matched by hand in two files with nothing
//! keeping them in step. Putting them on one screen is the point.
//!
//! The three don't share an ownership story, and the screen doesn't
//! pretend they do:
//!
//! - **Hyprland settings and animations** are an overlay — Hyprforge
//!   writes a file, that file wins because it's sourced last, and Reset
//!   stops writing the key so the user's config decides again. Each row
//!   says which of those it's showing.
//! - **Desktop settings** are shared state with one value and no layering.
//!   Writing one *is* the change; there is no generated file to delete to
//!   undo it. So they're written only when the user changes that control,
//!   and the previous value is offered back as the only undo there is.

use hyprforge_appearance::animations::{Animation, Curve, LiveAnimation};
use hyprforge_appearance::catalog::CATALOG;
use hyprforge_appearance::desktop::{self, Catalogue, DesktopKind, DesktopSetting};
use hyprforge_appearance::setup::{HyprConfig, SetupPlan};
use hyprforge_appearance::storage::Appearance;
use hyprforge_core::hlconfig::import::{Discovered, Live};
use hyprforge_core::hlconfig::{Invalid, Setting, Value};
use hyprforge_core::lua_setup;
use hyprforge_ui::theme::{spacing, FontScale, BASE_TEXT_SIZE};
use hyprforge_ui::widgets::{
    config_line, danger_button, divider, meta_text, primary_button, scaled_text,
    secondary_button, section, section_label,
};
use crate::modules::setup_notice::setup_notice;
use crate::module::SettingsModule;
use crate::modules::catalog_screen;
use iced::widget::{checkbox, column, container, pick_list, row, text_input};
use iced::{Element, Length, Task};
use std::collections::BTreeMap;

use super::setting_rows::{self, DynChoice, RowContext};

/// Which part of the screen is showing. All three are long enough that
/// one scrolling page would bury whichever the user didn't come for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Theme,
    Windows,
    Animations,
}

impl Tab {
    /// Every tab, for tests that walk them all. Nothing else lists them:
    /// each is a sidebar page now, and the shell names the one it wants.
    #[cfg(test)]
    const ALL: [Tab; 3] = [Tab::Theme, Tab::Windows, Tab::Animations];
}

#[derive(Debug, Clone)]
pub enum Message {
    TabSelected(Tab),
    // -- Hyprland settings --
    Set(&'static str, Value),
    DraftChanged(&'static str, String),
    ApplyDrafts,
    DiscardDrafts,
    Reset(&'static str),
    FilterChanged(String),
    LiveLoaded(Vec<Live>),
    // -- Animations --
    AnimationsLoaded(Vec<LiveAnimation>, Vec<Curve>),
    AnimationToggled(String, bool),
    AnimationSpeedChanged(String, String),
    AnimationSpeedSubmitted(String),
    AnimationCurveChosen(String, String),
    AnimationReset(String),
    // -- Desktop (gsettings) --
    DesktopLoaded(Vec<(&'static str, String)>),
    /// The themes and fonts installed on this machine, scanned once.
    InstalledLoaded(Installed),
    /// The connected monitors, for the settings that name one.
    MonitorsLoaded(Vec<String>),
    FontFamilyChosen(&'static str, String),
    FontSizeChanged(&'static str, String),
    FontSizeSubmitted(&'static str),
    DesktopDraftChanged(&'static str, String),
    DesktopCommit(&'static str),
    DesktopChosen(&'static str, String),
    DesktopUndo(&'static str, String),
    DesktopWritten(&'static str, Result<Option<String>, String>),
    // -- Import --
    ImportOpen,
    ImportEvaluated(hyprforge_lua_import::ImportResult),
    ImportToggle(usize),
    ImportConfirm,
    ImportCancel,
    Reloaded(Result<(), String>),
}

enum ImportState {
    Running,
    Ready(Vec<Candidate>),
}

/// An importable value, either a setting or an animation. Both come from
/// the same evaluation of the user's config and are reviewed together,
/// because "adopt what I already have" is one decision.
struct Candidate {
    label: String,
    detail: String,
    selected: bool,
    already_owned: bool,
    differs: bool,
    payload: Payload,
}

enum Payload {
    Setting(Discovered),
    Animation(String, Animation),
}

/// Everything pickable that had to be found on disk.
#[derive(Debug, Clone, Default)]
pub struct Installed {
    pub gtk: Vec<String>,
    pub icons: Vec<String>,
    pub cursors: Vec<String>,
    pub fonts: Vec<String>,
}

impl Installed {
    fn for_catalogue(&self, catalogue: Catalogue) -> &[String] {
        match catalogue {
            Catalogue::GtkThemes => &self.gtk,
            Catalogue::IconThemes => &self.icons,
            Catalogue::CursorThemes => &self.cursors,
        }
    }
}

pub struct AppearanceModule {
    tab: Tab,
    stored: Appearance,
    drafts: BTreeMap<&'static str, String>,
    /// Keys written since this window opened, for the "← changed" marks
    /// on the page's Writes block. Session-only on purpose: it answers
    /// "what did I just do", which a stored history would not.
    changed: std::collections::BTreeSet<&'static str>,
    draft_errors: BTreeMap<&'static str, String>,
    /// Speed fields mid-edit, keyed by leaf. Same reason as `drafts`:
    /// committing per keystroke would reload Hyprland once per character.
    animation_drafts: BTreeMap<String, String>,
    animation_errors: BTreeMap<String, String>,
    /// Desktop values as gsettings currently has them, and any field
    /// being typed into.
    desktop: BTreeMap<&'static str, String>,
    desktop_drafts: BTreeMap<&'static str, String>,
    desktop_errors: BTreeMap<&'static str, String>,
    /// The last desktop write, so it can be offered back. gsettings has
    /// no generated file to delete, so this is the only undo available.
    desktop_undo: Option<(&'static str, String)>,
    /// Themes and fonts found on disk, for the pickers. Empty until the
    /// scan returns; a row falls back to a text field so a slow or
    /// failed scan never leaves a setting uneditable.
    installed: Installed,
    /// Font sizes mid-edit, keyed by gsettings key.
    font_sizes: BTreeMap<&'static str, String>,
    /// Discovered options per setting, for the rows that can be picked
    /// rather than typed.
    choices: BTreeMap<&'static str, Vec<DynChoice>>,
    filter: String,
    config: HyprConfig,
    setup_plan: SetupPlan,
    error: Option<String>,
    status: Option<String>,
    store_unreadable: Option<String>,
    invalid: Vec<Invalid>,
    live: BTreeMap<&'static str, Live>,
    live_animations: Vec<LiveAnimation>,
    curves: Vec<Curve>,
    import_review: Option<ImportState>,
}

impl AppearanceModule {
    pub fn new() -> (Self, Task<Message>) {
        // A failure here must never look like "you've configured
        // nothing": that reading is what turns one bad parse into a wiped
        // store on the next save.
        let (stored, store_unreadable) =
            match hyprforge_appearance::storage::load(&appearance_toml_path()) {
                Ok(stored) => (stored, None),
                Err(e) => (Appearance::default(), Some(e.to_string())),
            };
        let invalid = stored.settings.validate(&CATALOG);
        let setup = lua_setup::bootstrap(
            &hyprforge_core::paths::hypr_config_dir(),
            &hyprforge_core::paths::hyprland_lua_path(),
            lua_setup::ModuleSetup {
                require_line: hyprforge_appearance::setup::REQUIRE_LINE,
                placement: hyprforge_appearance::setup::PLACEMENT,
                generated: (
                    appearance_lua_path(),
                    hyprforge_appearance::apply::generate(&Appearance::default(), None),
                ),
            },
        );
        (
            AppearanceModule {
                tab: Tab::Theme,
                stored,
                drafts: BTreeMap::new(),
                changed: std::collections::BTreeSet::new(),
                draft_errors: BTreeMap::new(),
                animation_drafts: BTreeMap::new(),
                animation_errors: BTreeMap::new(),
                desktop: BTreeMap::new(),
                desktop_drafts: BTreeMap::new(),
                desktop_errors: BTreeMap::new(),
                desktop_undo: None,
                installed: Installed::default(),
                font_sizes: BTreeMap::new(),
                choices: BTreeMap::new(),
                filter: String::new(),
                config: setup.config,
                setup_plan: setup.plan,
                error: setup.error,
                status: None,
                store_unreadable,
                invalid,
                live: BTreeMap::new(),
                live_animations: Vec::new(),
                curves: Vec::new(),
                import_review: None,
            },
            // All three reads are silent on failure by design: every
            // section still renders, it just can't claim to know what's
            // live.
            Task::batch([
                Task::perform(read_live(), Message::LiveLoaded),
                Task::perform(read_animations(), |(a, c)| Message::AnimationsLoaded(a, c)),
                Task::perform(read_desktop(), Message::DesktopLoaded),
                Task::perform(read_installed(), Message::InstalledLoaded),
                Task::perform(read_monitors(), Message::MonitorsLoaded),
            ]),
        )
    }

    /// The curve names currently defined, or `None` if the compositor
    /// couldn't be read. `None` means "can't check" rather than "none
    /// exist" — the difference between skipping a validation and failing
    /// every animation.
    fn known_curves(&self) -> Option<Vec<String>> {
        if self.curves.is_empty() {
            return None;
        }
        Some(self.curves.iter().map(|c| c.name.clone()).collect())
    }

    fn rows(&self) -> RowContext<'_, Message> {
        RowContext {
            settings: &self.stored.settings,
            live: &self.live,
            drafts: &self.drafts,
            draft_errors: &self.draft_errors,
            choices: &self.choices,
            on_set: Message::Set,
            on_draft: Message::DraftChanged,
            on_reset: Message::Reset,
            on_submit: Message::ApplyDrafts,
        }
    }

    /// Writes the canonical TOML. Nothing irreversible runs unless this
    /// returned `Ok` — the ordering rule that exists because doing it the
    /// other way round cost a real user 37 hand-written binds.
    fn persist(&mut self) -> Result<(), String> {
        if let Some(reason) = &self.store_unreadable {
            let message = format!(
                "Not saving — your appearance.toml couldn't be read, and \
                 overwriting it would lose whatever is in it. ({reason})"
            );
            self.error = Some(message.clone());
            return Err(message);
        }
        hyprforge_appearance::storage::save(&appearance_toml_path(), &self.stored).map_err(|e| {
            self.error = Some(e.to_string());
            e.to_string()
        })?;

        // The look the lock screen and the greeter read is derived from
        // what was just saved, so it is republished here rather than on
        // some later trigger. Without this the lock screen keeps its own
        // colours forever and the two drift apart again — which is the
        // whole reason any of this is shared.
        //
        // A failure here does not fail the save: the appearance settings
        // *are* saved by this point, and reporting otherwise would send
        // the user to fix something that already worked.
        crate::look::republish();
        Ok(())
    }

    fn save_and_maybe_reload(&mut self) -> Task<Message> {
        if self.persist().is_err() {
            return Task::none();
        }
        self.invalid = self.stored.settings.validate(&CATALOG);
        if !matches!(self.config, HyprConfig::Lua(_)) {
            self.error = None;
            self.status = Some(
                "Saved. Settings take effect once Hyprland setup is finished — see above."
                    .to_string(),
            );
            return Task::none();
        }
        if self.setup_plan != SetupPlan::AlreadyPresent {
            match hyprforge_appearance::setup::install(&hyprforge_core::paths::hyprland_lua_path())
            {
                Ok(plan) => self.setup_plan = plan,
                Err(e) => {
                    self.error = Some(e.to_string());
                    return Task::none();
                }
            }
        }
        Task::perform(
            regenerate_and_reload(self.stored.clone(), self.known_curves()),
            Message::Reloaded,
        )
    }

    fn apply_drafts(&mut self) -> Task<Message> {
        // What leaves the drafts is what got written; a draft the
        // catalogue refused stays, and is not marked as changed.
        let drafted: Vec<&'static str> = self.drafts.keys().copied().collect();
        let applied = catalog_screen::apply_drafts(
            &mut self.stored.settings,
            &CATALOG,
            &mut self.drafts,
            &mut self.draft_errors,
        );
        self.changed
            .extend(drafted.into_iter().filter(|k| !self.drafts.contains_key(k)));
        match applied {
            true => self.save_and_maybe_reload(),
            false => Task::none(),
        }
    }

    /// The animation a row should show: owned first, then whatever the
    /// compositor currently has. Same precedence as a setting row, and for
    /// the same reason — showing a default while something else is running
    /// is a screen that lies.
    fn effective_animation(&self, leaf: &str) -> (Animation, bool) {
        if let Some(a) = self.stored.animations.get(leaf) {
            return (a.clone(), true);
        }
        let mut live = self
            .live_animations
            .iter()
            .find(|l| l.leaf == leaf)
            .map(|l| l.animation.clone())
            .unwrap_or(Animation {
                enabled: true,
                speed: 0.0,
                bezier: String::new(),
                style: String::new(),
            });
        // Shown rather than a bare 0, which would read as "this animation
        // takes no time" when it means "inherits from its parent".
        if live.speed <= 0.0 {
            live.speed =
                hyprforge_appearance::animations::inherited_speed(leaf, &self.live_animations);
        }
        (live, false)
    }

    /// Takes ownership of a leaf, seeding it from whatever is running so
    /// touching one control doesn't silently reset the others.
    ///
    /// A leaf nothing has overridden reports speed 0, because Hyprland
    /// has it inherit rather than hold a value — and `hl.animation`
    /// refuses 0. Seeding what was reported would write a line the
    /// compositor rejects, which is a toggle that silently does nothing.
    /// The inherited speed is resolved instead, and the row shows the
    /// number rather than hiding it.
    fn own_animation(&mut self, leaf: &str) -> Animation {
        let (mut current, _) = self.effective_animation(leaf);
        if current.speed <= 0.0 || !current.speed.is_finite() {
            current.speed =
                hyprforge_appearance::animations::inherited_speed(leaf, &self.live_animations);
        }
        self.stored.animations.set(leaf, current.clone());
        current
    }

    /// Whether a setting is shown, for the text in the filter box — see
    /// [`catalog_screen::matches_filter`].
    fn matches_filter(&self, setting: &Setting) -> bool {
        catalog_screen::matches_filter(&self.filter, setting)
    }
}

impl SettingsModule for AppearanceModule {
    type Message = Message;

    /// Import reads Hyprland's own config, so it belongs on the two pages
    /// made of Hyprland settings and not on the desktop-theme page.
    fn header_actions(&self, _scale: FontScale) -> Option<Element<'_, Message>> {
        (self.tab != Tab::Theme)
            .then(|| secondary_button("Import from Hyprland").on_press(Message::ImportOpen).into())
    }

    /// The Hyprland half's settings, all on the Windows & workspaces page.
    fn search_entries(&self) -> Vec<crate::module::SearchEntry<Message>> {
        CATALOG
            .settings
            .iter()
            .map(|setting| crate::module::SearchEntry {
                label: setting.label,
                key: setting.key,
                value: self.stored.settings.get(setting.key).map(crate::module::value_text),
                // The shell opens the Windows & workspaces page, which
                // selects the tab; this only narrows the filter.
                reveal: vec![Message::FilterChanged(setting.key.to_string())],
            })
            .collect()
    }

    /// The Hyprland drafts only. The desktop and animation rows each
    /// write through their own Set button, so nothing of theirs is ever
    /// held back for this bar to apply.
    fn pending(&self) -> Option<crate::module::Pending<Message>> {
        (!self.drafts.is_empty()).then(|| crate::module::Pending {
            summary: hyprforge_ui::widgets::pending_label(self.drafts.len()),
            preview: crate::module::drafts_preview(self.drafts.iter().map(|(k, v)| (*k, v.as_str()))),
            apply: Message::ApplyDrafts,
            discard: Some(Message::DiscardDrafts),
        })
    }


    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::TabSelected(tab) => {
                self.tab = tab;
                Task::none()
            }
            Message::Set(key, value) => {
                self.stored.settings.set(key, value);
                self.changed.insert(key);
                self.status = None;
                self.save_and_maybe_reload()
            }
            Message::DraftChanged(key, raw) => {
                self.drafts.insert(key, raw);
                self.draft_errors.remove(key);
                Task::none()
            }
            Message::ApplyDrafts => self.apply_drafts(),
            Message::DiscardDrafts => {
                self.drafts.clear();
                self.draft_errors.clear();
                self.animation_drafts.clear();
                self.animation_errors.clear();
                Task::none()
            }
            Message::Reset(key) => {
                self.stored.settings.clear(key);
                self.drafts.remove(key);
                self.draft_errors.remove(key);
                self.status = None;
                self.save_and_maybe_reload()
            }
            Message::FilterChanged(q) => {
                self.filter = q;
                Task::none()
            }
            Message::LiveLoaded(live) => {
                self.live = live.into_iter().map(|l| (l.key, l)).collect();
                Task::none()
            }
            Message::AnimationsLoaded(animations, curves) => {
                self.live_animations = animations;
                self.curves = curves;
                Task::none()
            }
            Message::AnimationToggled(leaf, enabled) => {
                let mut a = self.own_animation(&leaf);
                a.enabled = enabled;
                self.stored.animations.set(&leaf, a);
                self.save_and_maybe_reload()
            }
            Message::AnimationCurveChosen(leaf, bezier) => {
                let mut a = self.own_animation(&leaf);
                a.bezier = bezier;
                self.stored.animations.set(&leaf, a);
                self.save_and_maybe_reload()
            }
            Message::AnimationSpeedChanged(leaf, raw) => {
                self.animation_errors.remove(&leaf);
                self.animation_drafts.insert(leaf, raw);
                Task::none()
            }
            Message::AnimationSpeedSubmitted(leaf) => {
                let Some(raw) = self.animation_drafts.get(&leaf).cloned() else {
                    return Task::none();
                };
                match raw.trim().parse::<f64>() {
                    Ok(speed) if speed.is_finite() && speed > 0.0 => {
                        let mut a = self.own_animation(&leaf);
                        a.speed = speed;
                        self.stored.animations.set(&leaf, a);
                        self.animation_drafts.remove(&leaf);
                        self.save_and_maybe_reload()
                    }
                    Ok(_) => {
                        self.animation_errors
                            .insert(leaf, "speed must be greater than 0".to_string());
                        Task::none()
                    }
                    Err(_) => {
                        self.animation_errors
                            .insert(leaf, "expected a number".to_string());
                        Task::none()
                    }
                }
            }
            Message::AnimationReset(leaf) => {
                self.stored.animations.clear(&leaf);
                self.animation_drafts.remove(&leaf);
                self.animation_errors.remove(&leaf);
                self.save_and_maybe_reload()
            }
            Message::DesktopLoaded(values) => {
                self.desktop = values.into_iter().collect();
                Task::none()
            }
            Message::DesktopDraftChanged(key, raw) => {
                self.desktop_errors.remove(key);
                self.desktop_drafts.insert(key, raw);
                Task::none()
            }
            Message::InstalledLoaded(installed) => {
                self.installed = installed;
                Task::none()
            }
            Message::MonitorsLoaded(monitors) => {
                self.choices.insert(
                    "cursor:default_monitor",
                    monitors
                        .into_iter()
                        .map(|name| DynChoice { label: name.clone(), value: name })
                        .collect(),
                );
                Task::none()
            }
            Message::DesktopChosen(key, value) => write_desktop(self, key, value),
            Message::FontFamilyChosen(key, family) => {
                // Only the family changes; the size is whatever is
                // currently stored, so picking a font can't silently
                // resize the interface.
                let (_, size) = desktop::split_font(self.desktop.get(key).map_or("", |v| v));
                write_desktop(self, key, desktop::join_font(&family, size))
            }
            Message::FontSizeChanged(key, raw) => {
                self.desktop_errors.remove(key);
                self.font_sizes.insert(key, raw);
                Task::none()
            }
            Message::FontSizeSubmitted(key) => {
                let Some(raw) = self.font_sizes.get(key).cloned() else {
                    return Task::none();
                };
                let (family, _) = desktop::split_font(self.desktop.get(key).map_or("", |v| v));
                match raw.trim().parse::<u32>() {
                    Ok(size) if size > 0 => {
                        self.font_sizes.remove(key);
                        write_desktop(self, key, desktop::join_font(&family, Some(size)))
                    }
                    _ => {
                        self.desktop_errors
                            .insert(key, "expected a whole number of points".to_string());
                        Task::none()
                    }
                }
            }
            Message::DesktopCommit(key) => {
                let Some(raw) = self.desktop_drafts.get(key).cloned() else {
                    return Task::none();
                };
                write_desktop(self, key, raw)
            }
            Message::DesktopUndo(key, previous) => write_desktop(self, key, previous),
            Message::DesktopWritten(key, Ok(previous)) => {
                self.desktop_drafts.remove(key);
                // Offered back only when it actually changed — a "put it
                // back" for a value that is already what it was is noise.
                self.desktop_undo = previous
                    .filter(|p| self.desktop.get(key) != Some(p))
                    .map(|p| (key, p));
                // The third writer on this screen, and the one that used to
                // forget. `look::resolve` takes the font family and size
                // straight from gsettings, so changing the system font here
                // updated the Settings app and left the lock screen and the
                // greeter on the old one — the two drifting apart, which is
                // the failure this suite exists to prevent, happening inside
                // the suite.
                //
                // After the write, not before: gsettings has stored the
                // value by now, so this publishes what it kept rather than
                // what we sent it.
                crate::look::republish();
                // Re-read rather than assume: gsettings normalises some
                // values, and showing what we sent instead of what it
                // stored would be the same lie the settings rows told.
                Task::perform(read_desktop(), Message::DesktopLoaded)
            }
            Message::DesktopWritten(key, Err(e)) => {
                self.desktop_errors.insert(key, e);
                Task::none()
            }
            Message::ImportOpen => {
                self.import_review = Some(ImportState::Running);
                Task::perform(super::evaluate_user_config(), Message::ImportEvaluated)
            }
            Message::ImportEvaluated(result) => {
                self.import_review = Some(ImportState::Ready(self.candidates(&result)));
                if !result.failures.is_empty() {
                    self.error = Some(format!(
                        "Some config files couldn't be read, so anything they set isn't listed: {}",
                        result
                            .failures
                            .iter()
                            .map(|(p, why)| format!("{} ({why})", p.display()))
                            .collect::<Vec<_>>()
                            .join("; ")
                    ));
                }
                Task::none()
            }
            Message::ImportToggle(i) => {
                if let Some(ImportState::Ready(candidates)) = &mut self.import_review {
                    if let Some(c) = candidates.get_mut(i) {
                        c.selected = !c.selected;
                    }
                }
                Task::none()
            }
            Message::ImportConfirm => {
                let chosen: Vec<Payload> = match self.import_review.take() {
                    Some(ImportState::Ready(candidates)) => candidates
                        .into_iter()
                        .filter(|c| c.selected)
                        .map(|c| c.payload)
                        .collect(),
                    _ => Vec::new(),
                };
                if chosen.is_empty() {
                    return Task::none();
                }
                let count = chosen.len();
                for payload in chosen {
                    match payload {
                        Payload::Setting(d) => self.stored.settings.set(d.key, d.value),
                        Payload::Animation(leaf, a) => self.stored.animations.set(&leaf, a),
                    }
                }
                self.status = Some(format!("Imported {count} item(s)."));
                self.save_and_maybe_reload()
            }
            Message::ImportCancel => {
                self.import_review = None;
                Task::none()
            }
            Message::Reloaded(Ok(())) => {
                self.error = None;
                self.status = Some("Saved.".to_string());
                // What's running just changed, so the cached live values
                // are stale — most visibly right after a Reset, where a
                // row falls back to them.
                Task::batch([
                    Task::perform(read_live(), Message::LiveLoaded),
                    Task::perform(read_animations(), |(a, c)| Message::AnimationsLoaded(a, c)),
                ])
            }
            Message::Reloaded(Err(e)) => {
                self.status = None;
                self.error = Some(e);
                Task::none()
            }
        }
    }

    fn view(&self, scale: FontScale) -> Element<'_, Message> {
        if let Some(review) = &self.import_review {
            return self.import_view(review, scale);
        }

        let mut content = column![].spacing(spacing::LG).width(Length::Fill);

        if let Some(notice) = setup_notice(&self.config, "appearance settings", scale) {
            content = content.push(notice);
        }
        if let Some(reason) = &self.store_unreadable {
            content = content.push(section(
                "Your settings file couldn't be read",
                scale,
                column![
                    scaled_text(
                        "Nothing will be saved until this is fixed — writing over it \
                         would lose whatever it contains.",
                        13.0,
                        scale,
                    ),
                    meta_text(reason.clone(), 12.0, scale),
                ]
                .spacing(spacing::SM),
            ));
        }
        // Animations are checked alongside settings, not separately: both
        // are skipped by codegen when invalid, and a skipped animation
        // with no banner is a toggle that silently did nothing.
        let bad_animations = self.stored.animations.invalid(self.known_curves().as_deref());
        if !self.invalid.is_empty() || !bad_animations.is_empty() {
            let mut list = column![scaled_text(
                "These are in your appearance.toml but aren't being applied. Fix \
                 or remove them:",
                13.0,
                scale,
            )]
            .spacing(spacing::XS);
            for bad in &self.invalid {
                list = list.push(meta_text(
                    format!("{} — {}", bad.key, bad.problem),
                    12.0,
                    scale,
                ));
            }
            for (leaf, problem) in &bad_animations {
                list = list.push(meta_text(
                    format!("animation {leaf} — {problem}"),
                    12.0,
                    scale,
                ));
            }
            content = content.push(section("Settings being skipped", scale, list));
        }
        if let Some(e) = &self.error {
            content = content.push(section(
                "Something went wrong",
                scale,
                scaled_text(e.clone(), 13.0, scale),
            ));
        }
        if let Some(s) = &self.status {
            content = content.push(meta_text(s.clone(), 12.0, scale));
        }

        // No tab row: each tab is a page of its own in the sidebar now
        // (Appearance, Windows & workspaces, Animations), and the shell
        // picks the tab by sending `TabSelected`. Import moved to the
        // title row.

        // Shown on every tab whose rows the filter actually hides.
        // Without it on Animations, a filter typed under Windows silently
        // hid animation rows with no visible box explaining why.
        if self.tab != Tab::Theme {
            content = content.push(
                text_input("Filter settings…", &self.filter)
                    .on_input(Message::FilterChanged)
                    .padding(spacing::SM)
                    .style(hyprforge_ui::widgets::inset_input_style),
            );
        }

        content = match self.tab {
            Tab::Theme => content.push(self.theme_view(scale)),
            Tab::Windows => self.windows_view(content, scale),
            Tab::Animations => content.push(self.animations_view(scale)),
        };

        // No scrollable of its own: the shell owns the page's one scroll
        // area, and a second one inside it scrolled the page twice.
        container(content).padding(spacing::LG).width(Length::Fill).into()
    }
}

impl AppearanceModule {
    /// The gsettings half. Kept visually first because it's the part a
    /// user is most likely to be looking for, and because it's the part
    /// that makes the Hyprland colours below make sense.
    fn theme_view(&self, scale: FontScale) -> Element<'_, Message> {
        let mut body = column![meta_text(
            "These apply to GTK and Qt apps, not to Hyprland itself. Unlike \
             everything else on this screen they're shared with the rest of your \
             desktop — changing one takes effect immediately and there's no \
             generated file to undo it with.",
            12.0,
            scale,
        )]
        .spacing(spacing::SM);

        if let Some((key, previous)) = &self.desktop_undo {
            body = body.push(
                row![
                    scaled_text(
                        format!("{} was \u{201c}{previous}\u{201d}", label_for(key)),
                        13.0,
                        scale,
                    ),
                    secondary_button("Put it back")
                        .on_press(Message::DesktopUndo(key, previous.clone())),
                ]
                .spacing(spacing::MD)
                .align_y(iced::Alignment::Center),
            );
        }

        for setting in desktop::SETTINGS {
            body = body.push(divider());
            body = body.push(self.desktop_row(setting, scale));
        }
        section("Desktop theme", scale, body)
    }

    fn desktop_row(&self, setting: &'static DesktopSetting, scale: FontScale) -> Element<'_, Message> {
        let key = setting.key;
        let current = self.desktop.get(key).cloned();
        let shown = self
            .desktop_drafts
            .get(key)
            .cloned()
            .or_else(|| current.clone())
            .unwrap_or_default();

        let control: Element<'_, Message> = match setting.kind {
            DesktopKind::Enum(choices) => {
                let options: Vec<String> = choices.iter().map(|c| c.to_string()).collect();
                let selected = current.clone().filter(|c| options.contains(c));
                pick_list(options, selected, move |choice: String| {
                    Message::DesktopChosen(key, choice)
                })
                .into()
            }
            // A scan that hasn't returned, or found nothing, falls back
            // to a text field rather than an empty dropdown — a setting
            // that can't be edited because a scan failed is worse than
            // one that has to be typed.
            DesktopKind::Installed(catalogue)
                if !self.installed.for_catalogue(catalogue).is_empty() =>
            {
                let options = hyprforge_appearance::themes::options_including(
                    self.installed.for_catalogue(catalogue),
                    current.as_deref(),
                );
                let selected = current.clone().filter(|c| options.contains(c));
                pick_list(options, selected, move |choice: String| {
                    Message::DesktopChosen(key, choice)
                })
                .into()
            }
            DesktopKind::Font if !self.installed.fonts.is_empty() => {
                let (family, size) = desktop::split_font(current.as_deref().unwrap_or(""));
                let options = hyprforge_appearance::themes::options_including(
                    &self.installed.fonts,
                    Some(&family),
                );
                let selected = Some(family.clone()).filter(|f| options.contains(f));
                let shown_size = self
                    .font_sizes
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| size.map(|s| s.to_string()).unwrap_or_default());
                row![
                    pick_list(options, selected, move |choice: String| {
                        Message::FontFamilyChosen(key, choice)
                    }),
                    text_input("size", &shown_size)
                        .on_input(move |raw| Message::FontSizeChanged(key, raw))
                        .on_submit(Message::FontSizeSubmitted(key))
                        .padding(spacing::SM)
                        .width(Length::Fixed(70.0)),
                ]
                .spacing(spacing::SM)
                .align_y(iced::Alignment::Center)
                .into()
            }
            _ => text_input("", &shown)
                .on_input(move |raw| Message::DesktopDraftChanged(key, raw))
                .on_submit(Message::DesktopCommit(key))
                .padding(spacing::SM)
                .into(),
        };

        let mut label_side = column![scaled_text(setting.label, BASE_TEXT_SIZE, scale)].spacing(2);
        label_side = label_side.push(meta_text(setting.help, 12.0, scale));
        if let Some(problem) = self.desktop_errors.get(key) {
            label_side = label_side.push(scaled_text(problem.clone(), 12.0, scale));
        }
        if current.is_none() {
            label_side = label_side.push(meta_text("Not available on this desktop", 12.0, scale));
        }
        // Only while the contesting option is actually on. Warning
        // unconditionally would cry wolf at users who already turned it
        // off, and this screen is where they'd have turned it off.
        if let Some(contested) = setting.contested_by {
            let on = self
                .live
                .get(contested.option)
                .and_then(|l| l.value.as_bool())
                .unwrap_or(true);
            if on {
                label_side = label_side.push(scaled_text(contested.warning, 12.0, scale));
            }
        }

        let control_side: Element<'_, Message> =
            if self.desktop_drafts.contains_key(key) {
                row![
                    container(control).width(Length::Fill),
                    primary_button("Set").on_press(Message::DesktopCommit(key)),
                ]
                .spacing(spacing::SM)
                .align_y(iced::Alignment::Center)
                .into()
            } else {
                control
            };

        row![
            container(label_side).width(Length::FillPortion(2)),
            container(control_side).width(Length::FillPortion(3)),
        ]
        .spacing(spacing::MD)
        .align_y(iced::Alignment::Center)
        .into()
    }

    /// The Hyprland settings half, one section per catalog category.
    /// What this page writes, as the file will hold it, with the lines
    /// changed this session marked — the mockup's config preview.
    ///
    /// The generated Lua itself, not a summary of it: the suite's promise
    /// is that nothing rewrites a dotfile silently, and the file's own
    /// text is the only thing that proves that. Absent when the page
    /// writes nothing, rather than a box saying so.
    fn writes_block(&self, scale: FontScale) -> Option<Element<'_, Message>> {
        if self.stored.settings.is_empty() {
            return None;
        }
        let lua = hyprforge_appearance::apply::generate(&self.stored, self.known_curves().as_deref());
        let mut lines = column![config_line(home_relative(&appearance_lua_path()), scale)]
        .spacing(2.0);
        for (line, changed) in annotate_generated(&lua, &self.changed) {
            let mut shown = row![config_line(line, scale).color(hyprforge_ui::theme::text())]
                .spacing(spacing::SM);
            if changed {
                shown = shown.push(config_line("← changed", scale).color(hyprforge_ui::theme::success()));
            }
            lines = lines.push(shown);
        }
        Some(
            column![
                section_label("Writes", scale),
                container(lines)
                    .padding(spacing::MD)
                    .width(Length::Fill)
                    .style(|_t: &iced::Theme| iced::widget::container::Style {
                        background: Some(iced::Background::Color(
                            hyprforge_ui::theme::surface::sidebar(),
                        )),
                        border: iced::Border {
                            radius: hyprforge_ui::density::inner_radius().into(),
                            width: 1.0,
                            color: hyprforge_ui::theme::surface::card_border(),
                        },
                        ..iced::widget::container::Style::default()
                    }),
            ]
            .spacing(spacing::SM)
            .into(),
        )
    }

    fn windows_view<'a>(
        &'a self,
        mut content: iced::widget::Column<'a, Message>,
        scale: FontScale,
    ) -> iced::widget::Column<'a, Message> {
        if let Some(writes) = self.writes_block(scale) {
            content = content.push(writes);
        }

        let rows = self.rows();
        let mut any = false;
        for category in CATALOG.categories {
            let settings: Vec<&'static Setting> = CATALOG
                .in_category(category.key)
                .filter(|s| self.matches_filter(s))
                .collect();
            if settings.is_empty() {
                continue;
            }
            any = true;
            let body: Vec<Element<'_, Message>> = settings
                .into_iter()
                .enumerate()
                .map(|(i, setting)| rows.row(setting, i, scale))
                .collect();
            content = content.push(super::setting_rows::category_group(category.label, category.help, body, scale));
        }

        if !any {
            content = content.push(scaled_text(
                format!("Nothing matches “{}”.", self.filter.trim()),
                13.0,
                scale,
            ));
        }
        for (_, why) in CATALOG.unsupported {
            content = content.push(meta_text(*why, 12.0, scale));
        }
        content
    }

    fn animations_view(&self, scale: FontScale) -> Element<'_, Message> {
        if self.live_animations.is_empty() {
            return section(
                "Animations",
                scale,
                scaled_text(
                    "Couldn't read the animation list from Hyprland, so there's \
                     nothing to show. Anything already saved still applies.",
                    13.0,
                    scale,
                ),
            );
        }

        let curve_names: Vec<String> = self.curves.iter().map(|c| c.name.clone()).collect();
        let mut body = column![meta_text(
            "Speed is Hyprland's own unit — higher is faster. Curves are the ones \
             your config defines; Hyprforge doesn't write curves, only picks from \
             them.",
            12.0,
            scale,
        )]
        .spacing(spacing::SM);

        let mut any = false;
        for live in &self.live_animations {
            let leaf = live.leaf.clone();
            if !self.filter.trim().is_empty()
                && !leaf.to_lowercase().contains(&self.filter.trim().to_lowercase())
            {
                continue;
            }
            any = true;
            let (current, owned) = self.effective_animation(&leaf);
            let shown_speed = self
                .animation_drafts
                .get(&leaf)
                .cloned()
                .unwrap_or_else(|| format!("{}", current.speed));

            let mut label_side = column![scaled_text(leaf.clone(), BASE_TEXT_SIZE, scale)].spacing(2);
            if let Some(problem) = self.animation_errors.get(&leaf) {
                label_side = label_side.push(scaled_text(problem.clone(), 12.0, scale));
            }
            label_side = label_side.push(meta_text(
                if owned {
                    "Set by Hyprforge"
                } else if live.overridden {
                    "From your Hyprland config"
                } else {
                    "Hyprland default"
                },
                12.0,
                scale,
            ));

            let for_toggle = leaf.clone();
            let for_speed = leaf.clone();
            let for_submit = leaf.clone();
            let for_curve = leaf.clone();
            let mut controls = row![
                checkbox(current.enabled)
                    .on_toggle(move |b| Message::AnimationToggled(for_toggle.clone(), b)),
                text_input("speed", &shown_speed)
                    .on_input(move |raw| Message::AnimationSpeedChanged(for_speed.clone(), raw))
                    .on_submit(Message::AnimationSpeedSubmitted(for_submit.clone()))
                    .padding(spacing::SM)
                    .width(Length::Fixed(90.0)),
                pick_list(
                    curve_names.clone(),
                    Some(current.bezier.clone()).filter(|b| curve_names.contains(b)),
                    move |name: String| Message::AnimationCurveChosen(for_curve.clone(), name),
                ),
            ]
            .spacing(spacing::SM)
            .align_y(iced::Alignment::Center);

            if owned {
                controls = controls
                    .push(danger_button("Reset", Message::AnimationReset(leaf.clone())));
            }

            body = body.push(divider());
            body = body.push(
                row![
                    container(label_side).width(Length::FillPortion(2)),
                    container(controls).width(Length::FillPortion(3)),
                ]
                .spacing(spacing::MD)
                .align_y(iced::Alignment::Center),
            );
        }
        if !any {
            body = body.push(scaled_text(
                format!("No animation matches “{}”.", self.filter.trim()),
                13.0,
                scale,
            ));
        }
        section("Animations", scale, body)
    }

    /// Both halves of an import, reviewed together — "adopt what I already
    /// have" is one decision, not two.
    fn candidates(&self, result: &hyprforge_lua_import::ImportResult) -> Vec<Candidate> {
        let hyprforge_dir = hyprforge_core::paths::hypr_hyprforge_dir();
        let mut found = Vec::new();
        let mut animations = Vec::new();
        for call in &result.calls {
            // Hyprforge's own generated file is `require()`d from
            // hyprland.lua too, so it evaluates alongside the user's own.
            // Including it would offer Hyprforge's values back as if the
            // user had written them.
            if call.source_path.starts_with(&hyprforge_dir) {
                continue;
            }
            found.extend(hyprforge_core::hlconfig::import::settings_from_call(
                &call.kind, &call.args, &CATALOG,
            ));
            if let Some(pair) =
                hyprforge_appearance::animations::animation_from_call(&call.kind, &call.args)
            {
                animations.push(pair);
            }
        }

        let mut out: Vec<Candidate> = hyprforge_core::hlconfig::import::candidates_from_config(
            &found,
            &CATALOG,
            &self.stored.settings,
        )
        .into_iter()
        .map(|d| Candidate {
            label: d.label.to_string(),
            detail: setting_rows::render_for_edit(&d.value),
            selected: !d.already_owned,
            already_owned: d.already_owned,
            differs: d.differs,
            payload: Payload::Setting(d),
        })
        .collect();

        // Later calls win, same as Hyprland applies them.
        let resolved: BTreeMap<String, Animation> = animations.into_iter().collect();
        for (leaf, animation) in resolved {
            let stored = self.stored.animations.get(&leaf);
            out.push(Candidate {
                label: format!("Animation: {leaf}"),
                detail: format!("speed {}", animation.speed),
                selected: stored.is_none(),
                already_owned: stored.is_some(),
                differs: stored.is_some_and(|s| *s != animation),
                payload: Payload::Animation(leaf, animation),
            });
        }
        out
    }

    fn import_view(&self, review: &ImportState, scale: FontScale) -> Element<'_, Message> {
        let body: Element<'_, Message> = match review {
            ImportState::Running => {
                scaled_text("Reading your Hyprland config…", 13.0, scale).into()
            }
            ImportState::Ready(candidates) if candidates.is_empty() => column![
                scaled_text(
                    "Your config doesn't set any appearance options Hyprforge can \
                     take over, so there's nothing to import.",
                    13.0,
                    scale,
                ),
                secondary_button("Close").on_press(Message::ImportCancel),
            ]
            .spacing(spacing::MD)
            .into(),
            ImportState::Ready(candidates) => {
                let mut list = column![scaled_text(
                    "These are the appearance options your own Hyprland config sets. \
                     Importing one hands it to Hyprforge, which from then on writes \
                     it and wins over your config file — the line in your config \
                     stays where it is, it just stops being the one that decides.",
                    13.0,
                    scale,
                )]
                .spacing(spacing::SM);
                for (i, c) in candidates.iter().enumerate() {
                    let mut entry = column![row![
                        checkbox(c.selected).on_toggle(move |_| Message::ImportToggle(i)),
                        scaled_text(format!("{} — {}", c.label, c.detail), 13.0, scale),
                    ]
                    .spacing(spacing::SM)
                    .align_y(iced::Alignment::Center)]
                    .spacing(2);
                    if c.differs {
                        entry = entry.push(meta_text(
                            "Different from what Hyprforge has — importing replaces it.",
                            12.0,
                            scale,
                        ));
                    } else if c.already_owned {
                        entry = entry.push(meta_text("Already imported.", 12.0, scale));
                    }
                    list = list.push(entry);
                }
                column![
                    list,
                    row![
                        primary_button("Import selected").on_press(Message::ImportConfirm),
                        secondary_button("Cancel").on_press(Message::ImportCancel),
                    ]
                    .spacing(spacing::MD),
                ]
                .spacing(spacing::LG)
                .into()
            }
        };
        // The shell's scroll area holds this too; see `view`.
        container(section("Import from Hyprland", scale, body)).padding(spacing::LG).into()
    }
}

fn label_for(key: &str) -> &'static str {
    desktop::get_setting(key).map(|s| s.label).unwrap_or("Setting")
}

/// Validates before writing, because a gsettings write is immediate and
/// desktop-wide — there is no generated file to roll back.
fn write_desktop(module: &mut AppearanceModule, key: &'static str, value: String) -> Task<Message> {
    let Some(setting) = desktop::get_setting(key) else {
        return Task::none();
    };
    if let Err(problem) = desktop::check(setting, &value) {
        module.desktop_errors.insert(key, problem);
        return Task::none();
    }
    module.desktop_errors.remove(key);
    Task::perform(write_desktop_value(key, value), move |r| {
        Message::DesktopWritten(key, r)
    })
}

async fn write_desktop_value(
    key: &'static str,
    value: String,
) -> Result<Option<String>, String> {
    tokio::task::spawn_blocking(move || desktop::set(key, &value).map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())?
}

/// Scans for installed themes and fonts. Blocking filesystem work plus
/// an `fc-list`, so it goes on the blocking pool rather than stalling the
/// first frame.
async fn read_installed() -> Installed {
    tokio::task::spawn_blocking(|| Installed {
        gtk: Catalogue::GtkThemes.installed(),
        icons: Catalogue::IconThemes.installed(),
        cursors: Catalogue::CursorThemes.installed(),
        fonts: hyprforge_appearance::themes::font_families(),
    })
    .await
    .unwrap_or_default()
}

async fn read_monitors() -> Vec<String> {
    tokio::task::spawn_blocking(hyprforge_core::monitors::connector_names)
        .await
        .unwrap_or_default()
}

async fn read_desktop() -> Vec<(&'static str, String)> {
    tokio::task::spawn_blocking(|| desktop::read_all().unwrap_or_default())
        .await
        .unwrap_or_default()
}

async fn read_live() -> Vec<Live> {
    tokio::task::spawn_blocking(|| {
        hyprforge_core::hlconfig::import::live(&CATALOG).unwrap_or_default()
    })
    .await
    .unwrap_or_default()
}

async fn read_animations() -> (Vec<LiveAnimation>, Vec<Curve>) {
    tokio::task::spawn_blocking(|| {
        hyprforge_appearance::animations::live().unwrap_or_default()
    })
    .await
    .unwrap_or_default()
}

async fn regenerate_and_reload(
    appearance: Appearance,
    known_curves: Option<Vec<String>>,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        hyprforge_appearance::apply::apply(
            &appearance_lua_path(),
            &appearance,
            known_curves.as_deref(),
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

fn appearance_toml_path() -> std::path::PathBuf {
    hyprforge_core::paths::hyprforge_config_dir().join("appearance.toml")
}

/// `path` with the home directory written `~`, the way a user would type
/// it and the way the rest of the page names files.
fn home_relative(path: &std::path::Path) -> String {
    match std::env::var_os("HOME").map(std::path::PathBuf::from) {
        Some(home) => match path.strip_prefix(&home) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => path.display().to_string(),
        },
        None => path.display().to_string(),
    }
}

/// The generated Lua's lines worth showing, each with whether its key was
/// changed this session.
///
/// Leaves out the "do not edit" banner and blank lines — the block names
/// the file above them — and rebuilds each value line's full key from the
/// nested tables it sits in (`general = {` then `gaps_in = 6,` is
/// `general:gaps_in`), which is the one shape `codegen::generate` writes.
fn annotate_generated(
    lua: &str,
    changed: &std::collections::BTreeSet<&'static str>,
) -> Vec<(String, bool)> {
    let mut path: Vec<&str> = Vec::new();
    let mut out = Vec::new();
    for line in lua.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("--") {
            continue;
        }
        let mut is_changed = false;
        if let Some(name) = trimmed.strip_suffix("= {").map(str::trim) {
            path.push(name);
        } else if trimmed.starts_with('}') {
            path.pop();
        } else if let Some((name, _)) = trimmed.split_once('=') {
            let key = path.iter().copied().chain([name.trim()]).collect::<Vec<_>>().join(":");
            is_changed = changed.contains(key.as_str());
        }
        out.push((line.to_string(), is_changed));
    }
    out
}

fn appearance_lua_path() -> std::path::PathBuf {
    hyprforge_core::paths::hypr_hyprforge_dir().join("appearance.lua")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Writes block marks a line by the full key it writes, rebuilt
    /// from the nested tables around it — so `gaps_in` inside `general`
    /// is `general:gaps_in`, and a change to one key marks exactly its
    /// line, against the generator's real output.
    #[test]
    fn a_changed_key_marks_its_own_line_in_the_generated_lua() {
        let mut stored = Appearance::default();
        stored.settings.set("general:gaps_in", Value::Int(6));
        stored.settings.set("decoration:blur:passes", Value::Int(3));
        let lua = hyprforge_appearance::apply::generate(&stored, None);
        let changed: std::collections::BTreeSet<&'static str> = ["decoration:blur:passes"].into();

        let lines = annotate_generated(&lua, &changed);
        let marked: Vec<&str> = lines.iter().filter(|(_, c)| *c).map(|(l, _)| l.trim()).collect();
        assert_eq!(marked, ["passes = 3,"], "{lua}");
        assert!(lines.iter().any(|(l, c)| l.trim() == "gaps_in = 6," && !c));
        assert!(lines.iter().all(|(l, _)| !l.trim_start().starts_with("--")), "no banner");
    }

    /// A fresh AppearanceModule against an isolated config home and greeter
    /// export dir — see [`crate::modules::with_temp_env`].
    fn with_temp_config<T>(f: impl FnOnce(&mut AppearanceModule) -> T) -> T {
        crate::modules::with_temp_env(|_dir| {
            let (mut module, _) = AppearanceModule::new();
            f(&mut module)
        })
    }

    fn live_animation(leaf: &str, speed: f64, overridden: bool) -> LiveAnimation {
        LiveAnimation {
            leaf: leaf.to_string(),
            animation: Animation {
                enabled: true,
                speed,
                bezier: "easeOutQuint".to_string(),
                style: String::new(),
            },
            overridden,
        }
    }

    /// Three things on this screen write the look, and this one used to
    /// forget to republish it. `look::resolve` reads the font family and
    /// size straight from gsettings, so changing the system font restyled
    /// the Settings app and left the lock screen and the greeter on the
    /// old one — the drift this whole shared-theme arrangement exists to
    /// prevent, happening inside the suite.
    #[test]
    fn a_font_change_reaches_the_lock_screen_and_the_greeter() {
        with_temp_config(|m| {
            let published = hyprforge_paths::lock_toml_path();
            assert!(!published.exists(), "nothing should be published yet");

            let _ = m.update(Message::DesktopWritten("font-name", Ok(None)));

            assert!(
                published.exists(),
                "a font change must republish the look, or only this app follows it"
            );
        });
    }

    #[test]
    fn a_font_change_that_failed_publishes_nothing() {
        with_temp_config(|m| {
            let _ = m.update(Message::DesktopWritten("font-name", Err("nope".to_string())));
            assert!(
                !hyprforge_paths::lock_toml_path().exists(),
                "a write that failed has nothing to publish"
            );
        });
    }

    #[test]
    fn a_new_module_owns_nothing() {
        with_temp_config(|m| {
            assert!(m.stored.is_empty());
            assert!(!m.rows().owns("decoration:rounding"));
        });
    }

    #[test]
    fn setting_a_value_takes_ownership() {
        with_temp_config(|m| {
            let _ = m.update(Message::Set("decoration:rounding", Value::Int(12)));
            assert_eq!(
                m.stored.settings.get("decoration:rounding"),
                Some(&Value::Int(12))
            );
        });
    }

    /// Reset stops writing the key rather than writing the default, so
    /// the user's own config decides again.
    #[test]
    fn reset_gives_the_key_back_rather_than_writing_the_default() {
        with_temp_config(|m| {
            let _ = m.update(Message::Set("decoration:rounding", Value::Int(12)));
            let _ = m.update(Message::Reset("decoration:rounding"));
            assert!(!m.rows().owns("decoration:rounding"));
            let lua = hyprforge_appearance::apply::generate(&m.stored, None);
            assert!(!lua.contains("rounding"), "{lua}");
        });
    }

    /// A colour is typed, so it goes through the draft path — and an
    /// invalid one must be refused, since Hyprland rejects it and a
    /// rejected value takes the whole generated file down.
    #[test]
    fn a_malformed_colour_is_refused_with_the_expected_form() {
        with_temp_config(|m| {
            let _ = m.update(Message::DraftChanged(
                "general:col:active_border",
                "rgba(bd93f9)".into(),
            ));
            let _ = m.update(Message::ApplyDrafts);
            assert!(!m.rows().owns("general:col:active_border"));
            let problem = &m.draft_errors["general:col:active_border"];
            assert!(problem.contains("rgba"), "{problem}");
        });
    }

    #[test]
    fn a_valid_colour_is_accepted() {
        with_temp_config(|m| {
            let _ = m.update(Message::DraftChanged(
                "general:col:active_border",
                "rgba(bd93f9ff)".into(),
            ));
            let _ = m.update(Message::ApplyDrafts);
            assert_eq!(
                m.stored.settings.get("general:col:active_border"),
                Some(&Value::Text("rgba(bd93f9ff)".into()))
            );
        });
    }

    /// Touching one animation control must not silently reset the
    /// others — taking ownership seeds the leaf from what's running.
    #[test]
    fn owning_an_animation_keeps_its_other_values() {
        with_temp_config(|m| {
            let _ = m.update(Message::AnimationsLoaded(
                vec![live_animation("windows", 4.79, true)],
                Vec::new(),
            ));
            let _ = m.update(Message::AnimationToggled("windows".into(), false));
            let stored = m.stored.animations.get("windows").unwrap();
            assert!(!stored.enabled);
            assert_eq!(stored.speed, 4.79, "speed must survive a toggle");
            assert_eq!(stored.bezier, "easeOutQuint", "curve must survive a toggle");
        });
    }

    #[test]
    fn an_animation_speed_commits_on_submit_not_per_keystroke() {
        with_temp_config(|m| {
            let _ = m.update(Message::AnimationsLoaded(
                vec![live_animation("fade", 3.0, false)],
                Vec::new(),
            ));
            let _ = m.update(Message::AnimationSpeedChanged("fade".into(), "5.5".into()));
            assert!(m.stored.animations.get("fade").is_none(), "still a draft");
            let _ = m.update(Message::AnimationSpeedSubmitted("fade".into()));
            assert_eq!(m.stored.animations.get("fade").unwrap().speed, 5.5);
        });
    }

    /// Hyprland refuses a speed of zero or less, and NaN would render as
    /// a syntax error taking the whole file with it.
    #[test]
    fn an_unusable_animation_speed_is_refused() {
        with_temp_config(|m| {
            let _ = m.update(Message::AnimationsLoaded(
                vec![live_animation("fade", 3.0, false)],
                Vec::new(),
            ));
            for bad in ["0", "-2", "nonsense"] {
                let _ = m.update(Message::AnimationSpeedChanged("fade".into(), bad.into()));
                let _ = m.update(Message::AnimationSpeedSubmitted("fade".into()));
                assert!(m.stored.animations.get("fade").is_none(), "{bad} was accepted");
                assert!(m.animation_errors.contains_key("fade"), "{bad}");
            }
        });
    }

    /// The defect a review pass found: `hyprctl` reports speed 0 for
    /// every un-overridden leaf, Hyprland refuses `speed = 0`, and codegen
    /// skips what it can't write — so toggling any of the 18 unconfigured
    /// animations on a typical config silently did nothing.
    #[test]
    fn toggling_an_unconfigured_animation_writes_a_usable_speed() {
        with_temp_config(|m| {
            let _ = m.update(Message::AnimationsLoaded(
                vec![
                    live_animation("global", 10.0, true),
                    live_animation("windows", 4.79, true),
                    // What the compositor really reports for a leaf
                    // nothing has overridden.
                    live_animation("windowsMove", 0.0, false),
                ],
                Vec::new(),
            ));
            let _ = m.update(Message::AnimationToggled("windowsMove".into(), false));

            let stored = m.stored.animations.get("windowsMove").unwrap();
            assert!(stored.speed > 0.0, "speed {} is unwritable", stored.speed);
            assert_eq!(stored.speed, 4.79, "should inherit from `windows`");
            assert_eq!(
                m.stored.animations.invalid(None),
                vec![],
                "nothing may be stored that codegen would skip"
            );
            let lua = hyprforge_appearance::apply::generate(&m.stored, None);
            assert!(lua.contains("windowsMove"), "the toggle must reach the file: {lua}");
        });
    }

    /// A row for an inheriting leaf shows the speed it would inherit, not
    /// a bare 0 — which would read as "this animation takes no time".
    #[test]
    fn an_inheriting_row_shows_the_speed_it_would_inherit() {
        with_temp_config(|m| {
            let _ = m.update(Message::AnimationsLoaded(
                vec![
                    live_animation("global", 10.0, true),
                    live_animation("fadeDpms", 0.0, false),
                ],
                Vec::new(),
            ));
            let (shown, owned) = m.effective_animation("fadeDpms");
            assert!(!owned);
            assert_eq!(shown.speed, 10.0);
        });
    }

    /// A curve can vanish without the user touching Hyprforge — delete
    /// an `hl.curve` line from your own config and a stored animation
    /// still names it. Hyprland refuses with "no such bezier", which
    /// aborts the whole file, so every other appearance setting would
    /// stop applying too.
    #[test]
    fn an_animation_naming_a_deleted_curve_is_surfaced_and_skipped() {
        with_temp_config(|m| {
            let _ = m.update(Message::AnimationsLoaded(
                vec![live_animation("windows", 4.79, true)],
                vec![Curve { name: "linear".into(), points: (0.0, 0.0, 1.0, 1.0) }],
            ));
            m.stored.animations.set(
                "windows",
                Animation {
                    enabled: true,
                    speed: 4.0,
                    bezier: "easeOutQuint".into(),
                    style: String::new(),
                },
            );
            let problems = m.stored.animations.invalid(m.known_curves().as_deref());
            assert_eq!(problems.len(), 1);
            assert!(problems[0].1.contains("easeOutQuint"), "{}", problems[0].1);
            // Driven off exactly this, so building the view proves the
            // banner is reachable.
            let _ = m.view(FontScale::default());
        });
    }

    /// With no curve list read, the check is skipped rather than failing
    /// every animation — otherwise an unreachable compositor would blank
    /// the whole section.
    #[test]
    fn an_unread_curve_list_does_not_invalidate_animations() {
        with_temp_config(|m| {
            assert!(m.known_curves().is_none(), "no curves read yet");
            m.stored.animations.set(
                "windows",
                Animation {
                    enabled: true,
                    speed: 4.0,
                    bezier: "whatever".into(),
                    style: String::new(),
                },
            );
            assert_eq!(m.stored.animations.invalid(m.known_curves().as_deref()), vec![]);
        });
    }

    /// A stored animation codegen would skip has to be visible, or the
    /// user sees a control that does nothing with no explanation.
    #[test]
    fn an_unwritable_animation_is_surfaced_on_screen() {
        with_temp_config(|m| {
            m.stored.animations.set(
                "windows",
                Animation {
                    enabled: true,
                    speed: 0.0,
                    bezier: String::new(),
                    style: String::new(),
                },
            );
            assert_eq!(m.stored.animations.invalid(None).len(), 1);
            // The banner is driven off exactly this, so building the view
            // is what proves it's reachable.
            let _ = m.view(FontScale::default());
        });
    }

    #[test]
    fn an_animation_reset_hands_the_leaf_back() {
        with_temp_config(|m| {
            let _ = m.update(Message::AnimationsLoaded(
                vec![live_animation("fade", 3.0, false)],
                Vec::new(),
            ));
            let _ = m.update(Message::AnimationToggled("fade".into(), false));
            assert!(m.stored.animations.get("fade").is_some());
            let _ = m.update(Message::AnimationReset("fade".into()));
            assert!(m.stored.animations.get("fade").is_none());
        });
    }

    /// The rule that keeps the data-loss bug from returning.
    #[test]
    fn an_unreadable_store_blocks_saving() {
        with_temp_config(|m| {
            m.store_unreadable = Some("bad toml".to_string());
            let _ = m.update(Message::Set("decoration:rounding", Value::Int(12)));
            let saved = hyprforge_appearance::storage::load(&appearance_toml_path()).unwrap();
            assert!(saved.is_empty(), "nothing may be written over an unreadable store");
            assert!(m.error.is_some(), "and the user has to be told why");
        });
    }

    #[test]
    fn a_readable_store_still_saves() {
        with_temp_config(|m| {
            let _ = m.update(Message::Set("decoration:rounding", Value::Int(12)));
            let saved = hyprforge_appearance::storage::load(&appearance_toml_path()).unwrap();
            assert_eq!(saved.settings.get("decoration:rounding"), Some(&Value::Int(12)));
        });
    }

    fn config_result(file: &str, calls: Vec<serde_json::Value>) -> hyprforge_lua_import::ImportResult {
        hyprforge_lua_import::ImportResult {
            calls: calls
                .into_iter()
                .map(|body| hyprforge_lua_import::RecordedCall {
                    kind: if body.get("leaf").is_some() { "animation" } else { "config" }
                        .to_string(),
                    source_path: hyprforge_core::paths::hypr_config_dir().join(file),
                    line: Some(1),
                    args: vec![body],
                })
                .collect(),
            failures: Vec::new(),
        }
    }

    /// Settings and animations are reviewed in one list, because "adopt
    /// what I already have" is one decision.
    #[test]
    fn import_offers_settings_and_animations_together() {
        with_temp_config(|m| {
            let _ = m.update(Message::ImportEvaluated(config_result(
                "hyprland.lua",
                vec![
                    serde_json::json!({ "decoration": { "rounding": 10 } }),
                    serde_json::json!({ "leaf": "windows", "speed": 4.79, "bezier": "easeOutQuint" }),
                ],
            )));
            let Some(ImportState::Ready(candidates)) = &m.import_review else {
                panic!("expected a review");
            };
            assert_eq!(candidates.len(), 2);
            assert!(candidates.iter().any(|c| c.label.contains("Animation: windows")));
            let _ = m.update(Message::ImportConfirm);
            assert_eq!(m.stored.settings.get("decoration:rounding"), Some(&Value::Int(10)));
            assert_eq!(m.stored.animations.get("windows").unwrap().speed, 4.79);
        });
    }

    /// Hyprforge's own generated file evaluates alongside the user's, so
    /// its values must never come back as importable.
    #[test]
    fn hyprforges_own_generated_file_is_never_offered_for_import() {
        with_temp_config(|m| {
            let mut result = config_result(
                "hyprland.lua",
                vec![serde_json::json!({ "decoration": { "rounding": 10 } })],
            );
            result.calls.push(hyprforge_lua_import::RecordedCall {
                kind: "config".to_string(),
                source_path: appearance_lua_path(),
                line: Some(5),
                args: vec![serde_json::json!({ "decoration": { "blur": { "passes": 9 } } })],
            });
            let _ = m.update(Message::ImportEvaluated(result));
            let Some(ImportState::Ready(candidates)) = &m.import_review else {
                panic!("expected a review");
            };
            assert_eq!(candidates.len(), 1);
            assert!(!candidates[0].label.contains("passes"));
        });
    }

    #[test]
    fn cancelling_an_import_changes_nothing() {
        with_temp_config(|m| {
            let _ = m.update(Message::ImportEvaluated(config_result(
                "hyprland.lua",
                vec![serde_json::json!({ "decoration": { "rounding": 10 } })],
            )));
            let _ = m.update(Message::ImportCancel);
            assert!(m.stored.is_empty());
            assert!(m.import_review.is_none());
        });
    }

    /// gsettings is written immediately and desktop-wide, so a bad value
    /// must be caught before the write rather than after.
    #[test]
    fn an_invalid_desktop_value_is_refused_before_writing() {
        with_temp_config(|m| {
            let _ = m.update(Message::DesktopDraftChanged("cursor-size", "enormous".into()));
            let _ = m.update(Message::DesktopCommit("cursor-size"));
            assert!(m.desktop_errors.contains_key("cursor-size"));

            let _ = m.update(Message::DesktopDraftChanged("gtk-theme", "  ".into()));
            let _ = m.update(Message::DesktopCommit("gtk-theme"));
            assert!(m.desktop_errors.contains_key("gtk-theme"));
        });
    }

    fn installed() -> Installed {
        Installed {
            gtk: vec!["Adwaita".into(), "Dracula".into()],
            icons: vec!["Adwaita".into(), "Dracula".into()],
            cursors: vec!["Dracula-cursors".into()],
            fonts: vec!["Cantarell".into(), "Noto Sans".into()],
        }
    }

    /// Picking a theme writes it straight through — there is no draft
    /// step, because choosing from a list is already a complete decision.
    #[test]
    fn choosing_an_installed_theme_writes_it() {
        with_temp_config(|m| {
            let _ = m.update(Message::InstalledLoaded(installed()));
            let _ = m.update(Message::DesktopLoaded(vec![("gtk-theme", "Adwaita".into())]));
            // The write itself goes through gsettings, which the test
            // can't do; what's checked is that it was accepted rather
            // than refused by validation.
            let _ = m.update(Message::DesktopChosen("gtk-theme", "Dracula".into()));
            assert!(!m.desktop_errors.contains_key("gtk-theme"));
        });
    }

    /// Changing the family must not resize the interface, and changing
    /// the size must not change the font. They're one gsettings string,
    /// so each has to preserve the other half.
    #[test]
    fn a_font_family_and_size_are_edited_independently() {
        with_temp_config(|m| {
            let _ = m.update(Message::InstalledLoaded(installed()));
            let _ = m.update(Message::DesktopLoaded(vec![("font-name", "Noto Sans  10".into())]));

            let (family, size) = desktop::split_font("Noto Sans  10");
            assert_eq!((family.as_str(), size), ("Noto Sans", Some(10)));
            assert_eq!(desktop::join_font("Cantarell", size), "Cantarell 10");
            assert_eq!(desktop::join_font(&family, Some(12)), "Noto Sans 12");
        });
    }

    #[test]
    fn a_bad_font_size_is_refused_rather_than_written() {
        with_temp_config(|m| {
            let _ = m.update(Message::InstalledLoaded(installed()));
            let _ = m.update(Message::DesktopLoaded(vec![("font-name", "Noto Sans 10".into())]));
            for bad in ["0", "huge", "-3"] {
                let _ = m.update(Message::FontSizeChanged("font-name", bad.into()));
                let _ = m.update(Message::FontSizeSubmitted("font-name"));
                assert!(m.desktop_errors.contains_key("font-name"), "{bad} was accepted");
            }
        });
    }

    /// A theme installed somewhere the scan doesn't look would otherwise
    /// vanish from its own picker, leaving nothing selected — and the
    /// next click would replace a working setting.
    #[test]
    fn a_theme_outside_the_scan_is_still_offered() {
        let options = hyprforge_appearance::themes::options_including(
            &installed().gtk,
            Some("Custom-Theme"),
        );
        assert!(options.contains(&"Custom-Theme".to_string()));
    }

    /// A scan that hasn't returned must not leave a setting uneditable —
    /// the row falls back to a text field.
    #[test]
    fn rows_still_render_before_the_scan_returns() {
        with_temp_config(|m| {
            assert!(m.installed.gtk.is_empty());
            let _ = m.view(FontScale::default());
            let _ = m.update(Message::InstalledLoaded(installed()));
            let _ = m.view(FontScale::default());
        });
    }

    /// With `cursor:sync_gsettings_theme` on — Hyprland's default —
    /// Hyprland pushes its own cursor theme into gsettings on every theme
    /// load, so a value set here is overwritten on the next reload. The
    /// row has to say so, or the screen looks broken rather than
    /// contested.
    #[test]
    fn a_contested_desktop_key_warns_only_while_it_is_contested() {
        let cursor = desktop::get_setting("cursor-theme").unwrap();
        let sync = cursor.contested_by.expect("cursor theme is contested");
        assert_eq!(sync.option, "cursor:sync_gsettings_theme");
        assert!(sync.warning.contains("overwritten"), "{}", sync.warning);

        // Uncontested keys must not carry a warning — crying wolf on
        // every row would make the real one invisible.
        assert!(desktop::get_setting("gtk-theme").unwrap().contested_by.is_none());
    }

    /// A row for a key this desktop's schema lacks has to say so rather
    /// than showing an empty box that silently does nothing.
    #[test]
    fn a_missing_desktop_key_still_renders() {
        with_temp_config(|m| {
            m.desktop.clear();
            let _ = m.view(FontScale::default());
        });
    }

    /// The filter hides animation rows too, so a filter typed under
    /// Windows was silently hiding them with no visible box to explain
    /// why. Both tabs that filter must show the control.
    #[test]
    fn a_filter_typed_on_one_tab_stays_visible_on_the_other() {
        with_temp_config(|m| {
            let _ = m.update(Message::AnimationsLoaded(
                vec![live_animation("windows", 4.79, true)],
                Vec::new(),
            ));
            let _ = m.update(Message::TabSelected(Tab::Windows));
            let _ = m.update(Message::FilterChanged("nothing matches this".into()));
            // Switching tabs keeps the filter, so the box has to come
            // with it — and the empty result has to say so.
            let _ = m.update(Message::TabSelected(Tab::Animations));
            assert_eq!(m.filter, "nothing matches this");
            let _ = m.view(FontScale::default());
        });
    }

    /// Builds the whole screen in each state it can be in.
    #[test]
    fn the_screen_builds_in_every_state() {
        with_temp_config(|m| {
            let scale = FontScale::default();
            for tab in Tab::ALL {
                let _ = m.update(Message::TabSelected(tab));
                let _ = m.view(scale);
            }

            let _ = m.update(Message::AnimationsLoaded(
                vec![live_animation("windows", 4.79, true)],
                vec![Curve { name: "linear".into(), points: (0.0, 0.0, 1.0, 1.0) }],
            ));
            let _ = m.update(Message::DesktopLoaded(vec![("gtk-theme", "Dracula".into())]));
            let _ = m.update(Message::TabSelected(Tab::Animations));
            let _ = m.view(scale);

            m.store_unreadable = Some("bad toml".into());
            m.error = Some("something failed".into());
            m.status = Some("saved".into());
            m.stored.settings.set("decoration:rounding", Value::Text("huge".into()));
            m.invalid = m.stored.settings.validate(&CATALOG);
            let _ = m.view(scale);

            m.store_unreadable = None;
            m.filter = "nothing matches this".into();
            let _ = m.update(Message::TabSelected(Tab::Windows));
            let _ = m.view(scale);
            m.filter = String::new();

            m.import_review = Some(ImportState::Running);
            let _ = m.view(scale);
            m.import_review = Some(ImportState::Ready(Vec::new()));
            let _ = m.view(scale);
        });
    }

    /// Every catalogued setting has to render — a kind the row builder
    /// doesn't handle would show as a blank row in front of a user.
    #[test]
    fn every_catalogued_setting_produces_a_row() {
        with_temp_config(|m| {
            let rows = m.rows();
            for setting in CATALOG.settings {
                let _ = rows.row(setting, 0, FontScale::default());
            }
        });
    }
}

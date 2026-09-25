//! One screen for any catalogue of `hl.config` settings.
//!
//! Input and System are the same screen with different catalogues, so
//! this is that screen once. Everything specific to a module — which
//! options exist, where its files live, what extra pickers it offers —
//! comes from a [`Catalogued`] implementation.
//!
//! The screen is generated from the catalogue rather than hand-laid-out:
//! the catalogue already knows every option's type, range and accepted
//! values, and a hand-written form would be a second, drifting copy of
//! all three. Adding an option means adding a catalogue entry and
//! nothing else.
//!
//! The central idea shows up directly in the UI: a setting is either
//! **owned** — Hyprforge writes it, and it wins — or absent, in which
//! case Hyprland's default or the user's own config decides. Every row
//! can be handed back with its Reset button, which is not the same as
//! setting it to the default value: one stops mentioning the key, the
//! other writes it.

use hyprforge_core::lua_setup;
use hyprforge_ui::theme::{spacing, FontScale};
use hyprforge_ui::widgets::{
    divider, meta_text, primary_button, scaled_text, secondary_button, section,
};
use super::setting_rows::{self, DynChoice, RowContext};
use crate::modules::setup_notice::setup_notice;
use crate::module::SettingsModule;
use hyprforge_core::hlconfig::Catalog;
use hyprforge_core::hlconfig::Setting;
use hyprforge_core::lua_setup::{Placement, SetupError};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use hyprforge_core::hlconfig::import::{Discovered, Live};
use hyprforge_core::hlconfig::{Invalid, Settings, Value};
use hyprforge_core::lua_setup::{HyprConfig, SetupPlan};
use iced::widget::{checkbox, column, container, row, text_input};
use iced::{Element, Length, Task};
use std::collections::BTreeMap;

/// Parses every pending draft into `settings`, reporting which ones
/// could not be parsed.
///
/// All-or-nothing per field: a field that does not parse keeps its
/// draft and its error, and the ones that do parse are still applied,
/// so one typo does not discard everything else that was typed.
///
/// A free function over the pieces rather than a method, because the
/// Appearance screen does exactly this over a `Settings` that lives
/// inside its own store rather than directly on the screen — it had a
/// line-for-line copy of it, differing only in where the settings were
/// reached and how the catalogue was named.
///
/// Returns whether anything was applied, which is what tells a caller
/// whether there is something to save.
pub(crate) fn apply_drafts(
    settings: &mut Settings,
    catalog: &'static Catalog,
    drafts: &mut BTreeMap<&'static str, String>,
    draft_errors: &mut BTreeMap<&'static str, String>,
) -> bool {
    draft_errors.clear();
    let pending: Vec<(&'static str, String)> = drafts.iter().map(|(k, v)| (*k, v.clone())).collect();
    let mut applied = false;
    for (key, raw) in pending {
        let Some(setting) = catalog.get(key) else {
            continue;
        };
        match setting_rows::parse_for(&setting.kind, &raw) {
            Ok(value) => {
                let problems = Settings::from_one(key, value.clone()).validate(catalog);
                if let Some(problem) = problems.first() {
                    draft_errors.insert(key, problem.problem.clone());
                } else {
                    settings.set(key, value);
                    drafts.remove(key);
                    applied = true;
                }
            }
            Err(problem) => {
                draft_errors.insert(key, problem);
            }
        }
    }
    applied
}

/// Whether a setting should be shown for the text in the filter box, so
/// a screen with fifty-odd options is still navigable.
///
/// Matches the label, the key and the help text: someone looking for
/// "natural scroll" and someone looking for `kb_options` both find what
/// they mean.
pub(crate) fn matches_filter(filter: &str, setting: &Setting) -> bool {
    let q = filter.trim().to_lowercase();
    q.is_empty()
        || setting.label.to_lowercase().contains(&q)
        || setting.key.to_lowercase().contains(&q)
        || setting.help.to_lowercase().contains(&q)
}

/// Everything a catalogue-backed screen needs that isn't the screen.
pub trait Catalogued: 'static {
    /// Named in error and status messages: "your {SUBJECT} couldn't be
    /// read".
    const SUBJECT: &'static str;
    /// The file the user edits by hand, for the message above.
    const STORE: &'static str;

    fn catalog() -> &'static Catalog;
    fn require_line() -> &'static str;
    fn placement() -> Placement;
    fn toml_path() -> PathBuf;
    fn lua_path() -> PathBuf;
    fn generate(settings: &Settings) -> String;
    fn apply(lua_path: &Path, settings: &Settings) -> Result<(), String>;
    fn install(hyprland_lua: &Path) -> Result<SetupPlan, SetupError>;

    /// Pickers built from what's installed on this machine — keyboard
    /// layouts, connected monitors. Rebuilt whenever the live values or
    /// the stored settings change, because some depend on both (the
    /// variants offered depend on the chosen layout).
    ///
    /// Empty by default: a catalogue with nothing discoverable gets text
    /// fields, which is the right answer rather than an empty dropdown.
    fn choices(
        _settings: &Settings,
        _live: &BTreeMap<&'static str, Live>,
        _installed: &Installed,
    ) -> BTreeMap<&'static str, Vec<DynChoice>> {
        BTreeMap::new()
    }

    /// Anything scanned once at startup and handed to [`Self::choices`].
    fn discover() -> Installed {
        Installed::default()
    }
}

/// What a module's [`Catalogued::discover`] found. Named `Installed` to
/// leave `Discovered` to `hlconfig::import`, which uses it for import
/// candidates.
#[derive(Debug, Clone, Default)]
pub struct Installed {
    pub xkb: hyprforge_input::xkb::Catalogue,
    pub monitors: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum Message {
    /// A control whose value is complete the moment it changes — a
    /// checkbox or a dropdown. Applied immediately, the way every settings
    /// app behaves.
    Set(&'static str, Value),
    /// A character typed into a text or number field. Held as a draft, not
    /// applied: applying per keystroke would reload Hyprland once per
    /// character.
    DraftChanged(&'static str, String),
    /// Enter in a single field, or the Apply button for all of them.
    ApplyDrafts,
    DiscardDrafts,
    /// Stop writing this key at all, giving it back to Hyprland and the
    /// user's own config.
    Reset(&'static str),
    FilterChanged(String),
    ImportOpen,
    /// The user's own config, evaluated. Import reads the config file, not
    /// the running compositor — see `hyprforge_input::import` for why.
    ImportEvaluated(hyprforge_lua_import::ImportResult),
    ImportToggle(usize),
    ImportConfirm,
    ImportCancel,
    /// The compositor's current value for every option, read once on open.
    LiveLoaded(Vec<Live>),
    /// Everything a picker on this screen is built from, scanned once.
    ChoicesLoaded(Installed),
    Reloaded(Result<(), String>),
}

/// An import run, from click to review.
pub enum ImportState {
    /// The user's config is being evaluated.
    Running,
    Ready(Vec<Candidate>),
}

pub struct Candidate {
    pub found: Discovered,
    pub selected: bool,
}

pub struct CatalogScreen<M: Catalogued> {
    // Crate-visible: the Input and System modules' tests drive this
    // screen directly, which is where its behaviour is described.
    pub(crate) settings: Settings,
    /// In-progress text for the fields that can't be committed per
    /// keystroke. Keyed by catalog key; absent means "showing the stored
    /// value".
    pub(crate) drafts: BTreeMap<&'static str, String>,
    /// Per-key reason the draft can't be applied, so a bad number is
    /// reported next to the field that holds it rather than as one vague
    /// banner at the top.
    pub(crate) draft_errors: BTreeMap<&'static str, String>,
    pub(crate) filter: String,
    pub(crate) config: HyprConfig,
    pub(crate) setup_plan: SetupPlan,
    pub(crate) error: Option<String>,
    pub(crate) status: Option<String>,
    /// Why the stored settings couldn't be read, if they couldn't. While
    /// set, the module refuses to write anything — an unreadable store is
    /// not an empty one, and saving over it would destroy what's there.
    pub(crate) store_unreadable: Option<String>,
    /// Stored values the catalog rejects, from a hand-edited file. Shown
    /// rather than dropped: they're skipped by codegen, so without this the
    /// user would see a setting in their TOML quietly doing nothing.
    pub(crate) invalid: Vec<Invalid>,
    pub(crate) import_review: Option<ImportState>,
    /// What Hyprland currently has for each option, and whether the user's
    /// own config is what put it there.
    ///
    /// Without this a row for an unowned key falls back to the *catalog*
    /// default, which is a different number from what's running whenever
    /// the user's config sets it — the screen would show `numlock` off
    /// while it's on. Empty until the read returns, and if it fails the
    /// rows fall back to the catalog default and say so.
    pub(crate) live: BTreeMap<&'static str, Live>,
    /// Discovered options per setting. Empty until the scan returns, and
    /// a setting with no entry falls back to a text field — a field that
    /// can't be edited because a scan failed is worse than one that has
    /// to be typed.
    pub(crate) choices: BTreeMap<&'static str, Vec<DynChoice>>,
    /// Kept so pickers can be rebuilt when the settings change — the
    /// variant list depends on the chosen layout.
    pub(crate) installed: Installed,
    module: PhantomData<M>,
}

impl<M: Catalogued> CatalogScreen<M> {
    pub fn new() -> (Self, Task<Message>) {
        // A failure here must never look like "you've configured nothing":
        // that reading is what turns one bad parse into a wiped store on
        // the next save.
        let (settings, store_unreadable) =
            match hyprforge_core::hlconfig::storage::load(&M::toml_path()) {
                Ok(settings) => (settings, None),
                Err(e) => (Settings::default(), Some(e.to_string())),
            };
        let invalid = settings.validate(M::catalog());
        let setup = lua_setup::bootstrap(
            &hyprforge_core::paths::hypr_config_dir(),
            &hyprforge_core::paths::hyprland_lua_path(),
            lua_setup::ModuleSetup {
                require_line: M::require_line(),
                placement: M::placement(),
                generated: (
                    M::lua_path(),
                    M::generate(&Settings::default()),
                ),
            },
        );
        (
            CatalogScreen::<M> {
                settings,
                drafts: BTreeMap::new(),
                draft_errors: BTreeMap::new(),
                filter: String::new(),
                config: setup.config,
                setup_plan: setup.plan,
                error: setup.error,
                status: None,
                store_unreadable,
                invalid,
                import_review: None,
                live: BTreeMap::new(),
                choices: BTreeMap::new(),
                installed: Installed::default(),
                module: PhantomData,
            },
            // Read what's actually running, so unowned rows show the truth
            // rather than the catalog default. A failure is silent by
            // design: the rows still render, they just can't claim to know
            // what's live.
            Task::batch([
                Task::perform(read_live::<M>(), Message::LiveLoaded),
                Task::perform(read_choices::<M>(), Message::ChoicesLoaded),
            ]),
        )
    }

    /// Writes the canonical TOML. Nothing irreversible may run unless this
    /// returned `Ok` — the ordering rule that exists because doing it the
    /// other way round cost a real user 37 hand-written binds.
    fn persist(&mut self) -> Result<(), String> {
        if let Some(reason) = &self.store_unreadable {
            let message = format!(
                "Not saving — your {} couldn't be read, and overwriting it would \
                 lose whatever is in it. ({reason})",
                M::STORE
            );
            self.error = Some(message.clone());
            return Err(message);
        }
        hyprforge_core::hlconfig::storage::save(
            &M::toml_path(),
            &self.settings,
        )
        .map_err(|e| {
            self.error = Some(e.to_string());
            e.to_string()
        })
    }

    fn save_and_maybe_reload(&mut self) -> Task<Message> {
        if self.persist().is_err() {
            return Task::none();
        }
        self.invalid = self.settings.validate(M::catalog());
        // Without a Lua config there's nothing to source the generated file
        // from, so reloading would be a no-op dressed up as success. The
        // TOML is still saved, and takes effect once setup is resolved.
        if !matches!(self.config, HyprConfig::Lua(_)) {
            self.error = None;
            self.status = Some(
                "Saved. Settings take effect once Hyprland setup is finished — see above."
                    .to_string(),
            );
            return Task::none();
        }
        // The require line is installed automatically on open; this only
        // retries if that attempt failed.
        if self.setup_plan != SetupPlan::AlreadyPresent {
            match M::install(&hyprforge_core::paths::hyprland_lua_path()) {
                Ok(plan) => self.setup_plan = plan,
                Err(e) => {
                    self.error = Some(e.to_string());
                    return Task::none();
                }
            }
        }
        Task::perform(regenerate_and_reload::<M>(self.settings.clone()), Message::Reloaded)
    }

    /// Parses every pending draft into the store. All-or-nothing per field:
    /// a field that doesn't parse keeps its draft and its error, and the
    /// ones that do parse are still applied, so one typo doesn't discard
    /// everything else the user typed.
    fn apply_drafts(&mut self) -> Task<Message> {
        let applied = apply_drafts(
            &mut self.settings,
            M::catalog(),
            &mut self.drafts,
            &mut self.draft_errors,
        );
        match applied {
            true => self.save_and_maybe_reload(),
            false => Task::none(),
        }
    }

    /// Whether a setting is shown, for the text currently in the filter
    /// box — see [`matches_filter`].
    pub(crate) fn matches_filter(&self, setting: &Setting) -> bool {
        matches_filter(&self.filter, setting)
    }
}

impl<M: Catalogued> SettingsModule for CatalogScreen<M> {
    type Message = Message;

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
            Message::Set(key, value) => {
                self.settings.set(key, value);
                self.status = None;
                // Some pickers depend on other settings — the keyboard
                // variants offered depend on the chosen layout.
                self.rebuild_choices();
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
                Task::none()
            }
            Message::Reset(key) => {
                self.settings.clear(key);
                self.drafts.remove(key);
                self.draft_errors.remove(key);
                self.status = None;
                self.save_and_maybe_reload()
            }
            Message::FilterChanged(q) => {
                self.filter = q;
                Task::none()
            }
            Message::ImportOpen => {
                self.import_review = Some(ImportState::Running);
                Task::perform(super::evaluate_user_config(), Message::ImportEvaluated)
            }
            Message::ImportEvaluated(result) => {
                let hyprforge_dir = hyprforge_core::paths::hypr_hyprforge_dir();
                let mut found = Vec::new();
                for call in &result.calls {
                    // Hyprforge's own generated file is `require()`d from
                    // hyprland.lua too, so it evaluates right alongside the
                    // user's own. Excluded here — including it would offer
                    // Hyprforge's own values back as if the user had
                    // written them, which is exactly how a stray
                    // `touchdevice:enabled = false` from a test run got
                    // imported and persisted on a real machine.
                    if call.source_path.starts_with(&hyprforge_dir) {
                        continue;
                    }
                    found.extend(hyprforge_core::hlconfig::import::settings_from_call(
                        &call.kind,
                        &call.args,
                        M::catalog(),
                    ));
                }
                // Reported rather than swallowed: a file that didn't
                // evaluate may be exactly the one holding the settings the
                // user came here to import (vision pillar #3).
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
                self.import_review = Some(ImportState::Ready(
                    hyprforge_core::hlconfig::import::candidates_from_config(&found, M::catalog(), &self.settings)
                        .into_iter()
                        // Anything already owned starts unticked: the user
                        // came here to adopt what they haven't got, and
                        // re-importing would overwrite a value they set in
                        // this app with one from their config file.
                        .map(|found| Candidate {
                            selected: !found.already_owned,
                            found,
                        })
                        .collect(),
                ));
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
                let chosen: Vec<Discovered> = match &self.import_review {
                    Some(ImportState::Ready(candidates)) => candidates
                        .iter()
                        .filter(|c| c.selected)
                        .map(|c| c.found.clone())
                        .collect(),
                    _ => Vec::new(),
                };
                self.import_review = None;
                if chosen.is_empty() {
                    return Task::none();
                }
                hyprforge_core::hlconfig::import::merge(&mut self.settings, &chosen);
                self.status = Some(format!("Imported {} setting(s).", chosen.len()));
                self.save_and_maybe_reload()
            }
            Message::ImportCancel => {
                self.import_review = None;
                Task::none()
            }
            Message::LiveLoaded(live) => {
                self.live = live.into_iter().map(|l| (l.key, l)).collect();
                self.rebuild_choices();
                Task::none()
            }
            Message::ChoicesLoaded(installed) => {
                self.installed = installed;
                self.rebuild_choices();
                Task::none()
            }
            Message::Reloaded(Ok(())) => {
                self.error = None;
                self.status = Some("Saved.".to_string());
                // The reload just changed what's running, so the cached
                // live values are stale. This matters most right after a
                // Reset: the row falls back to the live value, and without
                // re-reading it would show the value Hyprforge had been
                // setting rather than the one the user's config just took
                // back over.
                Task::perform(read_live::<M>(), Message::LiveLoaded)
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

        if let Some(notice) = setup_notice(&self.config, M::SUBJECT, scale) {
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
        if !self.invalid.is_empty() {
            let mut list = column![scaled_text(
                format!(
                    "These are in your {} but aren't being applied. Fix or remove them:",
                    M::STORE
                ),
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
            content = content.push(section("Settings being skipped", scale, list));
        }
        if let Some(e) = &self.error {
            content = content.push(section("Something went wrong", scale, scaled_text(e.clone(), 13.0, scale)));
        }
        if let Some(s) = &self.status {
            content = content.push(meta_text(s.clone(), 12.0, scale));
        }

        content = content.push(
            row![
                text_input("Filter settings…", &self.filter)
                    .on_input(Message::FilterChanged)
                    .padding(spacing::SM),
                secondary_button("Import from Hyprland").on_press(Message::ImportOpen),
            ]
            .spacing(spacing::MD)
            .align_y(iced::Alignment::Center),
        );

        let mut any_row = false;
        for category in M::catalog().categories {
            let rows: Vec<&'static Setting> = M::catalog().in_category(category.key)
                .filter(|s| self.matches_filter(s))
                .collect();
            if rows.is_empty() {
                continue;
            }
            any_row = true;
            let mut body = column![meta_text(category.help, 12.0, scale)].spacing(spacing::SM);
            for setting in rows {
                body = body.push(divider());
                body = body.push(self.setting_row(setting, scale));
            }
            content = content.push(section(category.label, scale, body));
        }

        if !any_row {
            content = content.push(scaled_text(
                format!("Nothing matches “{}”.", self.filter.trim()),
                13.0,
                scale,
            ));
        }

        for (category, why) in M::catalog().unsupported {
            if !self.filter.trim().is_empty() && !category.contains(&self.filter.trim().to_lowercase()) {
                continue;
            }
            content = content.push(meta_text(*why, 12.0, scale));
        }

        // No scrollable of its own: the shell owns the page's one scroll
        // area, and a second one inside it scrolled the page twice.
        container(content).padding(spacing::LG).width(Length::Fill).into()
    }
}

impl<M: Catalogued> CatalogScreen<M> {
    /// The shared row renderer, wired to this module's messages.
    /// Rebuilds the XKB pickers.
    ///
    /// Variants depend on the chosen layout — the rules file lists over
    /// 400 of them and only a handful belong to any one layout — so the
    /// list is rebuilt whenever the layout could have changed rather than
    /// computed once.
    pub(crate) fn rebuild_choices(&mut self) {
        self.choices = M::choices(&self.settings, &self.live, &self.installed);
    }

    pub(crate) fn rows(&self) -> RowContext<'_, Message> {
        RowContext {
            settings: &self.settings,
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

    pub(crate) fn setting_row(&self, setting: &'static Setting, scale: FontScale) -> Element<'_, Message> {
        self.rows().row(setting, scale)
    }

    fn import_view(&self, review: &ImportState, scale: FontScale) -> Element<'_, Message> {
        let body: Element<'_, Message> = match review {
            ImportState::Running => scaled_text("Reading your Hyprland config…", 13.0, scale).into(),
            ImportState::Ready(candidates) if candidates.is_empty() => column![
                scaled_text(
                    "Hyprland reports no input options set beyond its own defaults, \
                     so there's nothing to import.",
                    13.0,
                    scale,
                ),
                secondary_button("Close").on_press(Message::ImportCancel),
            ]
            .spacing(spacing::MD)
            .into(),
            ImportState::Ready(candidates) => {
                let mut list = column![scaled_text(
                    "These are the input options your own Hyprland config sets. \
                     Importing one hands it to Hyprforge, which from then on writes \
                     it and wins over your config file — the line in your config \
                     stays where it is, it just stops being the one that decides.",
                    13.0,
                    scale,
                )]
                .spacing(spacing::SM);
                for (i, c) in candidates.iter().enumerate() {
                    let mut label = column![row![
                        checkbox(c.selected).on_toggle(move |_| Message::ImportToggle(i)),
                        scaled_text(
                            format!("{} — {}", c.found.label, setting_rows::render_for_edit(&c.found.value)),
                            13.0,
                            scale,
                        ),
                    ]
                    .spacing(spacing::SM)
                    .align_y(iced::Alignment::Center)]
                    .spacing(2);
                    if c.found.differs {
                        label = label.push(meta_text(
                            "Different from what Hyprforge has — importing replaces it.",
                            12.0,
                            scale,
                        ));
                    } else if c.found.already_owned {
                        label = label.push(meta_text("Already imported.", 12.0, scale));
                    }
                    list = list.push(label);
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

/// Reads every option's live value. A failure yields an empty list rather
/// than an error: the screen is fully usable without it, rows just fall
/// back to the catalog default, and a banner about a background read the
/// user never asked for would be noise.
/// Scans for XKB data and connected monitors. Blocking file and
/// `hyprctl` work, so it goes on the blocking pool rather than stalling
/// the first frame.
async fn read_choices<M: Catalogued>() -> Installed {
    tokio::task::spawn_blocking(M::discover).await.unwrap_or_default()
}

async fn read_live<M: Catalogued>() -> Vec<Live> {
    tokio::task::spawn_blocking(|| {
        hyprforge_core::hlconfig::import::live(M::catalog()).unwrap_or_default()
    })
    .await
    .unwrap_or_default()
}

async fn regenerate_and_reload<M: Catalogued>(settings: Settings) -> Result<(), String> {
    tokio::task::spawn_blocking(move || M::apply(&M::lua_path(), &settings))
        .await
        .map_err(|e| e.to_string())?
}


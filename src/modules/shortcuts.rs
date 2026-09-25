use hyprforge_core::lua_setup;
use hyprforge_ui::theme::{spacing, FontScale, BASE_TEXT_SIZE};
use hyprforge_ui::widgets::{
    danger_button, divider, meta_text, primary_button, row_field, scaled_text, secondary_button,
    section,
};
use crate::module::SettingsModule;
use hyprforge_shortcuts::binds::LiveBind;
use hyprforge_shortcuts::catalog::{self, Category, Entry, ParamKind};
use hyprforge_shortcuts::model::{description_for, generate_shortcut_name};
use hyprforge_shortcuts::setup::{HyprConfig, SetupPlan};
use hyprforge_shortcuts::{codegen, lua, Action, BindFlags, KeyCombo, Modifier, ParamValue, Shortcut};
use iced::widget::{checkbox, column, container, row, scrollable, text_input};
use iced::{Element, Length, Task};
use std::collections::BTreeMap;

use super::keycapture;

/// The edit form's state.
///
/// Kept as display-shaped fields (`String`s and a selected catalog entry),
/// not the parsed `KeyCombo`/`Action`, so a half-typed value stays on screen
/// rather than being dropped. Every field here must round-trip through
/// [`ShortcutDraft::from_shortcut`]: a field that can be written but not read
/// back gets silently erased the next time the user edits the shortcut.
#[derive(Debug, Clone)]
struct ShortcutDraft {
    editing_index: Option<usize>,
    /// The description as it's currently live in the compositor (the
    /// prefixed form `binds::conflicts_for` expects), so editing a
    /// shortcut's own chord doesn't report a conflict with itself. `None`
    /// for a shortcut that's never been saved/applied yet.
    original_description: Option<String>,
    enabled: bool,
    mods: Vec<Modifier>,
    key: String,
    /// True while waiting for the user to press the chord to record.
    capturing: bool,
    /// CAPS/MOD2/MOD3/MOD5 are hidden until asked for — they're in the model
    /// because Hyprland's modmask has them, not because anyone binds them.
    show_all_mods: bool,
    /// The chosen catalog action. `None` means the raw editor is the only
    /// one that can express this action.
    entry: Option<&'static Entry>,
    /// Param values as typed, keyed by param name. Keyed rather than
    /// positional so switching actions and switching back doesn't shuffle
    /// values between fields.
    fields: BTreeMap<String, String>,
    /// The escape hatch: a dispatcher path and the Lua to put in its
    /// parentheses.
    raw_dispatcher: String,
    raw: String,
    use_raw: bool,
    /// Why the raw Lua won't compile, if it won't. Recomputed when either
    /// raw field changes rather than read off the draft on every frame —
    /// compiling is cheap per keystroke and wasteful per redraw.
    ///
    /// Only the raw path needs this. Everything the structured editor can
    /// produce is rendered by codegen from typed values, so it's valid by
    /// construction; raw text is the one thing that reaches the file
    /// unexamined.
    raw_error: Option<String>,
    flags: BindFlags,
    description: String,
    /// Set after a conflict check finds the chord already bound elsewhere.
    /// Cleared on any further edit, since the edit may have resolved it.
    conflict: Option<String>,
    /// Set once the user has been shown a save-time conflict, so a second
    /// Save goes through.
    ///
    /// Note this is not generation-tracked itself — the module's counter is
    /// the one source of "which chord is current", and every chord edit
    /// clears this along with the warning it belongs to.
    ///
    /// Without it, Save is an unbreakable dead end: every press re-runs the
    /// check, finds the same conflict, and refuses — leaving no way at all
    /// to deliberately take over a chord, which is the single most common
    /// reason to open this editor in the first place. Cleared with the
    /// conflict itself on any chord edit, so acknowledging one chord never
    /// silently waves through the next.
    conflict_acknowledged: bool,
    /// True while a save-time conflict check is in flight, so Save can't be
    /// pressed twice for the same draft.
    checking: bool,
}

impl ShortcutDraft {
    fn from_shortcut(index: usize, shortcut: &Shortcut) -> Self {
        let entry = catalog::resolve(&shortcut.action);
        let mut draft = Self::from_shortcut_unchecked(index, shortcut, entry);
        // A stored raw action is normally valid — it came from a config
        // Hyprland accepted — but `shortcuts.toml` is a plain file a user
        // can edit, so the editor says so on open rather than on save.
        draft.revalidate_raw();
        draft
    }

    fn from_shortcut_unchecked(
        index: usize,
        shortcut: &Shortcut,
        entry: Option<&'static Entry>,
    ) -> Self {
        let fields = shortcut
            .action
            .params
            .iter()
            .map(|(k, v)| (k.clone(), v.as_text()))
            .collect();
        ShortcutDraft {
            editing_index: Some(index),
            original_description: Some(description_for(shortcut)),
            enabled: shortcut.enabled,
            mods: shortcut.combo.mods.clone(),
            key: shortcut.combo.key.clone(),
            capturing: false,
            show_all_mods: shortcut.combo.mods.iter().any(|m| !PRIMARY_MODS.contains(m)),
            entry,
            fields,
            raw_dispatcher: shortcut.action.dispatcher.clone(),
            // An action the catalog can't express has to open in the editor
            // that *can* express it, prefilled with what's stored — the
            // alternative is an empty form that silently discards it on save.
            raw: shortcut
                .action
                .raw
                .clone()
                .unwrap_or_else(|| lua::render_table(&shortcut.action.params)),
            use_raw: entry.is_none(),
            raw_error: None,
            flags: shortcut.flags,
            description: shortcut.description.clone(),
            conflict: None,
            conflict_acknowledged: false,
            checking: false,
        }
    }

    fn combo(&self) -> KeyCombo {
        KeyCombo { mods: self.mods.clone(), key: self.key.clone() }
    }

    /// Re-checks the raw Lua and stores the verdict.
    ///
    /// Checks the whole rendered line, not the argument text alone: a
    /// dispatcher path like `window..close` is just as fatal as an
    /// unbalanced brace, and the line is what actually has to compile.
    /// Structured mode always passes, so leaving raw mode clears the error
    /// rather than stranding it on screen.
    fn revalidate_raw(&mut self) {
        self.raw_error = if self.use_raw {
            // The name doesn't affect whether the line parses, so the
            // preview's own placeholder is fine here.
            hyprforge_lua_import::check_syntax(&self.preview("preview".to_string())).err()
        } else {
            None
        };
    }

    /// The params as the model wants them: typed by the catalog, and with
    /// blank fields dropped rather than emitted as `key = ""` — which
    /// Hyprland rejects.
    fn params(&self) -> BTreeMap<String, ParamValue> {
        let Some(entry) = self.entry else {
            return BTreeMap::new();
        };
        entry
            .params
            .iter()
            .filter_map(|param| {
                let text = self.fields.get(param.key)?;
                let value = catalog::parse_value(param.kind, text)?;
                Some((param.key.to_string(), value))
            })
            .collect()
    }

    fn action(&self) -> Action {
        if self.use_raw {
            return Action::with_raw(self.raw_dispatcher.trim(), self.raw.trim());
        }
        match self.entry {
            Some(entry) => Action::with_params(entry.dispatcher, self.params()),
            None => Action::default(),
        }
    }

    /// The params the user still has to fill in before this will work.
    /// Empty means the form is complete.
    fn missing_required(&self) -> Vec<&'static str> {
        let Some(entry) = self.entry else {
            return Vec::new();
        };
        if self.use_raw {
            return Vec::new();
        }
        let params = self.params();
        entry
            .params
            .iter()
            .filter(|p| p.required && !params.contains_key(p.key))
            .map(|p| p.label)
            .collect()
    }

    /// Everything stopping this draft from being saved, in the order a user
    /// would fix them. Empty means Save is live.
    fn blockers(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.key.trim().is_empty() {
            out.push("Pick a key.".to_string());
        }
        if self.use_raw {
            if self.raw_dispatcher.trim().is_empty() {
                out.push("Enter a dispatcher.".to_string());
            }
        } else if self.entry.is_none() {
            out.push("Pick an action.".to_string());
        }
        let missing = self.missing_required();
        if !missing.is_empty() {
            out.push(format!("Fill in {}.", missing.join(", ")));
        }
        if let Some(error) = &self.raw_error {
            out.push(format!("That Lua doesn't parse: {error}"));
        }
        if let Some(conflict) = self.flags.conflict() {
            out.push(conflict.to_string());
        }
        out
    }

    fn into_shortcut(self, name: String) -> Shortcut {
        let action = self.action();
        Shortcut {
            name,
            enabled: self.enabled,
            combo: KeyCombo { mods: self.mods, key: self.key.trim().to_string() },
            action,
            description: self.description.trim().to_string(),
            flags: self.flags,
        }
    }

    /// The exact line this draft will write, for the preview.
    ///
    /// `name` has to be the name it would really be saved under, not a
    /// placeholder: a shortcut with no description of its own falls back to
    /// its name for the `description` field, so a stand-in here shows the
    /// user a description they will never actually get.
    fn preview(&self, name: String) -> String {
        codegen::render_one(&Shortcut {
            name,
            enabled: true,
            combo: self.combo(),
            action: self.action(),
            description: self.description.trim().to_string(),
            flags: self.flags,
        })
    }

    /// Whether this draft would produce a line at all. Codegen skips a
    /// shortcut missing either half, so the preview has nothing truthful to
    /// show for one.
    fn is_renderable(&self) -> bool {
        !self.key.trim().is_empty() && !self.action().is_empty()
    }
}

impl Default for ShortcutDraft {
    fn default() -> Self {
        ShortcutDraft {
            editing_index: None,
            original_description: None,
            enabled: true,
            mods: Vec::new(),
            key: String::new(),
            capturing: false,
            show_all_mods: false,
            entry: None,
            fields: BTreeMap::new(),
            raw_dispatcher: String::new(),
            raw: String::new(),
            use_raw: false,
            raw_error: None,
            flags: BindFlags::default(),
            description: String::new(),
            conflict: None,
            conflict_acknowledged: false,
            checking: false,
        }
    }
}

/// The modifiers a person actually presses. The rest of [`Modifier::ALL`] is
/// behind "Show all".
const PRIMARY_MODS: [Modifier; 4] =
    [Modifier::Shift, Modifier::Ctrl, Modifier::Alt, Modifier::Super];

/// One `hl.bind` flag, with the plain-English label the editor shows.
///
/// The flags themselves are booleans on [`BindFlags`]; this is only the
/// presentation of them, kept beside the editor rather than in the model.
struct FlagRow {
    label: &'static str,
    hint: &'static str,
    /// Picks this row's field out of a [`BindFlags`], for both reading the
    /// checkbox state and writing the toggle back.
    field: fn(&mut BindFlags) -> &mut bool,
}

const fn flag(
    label: &'static str,
    hint: &'static str,
    field: fn(&mut BindFlags) -> &mut bool,
) -> FlagRow {
    FlagRow { label, hint, field }
}

const FLAG_ROWS: [FlagRow; 6] = [
    flag("Mouse button", "Required for mouse:272-style binds", |f| &mut f.mouse),
    flag("Works while locked", "Fires even when an input inhibitor is active", |f| &mut f.locked),
    flag("Repeats when held", "Fires again while the key is held down", |f| &mut f.repeating),
    flag("On release", "Fires when the key is let go, not pressed", |f| &mut f.release),
    flag("On long press", "Fires only when the key is held briefly", |f| &mut f.long_press),
    flag("Passes through", "The window still receives the key", |f| &mut f.non_consuming),
];

/// One shortcut as the list deals with it: its position in the store, the
/// shortcut itself, and the catalog entry it resolves to.
///
/// The entry rides along because resolving scans the whole catalog, and
/// grouping, filtering and the row's own label all want the same answer.
#[derive(Clone, Copy)]
struct Row<'a> {
    /// Position in `self.shortcuts` — what every row action refers to, and
    /// therefore not derivable from a position in the filtered list.
    index: usize,
    shortcut: &'a Shortcut,
    /// `None` for an action the catalog can't name; such rows group under
    /// "Other" and label themselves by dispatcher.
    entry: Option<&'static Entry>,
}

impl Row<'_> {
    /// What the action is called on screen — the catalog's label, or the
    /// bare dispatcher marked as raw when there's no entry for it.
    fn action_label(&self) -> String {
        match self.entry {
            Some(entry) => entry.label.to_string(),
            None => format!("{} (raw)", self.shortcut.action.dispatcher),
        }
    }

    /// Whether this row matches an already-lowercased filter query.
    ///
    /// Each field is lowercased only if the search gets that far — a match
    /// on the description doesn't pay to render the chord and the label.
    fn matches(&self, query: &str) -> bool {
        let action_label = match self.entry {
            Some(entry) => entry.label,
            None => self.shortcut.action.dispatcher.as_str(),
        };
        [self.shortcut.description.as_str(), action_label]
            .into_iter()
            .any(|field| field.to_lowercase().contains(query))
            || self.shortcut.combo.to_bind_string().to_lowercase().contains(query)
    }
}

/// A catalog entry as the picker shows it: category-qualified, so a flat
/// dropdown still reads as grouped ("Window · Close window").
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntryChoice(&'static Entry);

impl std::fmt::Display for EntryChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} · {}", self.0.category.label(), self.0.label)
    }
}

/// Every catalog entry, category-ordered, for the action picker.
///
/// Built once: the catalog is a `const`, and `view()` runs on every redraw,
/// so rebuilding this list per frame is pure waste.
static ENTRY_CHOICES: std::sync::LazyLock<Vec<EntryChoice>> = std::sync::LazyLock::new(|| {
    Category::ALL.into_iter().flat_map(catalog::in_category).map(EntryChoice).collect()
});

#[derive(Debug, Clone)]
pub enum Message {
    Add,
    Edit(usize),
    /// Arms the confirm on a row; the delete itself is [`Message::DeleteConfirm`].
    Delete(usize),
    DeleteConfirm(usize),
    DeleteCancel,
    ToggleEnabled(usize),
    Filter(String),
    LiveBindsLoaded(Vec<LiveBind>),
    DraftToggleMod(Modifier, bool),
    DraftShowAllMods(bool),
    DraftKey(String),
    DraftCapture,
    DraftCaptured(Vec<Modifier>, String),
    DraftEntry(EntryChoice),
    DraftParam(&'static str, String),
    DraftFlag(usize, bool),
    DraftUseRaw(bool),
    DraftRawDispatcher(String),
    DraftRaw(String),
    DraftDescription(String),
    DraftEnabled(bool),
    DraftSave,
    DraftCancel,
    /// `(generation, result)` — a check for a chord the user has already
    /// moved on from is dropped rather than shown.
    ConflictChecked(u64, Result<Vec<String>, String>),
    /// Also generation-tagged: the chord can be edited while the save's
    /// check is in flight, and committing on a result that describes a
    /// chord the user has since changed would save something never checked.
    SaveConflictChecked(u64, Result<Vec<String>, String>),
    Reloaded(Result<(), String>),
    ImportFromConfig,
    ImportEvaluated(hyprforge_lua_import::ImportResult),
    ImportToggle(usize, bool),
    ImportConfirm,
    ImportCancel,
}

/// A shortcut found in the user's own `hyprland.lua`, pending review
/// before it's added to the store. No `name` yet — always freshly
/// generated on confirm via [`generate_shortcut_name`], never trusted
/// from the captured call.
struct ImportCandidate {
    combo: KeyCombo,
    action: Action,
    description: String,
    flags: BindFlags,
    checked: bool,
    /// A shortcut on this chord is already stored. Re-importing it would
    /// stack a second bind on the same keys, which happens easily: a
    /// multi-line `hl.bind` can never have its source line removed (by
    /// design — see `remove_matched_lines`), so it comes back in every
    /// subsequent import.
    already_imported: bool,
    /// Where the original call came from — kept so a checked-and-added
    /// candidate's hand-written source line can be removed after a
    /// successful import.
    source_path: std::path::PathBuf,
    line: Option<usize>,
}

impl ImportCandidate {
    /// Whether this would actually produce a working bind.
    ///
    /// False for a bind whose action couldn't be read at all — one calling
    /// something other than `hl.dsp.*`. Those are still worth showing (the
    /// chord and description survive, and the user can finish them in the
    /// raw editor), but they must never be treated as a completed import:
    /// codegen skips an empty action, so removing the original line would
    /// delete a working bind and put nothing back.
    fn is_complete(&self) -> bool {
        !self.action.is_empty()
    }
}

/// Where an "Import from config" run has got to.
///
/// A two-state enum rather than a nested `Option`, so the meaning is in the
/// type instead of only in a comment about which layer means what.
enum ImportState {
    /// The evaluator is running over the user's config.
    Running,
    Ready(ImportReview),
}

/// The result of the last "Import from config" run, awaiting review.
#[derive(Default)]
struct ImportReview {
    shortcuts: Vec<ImportCandidate>,
    /// `(file, reason)` — files that didn't evaluate cleanly. Shown as-is
    /// rather than dropped (vision pillar #3: no dead ends).
    failures: Vec<(std::path::PathBuf, String)>,
}

pub struct ShortcutsModule {
    shortcuts: Vec<Shortcut>,
    draft: Option<ShortcutDraft>,
    /// Which Hyprland config the user has. Only [`HyprConfig::Lua`] can be
    /// served by inserting a require line, so this gates the whole setup
    /// flow rather than being a detail inside it.
    config: HyprConfig,
    /// Only meaningful when `config` is [`HyprConfig::Lua`].
    setup_plan: SetupPlan,
    error: Option<String>,
    status: Option<String>,
    /// `None` when no import is in progress or under review.
    import_review: Option<ImportState>,
    /// Filters the list. Module-local rather than the shell's search box,
    /// which filters the sidebar nav.
    filter: String,
    /// Everything the compositor currently has bound, so the list can flag a
    /// chord that collides with something else without asking `hyprctl` once
    /// per row. Empty when Hyprland isn't reachable — in which case no badge
    /// is shown at all, rather than a wrong one.
    live_binds: Vec<LiveBind>,
    /// Why the stored shortcuts couldn't be read, if they couldn't.
    ///
    /// An unreadable store is not an empty one. Loading used to be
    /// `unwrap_or_default()`, so a read or parse failure silently produced
    /// an empty list — the UI said "No shortcuts yet" and the very next save
    /// wrote that emptiness over the file. When this is set, the module
    /// refuses to write at all and says why.
    store_unreadable: Option<String>,
    /// Bumped on every chord change. A conflict check's result is dropped
    /// unless it still matches — without it, a slow check for an old chord
    /// lands after a newer one and reports a conflict the user already
    /// fixed, or worse, saves a chord that was never checked.
    conflict_generation: u64,
    /// The row whose Delete is armed, if any. Deleting rewrites the config
    /// immediately and there's nothing to undo it with, so it takes two
    /// presses (vision pillar #4: destructive actions are confirmed or
    /// reversible). Inline on the row rather than a modal — the thing being
    /// deleted stays on screen and readable.
    pending_delete: Option<usize>,
}

impl ShortcutsModule {
    pub fn new() -> (Self, Task<Message>) {
        // A failure here must never look like "you have no shortcuts": that
        // reading is what turns one bad parse into a wiped store on the next
        // save.
        let (shortcuts, store_unreadable) =
            match hyprforge_shortcuts::storage::load(&hyprforge_core::paths::shortcuts_toml_path())
            {
                Ok(shortcuts) => (shortcuts, None),
                Err(e) => (Vec::new(), Some(e.to_string())),
            };
        let setup = lua_setup::bootstrap(
            &hyprforge_core::paths::hypr_config_dir(),
            &hyprforge_core::paths::hyprland_lua_path(),
            lua_setup::ModuleSetup {
                require_line: hyprforge_shortcuts::setup::REQUIRE_LINE,
                placement: hyprforge_shortcuts::setup::PLACEMENT,
                generated: (
                    hyprforge_core::paths::keybinds_lua_path(),
                    hyprforge_shortcuts::codegen::generate(&[]),
                ),
            },
        );
        let (config, setup_plan, error) = (setup.config, setup.plan, setup.error);
        (
            ShortcutsModule {
                shortcuts,
                draft: None,
                config,
                setup_plan,
                error,
                status: None,
                import_review: None,
                filter: String::new(),
                live_binds: Vec::new(),
                store_unreadable,
                conflict_generation: 0,
                pending_delete: None,
            },
            Task::perform(load_live_binds(), Message::LiveBindsLoaded),
        )
    }

    /// Replaces whatever is being edited, invalidating every conflict check
    /// still in flight.
    ///
    /// Every draft lifecycle change goes through here, because the
    /// generation counter is what stops a reply landing on the wrong draft
    /// and `edit_chord` bumping it alone was not enough. Save shells out to
    /// `hyprctl`, so there is a real window in which the user can cancel and
    /// start a new shortcut; without this, the reply for the *saved* chord
    /// arrived with the counter unchanged, matched, and committed the blank
    /// draft that had replaced it — writing a junk bind and reloading
    /// Hyprland over the edit the user actually wanted.
    fn set_draft(&mut self, draft: Option<ShortcutDraft>) {
        self.conflict_generation += 1;
        self.draft = draft;
    }

    fn edit_draft(&mut self, f: impl FnOnce(&mut ShortcutDraft)) -> Task<Message> {
        if let Some(d) = &mut self.draft {
            f(d);
        }
        Task::none()
    }

    /// An edit that changed the chord: re-check it against the compositor
    /// straight away rather than waiting for Save, so the warning arrives
    /// while the user is still looking at the field they typed it in.
    fn edit_chord(&mut self, f: impl FnOnce(&mut ShortcutDraft)) -> Task<Message> {
        self.conflict_generation += 1;
        let generation = self.conflict_generation;
        let Some(draft) = &mut self.draft else {
            return Task::none();
        };
        f(draft);
        // The old warning describes a chord that no longer exists, and so
        // does the user's acknowledgement of it.
        draft.conflict = None;
        draft.conflict_acknowledged = false;
        if draft.key.trim().is_empty() {
            return Task::none();
        }
        let combo = draft.combo();
        let own = draft.original_description.clone();
        Task::perform(check_conflicts(combo, own), move |result| {
            Message::ConflictChecked(generation, result)
        })
    }

    /// Whether the module is waiting for the user to press a chord. The
    /// shell asks, so its own Ctrl+key shortcuts can stand down rather than
    /// swallowing the very keys being captured.
    pub fn is_capturing(&self) -> bool {
        self.draft.as_ref().is_some_and(|d| d.capturing)
    }

    /// Live key presses, but only while capturing — an always-on keyboard
    /// subscription would intercept typing in every other field.
    pub fn subscription(&self) -> iced::Subscription<Message> {
        if !self.is_capturing() {
            return iced::Subscription::none();
        }
        iced::keyboard::listen().filter_map(|event| {
            let iced::keyboard::Event::KeyPressed { key, physical_key, modifiers, .. } = event
            else {
                return None;
            };
            // Escape leaves capture without recording, which is the only way
            // out that doesn't require binding something.
            if matches!(key, iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape)) {
                return Some(Message::DraftCapture);
            }
            let name = keycapture::key_name(&key, &physical_key)?;
            Some(Message::DraftCaptured(keycapture::modifiers_from(modifiers), name))
        })
    }

    /// The shortcuts the list should show, grouped by category, each with
    /// its real index and its resolved catalog entry.
    ///
    /// The index is what every row action refers to, so both filtering and
    /// grouping have to carry it rather than re-deriving it from a position
    /// in the reordered list. The entry is carried for the same reason in
    /// reverse: the row needs it to label the action, and resolving is a
    /// linear scan of the whole catalog, so doing it once per shortcut here
    /// beats doing it once per shortcut *per category* plus again per row —
    /// which is what a filter-per-category shape costs, on every redraw.
    ///
    /// Grouping is a real partition, not a "header whenever the category
    /// changes" — shortcuts are stored in the order they were added, so the
    /// naive version emits a fresh header for almost every row.
    fn grouped(&self) -> Vec<(Option<Category>, Vec<Row<'_>>)> {
        // `None` — the catalog can't name it — goes last, under its own
        // heading: visible rather than hidden, since these are the ones most
        // likely to need attention.
        let mut groups: Vec<(Option<Category>, Vec<Row<'_>>)> =
            Category::ALL.into_iter().map(Some).chain([None]).map(|c| (c, Vec::new())).collect();
        for row in self.visible() {
            let category = row.entry.map(|e| e.category);
            let group = groups
                .iter_mut()
                .find(|(c, _)| *c == category)
                .expect("every category has a group, and `None` has the last one");
            group.1.push(row);
        }
        groups.retain(|(_, rows)| !rows.is_empty());
        groups
    }

    /// The rows passing the filter, each resolved exactly once.
    fn visible(&self) -> Vec<Row<'_>> {
        let query = self.filter.trim().to_lowercase();
        self.shortcuts
            .iter()
            .enumerate()
            .map(|(index, shortcut)| Row {
                index,
                shortcut,
                entry: catalog::resolve(&shortcut.action),
            })
            .filter(|row| query.is_empty() || row.matches(&query))
            .collect()
    }

    /// What else is bound to this shortcut's chord, if anything.
    ///
    /// Excludes the shortcut's own bind by description — otherwise every
    /// applied shortcut would report a conflict with itself.
    fn conflicts_for(&self, shortcut: &Shortcut) -> Vec<String> {
        if self.live_binds.is_empty() {
            return Vec::new();
        }
        hyprforge_shortcuts::binds::conflict_labels(
            &self.live_binds,
            &shortcut.combo,
            Some(&description_for(shortcut)),
        )
    }

    fn commit_draft(&mut self) {
        let Some(draft) = self.draft.take() else {
            return;
        };
        let name = self.name_for(&draft);
        match draft.editing_index {
            Some(i) => self.shortcuts[i] = draft.into_shortcut(name),
            None => self.shortcuts.push(draft.into_shortcut(name)),
        }
    }

    /// The name this draft will be stored under — its existing one when
    /// editing, a freshly generated one when new.
    ///
    /// Shared with the preview rather than computed at save time only,
    /// because the name is visible in the generated line (it's the fallback
    /// description) and a preview that shows a different one is wrong.
    fn name_for(&self, draft: &ShortcutDraft) -> String {
        if let Some(i) = draft.editing_index {
            return self.shortcuts[i].name.clone();
        }
        let label = if draft.description.trim().is_empty() {
            draft.key.trim()
        } else {
            draft.description.trim()
        };
        let existing: Vec<String> = self.shortcuts.iter().map(|s| s.name.clone()).collect();
        generate_shortcut_name(label, &existing)
    }

    /// Writes the canonical TOML, or reports why it couldn't.
    ///
    /// Split out from [`save_and_maybe_reload`](Self::save_and_maybe_reload)
    /// so a caller that is about to do something irreversible can find out
    /// whether the store is safely on disk *first*. See
    /// [`Message::ImportConfirm`].
    fn persist(&mut self) -> Result<(), String> {
        // Refusing to write is the whole point of `store_unreadable`: if the
        // file couldn't be parsed, `self.shortcuts` is an empty list that
        // means "unknown", not "none", and saving it would replace whatever
        // is really in there with nothing.
        if let Some(reason) = &self.store_unreadable {
            let message = format!(
                "Not saving — your shortcuts.toml couldn't be read, and \
                 overwriting it would lose whatever is in it. ({reason})"
            );
            self.error = Some(message.clone());
            return Err(message);
        }
        hyprforge_shortcuts::storage::save(
            &hyprforge_core::paths::shortcuts_toml_path(),
            &self.shortcuts,
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
        // Without a Lua config there is nothing to source the generated file
        // from, so reloading would be a no-op dressed up as success. The
        // TOML is still saved — shortcuts authored now take effect as soon
        // as this is resolved (informational notice only, nothing to click).
        if !matches!(self.config, HyprConfig::Lua(_)) {
            self.error = None;
            self.status =
                Some("Saved. Shortcuts take effect once Hyprland setup is finished — see above.".to_string());
            return Task::none();
        }
        // The require line is installed automatically on open; this only
        // retries if that attempt failed (e.g. a transient permission
        // issue) rather than asking the user to confirm anything.
        if self.setup_plan != SetupPlan::AlreadyPresent {
            match hyprforge_shortcuts::setup::install(&hyprforge_core::paths::hyprland_lua_path()) {
                Ok(plan) => self.setup_plan = plan,
                Err(e) => {
                    self.error = Some(e.to_string());
                    return Task::none();
                }
            }
        }
        Task::perform(regenerate_and_reload(self.shortcuts.clone()), Message::Reloaded)
    }

    /// The recovery banner for the two config shapes the require-line flow
    /// can't serve. Never blocks editing — shortcuts still save to TOML and
    /// activate once setup is done (vision pillar #3: no dead ends).
    /// `Missing` normally never reaches here — `new()` creates a minimal
    /// `hyprland.lua` automatically — so seeing it means that attempt
    /// failed; the reason is already in `self.error`, shown generically
    /// above this.
    fn setup_notice(&self, scale: FontScale) -> Option<Element<'_, Message>> {
        crate::modules::setup_notice::setup_notice(&self.config, "binds", scale)
    }
}

impl SettingsModule for ShortcutsModule {
    type Message = Message;

    fn subtitle(&self) -> Option<String> {
        Some(match self.shortcuts.len() {
            1 => "1 bind".into(),
            n => format!("{n} binds"),
        })
    }


    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Add => {
                self.set_draft(Some(ShortcutDraft::default()));
                Task::none()
            }
            Message::Edit(i) => {
                if let Some(shortcut) = self.shortcuts.get(i) {
                    self.set_draft(Some(ShortcutDraft::from_shortcut(i, shortcut)));
                }
                Task::none()
            }
            Message::Delete(i) => {
                self.pending_delete = Some(i);
                Task::none()
            }
            Message::DeleteCancel => {
                self.pending_delete = None;
                Task::none()
            }
            Message::DeleteConfirm(i) => {
                self.pending_delete = None;
                if i < self.shortcuts.len() {
                    self.shortcuts.remove(i);
                }
                self.save_and_maybe_reload()
            }
            Message::ToggleEnabled(i) => {
                if let Some(shortcut) = self.shortcuts.get_mut(i) {
                    shortcut.enabled = !shortcut.enabled;
                }
                self.save_and_maybe_reload()
            }
            Message::Filter(v) => {
                self.filter = v;
                Task::none()
            }
            Message::LiveBindsLoaded(binds) => {
                self.live_binds = binds;
                Task::none()
            }
            Message::DraftToggleMod(m, on) => self.edit_chord(|d| {
                if on {
                    if !d.mods.contains(&m) {
                        d.mods.push(m);
                    }
                } else {
                    d.mods.retain(|existing| *existing != m);
                }
            }),
            Message::DraftShowAllMods(v) => self.edit_draft(|d| d.show_all_mods = v),
            Message::DraftKey(v) => self.edit_chord(|d| d.key = v),
            Message::DraftCapture => self.edit_draft(|d| d.capturing = !d.capturing),
            Message::DraftCaptured(mods, key) => self.edit_chord(|d| {
                d.mods = mods;
                d.key = key;
                d.capturing = false;
            }),
            Message::DraftEntry(choice) => self.edit_draft(|d| {
                d.entry = Some(choice.0);
                // Leaving raw mode on would hide the fields just chosen.
                d.use_raw = false;
                d.raw_error = None;
                // Prefill single-choice enums: a dropdown with one sensible
                // default shouldn't make the user open it to pick the only
                // thing that makes sense.
                for param in choice.0.params {
                    if let ParamKind::Enum(values) = param.kind {
                        d.fields
                            .entry(param.key.to_string())
                            .or_insert_with(|| values[0].to_string());
                    }
                }
            }),
            Message::DraftParam(key, value) => {
                self.edit_draft(|d| {
                    d.fields.insert(key.to_string(), value);
                })
            }
            Message::DraftFlag(i, on) => self.edit_draft(|d| {
                if let Some(row) = FLAG_ROWS.get(i) {
                    *(row.field)(&mut d.flags) = on;
                }
            }),
            Message::DraftUseRaw(v) => self.edit_draft(|d| {
                // Switching into raw prefills it from the structured fields,
                // so "show me the Lua" is a starting point rather than a
                // blank box.
                if v && d.raw.trim().is_empty() {
                    if let Some(entry) = d.entry {
                        d.raw_dispatcher = entry.dispatcher.to_string();
                        // Rendered by the same rule codegen uses, since this
                        // is the text the user then edits and saves.
                        d.raw = codegen::render_argument(&d.action());
                    }
                }
                d.use_raw = v;
                d.revalidate_raw();
            }),
            Message::DraftRawDispatcher(v) => self.edit_draft(|d| {
                d.raw_dispatcher = v;
                d.revalidate_raw();
            }),
            Message::DraftRaw(v) => self.edit_draft(|d| {
                d.raw = v;
                d.revalidate_raw();
            }),
            Message::DraftDescription(v) => self.edit_draft(|d| d.description = v),
            Message::DraftEnabled(v) => self.edit_draft(|d| d.enabled = v),
            Message::DraftSave => {
                let Some(draft) = &self.draft else {
                    return Task::none();
                };
                // A draft that can't produce a working bind must not reach
                // the compositor — the whole file fails, not just this line.
                if draft.checking || !draft.blockers().is_empty() {
                    return Task::none();
                }
                let combo = draft.combo();
                let own_description = draft.original_description.clone();
                let generation = self.conflict_generation;
                if let Some(d) = &mut self.draft {
                    d.checking = true;
                }
                Task::perform(check_conflicts(combo, own_description), move |result| {
                    Message::SaveConflictChecked(generation, result)
                })
            }
            Message::DraftCancel => {
                self.set_draft(None);
                Task::none()
            }
            Message::ConflictChecked(generation, result) => {
                // A check for a chord the user has already changed says
                // nothing about the one now on screen.
                if generation != self.conflict_generation {
                    return Task::none();
                }
                let Some(draft) = &mut self.draft else {
                    return Task::none();
                };
                if let Ok(conflicts) = result {
                    if !conflicts.is_empty() {
                        draft.conflict =
                            Some(format!("Already bound to: {}", conflicts.join(", ")));
                    }
                }
                Task::none()
            }
            Message::SaveConflictChecked(generation, result) => {
                let current = self.conflict_generation;
                let Some(draft) = &mut self.draft else {
                    return Task::none();
                };
                draft.checking = false;
                // The chord moved on while this was in flight, so the result
                // says nothing about what would now be saved. Dropping it
                // leaves Save pressable again for the chord now on screen.
                if generation != current {
                    return Task::none();
                }
                match result {
                    // Reported once, then overridable. Taking over a chord
                    // something else already owns is a legitimate thing to
                    // want — refusing outright would leave the user no way
                    // to do it at all, which is the dead end pillar #3
                    // rules out. The second press goes through.
                    Ok(conflicts) if !conflicts.is_empty() && !draft.conflict_acknowledged => {
                        draft.conflict = Some(format!(
                            "Already bound to: {}. Save again to bind it anyway.",
                            conflicts.join(", ")
                        ));
                        draft.conflict_acknowledged = true;
                        Task::none()
                    }
                    // Genuinely free, already acknowledged, or Hyprland
                    // isn't reachable to check against — in the last case
                    // there's nothing to conflict with, so saving proceeds
                    // rather than stalling behind a check that can never
                    // complete (vision pillar #3: no dead ends).
                    Ok(_) | Err(_) => {
                        self.commit_draft();
                        self.save_and_maybe_reload()
                    }
                }
            }
            Message::Reloaded(Ok(())) => {
                self.status = Some("Saved and reloaded.".to_string());
                self.error = None;
                // The compositor's binds just changed, so the list's
                // conflict badges are now stale.
                Task::perform(load_live_binds(), Message::LiveBindsLoaded)
            }
            Message::Reloaded(Err(e)) => {
                self.error = Some(e);
                Task::none()
            }
            Message::ImportFromConfig => {
                self.import_review = Some(ImportState::Running);
                Task::perform(super::evaluate_user_config(), Message::ImportEvaluated)
            }
            Message::ImportEvaluated(result) => {
                let hyprforge_dir = hyprforge_core::paths::hypr_hyprforge_dir();
                let mut shortcuts = Vec::new();
                let mut unreadable = Vec::new();
                for call in &result.calls {
                    // Hyprforge's own generated file is `require()`d from
                    // hyprland.lua too, so it gets evaluated right along
                    // with the user's own — excluded here, or every
                    // existing shortcut would show up as importable again.
                    if call.source_path.starts_with(&hyprforge_dir) {
                        continue;
                    }
                    let imported = match hyprforge_shortcuts::import::shortcut_from_call(
                        &call.kind,
                        &call.args,
                    ) {
                        // A bind that couldn't be read joins the same list
                        // as a file that couldn't be evaluated, because
                        // from the user's side it is the same problem:
                        // something in their config is in effect and this
                        // list isn't showing it. Dropping it silently made
                        // a misread config look like a config with nothing
                        // in it.
                        Some(Err(why)) => {
                            unreadable.push((call.source_path.clone(), why));
                            continue;
                        }
                        Some(Ok(imported)) => imported,
                        None => continue,
                    };
                    {
                        let already_imported =
                            self.shortcuts.iter().any(|s| s.combo == imported.combo);
                        // Pre-checked only when importing it is
                        // unambiguously the right thing: anything already
                        // stored, or whose action couldn't be read, is left
                        // for the user to opt into.
                        let checked = !already_imported && !imported.action.is_empty();
                        shortcuts.push(ImportCandidate {
                            combo: imported.combo,
                            action: imported.action,
                            description: imported.description,
                            flags: imported.flags,
                            checked,
                            already_imported,
                            source_path: call.source_path.clone(),
                            line: call.line,
                        });
                    }
                }
                let mut failures = result.failures;
                failures.extend(unreadable);
                self.import_review =
                    Some(ImportState::Ready(ImportReview { shortcuts, failures }));
                Task::none()
            }
            Message::ImportToggle(i, checked) => {
                if let Some(ImportState::Ready(review)) = &mut self.import_review {
                    if let Some(candidate) = review.shortcuts.get_mut(i) {
                        candidate.checked = checked;
                    }
                }
                Task::none()
            }
            Message::ImportCancel => {
                self.import_review = None;
                Task::none()
            }
            Message::ImportConfirm => {
                // Collected as we go, so removal only ever targets a line
                // that actually became a stored shortcut.
                let mut to_remove: Vec<(std::path::PathBuf, usize)> = Vec::new();
                if let Some(ImportState::Ready(review)) = self.import_review.take() {
                    for candidate in review.shortcuts.into_iter().filter(|c| c.checked) {
                        let existing: Vec<String> = self.shortcuts.iter().map(|s| s.name.clone()).collect();
                        let label = if candidate.description.trim().is_empty() {
                            candidate.combo.key.clone()
                        } else {
                            candidate.description.clone()
                        };
                        let name = generate_shortcut_name(&label, &existing);
                        // Only a shortcut that will really be regenerated
                        // earns the removal of its original. An incomplete
                        // one is stored as a stub to finish in the editor,
                        // but codegen emits nothing for it — removing the
                        // hand-written line would delete a bind that works
                        // and replace it with nothing at all.
                        if let (Some(line), true) = (candidate.line, candidate.is_complete()) {
                            to_remove.push((candidate.source_path.clone(), line));
                        }
                        self.shortcuts.push(Shortcut {
                            name,
                            enabled: true,
                            combo: candidate.combo,
                            action: candidate.action,
                            description: candidate.description,
                            flags: candidate.flags,
                        });
                    }
                }
                // Order matters, and getting it wrong cost a real user 37
                // hand-written binds: the store has to be *on disk* before
                // anything is deleted from their config. Adding to
                // `self.shortcuts` above is not "safely added" — it's an
                // in-memory Vec, and a failed write, a crash, or a closed
                // window between there and here loses the binds with no copy
                // anywhere.
                //
                // So: persist first, and only remove source lines if that
                // actually succeeded. A failure now leaves the user's config
                // untouched, which is recoverable; the other order isn't.
                if self.persist().is_err() {
                    return Task::none();
                }
                // Best-effort from here: the entries are on disk, and
                // `remove_matched_lines` only ever removes a line that still
                // verifiably looks like the exact call it recorded.
                let _ = lua_setup::remove_matched_lines(&to_remove);
                self.save_and_maybe_reload()
            }
        }
    }

    fn view(&self, scale: FontScale) -> Element<'_, Message> {
        if let Some(draft) = &self.draft {
            return self.draft_view(draft, scale);
        }

        if let Some(state) = &self.import_review {
            return self.import_review_view(state, scale);
        }

        let mut content = column![].spacing(spacing::LG);

        if let Some(notice) = self.setup_notice(scale) {
            content = content.push(notice);
        }

        if let Some(reason) = &self.store_unreadable {
            content = content.push(section(
                "Your shortcuts couldn't be read",
                scale,
                column![
                    scaled_text(
                        "Hyprforge won't save anything until this is sorted out — writing \
                         now would replace whatever is in the file with an empty list.",
                        13.0,
                        scale,
                    )
                    .color(hyprforge_ui::theme::warning()),
                    meta_text(
                        format!(
                            "{}\n{reason}",
                            hyprforge_core::paths::shortcuts_toml_path().display()
                        ),
                        12.0,
                        scale,
                    ),
                ]
                .spacing(spacing::SM),
            ));
        }
        if let Some(err) = &self.error {
            content = content.push(scaled_text(format!("Error: {err}"), 13.0, scale));
        }
        if let Some(status) = &self.status {
            content = content.push(meta_text(status.clone(), 13.0, scale));
        }

        let groups = self.grouped();
        let mut list = column![].spacing(spacing::SM);
        if self.shortcuts.is_empty() {
            list = list.push(meta_text("No shortcuts yet.", BASE_TEXT_SIZE, scale));
        } else if groups.is_empty() {
            list = list.push(meta_text("No shortcuts match that.", BASE_TEXT_SIZE, scale));
        }

        for (group_index, (category, rows)) in groups.into_iter().enumerate() {
            if group_index > 0 {
                list = list.push(divider());
            }
            let header = match category {
                Some(c) => c.label().to_uppercase(),
                None => "OTHER".to_string(),
            };
            list = list.push(meta_text(header, 11.0, scale));

            for row in rows {
                let Row { index: i, shortcut, .. } = row;
                let label = if shortcut.description.trim().is_empty() {
                    shortcut.combo.to_bind_string()
                } else {
                    shortcut.description.clone()
                };
                let mut info = column![
                    scaled_text(label, BASE_TEXT_SIZE, scale),
                    meta_text(
                        format!("{}  →  {}", shortcut.combo.to_bind_string(), row.action_label()),
                        12.0,
                        scale,
                    ),
                ]
                .spacing(spacing::XS)
                .width(Length::Fill);

                let conflicts = self.conflicts_for(shortcut);
                if !conflicts.is_empty() {
                    info = info.push(
                        scaled_text(
                            format!("Also bound to: {}", conflicts.join(", ")),
                            12.0,
                            scale,
                        )
                        .color(hyprforge_ui::theme::warning()),
                    );
                }

                // Armed rows swap Edit/Delete for the confirm pair, so the
                // only two things that can happen next are the two the user
                // is being asked about.
                let actions: Vec<Element<'_, Message>> = if self.pending_delete == Some(i) {
                    vec![
                        secondary_button("Keep").on_press(Message::DeleteCancel).into(),
                        danger_button("Delete for good", Message::DeleteConfirm(i)),
                    ]
                } else {
                    vec![
                        secondary_button("Edit").on_press(Message::Edit(i)).into(),
                        danger_button("Delete", Message::Delete(i)),
                    ]
                };

                list = list.push(
                    container(
                        row![
                            checkbox(shortcut.enabled)
                                .on_toggle(move |_| Message::ToggleEnabled(i)),
                            info,
                        ]
                        .extend(actions)
                        .spacing(spacing::SM)
                        .align_y(iced::Alignment::Center),
                    )
                    .padding([spacing::SM, 0.0]),
                );
            }
        }

        let count = if self.shortcuts.is_empty() {
            "Shortcuts".to_string()
        } else {
            format!("Shortcuts ({})", self.shortcuts.len())
        };
        content = content.push(section(
            count,
            scale,
            column![
                text_input("Filter by name, key or action…", &self.filter)
                    .on_input(Message::Filter)
                    .padding(8),
                container(scrollable(list).width(Length::Fill).height(Length::Shrink))
                    .max_height(420.0),
            ]
            .spacing(spacing::SM),
        ));
        content = content.push(
            container(
                row![
                    secondary_button("Import from config").on_press(Message::ImportFromConfig),
                    primary_button("Add shortcut").on_press(Message::Add),
                ]
                .spacing(spacing::SM),
            )
            .width(Length::Fill)
            .align_x(iced::alignment::Horizontal::Right),
        );

        container(content).padding(spacing::LG).into()
    }
}

impl ShortcutsModule {
    /// The "Import from config" flow's loading state and review list.
    /// Reachable any time, not just on first run — opt-in and re-runnable
    /// whenever the user wants to pick up hand-written binds added since
    /// the last import.
    fn import_review_view(&self, state: &ImportState, scale: FontScale) -> Element<'_, Message> {
        let ImportState::Ready(review) = state else {
            return container(scaled_text("Reading your hyprland.lua…", BASE_TEXT_SIZE, scale))
                .padding(spacing::LG)
                .into();
        };

        let mut body = column![scaled_text("Import from config", 22.0, scale)].spacing(spacing::LG);

        if !review.failures.is_empty() {
            let mut failures = column![meta_text(
                format!(
                    "{} file{} couldn't be imported:",
                    review.failures.len(),
                    if review.failures.len() == 1 { "" } else { "s" }
                ),
                13.0,
                scale,
            )]
            .spacing(spacing::XS);
            for (path, reason) in &review.failures {
                failures = failures.push(meta_text(format!("{} — {reason}", path.display()), 12.0, scale));
            }
            body = body.push(section("Couldn't import", scale, failures.spacing(spacing::XS)));
        }

        if review.shortcuts.is_empty() {
            body = body.push(meta_text("No importable shortcuts found.", BASE_TEXT_SIZE, scale));
        } else {
            let mut list = column![].spacing(spacing::SM);
            for (i, candidate) in review.shortcuts.iter().enumerate() {
                let summary = if candidate.description.trim().is_empty() {
                    candidate.combo.to_bind_string()
                } else {
                    candidate.description.clone()
                };
                let detail = if !candidate.is_complete() {
                    format!(
                        "{}  →  (action not recoverable — bound to something other than \
                         hl.dsp.*). Importing this keeps the keys and description as a \
                         stub to finish in the editor; your original line stays put.",
                        candidate.combo.to_bind_string()
                    )
                } else {
                    // The same human label the list uses, so a shortcut
                    // doesn't change its name the moment it's imported.
                    let action_label = match catalog::resolve(&candidate.action) {
                        Some(entry) => entry.label.to_string(),
                        None => format!("{} (raw)", candidate.action.dispatcher),
                    };
                    format!("{}  →  {}", candidate.combo.to_bind_string(), action_label)
                };
                let mut info = column![
                    scaled_text(summary, BASE_TEXT_SIZE, scale),
                    meta_text(detail, 12.0, scale),
                ]
                .spacing(spacing::XS)
                .width(Length::Fill);
                if candidate.already_imported {
                    info = info.push(
                        scaled_text(
                            "You already have a shortcut on these keys — importing it \
                             again would bind them twice.",
                            12.0,
                            scale,
                        )
                        .color(hyprforge_ui::theme::warning()),
                    );
                }
                list = list.push(
                    row![
                        checkbox(candidate.checked).on_toggle(move |v| Message::ImportToggle(i, v)),
                        info,
                    ]
                    .spacing(spacing::SM)
                    .align_y(iced::Alignment::Center),
                );
            }
            body = body.push(section("Shortcuts", scale, list));
        }

        body = body.push(meta_text(
            "Checked entries are added here and their original line is removed \
             from your config — only when Hyprforge can regenerate the bind, \
             only when the line still matches exactly what was imported, and \
             only after it's safely added. A multi-line bind, one edited since \
             this list was generated, or one whose action couldn't be read is \
             left in place instead of guessed at.",
            12.0,
            scale,
        ));

        body = body.push(
            container(
                row![
                    secondary_button("Cancel").on_press(Message::ImportCancel),
                    primary_button("Add checked").on_press(Message::ImportConfirm),
                ]
                .spacing(spacing::SM),
            )
            .width(Length::Fill)
            .align_x(iced::alignment::Horizontal::Right),
        );

        container(body).padding(spacing::LG).into()
    }

    fn draft_view(&self, draft: &ShortcutDraft, scale: FontScale) -> Element<'_, Message> {
        let title = if draft.editing_index.is_some() { "Edit shortcut" } else { "New shortcut" };

        let mut body = column![scaled_text(title, 22.0, scale)]
            .spacing(spacing::LG)
            .max_width(640.0);

        body = body.push(section("Key", scale, self.key_section(draft, scale)));
        body = body.push(section("Action", scale, self.action_section(draft, scale)));
        body = body.push(section("Options", scale, self.options_section(draft, scale)));
        body = body.push(section("Writes", scale, self.preview_section(draft, scale)));

        let blockers = draft.blockers();
        if let Some(first) = blockers.first() {
            body = body.push(
                scaled_text(first.clone(), 12.0, scale).color(hyprforge_ui::theme::warning()),
            );
        }

        let save_label = match (draft.checking, draft.conflict_acknowledged) {
            (true, _) => "Checking…",
            // The chord is taken and the user has been told: say what the
            // button now does rather than making them guess that pressing
            // the same button twice means something different.
            (false, true) => "Save anyway",
            (false, false) => "Save",
        };
        let save = primary_button(save_label);
        // Disabled rather than hidden, with the reason shown above: a button
        // that vanishes leaves no clue what's missing.
        let save = if blockers.is_empty() { save.on_press(Message::DraftSave) } else { save };
        body = body.push(
            container(
                row![secondary_button("Cancel").on_press(Message::DraftCancel), save]
                    .spacing(spacing::SM),
            )
            .width(Length::Fill)
            .align_x(iced::alignment::Horizontal::Right),
        );

        // No scrollable here — the app shell already wraps every screen in
        // one. A second, content-sized scrollable nested inside it puts a
        // stray scrollbar partway across the window.
        container(body).padding(spacing::LG).into()
    }

    fn key_section(&self, draft: &ShortcutDraft, scale: FontScale) -> Element<'_, Message> {
        let shown_mods: Vec<Modifier> = if draft.show_all_mods {
            Modifier::ALL.to_vec()
        } else {
            PRIMARY_MODS.to_vec()
        };
        let mut mod_row = row![].spacing(spacing::SM);
        for m in shown_mods {
            let on = draft.mods.contains(&m);
            mod_row = mod_row.push(
                checkbox(on).label(m.keyword()).on_toggle(move |v| Message::DraftToggleMod(m, v)),
            );
        }

        let capture = if draft.capturing {
            primary_button("Press a key…").on_press(Message::DraftCapture)
        } else {
            secondary_button("Press a key").on_press(Message::DraftCapture)
        };

        let mut form = column![
            row_field("Modifiers", mod_row),
            checkbox(draft.show_all_mods)
                .label("Show CAPS and MOD2–5")
                .on_toggle(Message::DraftShowAllMods),
            row_field(
                "Key",
                row![
                    text_input("e.g. Q, Return, XF86AudioRaiseVolume", &draft.key)
                        .on_input(Message::DraftKey),
                    capture,
                ]
                .spacing(spacing::SM)
                .align_y(iced::Alignment::Center),
            ),
        ]
        .spacing(spacing::MD);

        if draft.capturing {
            // Said plainly because it *will* happen: the compositor eats a
            // chord it already owns before this window ever sees it.
            form = form.push(meta_text(
                "Press the combination now. Esc cancels. A chord Hyprland has \
                 already bound never reaches this window — type those in the \
                 field instead.",
                12.0,
                scale,
            ));
        }

        if !draft.key.trim().is_empty() {
            form = form.push(meta_text(
                format!("Binds: {}", draft.combo().to_bind_string()),
                12.0,
                scale,
            ));
        }
        if let Some(conflict) = &draft.conflict {
            form = form.push(
                scaled_text(conflict.clone(), 12.0, scale).color(hyprforge_ui::theme::warning()),
            );
        }
        form.into()
    }

    fn action_section(&self, draft: &ShortcutDraft, scale: FontScale) -> Element<'_, Message> {
        let selected = draft.entry.map(EntryChoice);
        let mut form = column![row_field(
            "Action",
            iced::widget::pick_list(ENTRY_CHOICES.as_slice(), selected, Message::DraftEntry)
                .placeholder("Choose what this does…"),
        )]
        .spacing(spacing::MD);

        if draft.use_raw {
            form = form.push(meta_text(
                "Raw mode: the text below goes inside hl.dsp.<dispatcher>( … ) \
                 exactly as written. It's checked for valid Lua, but not for \
                 whether Hyprland knows that dispatcher — that's only found \
                 out on reload.",
                12.0,
                scale,
            ));
            form = form.push(row_field(
                "Dispatcher",
                text_input("e.g. window.close", &draft.raw_dispatcher)
                    .on_input(Message::DraftRawDispatcher),
            ));
            form = form.push(row_field(
                "Lua argument",
                text_input("e.g. { direction = [[left]] }", &draft.raw)
                    .on_input(Message::DraftRaw),
            ));
        } else if let Some(entry) = draft.entry {
            for param in entry.params {
                let value = draft.fields.get(param.key).cloned().unwrap_or_default();
                let key = param.key;
                let field: Element<'_, Message> = match param.kind {
                    // The accepted values are already `&'static str`; the
                    // picker takes them as they are rather than allocating a
                    // fresh `Vec<String>` on every redraw.
                    ParamKind::Enum(values) => {
                        let selected = values.iter().copied().find(|v| *v == value);
                        iced::widget::pick_list(values, selected, move |v: &'static str| {
                            Message::DraftParam(key, v.to_string())
                        })
                        .placeholder("Choose…")
                        .into()
                    }
                    ParamKind::Bool => checkbox(value == "true")
                        .on_toggle(move |v| {
                            Message::DraftParam(key, if v { "true" } else { "" }.to_string())
                        })
                        .into(),
                    _ => text_input(param.hint, &value)
                        .on_input(move |v| Message::DraftParam(key, v))
                        .into(),
                };
                let label = if param.required {
                    param.label.to_string()
                } else {
                    format!("{} (optional)", param.label)
                };
                form = form.push(row_field(label, field));
                // Hints go under text fields only — a dropdown's options
                // already say what's allowed, and a checkbox's label does.
                if !param.hint.is_empty()
                    && !matches!(param.kind, ParamKind::Enum(_) | ParamKind::Bool)
                {
                    form = form.push(meta_text(param.hint, 11.0, scale));
                }
            }
            if entry.params.is_empty() {
                form = form.push(meta_text("This action takes no settings.", 12.0, scale));
            }
        }

        form = form.push(
            checkbox(draft.use_raw)
                .label("Write the Lua myself")
                .on_toggle(Message::DraftUseRaw),
        );
        form.into()
    }

    fn options_section(&self, draft: &ShortcutDraft, scale: FontScale) -> Element<'_, Message> {
        let mut form = column![
            row_field(
                "Description",
                text_input("Shown in hyprctl binds", &draft.description)
                    .on_input(Message::DraftDescription),
            ),
            checkbox(draft.enabled).label("Enabled").on_toggle(Message::DraftEnabled),
        ]
        .spacing(spacing::MD);

        let mut flags = column![].spacing(spacing::XS);
        for (i, row) in FLAG_ROWS.iter().enumerate() {
            let mut copy = draft.flags;
            let on = *(row.field)(&mut copy);
            flags = flags.push(
                column![
                    checkbox(on).label(row.label).on_toggle(move |v| Message::DraftFlag(i, v)),
                    meta_text(row.hint, 11.0, scale),
                ]
                .spacing(0.0),
            );
        }
        form = form.push(row_field("Behaviour", flags));

        if let Some(conflict) = draft.flags.conflict() {
            form = form.push(
                scaled_text(conflict, 12.0, scale).color(hyprforge_ui::theme::warning()),
            );
        }
        form.into()
    }

    /// The exact line this draft writes. Rendered by the same codegen the
    /// real file gets, so it can't drift into being a plausible-looking lie.
    fn preview_section(&self, draft: &ShortcutDraft, scale: FontScale) -> Element<'_, Message> {
        // A draft missing its key or its action renders as a line codegen
        // would never write (`hl.dsp.({})`). Showing that as "what this
        // writes" is worse than saying nothing yet.
        let text = if draft.is_renderable() {
            draft.preview(self.name_for(draft)).trim().to_string()
        } else {
            "Nothing yet — this shortcut is incomplete.".to_string()
        };
        column![
            scaled_text(text, 12.0, scale).font(hyprforge_ui::theme::mono_font()),
            meta_text("Written to ~/.config/hypr/hyprforge/keybinds.lua", 11.0, scale),
        ]
        .spacing(spacing::XS)
        .into()
    }
}

/// Everything the compositor currently has bound.
///
/// Shells out, so it goes on the blocking pool. An unreachable Hyprland
/// yields an empty list rather than an error: the list's conflict badges are
/// an aid, and showing none is better than blocking the screen on a check
/// that can't run.
async fn load_live_binds() -> Vec<LiveBind> {
    tokio::task::spawn_blocking(|| hyprforge_shortcuts::binds::list_binds().unwrap_or_default())
        .await
        .unwrap_or_default()
}

/// Checks whether `combo` is already bound in the live compositor, excluding
/// the shortcut's own bind (if any). Blocking work (it shells out to
/// `hyprctl`), so it goes on the blocking pool rather than stalling the UI
/// thread — same treatment `regenerate_and_reload` gets below.
async fn check_conflicts(
    combo: KeyCombo,
    own_description: Option<String>,
) -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(move || {
        let binds = hyprforge_shortcuts::binds::list_binds().map_err(|e| e.to_string())?;
        Ok(hyprforge_shortcuts::binds::conflict_labels(
            &binds,
            &combo,
            own_description.as_deref(),
        ))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Writes `keybinds.lua` and reloads, refusing to write anything that isn't
/// valid Lua.
///
/// The editor checks raw Lua as it's typed, but that only covers the entries
/// a user opened. This covers every path to the file — an imported raw
/// action, a bulk enable/disable, a hand-edited `shortcuts.toml` — and turns
/// what would otherwise be a write/reload/reject/roll-back round trip into a
/// message. It's the same compile the editor does, over the whole file.
async fn regenerate_and_reload(shortcuts: Vec<Shortcut>) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let lua = hyprforge_shortcuts::codegen::generate(&shortcuts);
        if let Err(e) = hyprforge_lua_import::check_syntax(&lua) {
            return Err(format!(
                "Not writing keybinds.lua — the generated file isn't valid Lua ({e}). \
                 This is almost always a hand-written Lua argument on one of your \
                 shortcuts; open it and the editor will point at the problem."
            ));
        }
        hyprforge_shortcuts::apply::apply(&hyprforge_core::paths::keybinds_lua_path(), &shortcuts)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}


#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, ParamValue)]) -> BTreeMap<String, ParamValue> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    /// A module with no side effects on the real config — `new()` installs
    /// require lines and writes files, which a unit test must not do.
    fn test_module() -> ShortcutsModule {
        ShortcutsModule {
            shortcuts: Vec::new(),
            draft: None,
            config: HyprConfig::ConfOnly(std::path::PathBuf::new()),
            setup_plan: SetupPlan::AlreadyPresent,
            error: None,
            status: None,
            import_review: None,
            filter: String::new(),
            live_binds: Vec::new(),
            store_unreadable: None,
            conflict_generation: 0,
            pending_delete: None,
        }
    }

    fn shortcut_with(description: &str, action: Action) -> Shortcut {
        Shortcut {
            name: format!("hyprforge-{description}"),
            enabled: true,
            combo: KeyCombo { mods: vec![Modifier::Super], key: "Q".to_string() },
            action,
            description: description.to_string(),
            flags: BindFlags::default(),
        }
    }

    fn fully_populated_shortcut() -> Shortcut {
        Shortcut {
            name: "hyprforge-test-1".to_string(),
            enabled: true,
            combo: KeyCombo { mods: vec![Modifier::Super, Modifier::Shift], key: "Q".to_string() },
            action: Action::with_params(
                "window.move",
                params(&[
                    ("workspace", ParamValue::Int(3)),
                    ("follow", ParamValue::Bool(true)),
                ]),
            ),
            description: "Move to workspace 3".to_string(),
            flags: BindFlags { locked: true, ..BindFlags::default() },
        }
    }

    /// The invariant that keeps editing non-destructive: anything the form
    /// can hold must survive load → save unchanged.
    #[test]
    fn draft_round_trips_every_field() {
        let shortcut = fully_populated_shortcut();
        let draft = ShortcutDraft::from_shortcut(0, &shortcut);
        let name = shortcut.name.clone();
        let rebuilt = draft.into_shortcut(name);
        assert_eq!(rebuilt, shortcut);
    }

    /// The escape hatch has to round-trip too — an action the catalog can't
    /// name is exactly the one a user can least afford to have silently
    /// rewritten by opening the editor.
    #[test]
    fn a_raw_draft_round_trips() {
        let shortcut = Shortcut {
            name: "hyprforge-raw-1".to_string(),
            enabled: true,
            combo: KeyCombo { mods: vec![Modifier::Super], key: "X".to_string() },
            action: Action::with_raw("some_future_thing", "{ mode = [[wild]] }"),
            description: "Something new".to_string(),
            flags: BindFlags::default(),
        };
        let draft = ShortcutDraft::from_shortcut(0, &shortcut);
        assert!(draft.use_raw, "an unresolvable action must open in the raw editor");
        assert_eq!(draft.into_shortcut(shortcut.name.clone()), shortcut);
    }

    /// Opening a catalog action must not switch the form into raw mode —
    /// that would turn every subsequent save into raw text.
    #[test]
    fn a_catalog_action_opens_in_the_structured_editor() {
        let draft = ShortcutDraft::from_shortcut(0, &fully_populated_shortcut());
        assert!(!draft.use_raw);
        assert_eq!(draft.entry.map(|e| e.id), Some("window.move.workspace"));
        assert_eq!(draft.fields.get("workspace").map(String::as_str), Some("3"));
    }

    /// Save stays disabled until the form can produce a bind that loads —
    /// a half-filled action would fail the whole generated file.
    /// A conflict check is asynchronous: `Save` shells out to `hyprctl`,
    /// which leaves a real window in which the user can give up on that
    /// chord and start editing something else. The reply belongs to the
    /// draft that asked for it, not to whatever happens to be open when it
    /// lands.
    ///
    /// Before the draft lifecycle went through `set_draft`, only
    /// `edit_chord` bumped the generation counter — so Save, Cancel, Add
    /// left it unchanged, the stale reply matched, and the `Ok(_) | Err(_)`
    /// arm committed the *blank* draft that had replaced the real one: a
    /// junk bind written and Hyprland reloaded over the edit the user
    /// wanted.
    #[test]
    fn a_reply_for_a_cancelled_draft_never_commits_the_one_that_replaced_it() {
        let mut module = test_module();
        // `persist` writes to the user's own shortcuts.toml. Nothing in a
        // unit test may reach it, and `store_unreadable` is the existing
        // refuse-to-write path rather than a test-only branch.
        module.store_unreadable = Some("test fixture: never write".to_string());

        let _ = module.update(Message::Add);
        let _ = module.update(Message::DraftKey("Q".to_string()));
        let _ = module.update(Message::DraftEntry(EntryChoice(
            catalog::by_id("window.close").expect("catalog entry"),
        )));
        let in_flight = module.conflict_generation;
        let _ = module.update(Message::DraftSave);

        // The user gives up on that chord and starts a fresh shortcut.
        let _ = module.update(Message::DraftCancel);
        let _ = module.update(Message::Add);

        let _ = module.update(Message::SaveConflictChecked(in_flight, Ok(Vec::new())));

        assert!(
            module.shortcuts.is_empty(),
            "a stale reply committed a draft the user had already abandoned: {:?}",
            module.shortcuts
        );
        assert!(
            module.draft.is_some(),
            "the new draft must still be open — the stale reply took it"
        );
    }

    #[test]
    fn blockers_name_what_is_missing() {
        let mut draft = ShortcutDraft::default();
        assert!(draft.blockers().iter().any(|b| b.contains("key")));

        draft.key = "Q".to_string();
        assert!(draft.blockers().iter().any(|b| b.contains("action")));

        draft.entry = catalog::by_id("window.move.workspace");
        assert!(
            draft.blockers().iter().any(|b| b.contains("Workspace")),
            "a required param that is empty must block: {:?}",
            draft.blockers()
        );

        draft.fields.insert("workspace".to_string(), "3".to_string());
        assert!(draft.blockers().is_empty(), "{:?}", draft.blockers());
    }

    /// The compositor refuses this combination, and a refused bind fails the
    /// whole file — so it has to be caught before save, not after.
    #[test]
    fn incompatible_flags_block_saving() {
        let mut draft = ShortcutDraft {
            key: "Q".to_string(),
            entry: catalog::by_id("window.close"),
            flags: BindFlags { repeating: true, release: true, ..BindFlags::default() },
            ..Default::default()
        };
        assert!(!draft.blockers().is_empty());
        draft.flags.release = false;
        assert!(draft.blockers().is_empty(), "{:?}", draft.blockers());
    }

    /// The preview is the real thing, not an approximation of it.
    #[test]
    fn the_preview_matches_what_codegen_writes() {
        let shortcut = fully_populated_shortcut();
        let draft = ShortcutDraft::from_shortcut(0, &shortcut);
        let preview = draft.preview(shortcut.name.clone());
        assert_eq!(preview, codegen::render_one(&shortcut));
        assert!(preview.contains("hl.dsp.window.move({ follow = true, workspace = 3 })"));
    }

    /// A shortcut with no description of its own is described by its name,
    /// so the preview has to use the name it will really be saved under —
    /// otherwise it advertises a description the user never gets.
    #[test]
    fn the_preview_shows_the_description_the_shortcut_will_really_have() {
        let mut module = test_module();
        module.draft = Some(ShortcutDraft {
            key: "P".to_string(),
            entry: catalog::by_id("window.close"),
            ..Default::default()
        });
        let draft = module.draft.as_ref().unwrap();
        let preview = draft.preview(module.name_for(draft));

        assert!(!preview.contains("preview"), "placeholder name leaked: {preview}");
        assert!(preview.contains("description = [[hyprforge: hyprforge-p-1]]"), "got: {preview}");

        // ...and that is the name it actually gets on save.
        module.commit_draft();
        assert_eq!(module.shortcuts[0].name, "hyprforge-p-1");
        assert_eq!(codegen::render_one(&module.shortcuts[0]), preview);
    }

    /// An incomplete draft renders as a line codegen would never write, so
    /// the preview must not present it as what this shortcut writes.
    #[test]
    fn an_incomplete_draft_has_nothing_to_preview() {
        let mut draft = ShortcutDraft { key: "P".to_string(), ..Default::default() };
        assert!(!draft.is_renderable(), "no action yet");

        draft.entry = catalog::by_id("window.close");
        assert!(draft.is_renderable());

        draft.key = "  ".to_string();
        assert!(!draft.is_renderable(), "no key");
    }

    /// Shortcuts are stored in the order they were added, so grouping has to
    /// partition the list — the naive "header when the category changes"
    /// version emits a header for nearly every row.
    #[test]
    fn grouping_partitions_rather_than_following_list_order() {
        let mut module = test_module();
        module.shortcuts = vec![
            shortcut_with("a", Action::with_params("exec_cmd", params(&[("cmd", ParamValue::Str("x".into()))]))),
            shortcut_with("b", Action::with_params("window.close", BTreeMap::new())),
            shortcut_with("c", Action::with_params("exec_cmd", params(&[("cmd", ParamValue::Str("y".into()))]))),
        ];
        let groups = module.grouped();
        assert_eq!(groups.len(), 2, "interleaved categories must collapse into two groups");
        let launch = groups.iter().find(|(c, _)| *c == Some(Category::Launch)).unwrap();
        assert_eq!(launch.1.len(), 2);
        // The real indices survive the regrouping — every row action uses them.
        assert_eq!(launch.1.iter().map(|row| row.index).collect::<Vec<_>>(), vec![0, 2]);
    }

    /// An action the catalog can't name still gets a row, in its own group.
    #[test]
    fn unresolvable_actions_group_under_other() {
        let mut module = test_module();
        module.shortcuts = vec![shortcut_with("x", Action::with_raw("mystery", "{}"))];
        let groups = module.grouped();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, None);
    }

    #[test]
    fn the_filter_matches_chord_description_and_action() {
        let mut module = test_module();
        module.shortcuts = vec![
            shortcut_with("Launch terminal", Action::with_params("exec_cmd", params(&[("cmd", ParamValue::Str("ghostty".into()))]))),
            shortcut_with("Close it", Action::with_params("window.close", BTreeMap::new())),
        ];
        module.filter = "terminal".to_string();
        assert_eq!(module.visible().len(), 1);
        // By action label, not just by name.
        module.filter = "close window".to_string();
        assert_eq!(module.visible().len(), 1);
        module.filter = "nothing here".to_string();
        assert!(module.visible().is_empty());
    }

    /// A blank optional field must not become `key = ""`, which Hyprland
    /// rejects — it has to be absent.
    #[test]
    fn a_blank_optional_field_is_dropped_not_emitted() {
        let draft = ShortcutDraft {
            key: "Q".to_string(),
            entry: catalog::by_id("window.move.workspace"),
            fields: [("workspace".to_string(), "3".to_string()), ("follow".to_string(), String::new())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let action = draft.action();
        assert!(action.params.contains_key("workspace"));
        assert!(!action.params.contains_key("follow"));
    }

    #[test]
    fn blank_draft_produces_an_empty_shortcut() {
        let draft = ShortcutDraft::default();
        let shortcut = draft.into_shortcut("hyprforge-x-1".to_string());
        assert!(shortcut.combo.is_empty());
        assert!(shortcut.action.is_empty());
    }

    #[test]
    fn toggling_a_modifier_off_removes_only_that_one() {
        let mut draft = ShortcutDraft {
            mods: vec![Modifier::Super, Modifier::Shift],
            ..Default::default()
        };
        draft.mods.retain(|m| *m != Modifier::Shift);
        assert_eq!(draft.mods, vec![Modifier::Super]);
    }

    #[test]
    fn original_description_is_carried_for_conflict_exclusion() {
        let shortcut = fully_populated_shortcut();
        let draft = ShortcutDraft::from_shortcut(0, &shortcut);
        assert_eq!(draft.original_description, Some(description_for(&shortcut)));
    }

    #[test]
    fn a_new_draft_has_no_original_description() {
        assert_eq!(ShortcutDraft::default().original_description, None);
    }

    /// Raw Lua is the one thing that reaches the generated file unexamined,
    /// and a syntax error in it takes down every *other* shortcut with it —
    /// so it has to block the save, not be discovered on reload.
    #[test]
    fn unparseable_raw_lua_blocks_saving() {
        let mut module = test_module();
        module.draft = Some(ShortcutDraft {
            key: "X".to_string(),
            use_raw: true,
            raw_dispatcher: "focus".to_string(),
            ..Default::default()
        });
        let _ = module.update(Message::DraftRaw("{ direction = [[left]] ".to_string()));
        let draft = module.draft.as_ref().unwrap();
        assert!(draft.raw_error.is_some());
        assert!(
            draft.blockers().iter().any(|b| b.contains("doesn't parse")),
            "{:?}",
            draft.blockers()
        );

        // Correcting it clears the block.
        let _ = module.update(Message::DraftRaw("{ direction = [[left]] }".to_string()));
        let draft = module.draft.as_ref().unwrap();
        assert_eq!(draft.raw_error, None);
        assert!(draft.blockers().is_empty(), "{:?}", draft.blockers());
    }

    /// The dispatcher path is spliced into the same line, so it's checked
    /// too — the whole rendered line is what has to compile, not just the
    /// argument.
    ///
    /// Note what this can and can't catch: a stray paren is a syntax error,
    /// but `window..close` is *valid* Lua (string concatenation), so plenty
    /// of typos still get through to the reload. Hence the editor's wording
    /// — checked for valid Lua, not for a dispatcher Hyprland knows.
    #[test]
    fn an_unparseable_dispatcher_path_is_caught_too() {
        let mut module = test_module();
        module.draft = Some(ShortcutDraft {
            key: "X".to_string(),
            use_raw: true,
            raw: "{}".to_string(),
            ..Default::default()
        });
        let _ = module.update(Message::DraftRawDispatcher("window.close(".to_string()));
        assert!(module.draft.as_ref().unwrap().raw_error.is_some());
    }

    /// The structured editor renders from typed values and is valid by
    /// construction — it must never inherit a stale error from raw mode.
    #[test]
    fn leaving_raw_mode_clears_the_error() {
        let mut module = test_module();
        module.draft = Some(ShortcutDraft {
            key: "X".to_string(),
            use_raw: true,
            raw_dispatcher: "focus".to_string(),
            raw: "{ oops ".to_string(),
            raw_error: Some("stale".to_string()),
            ..Default::default()
        });
        let _ = module.update(Message::DraftUseRaw(false));
        assert_eq!(module.draft.as_ref().unwrap().raw_error, None);
    }

    /// Points `$XDG_CONFIG_HOME` at a throwaway directory for the duration
    /// of a test, so anything that reaches `save_and_maybe_reload` writes
    /// there instead of over the developer's real `shortcuts.toml`.
    /// Serialised, because the environment is process-wide.
    struct TempConfig {
        _dir: tempfile::TempDir,
        _lock: std::sync::MutexGuard<'static, ()>,
        previous: Option<std::ffi::OsString>,
    }

    impl TempConfig {
        fn new() -> Self {
            let lock = crate::modules::CONFIG_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let dir = tempfile::tempdir().unwrap();
            let previous = std::env::var_os("XDG_CONFIG_HOME");
            // SAFETY: `CONFIG_ENV_LOCK` serialises every env mutation in
            // this crate's tests.
            unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
            TempConfig { _dir: dir, _lock: lock, previous }
        }
    }

    impl Drop for TempConfig {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                    None => std::env::remove_var("XDG_CONFIG_HOME"),
                }
            }
        }
    }

    /// Deleting rewrites the config immediately and nothing undoes it, so
    /// one press must only arm the row.
    #[test]
    fn deleting_takes_two_presses() {
        let _env = TempConfig::new();
        let mut module = test_module();
        module.shortcuts = vec![shortcut_with("a", Action::with_params("window.close", BTreeMap::new()))];

        let _ = module.update(Message::Delete(0));
        assert_eq!(module.shortcuts.len(), 1, "one press must not delete anything");
        assert_eq!(module.pending_delete, Some(0));

        let _ = module.update(Message::DeleteCancel);
        assert_eq!(module.shortcuts.len(), 1);
        assert_eq!(module.pending_delete, None, "cancelling disarms the row");

        let _ = module.update(Message::Delete(0));
        let _ = module.update(Message::DeleteConfirm(0));
        assert!(module.shortcuts.is_empty());
        assert_eq!(module.pending_delete, None);
    }

    /// Taking over a chord something else already owns is a legitimate
    /// thing to want. Reporting the conflict forever, with no way past it,
    /// is the dead end pillar #3 rules out.
    #[test]
    fn a_conflicting_chord_can_still_be_saved_on_the_second_press() {
        let _env = TempConfig::new();
        let mut module = test_module();
        module.draft = Some(ShortcutDraft {
            key: "Q".to_string(),
            entry: catalog::by_id("window.close"),
            ..Default::default()
        });

        let conflict = Ok(vec!["Close window".to_string()]);
        let _ = module.update(Message::SaveConflictChecked(0, conflict.clone()));
        let draft = module.draft.as_ref().expect("the first check must not save");
        assert!(draft.conflict.as_deref().unwrap().contains("Close window"));
        assert!(draft.conflict_acknowledged);
        assert!(module.shortcuts.is_empty());

        // The same conflict, reported again on the second press.
        let _ = module.update(Message::SaveConflictChecked(0, conflict));
        assert!(module.draft.is_none(), "the second press must go through");
        assert_eq!(module.shortcuts.len(), 1);
    }

    /// A result for a chord the user has since changed must not commit the
    /// chord now on screen, which was never checked.
    #[test]
    fn a_stale_save_check_does_not_commit() {
        let _env = TempConfig::new();
        let mut module = test_module();
        module.draft = Some(ShortcutDraft {
            key: "Q".to_string(),
            entry: catalog::by_id("window.close"),
            ..Default::default()
        });
        // The chord moved on after the check went out.
        let _ = module.update(Message::DraftKey("W".to_string()));
        let _ = module.update(Message::SaveConflictChecked(0, Ok(Vec::new())));
        assert!(module.draft.is_some(), "a stale all-clear must not save");
        assert!(module.shortcuts.is_empty());
    }

    /// Acknowledging one chord must not wave through the next one typed.
    #[test]
    fn changing_the_chord_withdraws_the_acknowledgement() {
        let mut module = test_module();
        module.draft = Some(ShortcutDraft {
            key: "Q".to_string(),
            conflict: Some("Already bound".to_string()),
            conflict_acknowledged: true,
            ..Default::default()
        });
        let _ = module.update(Message::DraftKey("W".to_string()));
        let draft = module.draft.as_ref().unwrap();
        assert!(!draft.conflict_acknowledged);
        assert_eq!(draft.conflict, None);
    }

    fn recorded_bind(lua: &str, path: &std::path::Path, line: usize) -> hyprforge_lua_import::RecordedCall {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hyprland.lua"), lua).unwrap();
        let result = hyprforge_lua_import::evaluate(dir.path());
        let mut call = result.calls.into_iter().next().expect("one recorded call");
        // Re-pointed at the file the test actually wants to watch, since the
        // evaluator's own temp dir is gone by the time it's checked.
        call.source_path = path.to_path_buf();
        call.line = Some(line);
        call
    }

    /// A bind whose action couldn't be read is stored as a stub the user
    /// can finish — but codegen emits nothing for it, so removing the
    /// hand-written original would delete a working bind and put nothing
    /// back in its place.
    #[test]
    fn an_unreadable_action_never_costs_the_user_their_original_line() {
        let _env = TempConfig::new();
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("hyprland.lua");
        let source = "hl.bind([[SUPER + X]], my_own_function())\n";
        std::fs::write(&config, source).unwrap();

        let mut module = test_module();
        let call = recorded_bind(
            "local function my_own_function() return 1 end\nhl.bind([[SUPER + X]], my_own_function())\n",
            &config,
            1,
        );
        let _ = module.update(Message::ImportEvaluated(hyprforge_lua_import::ImportResult {
            calls: vec![call],
            failures: Vec::new(),
        }));

        let Some(ImportState::Ready(review)) = &module.import_review else {
            panic!("the review should be ready");
        };
        assert_eq!(review.shortcuts.len(), 1);
        assert!(!review.shortcuts[0].is_complete());
        assert!(!review.shortcuts[0].checked, "an unreadable action must not be pre-checked");

        // Import it anyway — the stub is useful, deleting the original is not.
        let _ = module.update(Message::ImportToggle(0, true));
        let _ = module.update(Message::ImportConfirm);
        assert_eq!(module.shortcuts.len(), 1, "the stub is still stored");
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            source,
            "the hand-written bind must survive an import that can't replace it"
        );
    }

    /// The defect that cost a real user 37 hand-written binds: the import
    /// deleted their source lines before the store was ever written, so a
    /// failure in between left the binds in neither place.
    #[test]
    fn a_failed_save_leaves_the_users_config_untouched() {
        let _env = TempConfig::new();
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("hyprland.lua");
        let source = "hl.bind([[SUPER + Q]], hl.dsp.window.close())\n";
        std::fs::write(&config, source).unwrap();

        let mut module = test_module();
        // Make the write fail the way a real one would: the store's parent
        // is a *file*, so no shortcuts.toml can be created under it.
        module.store_unreadable = Some("simulated unreadable store".to_string());

        let call = recorded_bind(
            r#"hl.bind("SUPER + Q", hl.dsp.window.close())"#,
            &config,
            1,
        );
        let _ = module.update(Message::ImportEvaluated(hyprforge_lua_import::ImportResult {
            calls: vec![call],
            failures: Vec::new(),
        }));
        let _ = module.update(Message::ImportToggle(0, true));
        let _ = module.update(Message::ImportConfirm);

        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            source,
            "nothing may be removed from the user's config when the store wasn't written"
        );
        assert!(module.error.is_some(), "and the failure has to be visible");
    }

    /// An unreadable store is not an empty one. Treating it as empty is what
    /// turns a single bad parse into a wiped file on the next save.
    #[test]
    fn an_unreadable_store_is_never_overwritten() {
        let _env = TempConfig::new();
        let path = hyprforge_core::paths::shortcuts_toml_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let precious = "this file did not parse but must survive\n";
        std::fs::write(&path, precious).unwrap();

        let mut module = test_module();
        module.store_unreadable = Some("expected an equals".to_string());
        module.shortcuts.push(shortcut_with(
            "new",
            Action::with_params("window.close", BTreeMap::new()),
        ));

        assert!(module.persist().is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            precious,
            "the unparseable file must be left exactly as it was"
        );
        assert!(module.error.as_deref().unwrap().contains("couldn't be read"));
    }

    /// ...and a readable store still saves normally.
    #[test]
    fn a_readable_store_still_saves() {
        let _env = TempConfig::new();
        let mut module = test_module();
        module.shortcuts.push(shortcut_with(
            "new",
            Action::with_params("window.close", BTreeMap::new()),
        ));
        assert!(module.persist().is_ok());
        let written =
            std::fs::read_to_string(hyprforge_core::paths::shortcuts_toml_path()).unwrap();
        assert!(written.contains("window.close"), "got: {written}");
    }

    /// A multi-line `hl.bind` can never have its source line removed, so it
    /// reappears in every subsequent import — pre-checking it would stack a
    /// second bind on the same keys each time.
    #[test]
    fn a_chord_already_stored_is_not_pre_checked_for_import() {
        let mut module = test_module();
        module.shortcuts = vec![Shortcut {
            combo: KeyCombo { mods: vec![Modifier::Super], key: "Q".to_string() },
            ..shortcut_with("Close", Action::with_params("window.close", BTreeMap::new()))
        }];
        let call = recorded_bind(
            r#"hl.bind("SUPER + Q", hl.dsp.window.close())"#,
            std::path::Path::new("/nonexistent/hyprland.lua"),
            1,
        );
        let _ = module.update(Message::ImportEvaluated(hyprforge_lua_import::ImportResult {
            calls: vec![call],
            failures: Vec::new(),
        }));

        let Some(ImportState::Ready(review)) = &module.import_review else {
            panic!("the review should be ready");
        };
        assert!(review.shortcuts[0].already_imported);
        assert!(!review.shortcuts[0].checked);
    }
}

use hyprforge_core::lua_setup;
use hyprforge_ui::theme::{spacing, FontScale, BASE_TEXT_SIZE};
use hyprforge_ui::widgets::{
    config_line, danger_button, meta_text, primary_button, row_field, scaled_text,
    secondary_button, section, section_label, setting_list, setting_row, toggle, tri_state,
};
use crate::module::SettingsModule;
use hyprforge_windowrules::clients::Client;
use hyprforge_windowrules::model::{
    generate_rule_name, Effects, Matcher, Opacity, Workspace, WorkspaceRule,
};
use hyprforge_windowrules::monitors::Monitor;
use hyprforge_windowrules::setup::{HyprConfig, SetupPlan};
use hyprforge_windowrules::Rule;
use iced::widget::{checkbox, column, container, row, scrollable, text_input};
use iced::{Element, Length, Task};

/// The edit form's state.
///
/// Everything is kept as a `String` while editing — including the numeric
/// and expression fields — so a half-typed value stays on screen instead of
/// being silently dropped by a failed parse. Conversion happens once, in
/// [`RuleDraft::into_matcher_effects`].
///
/// Every field here must round-trip through [`RuleDraft::from_rule`]: a
/// field that can be written but not read back gets silently erased the next
/// time the user edits the rule.
#[derive(Debug, Clone, Default)]
struct RuleDraft {
    editing_index: Option<usize>,
    class: String,
    title: String,
    initial_class: String,
    initial_title: String,
    /// Tri-state: `None` means "don't match on this at all", which is
    /// distinct from `Some(false)` — `floating = false` is a real matcher
    /// that selects tiled windows.
    fullscreen: Option<bool>,
    floating: Option<bool>,
    xwayland: Option<bool>,
    match_tag: String,
    content: String,
    workspace: String,
    workspace_silent: bool,
    tag: String,
    float: bool,
    no_blur: bool,
    rounding: String,
    border_color: String,
    move_x: String,
    move_y: String,
    size_w: String,
    size_h: String,
    opacity_active: String,
    opacity_inactive: String,
    opacity_fullscreen: String,
    opacity_override: bool,
    opaque: bool,
    no_anim: bool,
    no_focus: bool,
    stay_focused: bool,
    dim_around: bool,
    keep_aspect_ratio: bool,
    border_size: String,
    min_w: String,
    min_h: String,
    max_w: String,
    max_h: String,
    animation: String,
    /// Empty means "don't set it". Never free text in the view — Hyprland
    /// rejects an unknown mode and that aborts the whole generated file.
    idle_inhibit: String,
    tile: bool,
    fullscreen_effect: bool,
    maximize: bool,
    pin: bool,
    center: bool,
    no_initial_focus: bool,
    /// A `desc:`-style selector, empty for "wherever it would open anyway".
    monitor: String,
    suppress_event: String,
    group: String,
    no_close_for: String,
    /// The effects this draft was opened with.
    ///
    /// Consulted only for the boolean effects, and only to answer one
    /// question: was this stored as an explicit `false`? A checkbox has two
    /// states and the model has three (`Some(true)`, `Some(false)`, `None`),
    /// so unchecked would otherwise mean "unset" and quietly destroy a
    /// `float = false` that came in from a user's own config. `false` is not
    /// the same as absent to Hyprland — rules are ordered, and an explicit
    /// `false` is how a later rule cancels an earlier one.
    ///
    /// The matchers already model this properly, as tri-states; the effects
    /// can't without turning thirteen checkboxes into dropdowns for a case
    /// almost nobody authors by hand. This preserves what's stored without
    /// making the common path worse.
    stored_effects: Effects,
}

impl RuleDraft {
    fn from_rule(index: usize, rule: &Rule) -> Self {
        let m = &rule.matcher;
        let e = &rule.effects;
        let pair = |v: &Option<[String; 2]>| match v {
            Some([a, b]) => (a.clone(), b.clone()),
            None => (String::new(), String::new()),
        };
        let (move_x, move_y) = pair(&e.r#move);
        let (size_w, size_h) = pair(&e.size);
        let opacity = |v: Option<f32>| v.map(|f| f.to_string()).unwrap_or_default();

        RuleDraft {
            editing_index: Some(index),
            class: m.class.clone().unwrap_or_default(),
            title: m.title.clone().unwrap_or_default(),
            initial_class: m.initial_class.clone().unwrap_or_default(),
            initial_title: m.initial_title.clone().unwrap_or_default(),
            fullscreen: m.fullscreen,
            floating: m.floating,
            xwayland: m.xwayland,
            match_tag: m.tag.clone().unwrap_or_default(),
            content: m.content.clone().unwrap_or_default(),
            workspace: e.workspace.name.clone(),
            workspace_silent: e.workspace.silent,
            tag: e.tag.clone().unwrap_or_default(),
            float: e.float.unwrap_or(false),
            no_blur: e.no_blur.unwrap_or(false),
            rounding: e.rounding.map(|r| r.to_string()).unwrap_or_default(),
            border_color: e.border_color.clone().unwrap_or_default(),
            move_x,
            move_y,
            size_w,
            size_h,
            opacity_active: opacity(e.opacity.active),
            opacity_inactive: opacity(e.opacity.inactive),
            opacity_fullscreen: opacity(e.opacity.fullscreen),
            opacity_override: e.opacity.is_override,
            opaque: e.opaque.unwrap_or(false),
            no_anim: e.no_anim.unwrap_or(false),
            no_focus: e.no_focus.unwrap_or(false),
            stay_focused: e.stay_focused.unwrap_or(false),
            dim_around: e.dim_around.unwrap_or(false),
            keep_aspect_ratio: e.keep_aspect_ratio.unwrap_or(false),
            border_size: e.border_size.map(|v| v.to_string()).unwrap_or_default(),
            min_w: int_pair(&e.min_size).0,
            min_h: int_pair(&e.min_size).1,
            max_w: int_pair(&e.max_size).0,
            max_h: int_pair(&e.max_size).1,
            animation: e.animation.clone().unwrap_or_default(),
            idle_inhibit: e.idle_inhibit.clone().unwrap_or_default(),
            tile: e.tile.unwrap_or(false),
            fullscreen_effect: e.fullscreen.unwrap_or(false),
            maximize: e.maximize.unwrap_or(false),
            pin: e.pin.unwrap_or(false),
            center: e.center.unwrap_or(false),
            no_initial_focus: e.no_initial_focus.unwrap_or(false),
            monitor: e.monitor.clone().unwrap_or_default(),
            suppress_event: e.suppress_event.clone().unwrap_or_default(),
            group: e.group.clone().unwrap_or_default(),
            no_close_for: e.no_close_for.map(|v| v.to_string()).unwrap_or_default(),
            stored_effects: e.clone(),
        }
    }

    fn into_matcher_effects(self) -> (Matcher, Effects) {
        let matcher = Matcher {
            class: non_empty(self.class),
            title: non_empty(self.title),
            initial_class: non_empty(self.initial_class),
            initial_title: non_empty(self.initial_title),
            fullscreen: self.fullscreen,
            floating: self.floating,
            xwayland: self.xwayland,
            tag: non_empty(self.match_tag),
            content: non_empty(self.content),
        };
        let effects = Effects {
            // The silent flag is meaningless without a workspace, so it's
            // dropped along with a blank one rather than persisting as a
            // setting with nothing to apply to.
            workspace: Workspace {
                name: non_empty(self.workspace).unwrap_or_default(),
                silent: self.workspace_silent,
            },
            tag: non_empty(self.tag),
            float: keep_false(self.float, self.stored_effects.float),
            no_blur: keep_false(self.no_blur, self.stored_effects.no_blur),
            rounding: self.rounding.trim().parse().ok(),
            border_color: non_empty(self.border_color),
            // move/size are two-part; a half-filled pair isn't expressible
            // in Hyprland's `{ x, y }` form, so it's dropped rather than
            // guessed at. Values pass through as typed — the codegen decides
            // literal-vs-expression quoting.
            r#move: pair_or_none(self.move_x, self.move_y),
            size: pair_or_none(self.size_w, self.size_h),
            opacity: Opacity {
                active: parse_opacity(&self.opacity_active),
                inactive: parse_opacity(&self.opacity_inactive),
                fullscreen: parse_opacity(&self.opacity_fullscreen),
                is_override: self.opacity_override,
            },
            opaque: keep_false(self.opaque, self.stored_effects.opaque),
            no_anim: keep_false(self.no_anim, self.stored_effects.no_anim),
            no_focus: keep_false(self.no_focus, self.stored_effects.no_focus),
            stay_focused: keep_false(self.stay_focused, self.stored_effects.stay_focused),
            dim_around: keep_false(self.dim_around, self.stored_effects.dim_around),
            keep_aspect_ratio: keep_false(self.keep_aspect_ratio, self.stored_effects.keep_aspect_ratio),
            border_size: self.border_size.trim().parse().ok(),
            min_size: int_pair_or_none(&self.min_w, &self.min_h),
            max_size: int_pair_or_none(&self.max_w, &self.max_h),
            animation: non_empty(self.animation),
            idle_inhibit: non_empty(self.idle_inhibit),
            tile: keep_false(self.tile, self.stored_effects.tile),
            fullscreen: keep_false(self.fullscreen_effect, self.stored_effects.fullscreen),
            maximize: keep_false(self.maximize, self.stored_effects.maximize),
            pin: keep_false(self.pin, self.stored_effects.pin),
            center: keep_false(self.center, self.stored_effects.center),
            no_initial_focus: keep_false(self.no_initial_focus, self.stored_effects.no_initial_focus),
            monitor: non_empty(self.monitor),
            suppress_event: non_empty(self.suppress_event),
            group: non_empty(self.group),
            no_close_for: self.no_close_for.trim().parse().ok(),
        };
        (matcher, effects)
    }

    /// Whether any match field is set, without consuming the draft.
    ///
    /// Hyprland requires a rule to match on something, and
    /// [`codegen::generate`](hyprforge_windowrules::codegen::generate) skips
    /// a rule whose matcher is empty — so a rule saved without one is stored,
    /// listed, and silently never applied.
    fn matches_nothing(&self) -> bool {
        [&self.class, &self.title, &self.initial_class, &self.initial_title, &self.match_tag, &self.content]
            .iter()
            .all(|f| f.trim().is_empty())
            && self.fullscreen.is_none()
            && self.floating.is_none()
            && self.xwayland.is_none()
    }

    /// The exact line this draft will write, for the preview.
    ///
    /// Rendered by the same codegen the real file gets, so it can't drift
    /// into being a plausible-looking lie. A rule matching nothing renders
    /// as a line codegen would skip, so callers should check
    /// [`matches_nothing`](Self::matches_nothing) first.
    fn preview(&self, name: &str) -> String {
        let (matcher, effects) = self.clone().into_matcher_effects();
        hyprforge_windowrules::codegen::render_one(&Rule {
            name: name.to_string(),
            enabled: true,
            matcher,
            effects,
        })
    }

    /// Everything stopping this draft from being saved, in the order a user
    /// would fix them. Empty means Save is live.
    ///
    /// This exists because every conversion in
    /// [`into_matcher_effects`](Self::into_matcher_effects) is lossy on bad
    /// input by design — `parse().ok()` drops what it can't read, and a
    /// half-filled pair drops both halves. That's the right behaviour for
    /// *storage* (never write a field Hyprland would reject) and a terrible
    /// one for a user, who typed something and watched it disappear without
    /// a word. Naming the problem here is what turns a silent drop into a
    /// fixable message.
    fn blockers(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.matches_nothing() {
            out.push(
                "Match at least one window property — a rule that matches nothing never applies."
                    .to_string(),
            );
        }
        for (label, text) in [
            ("Rounding", &self.rounding),
            ("Border size", &self.border_size),
            ("Refuse close for", &self.no_close_for),
        ] {
            if !text.trim().is_empty() && text.trim().parse::<i32>().is_err() {
                out.push(format!("{label} must be a whole number."));
            }
        }
        for (label, text) in [
            ("Active opacity", &self.opacity_active),
            ("Inactive opacity", &self.opacity_inactive),
            ("Fullscreen opacity", &self.opacity_fullscreen),
        ] {
            if !text.trim().is_empty() && parse_opacity(text).is_none() {
                out.push(format!("{label} must be a number, e.g. 0.9."));
            }
        }
        // Hyprland's `{ a, b }` can't express one half without the other, so
        // a half-filled pair is dropped entirely on save. Say so rather than
        // letting the typed half vanish.
        for (label, a, b) in [
            ("Position", &self.move_x, &self.move_y),
            ("Size", &self.size_w, &self.size_h),
        ] {
            if a.trim().is_empty() != b.trim().is_empty() {
                out.push(format!("{label} needs both values, or neither."));
            }
        }
        for (label, a, b) in [
            ("Minimum size", &self.min_w, &self.min_h),
            ("Maximum size", &self.max_w, &self.max_h),
        ] {
            let typed = !a.trim().is_empty() || !b.trim().is_empty();
            if typed && int_pair_or_none(a, b).is_none() {
                out.push(format!("{label} needs a whole number for both width and height."));
            }
        }
        out
    }
}

/// A checkbox's value as the model's tri-state.
///
/// On is always `Some(true)`. Off is `None` — *unless* the value was stored
/// as an explicit `false`, which a checkbox has no way to show and no way to
/// re-enter once lost, so leaving it alone preserves it. See
/// [`RuleDraft::stored_effects`].
fn keep_false(on: bool, stored: Option<bool>) -> Option<bool> {
    match (on, stored) {
        (true, _) => Some(true),
        (false, Some(false)) => Some(false),
        (false, _) => None,
    }
}

fn non_empty(s: String) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Splits a stored integer pair back into two draft strings.
fn int_pair(v: &Option<[i32; 2]>) -> (String, String) {
    match v {
        Some([a, b]) => (a.to_string(), b.to_string()),
        None => (String::new(), String::new()),
    }
}

/// Both halves or neither: Hyprland's `{ w, h }` can't express one without
/// the other, same as `move`/`size`.
fn int_pair_or_none(a: &str, b: &str) -> Option<[i32; 2]> {
    match (a.trim().parse().ok(), b.trim().parse().ok()) {
        (Some(a), Some(b)) => Some([a, b]),
        _ => None,
    }
}

fn pair_or_none(a: String, b: String) -> Option<[String; 2]> {
    match (non_empty(a), non_empty(b)) {
        (Some(a), Some(b)) => Some([a, b]),
        _ => None,
    }
}

fn parse_opacity(s: &str) -> Option<f32> {
    s.trim().parse().ok()
}

/// One entry in the idle-inhibit dropdown.
///
/// A dropdown rather than a text field because Hyprland validates this one:
/// an unrecognised mode is rejected outright, and a rejected field aborts the
/// whole generated file, taking every other rule down with it. The empty
/// `mode` is the "leave it unset" entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleInhibitChoice {
    mode: String,
}

impl std::fmt::Display for IdleInhibitChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.mode.is_empty() {
            write!(f, "(not set)")
        } else {
            write!(f, "{}", self.mode)
        }
    }
}

impl IdleInhibitChoice {
    fn all() -> Vec<IdleInhibitChoice> {
        std::iter::once(String::new())
            .chain(
                hyprforge_windowrules::model::IDLE_INHIBIT_MODES
                    .iter()
                    .map(|m| m.to_string()),
            )
            .map(|mode| IdleInhibitChoice { mode })
            .collect()
    }
}

/// One entry in a pin's monitor dropdown.
///
/// Carries the `desc:`-style selector that gets stored alongside the label
/// that gets shown, so the widget never has to reconstruct one from the
/// other — reconstructing is where a description that Hyprland won't
/// recognise would creep in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorChoice {
    label: String,
    selector: String,
}

impl std::fmt::Display for MonitorChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)
    }
}

impl MonitorChoice {
    fn from_monitor(m: &Monitor) -> Self {
        MonitorChoice {
            label: m.label(),
            selector: m.rule_selector(),
        }
    }

    /// "Wherever it would open anyway" — the absence of a monitor rule,
    /// which has to be selectable so a chosen monitor can be un-chosen.
    fn any() -> Self {
        MonitorChoice {
            label: "(any monitor)".to_string(),
            selector: String::new(),
        }
    }

    /// A selector already stored in a pin, for a monitor that isn't connected
    /// right now. Shown as-is so unplugging a display doesn't make its pin
    /// look empty — or worse, let an edit silently clear it.
    fn from_stored(selector: &str) -> Self {
        MonitorChoice {
            label: format!("{selector} (not connected)"),
            selector: selector.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Add,
    Edit(usize),
    /// Arms the confirm on a row; the delete itself is [`Message::DeleteConfirm`].
    Delete(usize),
    DeleteConfirm(usize),
    /// Arms the confirm on a workspace pin.
    DeleteWorkspacePinConfirm(usize),
    DeleteCancel,
    MoveUp(usize),
    MoveDown(usize),
    ToggleEnabled(usize),
    DraftClass(String),
    DraftTitle(String),
    DraftInitialClass(String),
    DraftInitialTitle(String),
    DraftFullscreen(Option<bool>),
    DraftFloating(Option<bool>),
    DraftXwayland(Option<bool>),
    DraftMatchTag(String),
    DraftContent(String),
    DraftWorkspace(String),
    DraftWorkspaceSilent(bool),
    DraftTag(String),
    DraftFloat(bool),
    DraftNoBlur(bool),
    DraftRounding(String),
    DraftBorderColor(String),
    DraftMoveX(String),
    DraftMoveY(String),
    DraftSizeW(String),
    DraftSizeH(String),
    DraftOpacityActive(String),
    DraftOpacityInactive(String),
    DraftOpacityFullscreen(String),
    DraftOpacityOverride(bool),
    DraftOpaque(bool),
    DraftNoAnim(bool),
    DraftNoFocus(bool),
    DraftStayFocused(bool),
    DraftDimAround(bool),
    DraftKeepAspectRatio(bool),
    DraftBorderSize(String),
    DraftMinW(String),
    DraftMinH(String),
    DraftMaxW(String),
    DraftMaxH(String),
    DraftAnimation(String),
    DraftIdleInhibit(IdleInhibitChoice),
    DraftTile(bool),
    DraftFullscreenEffect(bool),
    DraftMaximize(bool),
    DraftPin(bool),
    DraftCenter(bool),
    DraftNoInitialFocus(bool),
    DraftMonitor(MonitorChoice),
    DraftSuppressEvent(String),
    DraftGroup(String),
    DraftNoCloseFor(String),
    ToggleAdvanced,
    OpenPicker,
    ClosePicker,
    ClientsLoaded(Result<Vec<Client>, String>),
    /// Index into `picker`'s loaded list — the class is taken from there
    /// rather than carried in the message, so a stale click can't inject a
    /// class that isn't on screen.
    PickClass(usize),
    PickTitle(usize),
    AddWorkspacePin,
    DeleteWorkspacePin(usize),
    PinWorkspace(usize, String),
    PinMonitor(usize, MonitorChoice),
    PinDefault(usize, bool),
    PinPersistent(usize, bool),
    MonitorsLoaded(Vec<Monitor>),
    DraftSave,
    DraftCancel,
    Reloaded(Result<(), String>),
    ImportFromConfig,
    ImportEvaluated(hyprforge_lua_import::ImportResult),
    ImportToggleRule(usize, bool),
    ImportToggleWorkspaceRule(usize, bool),
    ImportConfirm,
    ImportCancel,
}

/// A window rule found in the user's own `hyprland.lua`, pending review
/// before it's added to the store. Kept separate from `Rule` — it has no
/// name yet (never trusted from the captured call; always freshly
/// generated on confirm, same as any other new rule) and carries the
/// checkbox state the review list needs.
struct ImportCandidateRule {
    matcher: Matcher,
    effects: Effects,
    enabled: bool,
    checked: bool,
    /// Keys of the original call this importer does not model.
    ///
    /// Non-empty means regenerating the rule would *widen* it — a rule
    /// that matched `class` and `workspace` comes back matching `class`
    /// alone. Such a rule is still worth importing, but its hand-written
    /// source line must survive, or the narrower rule is gone and the
    /// broader one replaces it everywhere.
    dropped: Vec<String>,
    /// A rule with this exact matcher is already stored.
    ///
    /// This matters more here than it looks: a `hl.window_rule` is usually
    /// written across several lines, and a multi-line call can never have
    /// its source line removed (by design — see `remove_matched_lines`), so
    /// it comes back in every subsequent import. Pre-checking it would add
    /// another copy each time, and duplicate rules don't merely clutter the
    /// list — they re-apply, with later ones overriding earlier ones.
    already_imported: bool,
    /// Where the original call came from — kept so a checked-and-added
    /// candidate's hand-written source line can be removed after a
    /// successful import (see [`Message::ImportConfirm`]'s handler).
    /// `line: None` means the interpreter couldn't place it (shouldn't
    /// happen in practice), which is treated the same as "don't remove
    /// anything" rather than guessed at.
    source_path: std::path::PathBuf,
    line: Option<usize>,
}

struct ImportCandidateWorkspaceRule {
    rule: WorkspaceRule,
    checked: bool,
    /// Keys of the original call this importer does not model — see
    /// [`ImportCandidateRule::dropped`]. `on_created_empty`, `gaps_in`,
    /// `gaps_out` and `decorate` are all in this category.
    dropped: Vec<String>,
    /// A pin for this workspace is already stored — see
    /// [`ImportCandidateRule::already_imported`].
    already_imported: bool,
    source_path: std::path::PathBuf,
    line: Option<usize>,
}

/// Where an "Import from config" run has got to.
///
/// A two-state enum rather than a nested `Option`, so the meaning lives in
/// the type instead of only in a comment about which layer means what.
enum ImportState {
    /// The evaluator is running over the user's config.
    Running,
    Ready(ImportReview),
}

/// The result of the last "Import from config" run, awaiting the user's
/// review before anything is added to the store.
#[derive(Default)]
struct ImportReview {
    rules: Vec<ImportCandidateRule>,
    workspace_rules: Vec<ImportCandidateWorkspaceRule>,
    /// `(file, reason)` — files that didn't evaluate cleanly. Shown as-is
    /// rather than dropped (vision pillar #3: no dead ends, no silent
    /// partial results).
    failures: Vec<(std::path::PathBuf, String)>,
}

pub struct WindowRulesModule {
    rules: Vec<Rule>,
    /// Workspace→monitor pins. Held beside `rules` rather than inside a
    /// `storage::Rules` so the list operations below can keep indexing one
    /// flat Vec; the two are recombined on save.
    workspace_rules: Vec<WorkspaceRule>,
    draft: Option<RuleDraft>,
    /// Which Hyprland config the user has. `None` of the non-`Lua` variants
    /// can be served by inserting a require line, so this gates the whole
    /// setup flow rather than being a detail inside it.
    config: HyprConfig,
    /// Only meaningful when `config` is [`HyprConfig::Lua`].
    setup_plan: SetupPlan,
    /// Collapsed by default — the raw Hyprland vocabulary (move/size
    /// expressions, per-state opacity, xwayland matching) is the power-user
    /// surface, kept out of the way of "float Discord" (vision pillar #1).
    show_advanced: bool,
    /// Monitors to choose from when pinning a workspace. Empty until the
    /// `hyprctl` read returns, or if it fails — the pin's monitor is then
    /// simply not selectable, which beats blocking the whole section.
    monitors: Vec<Monitor>,
    /// The open-window picker, when it's showing. `None` means closed, which
    /// is distinct from `Some(Loading)` — the panel has to be on screen while
    /// the `hyprctl` call is in flight or the button looks dead.
    picker: Option<PickerState>,
    error: Option<String>,
    status: Option<String>,
    /// `None` when no import is in progress or under review.
    import_review: Option<ImportState>,
    /// Why the stored rules couldn't be read, if they couldn't. While set,
    /// the module refuses to write anything — an unreadable store is not an
    /// empty one, and saving over it would destroy what's really there.
    store_unreadable: Option<String>,
    /// The row whose Delete is armed, if any.
    ///
    /// Deleting rewrites the config immediately and there's nothing to undo
    /// it with, so it takes two presses (vision pillar #4: destructive
    /// actions are confirmed or reversible). A window rule is a page of
    /// fields to reconstruct from memory, which makes an accidental click
    /// far more expensive here than a mis-click usually is.
    pending_delete: Option<PendingDelete>,
}

/// Which row is waiting on a delete confirmation. One field for both lists
/// so arming a rule disarms a pin and vice versa — two independent armed
/// rows on screen at once would be its own kind of confusing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingDelete {
    Rule(usize),
    WorkspacePin(usize),
}

/// Shown against an import candidate that duplicates something already
/// stored. A duplicate window rule isn't just clutter — rules are ordered
/// and later ones override earlier ones, so a second copy changes what
/// actually applies.
fn already_imported_note<'a>(scale: FontScale) -> Element<'a, Message> {
    scaled_text(
        "Already in your rules — importing it again would apply it twice.",
        12.0,
        scale,
    )
    .color(hyprforge_ui::theme::warning())
    .into()
}

/// Shown against a candidate whose original call used keys this importer
/// does not model.
///
/// Says what will happen rather than that something went wrong: the rule
/// is still worth importing, it just comes back *wider* than it was
/// written, so the hand-written line has to stay. Without this the
/// review screen showed a widened rule as a clean import.
fn dropped_keys_note<'a>(dropped: &[String], scale: FontScale) -> Element<'a, Message> {
    scaled_text(
        format!(
            "Can't read {} yet, so your original line is kept as well. \
             Delete it by hand once you're happy with the imported rule.",
            dropped.join(", ")
        ),
        12.0,
        scale,
    )
    .color(hyprforge_ui::theme::warning())
    .into()
}

/// Where the window picker is in its one-shot load.
enum PickerState {
    Loading,
    Loaded(Vec<Client>),
    /// Hyprland isn't running, or `hyprctl` isn't installed. Not an error
    /// dialog: the class and title fields still work by hand, so this is a
    /// note in the panel and nothing more (vision pillar #3 — the dead end
    /// would be a modal you can't get past to type the class yourself).
    Unavailable(String),
}

impl WindowRulesModule {
    pub fn new() -> (Self, Task<Message>) {
        // A failure here must never look like "you have no rules": that
        // reading is what turns one bad parse into a wiped store on the next
        // save.
        let (stored, store_unreadable) = match hyprforge_windowrules::storage::load(
            &hyprforge_core::paths::window_rules_toml_path(),
        ) {
            Ok(stored) => (stored, None),
            Err(e) => (Default::default(), Some(e.to_string())),
        };
        let (rules, workspace_rules) = (stored.rules, stored.workspace_rules);
        let setup = lua_setup::bootstrap(
            &hyprforge_core::paths::hypr_config_dir(),
            &hyprforge_core::paths::hyprland_lua_path(),
            lua_setup::ModuleSetup {
                require_line: hyprforge_windowrules::setup::REQUIRE_LINE,
                placement: hyprforge_windowrules::setup::PLACEMENT,
                generated: (
                    hyprforge_core::paths::window_rules_lua_path(),
                    hyprforge_windowrules::codegen::generate(&[], &[]),
                ),
            },
        );
        let (config, setup_plan, error) = (setup.config, setup.plan, setup.error);
        (
            WindowRulesModule {
                rules,
                workspace_rules,
                draft: None,
                config,
                setup_plan,
                show_advanced: false,
                monitors: Vec::new(),
                picker: None,
                error,
                status: None,
                import_review: None,
                store_unreadable,
                pending_delete: None,
            },
            // Read the monitors up front: the workspace-pin dropdown needs
            // them, and a failure here is silent by design — the section
            // still lists existing pins, it just can't offer new monitors.
            Task::perform(load_monitors(), Message::MonitorsLoaded),
        )
    }

    /// Applies `f` to the open draft, if there is one. Keeps the ~19 field
    /// -edit message arms to one line each.
    /// Applies `f` to the pin at `index`, then persists.
    ///
    /// A blank pin is legitimate mid-edit — you add a row before you've typed
    /// a workspace into it — so an incomplete one is saved as-is and simply
    /// skipped by the codegen rather than blocking the save.
    fn edit_pin(&mut self, index: usize, f: impl FnOnce(&mut WorkspaceRule)) -> Task<Message> {
        let Some(pin) = self.workspace_rules.get_mut(index) else {
            return Task::none();
        };
        f(pin);
        self.save_and_maybe_reload()
    }

    /// Both rule kinds as storage and codegen want them. The module keeps
    /// them apart while editing because only window rules are reorderable.
    fn stored_rules(&self) -> hyprforge_windowrules::storage::Rules {
        hyprforge_windowrules::storage::Rules {
            rules: self.rules.clone(),
            workspace_rules: self.workspace_rules.clone(),
        }
    }

    /// The window at `index` in the picker's loaded list, if the picker is
    /// still showing that list. Indices are only meaningful against the
    /// snapshot they were rendered from.
    fn picked_client(&self, index: usize) -> Option<&Client> {
        match &self.picker {
            Some(PickerState::Loaded(clients)) => clients.get(index),
            _ => None,
        }
    }

    fn edit_draft(&mut self, f: impl FnOnce(&mut RuleDraft)) -> Task<Message> {
        if let Some(d) = &mut self.draft {
            f(d);
        }
        Task::none()
    }

    fn commit_draft(&mut self) {
        let Some(draft) = self.draft.take() else {
            return;
        };
        // Taken before the draft is consumed, and by the same rule the
        // preview used, so what the user was shown is what gets written.
        let name = self.name_for(&draft);
        let editing_index = draft.editing_index;
        let (matcher, effects) = draft.into_matcher_effects();
        match editing_index {
            Some(i) => {
                self.rules[i].matcher = matcher;
                self.rules[i].effects = effects;
            }
            None => {
                self.rules.push(Rule {
                    name,
                    enabled: true,
                    matcher,
                    effects,
                });
            }
        }
    }

    /// Writes the canonical TOML, or reports why it couldn't.
    ///
    /// Split out so a caller about to do something irreversible can find out
    /// whether the store is safely on disk *first* — see
    /// [`Message::ImportConfirm`].
    fn persist(&mut self) -> Result<(), String> {
        // If the file couldn't be parsed, the in-memory lists mean "unknown",
        // not "none", and writing them would replace what's really there.
        if let Some(reason) = &self.store_unreadable {
            let message = format!(
                "Not saving — your window-rules.toml couldn't be read, and \
                 overwriting it would lose whatever is in it. ({reason})"
            );
            self.error = Some(message.clone());
            return Err(message);
        }
        hyprforge_windowrules::storage::save(
            &hyprforge_core::paths::window_rules_toml_path(),
            &self.stored_rules(),
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
        // from, so reloading would be a no-op dressed up as success. The TOML
        // is still saved — rules authored now take effect as soon as this
        // is resolved (informational notice only, nothing to click).
        if !matches!(self.config, HyprConfig::Lua(_)) {
            self.error = None;
            self.status = Some("Saved. Rules take effect once Hyprland setup is finished — see above.".to_string());
            return Task::none();
        }
        // The require line is installed automatically on open; this only
        // retries if that attempt failed (e.g. a transient permission
        // issue) rather than asking the user to confirm anything.
        if self.setup_plan != SetupPlan::AlreadyPresent {
            match hyprforge_windowrules::setup::install(&hyprforge_core::paths::hyprland_lua_path()) {
                Ok(plan) => self.setup_plan = plan,
                Err(e) => {
                    self.error = Some(e.to_string());
                    return Task::none();
                }
            }
        }
        Task::perform(regenerate_and_reload(self.stored_rules()), Message::Reloaded)
    }

    /// The recovery banner for the two config shapes the require-line flow
    /// can't serve. Never blocks rule editing — rules still save to TOML and
    /// activate once setup is done (vision pillar #3: no dead ends).
    ///
    /// `Missing` normally never reaches here — `new()` creates a minimal
    /// `hyprland.lua` automatically — so seeing it means that attempt
    /// failed; the reason is already in `self.error`, shown generically
    /// above this.
    fn setup_notice(&self, scale: FontScale) -> Option<Element<'_, Message>> {
        crate::modules::setup_notice::setup_notice(&self.config, "rules", scale)
    }
}

impl SettingsModule for WindowRulesModule {
    type Message = Message;

    fn subtitle(&self) -> Option<String> {
        Some(match self.rules.len() {
            1 => "1 rule".into(),
            n => format!("{n} rules"),
        })
    }

    /// How many rules there are, as in the mockup's `Window rules  14`.
    fn nav_badge(&self) -> Option<crate::module::NavBadge> {
        (!self.rules.is_empty()).then(|| crate::module::NavBadge::Text(self.rules.len().to_string()))
    }


    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Add => {
                self.draft = Some(RuleDraft::default());
                Task::none()
            }
            Message::Edit(i) => {
                if let Some(rule) = self.rules.get(i) {
                    self.draft = Some(RuleDraft::from_rule(i, rule));
                }
                Task::none()
            }
            Message::Delete(i) => {
                self.pending_delete = Some(PendingDelete::Rule(i));
                Task::none()
            }
            Message::DeleteConfirm(i) => {
                self.pending_delete = None;
                if i < self.rules.len() {
                    self.rules.remove(i);
                }
                self.save_and_maybe_reload()
            }
            Message::DeleteCancel => {
                self.pending_delete = None;
                Task::none()
            }
            Message::MoveUp(i) => {
                if i > 0 && i < self.rules.len() {
                    self.rules.swap(i - 1, i);
                }
                self.save_and_maybe_reload()
            }
            Message::MoveDown(i) => {
                if i + 1 < self.rules.len() {
                    self.rules.swap(i, i + 1);
                }
                self.save_and_maybe_reload()
            }
            Message::ToggleEnabled(i) => {
                if let Some(rule) = self.rules.get_mut(i) {
                    rule.enabled = !rule.enabled;
                }
                self.save_and_maybe_reload()
            }
            // Every draft field edit is the same shape: mutate the draft if
            // one is open, never re-render anything else.
            Message::DraftClass(v) => self.edit_draft(|d| d.class = v),
            Message::DraftTitle(v) => self.edit_draft(|d| d.title = v),
            Message::DraftInitialClass(v) => self.edit_draft(|d| d.initial_class = v),
            Message::DraftInitialTitle(v) => self.edit_draft(|d| d.initial_title = v),
            Message::DraftFullscreen(v) => self.edit_draft(|d| d.fullscreen = v),
            Message::DraftFloating(v) => self.edit_draft(|d| d.floating = v),
            Message::DraftXwayland(v) => self.edit_draft(|d| d.xwayland = v),
            Message::DraftMatchTag(v) => self.edit_draft(|d| d.match_tag = v),
            Message::DraftContent(v) => self.edit_draft(|d| d.content = v),
            Message::DraftWorkspace(v) => self.edit_draft(|d| d.workspace = v),
            Message::DraftWorkspaceSilent(v) => self.edit_draft(|d| d.workspace_silent = v),
            Message::DraftTag(v) => self.edit_draft(|d| d.tag = v),
            Message::DraftFloat(v) => self.edit_draft(|d| d.float = v),
            Message::DraftNoBlur(v) => self.edit_draft(|d| d.no_blur = v),
            Message::DraftRounding(v) => self.edit_draft(|d| d.rounding = v),
            Message::DraftBorderColor(v) => self.edit_draft(|d| d.border_color = v),
            Message::DraftMoveX(v) => self.edit_draft(|d| d.move_x = v),
            Message::DraftMoveY(v) => self.edit_draft(|d| d.move_y = v),
            Message::DraftSizeW(v) => self.edit_draft(|d| d.size_w = v),
            Message::DraftSizeH(v) => self.edit_draft(|d| d.size_h = v),
            Message::DraftOpacityActive(v) => self.edit_draft(|d| d.opacity_active = v),
            Message::DraftOpacityInactive(v) => self.edit_draft(|d| d.opacity_inactive = v),
            Message::DraftOpacityFullscreen(v) => self.edit_draft(|d| d.opacity_fullscreen = v),
            Message::DraftOpacityOverride(v) => self.edit_draft(|d| d.opacity_override = v),
            Message::DraftOpaque(v) => self.edit_draft(|d| d.opaque = v),
            Message::DraftNoAnim(v) => self.edit_draft(|d| d.no_anim = v),
            Message::DraftNoFocus(v) => self.edit_draft(|d| d.no_focus = v),
            Message::DraftStayFocused(v) => self.edit_draft(|d| d.stay_focused = v),
            Message::DraftDimAround(v) => self.edit_draft(|d| d.dim_around = v),
            Message::DraftKeepAspectRatio(v) => self.edit_draft(|d| d.keep_aspect_ratio = v),
            Message::DraftBorderSize(v) => self.edit_draft(|d| d.border_size = v),
            Message::DraftMinW(v) => self.edit_draft(|d| d.min_w = v),
            Message::DraftMinH(v) => self.edit_draft(|d| d.min_h = v),
            Message::DraftMaxW(v) => self.edit_draft(|d| d.max_w = v),
            Message::DraftMaxH(v) => self.edit_draft(|d| d.max_h = v),
            Message::DraftAnimation(v) => self.edit_draft(|d| d.animation = v),
            Message::DraftIdleInhibit(c) => self.edit_draft(|d| d.idle_inhibit = c.mode),
            Message::DraftTile(v) => self.edit_draft(|d| d.tile = v),
            Message::DraftFullscreenEffect(v) => self.edit_draft(|d| d.fullscreen_effect = v),
            Message::DraftMaximize(v) => self.edit_draft(|d| d.maximize = v),
            Message::DraftPin(v) => self.edit_draft(|d| d.pin = v),
            Message::DraftCenter(v) => self.edit_draft(|d| d.center = v),
            Message::DraftNoInitialFocus(v) => self.edit_draft(|d| d.no_initial_focus = v),
            Message::DraftMonitor(c) => self.edit_draft(|d| d.monitor = c.selector),
            Message::DraftSuppressEvent(v) => self.edit_draft(|d| d.suppress_event = v),
            Message::DraftGroup(v) => self.edit_draft(|d| d.group = v),
            Message::DraftNoCloseFor(v) => self.edit_draft(|d| d.no_close_for = v),
            Message::OpenPicker => {
                self.picker = Some(PickerState::Loading);
                Task::perform(load_clients(), Message::ClientsLoaded)
            }
            Message::ClosePicker => {
                self.picker = None;
                Task::none()
            }
            Message::ClientsLoaded(result) => {
                // Dropped entirely if the picker was closed while the call
                // was in flight — reopening it starts a fresh load, and
                // letting this land would put the old list under the new
                // spinner.
                if self.picker.is_some() {
                    self.picker = Some(match result {
                        Ok(clients) => PickerState::Loaded(clients),
                        Err(e) => PickerState::Unavailable(e),
                    });
                }
                Task::none()
            }
            Message::PickClass(i) => {
                let class = self.picked_client(i).map(|c| c.class.clone());
                match class {
                    Some(class) => self.edit_draft(|d| d.class = class),
                    None => Task::none(),
                }
            }
            Message::PickTitle(i) => {
                let title = self.picked_client(i).map(|c| c.title.clone());
                match title {
                    Some(title) => self.edit_draft(|d| d.title = title),
                    None => Task::none(),
                }
            }
            Message::MonitorsLoaded(monitors) => {
                self.monitors = monitors;
                Task::none()
            }
            Message::AddWorkspacePin => {
                self.workspace_rules.push(WorkspaceRule::default());
                Task::none()
            }
            Message::DeleteWorkspacePin(i) => {
                self.pending_delete = Some(PendingDelete::WorkspacePin(i));
                Task::none()
            }
            Message::DeleteWorkspacePinConfirm(i) => {
                self.pending_delete = None;
                if i < self.workspace_rules.len() {
                    self.workspace_rules.remove(i);
                    return self.save_and_maybe_reload();
                }
                Task::none()
            }
            Message::PinWorkspace(i, v) => self.edit_pin(i, |p| p.workspace = v),
            Message::PinMonitor(i, choice) => self.edit_pin(i, |p| p.monitor = choice.selector),
            Message::PinDefault(i, v) => self.edit_pin(i, |p| p.default = v),
            Message::PinPersistent(i, v) => self.edit_pin(i, |p| p.persistent = v),
            Message::ToggleAdvanced => {
                self.show_advanced = !self.show_advanced;
                Task::none()
            }
            Message::DraftSave => {
                // A draft that can't produce a working rule must not be
                // stored: an unmatched rule is skipped by codegen and an
                // unparseable field is dropped, either way leaving the user
                // with a rule that silently isn't what they wrote.
                if self.draft.as_ref().is_some_and(|d| !d.blockers().is_empty()) {
                    return Task::none();
                }
                self.commit_draft();
                self.save_and_maybe_reload()
            }
            Message::DraftCancel => {
                self.draft = None;
                Task::none()
            }
            Message::Reloaded(Ok(())) => {
                self.status = Some("Saved and reloaded.".to_string());
                self.error = None;
                Task::none()
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
                let mut rules = Vec::new();
                let mut workspace_rules = Vec::new();
                for call in &result.calls {
                    // Hyprforge's own generated file is `require()`d from
                    // hyprland.lua too, so it gets evaluated right along
                    // with the user's own — excluded here, or every
                    // existing rule would show up as importable again.
                    if call.source_path.starts_with(&hyprforge_dir) {
                        continue;
                    }
                    if let Some(imported) =
                        hyprforge_windowrules::import::rule_from_call(&call.kind, &call.args)
                    {
                        let hyprforge_windowrules::import::ImportedRule {
                            matcher,
                            effects,
                            enabled,
                            dropped,
                        } = imported;
                        // Matched on the matcher alone: it's what decides
                        // which windows a rule claims, so a second rule with
                        // the same one is a duplicate however its effects
                        // have since been edited.
                        let already_imported =
                            self.rules.iter().any(|r| r.matcher == matcher);
                        rules.push(ImportCandidateRule {
                            matcher,
                            effects,
                            enabled,
                            checked: !already_imported,
                            already_imported,
                            dropped,
                            source_path: call.source_path.clone(),
                            line: call.line,
                        });
                    } else if let Some(rule) =
                        hyprforge_windowrules::import::workspace_rule_from_call(&call.kind, &call.args)
                    {
                        let dropped = rule.dropped.clone();
                        let rule = rule.rule;
                        let already_imported = self
                            .workspace_rules
                            .iter()
                            .any(|w| w.workspace.trim() == rule.workspace.trim());
                        workspace_rules.push(ImportCandidateWorkspaceRule {
                            rule,
                            dropped,
                            checked: !already_imported,
                            already_imported,
                            source_path: call.source_path.clone(),
                            line: call.line,
                        });
                    }
                }
                self.import_review = Some(ImportState::Ready(ImportReview {
                    rules,
                    workspace_rules,
                    failures: result.failures,
                }));
                Task::none()
            }
            Message::ImportToggleRule(i, checked) => {
                if let Some(ImportState::Ready(review)) = &mut self.import_review {
                    if let Some(candidate) = review.rules.get_mut(i) {
                        candidate.checked = checked;
                    }
                }
                Task::none()
            }
            Message::ImportToggleWorkspaceRule(i, checked) => {
                if let Some(ImportState::Ready(review)) = &mut self.import_review {
                    if let Some(candidate) = review.workspace_rules.get_mut(i) {
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
                // that actually became a stored rule — never a checked
                // -but-somehow-not-added candidate.
                let mut to_remove: Vec<(std::path::PathBuf, usize)> = Vec::new();
                if let Some(ImportState::Ready(review)) = self.import_review.take() {
                    for candidate in review.rules.into_iter().filter(|c| c.checked) {
                        let existing: Vec<String> = self.rules.iter().map(|r| r.name.clone()).collect();
                        let label = candidate
                            .matcher
                            .class
                            .clone()
                            .or_else(|| candidate.matcher.title.clone())
                            .unwrap_or_else(|| "rule".to_string());
                        let name = generate_rule_name(&label, &existing);
                        // Only a rule that will really be regenerated
                        // earns the removal of its original. A rule whose
                        // matcher lost a key we don't model comes back
                        // *wider* than it was written — `class` kept,
                        // `workspace` dropped — so deleting the source
                        // line replaces a narrow rule with a broad one
                        // and nothing says it happened. Same gate
                        // hyprforge-shortcuts applies via `is_complete`.
                        if let (Some(line), true) = (candidate.line, candidate.dropped.is_empty()) {
                            to_remove.push((candidate.source_path.clone(), line));
                        }
                        self.rules.push(Rule {
                            name,
                            enabled: candidate.enabled,
                            matcher: candidate.matcher,
                            effects: candidate.effects,
                        });
                    }
                    for candidate in review.workspace_rules.into_iter().filter(|c| c.checked) {
                        // Same gate as the window rules above.
                        if let (Some(line), true) = (candidate.line, candidate.dropped.is_empty()) {
                            to_remove.push((candidate.source_path.clone(), line));
                        }
                        self.workspace_rules.push(candidate.rule);
                    }
                }
                // Order matters: the store has to be *on disk* before
                // anything is deleted from the user's config. Pushing onto
                // `self.rules` above is not "safely added" — it's an
                // in-memory Vec, and a failed write or a crash between there
                // and here would lose the rules with no copy anywhere. This
                // exact ordering cost a real user 37 hand-written binds in
                // the Shortcuts module.
                if self.persist().is_err() {
                    return Task::none();
                }
                // Best-effort from here: the entries are on disk, and
                // `remove_matched_lines` itself only ever removes a line
                // that still verifiably looks like the exact call it
                // recorded — see its doc comment for why that's safe to
                // not gate behind another confirmation.
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
                "Your rules couldn't be read",
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
                            hyprforge_core::paths::window_rules_toml_path().display()
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

        let mut rows: Vec<Element<'_, Message>> = Vec::new();
        for (i, rule) in self.rules.iter().enumerate() {
            let summary = rule
                .matcher
                .class
                .clone()
                .or_else(|| rule.matcher.title.clone())
                .unwrap_or_else(|| "(no match)".to_string());
            // An armed row swaps its whole action set for the confirm pair,
            // so the only two things that can happen next are the two the
            // user is being asked about. Delete is quiet until then, the
            // same as on Keybinds: red on every row made the danger colour
            // the list's texture instead of a warning.
            let actions: Element<'_, Message> = if self.pending_delete == Some(PendingDelete::Rule(i)) {
                row![
                    secondary_button("Keep").on_press(Message::DeleteCancel),
                    danger_button("Delete for good", Message::DeleteConfirm(i)),
                ]
                .spacing(spacing::XS)
                .into()
            } else {
                row![
                    secondary_button("Up").on_press(Message::MoveUp(i)),
                    secondary_button("Down").on_press(Message::MoveDown(i)),
                    secondary_button("Edit").on_press(Message::Edit(i)),
                    secondary_button("Delete").on_press(Message::Delete(i)),
                ]
                .spacing(spacing::XS)
                .into()
            };
            rows.push(setting_row(
                i,
                summary,
                Some(config_line(rule.name.clone(), scale).into()),
                row![toggle(rule.enabled, scale).on_toggle(move |_| Message::ToggleEnabled(i)), actions]
                    .spacing(spacing::MD)
                    .align_y(iced::Alignment::Center),
                scale,
            ));
        }

        // The whole list, in the page's one scroll area: it sat in a
        // scrollable of its own capped at 360px, so a long list scrolled
        // inside a page that also scrolled. (The window picker keeps its
        // capped list — that one is a chooser, like a dropdown's menu.)
        let list: Element<'_, Message> = if rows.is_empty() {
            meta_text("No rules yet.", BASE_TEXT_SIZE, scale).into()
        } else {
            setting_list(rows).into()
        };
        content = content.push(column![section_label("Rules", scale), list].spacing(spacing::SM));
        content = content.push(
            container(
                row![
                    secondary_button("Import from config").on_press(Message::ImportFromConfig),
                    primary_button("Add rule").on_press(Message::Add),
                ]
                .spacing(spacing::SM),
            )
            .width(Length::Fill)
            .align_x(iced::alignment::Horizontal::Right),
        );

        content = content.push(self.workspace_pins_view(scale));

        container(content).padding(spacing::LG).into()
    }
}

impl WindowRulesModule {
    /// The "Import from config" flow's loading state and review list.
    /// Reachable any time, not just on first run — unlike the setup
    /// banner, this is opt-in and re-runnable whenever the user wants to
    /// pick up hand-written rules added since the last import.
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
                failures = failures.push(meta_text(
                    format!("{} — {reason}", path.display()),
                    12.0,
                    scale,
                ));
            }
            body = body.push(section("Couldn't import", scale, failures.spacing(spacing::XS)));
        }

        if review.rules.is_empty() && review.workspace_rules.is_empty() {
            body = body.push(meta_text(
                "No importable window rules or workspace pins found.",
                BASE_TEXT_SIZE,
                scale,
            ));
        }

        if !review.rules.is_empty() {
            let mut list = column![].spacing(spacing::SM);
            for (i, candidate) in review.rules.iter().enumerate() {
                let summary = candidate
                    .matcher
                    .class
                    .clone()
                    .or_else(|| candidate.matcher.title.clone())
                    .unwrap_or_else(|| "(no match)".to_string());
                let mut info = column![scaled_text(summary, BASE_TEXT_SIZE, scale)]
                    .spacing(spacing::XS)
                    .width(Length::Fill);
                if candidate.already_imported {
                    info = info.push(already_imported_note(scale));
                }
                if !candidate.dropped.is_empty() {
                    info = info.push(dropped_keys_note(&candidate.dropped, scale));
                }
                list = list.push(
                    row![
                        checkbox(candidate.checked)
                            .on_toggle(move |v| Message::ImportToggleRule(i, v)),
                        info,
                    ]
                    .spacing(spacing::SM)
                    .align_y(iced::Alignment::Center),
                );
            }
            body = body.push(section("Window rules", scale, list));
        }

        if !review.workspace_rules.is_empty() {
            let mut list = column![].spacing(spacing::SM);
            for (i, candidate) in review.workspace_rules.iter().enumerate() {
                let mut info =
                    column![scaled_text(candidate.rule.workspace.clone(), BASE_TEXT_SIZE, scale)]
                        .spacing(spacing::XS)
                        .width(Length::Fill);
                if candidate.already_imported {
                    info = info.push(already_imported_note(scale));
                }
                if !candidate.dropped.is_empty() {
                    info = info.push(dropped_keys_note(&candidate.dropped, scale));
                }
                list = list.push(
                    row![
                        checkbox(candidate.checked)
                            .on_toggle(move |v| Message::ImportToggleWorkspaceRule(i, v)),
                        info,
                    ]
                    .spacing(spacing::SM)
                    .align_y(iced::Alignment::Center),
                );
            }
            body = body.push(section("Workspace pins", scale, list));
        }

        body = body.push(meta_text(
            "Checked entries are added here and their original line is removed \
             from your config — only when it still matches exactly what was \
             imported, and only after it's safely added. A multi-line entry, or \
             one edited since this list was generated, is left in place instead \
             of guessed at.",
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

    /// Workspace→monitor pins.
    ///
    /// A separate section from the rules list because it's a different kind
    /// of statement: a rule says what a *window* does, a pin says where a
    /// *workspace* lives. Together they're how "Steam on the external
    /// display" gets expressed — the rule sends Steam to a workspace, the pin
    /// puts that workspace on the monitor.
    /// The exact line this draft writes. Rendered by the same codegen the
    /// real file gets — a window rule is the most opaque thing this module
    /// produces, and showing it is what makes the form's effect legible
    /// (vision pillar #4: instant, visible feedback).
    fn preview_section(&self, draft: &RuleDraft, scale: FontScale) -> Element<'_, Message> {
        let text = if draft.matches_nothing() {
            "Nothing yet — a rule needs something to match on.".to_string()
        } else {
            draft.preview(&self.name_for(draft)).trim().to_string()
        };
        column![
            scaled_text(text, 12.0, scale).font(hyprforge_ui::theme::mono_font()),
            meta_text("Written to ~/.config/hypr/hyprforge/window-rules.lua", 11.0, scale),
        ]
        .spacing(spacing::XS)
        .into()
    }

    /// The name this draft will be stored under — its existing one when
    /// editing, a freshly generated one when new.
    ///
    /// Shared with the preview rather than computed at save time only: the
    /// name is emitted in the generated line, so a preview showing a
    /// different one would be wrong.
    fn name_for(&self, draft: &RuleDraft) -> String {
        if let Some(i) = draft.editing_index {
            return self.rules[i].name.clone();
        }
        let label = if !draft.class.trim().is_empty() {
            draft.class.trim()
        } else if !draft.title.trim().is_empty() {
            draft.title.trim()
        } else {
            "rule"
        };
        let existing: Vec<String> = self.rules.iter().map(|r| r.name.clone()).collect();
        generate_rule_name(label, &existing)
    }

    fn workspace_pins_view(&self, scale: FontScale) -> Element<'_, Message> {
        let mut choices: Vec<MonitorChoice> =
            self.monitors.iter().map(MonitorChoice::from_monitor).collect();

        let mut list = column![].spacing(spacing::MD);
        if self.workspace_rules.is_empty() {
            list = list.push(meta_text(
                "No workspaces pinned. Pin one to a monitor, then send windows \
                 to that workspace with a rule's Workspace field.",
                13.0,
                scale,
            ));
        }

        for (i, pin) in self.workspace_rules.iter().enumerate() {
            // A pin for a monitor that isn't plugged in right now still has
            // to show what it points at, or editing anything else on the row
            // would look like the monitor was never set.
            let selected = if pin.monitor.trim().is_empty() {
                None
            } else {
                let known = choices.iter().find(|c| c.selector == pin.monitor).cloned();
                Some(known.unwrap_or_else(|| {
                    let stored = MonitorChoice::from_stored(&pin.monitor);
                    if !choices.contains(&stored) {
                        choices.push(stored.clone());
                    }
                    stored
                }))
            };

            let pin_actions: Vec<Element<'_, Message>> =
                if self.pending_delete == Some(PendingDelete::WorkspacePin(i)) {
                    vec![
                        secondary_button("Keep").on_press(Message::DeleteCancel).into(),
                        danger_button("Remove for good", Message::DeleteWorkspacePinConfirm(i)),
                    ]
                } else {
                    vec![danger_button("Remove", Message::DeleteWorkspacePin(i))]
                };
            let mut entry = column![
                row![
                    text_input("workspace, e.g. 3 or name:gaming", &pin.workspace)
                        .on_input(move |v| Message::PinWorkspace(i, v)),
                    iced::widget::pick_list(choices.clone(), selected, move |c| {
                        Message::PinMonitor(i, c)
                    })
                    .placeholder("on monitor…"),
                ]
                .extend(pin_actions)
                .spacing(spacing::SM)
                .align_y(iced::Alignment::Center),
                row_field("Default workspace for that monitor", toggle(pin.default, scale).on_toggle(move |v| Message::PinDefault(i, v))),
                row_field("Keep alive when empty", toggle(pin.persistent, scale).on_toggle(move |v| Message::PinPersistent(i, v))),
            ]
            .spacing(spacing::XS);
            // A pin with no workspace names nothing, so codegen skips it —
            // silently, which from the user's side is a row that sits there
            // looking configured and does nothing at all.
            if pin.workspace.trim().is_empty() {
                entry = entry.push(
                    scaled_text(
                        "Name a workspace — this pin does nothing until you do.",
                        12.0,
                        scale,
                    )
                    .color(hyprforge_ui::theme::warning()),
                );
            }
            list = list.push(entry);
        }

        let note = if self.monitors.is_empty() {
            Some(meta_text(
                "Couldn't read the connected monitors, so there's nothing to \
                 choose from — existing pins are still listed and kept.",
                12.0,
                scale,
            ))
        } else {
            // Worth saying out loud: this is why a pin survives a replug.
            Some(meta_text(
                "Monitors are matched by their description rather than their \
                 connector, so a pin still applies after replugging.",
                12.0,
                scale,
            ))
        };

        let mut body = column![list].spacing(spacing::MD);
        if let Some(note) = note {
            body = body.push(note);
        }
        body = body.push(
            container(secondary_button("Pin a workspace").on_press(Message::AddWorkspacePin))
                .width(Length::Fill),
        );

        section("Workspaces on monitors", scale, body)
    }

    /// The open-window list, inline under the form rather than as a modal —
    /// you're choosing a value *for* a field, and the field should stay
    /// visible while you do it.
    fn picker_view<'a>(
        &'a self,
        picker: &'a PickerState,
        scale: FontScale,
    ) -> Element<'a, Message> {
        let inner: Element<'_, Message> = match picker {
            PickerState::Loading => meta_text("Reading open windows…", 13.0, scale).into(),
            PickerState::Unavailable(why) => column![
                meta_text(
                    "Couldn't read the open windows — is Hyprland running? \
                     You can still type the class in by hand.",
                    13.0,
                    scale,
                ),
                meta_text(why.as_str(), 12.0, scale),
            ]
            .spacing(spacing::SM)
            .into(),
            PickerState::Loaded(clients) if clients.is_empty() => {
                meta_text("No open windows to pick from.", 13.0, scale).into()
            }
            PickerState::Loaded(clients) => {
                let mut list = column![].spacing(spacing::SM);
                for (i, client) in clients.iter().enumerate() {
                    // Class is the whole row: it's the choice you almost
                    // always want. The title is a separate, smaller action
                    // because a title-matched rule quietly stops working when
                    // the app renames its window — a browser tab, a file path
                    // — and that should be deliberate.
                    let mut actions = row![secondary_button(client.label()).on_press(
                        Message::PickClass(i)
                    )]
                    .spacing(spacing::SM);
                    if !client.title.trim().is_empty() {
                        actions = actions
                            .push(secondary_button("+ title").on_press(Message::PickTitle(i)));
                    }
                    list = list.push(
                        column![
                            actions,
                            meta_text(
                                format!(
                                    "workspace {}{}",
                                    client.workspace.name,
                                    if client.xwayland { " · XWayland" } else { "" }
                                ),
                                11.0,
                                scale,
                            ),
                        ]
                        .spacing(2),
                    );
                }
                column![
                    meta_text("Fills the class. Add the title only if you want the rule to apply to this one window.", 12.0, scale),
                    container(scrollable(list).width(Length::Fill)).max_height(240.0),
                ]
                .spacing(spacing::SM)
                .into()
            }
        };

        section(
            "Open windows",
            scale,
            column![
                inner,
                container(secondary_button("Close").on_press(Message::ClosePicker))
                    .width(Length::Fill),
            ]
            .spacing(spacing::MD),
        )
    }

    fn draft_view(&self, draft: &RuleDraft, scale: FontScale) -> Element<'_, Message> {
        let title = if draft.editing_index.is_some() {
            "Edit rule"
        } else {
            "New rule"
        };
        let form = column![
            row_field(
                "Class",
                row![
                    text_input("Window class (regex)", &draft.class)
                        .on_input(Message::DraftClass),
                    secondary_button("Pick a window…").on_press(Message::OpenPicker),
                ]
                .spacing(spacing::SM),
            ),
            row_field(
                "Title",
                text_input("Window title (regex)", &draft.title).on_input(Message::DraftTitle),
            ),
            // Workspace assignment sits in the primary form rather than
            // behind "advanced": it's the rule most people are here to
            // write, and burying it would make the common case the hidden
            // one.
            row_field(
                "Workspace",
                text_input("e.g. 3, name:coding, special:scratchpad", &draft.workspace)
                    .on_input(Message::DraftWorkspace),
            ),
            row_field("Open there without switching to it", toggle(draft.workspace_silent, scale).on_toggle(Message::DraftWorkspaceSilent)),
            row_field("Float", toggle(draft.float, scale).on_toggle(Message::DraftFloat)),
            row_field("Disable blur", toggle(draft.no_blur, scale).on_toggle(Message::DraftNoBlur)),
            row_field(
                "Rounding (px)",
                text_input("e.g. 8", &draft.rounding).on_input(Message::DraftRounding),
            ),
            row_field(
                "Border color",
                text_input("e.g. rgb(FF0000)", &draft.border_color)
                    .on_input(Message::DraftBorderColor),
            ),
        ]
        .spacing(spacing::MD);

        let mut body = column![
            // A heading, not a second page title: the shell already says
            // "Window rules" above this.
            scaled_text(title, 16.0, scale)
                .font(iced::Font { weight: iced::font::Weight::Semibold, ..iced::Font::DEFAULT }),
            section("Rule", scale, form),
        ]
        .spacing(spacing::LG)
        .max_width(520.0);

        if let Some(picker) = &self.picker {
            body = body.push(self.picker_view(picker, scale));
        }

        body = body.push(
            container(
                secondary_button(if self.show_advanced {
                    "Hide advanced"
                } else {
                    "Show advanced"
                })
                .on_press(Message::ToggleAdvanced),
            )
            .width(Length::Fill),
        );

        if self.show_advanced {
            body = body.push(section(
                "Match on more",
                scale,
                column![
                    // initial_* match the class/title the window had when it
                    // opened, which is what you need for apps that rename
                    // themselves after startup.
                    row_field(
                        "Initial class",
                        text_input("Class at open time (regex)", &draft.initial_class)
                            .on_input(Message::DraftInitialClass),
                    ),
                    row_field(
                        "Initial title",
                        text_input("Title at open time (regex)", &draft.initial_title)
                            .on_input(Message::DraftInitialTitle),
                    ),
                    row_field("Fullscreen", tri_state(draft.fullscreen, Message::DraftFullscreen)),
                    row_field("Floating", tri_state(draft.floating, Message::DraftFloating)),
                    row_field("XWayland", tri_state(draft.xwayland, Message::DraftXwayland)),
                    row_field(
                        "Tag",
                        text_input("e.g. term — also matches term*", &draft.match_tag)
                            .on_input(Message::DraftMatchTag),
                    ),
                    row_field(
                        "Content type",
                        text_input("e.g. game, video", &draft.content)
                            .on_input(Message::DraftContent),
                    ),
                ]
                .spacing(spacing::MD),
            ));

            body = body.push(section(
                "Tag",
                scale,
                column![
                    row_field(
                        "Apply tag",
                        text_input("e.g. +code", &draft.tag).on_input(Message::DraftTag),
                    ),
                    meta_text(
                        "Prefix + to add and - to remove; no prefix toggles. Tagged \
                         windows can then be matched by other rules.",
                        12.0,
                        scale,
                    ),
                ]
                .spacing(spacing::SM),
            ));

            body = body.push(section(
                "Position & size",
                scale,
                column![
                    meta_text(
                        "Plain numbers are pixels. Anything else is passed to Hyprland \
                         as an expression, e.g. cursor_x-(window_w*0.5) or 60%.",
                        12.0,
                        scale,
                    ),
                    row![
                        text_input("x", &draft.move_x).on_input(Message::DraftMoveX),
                        text_input("y", &draft.move_y).on_input(Message::DraftMoveY),
                    ]
                    .spacing(spacing::SM),
                    row![
                        text_input("width", &draft.size_w).on_input(Message::DraftSizeW),
                        text_input("height", &draft.size_h).on_input(Message::DraftSizeH),
                    ]
                    .spacing(spacing::SM),
                    meta_text("Both halves of a pair are needed for it to apply.", 12.0, scale),
                ]
                .spacing(spacing::SM),
            ));

            body = body.push(section(
                "Opacity",
                scale,
                column![
                    row_field(
                        "Active",
                        text_input("1.0", &draft.opacity_active)
                            .on_input(Message::DraftOpacityActive),
                    ),
                    row_field(
                        "Inactive",
                        text_input("1.0", &draft.opacity_inactive)
                            .on_input(Message::DraftOpacityInactive),
                    ),
                    row_field(
                        "Fullscreen",
                        text_input("1.0", &draft.opacity_fullscreen)
                            .on_input(Message::DraftOpacityFullscreen),
                    ),
                    row_field("Absolute (override) rather than multiplied", toggle(draft.opacity_override, scale).on_toggle(Message::DraftOpacityOverride)),
                ]
                .spacing(spacing::MD),
            ));

            body = body.push(section(
                "Appearance",
                scale,
                column![
                    row_field("Force opaque", toggle(draft.opaque, scale).on_toggle(Message::DraftOpaque)),
                    row_field("Disable animations", toggle(draft.no_anim, scale).on_toggle(Message::DraftNoAnim)),
                    row_field("Dim everything around it", toggle(draft.dim_around, scale).on_toggle(Message::DraftDimAround)),
                    row_field(
                        "Border size (px)",
                        text_input("e.g. 4 — 0 removes the border", &draft.border_size)
                            .on_input(Message::DraftBorderSize),
                    ),
                    row_field(
                        "Animation",
                        text_input("e.g. popin, or popin 80%", &draft.animation)
                            .on_input(Message::DraftAnimation),
                    ),
                ]
                .spacing(spacing::MD),
            ));

            body = body.push(section(
                "Focus & sizing",
                scale,
                column![
                    row_field("Never focus this window", toggle(draft.no_focus, scale).on_toggle(Message::DraftNoFocus)),
                    row_field("Keep focus while visible", toggle(draft.stay_focused, scale).on_toggle(Message::DraftStayFocused)),
                    row_field("Keep aspect ratio when resizing", toggle(draft.keep_aspect_ratio, scale).on_toggle(Message::DraftKeepAspectRatio)),
                    row_field(
                        "Idle inhibit",
                        iced::widget::pick_list(
                            IdleInhibitChoice::all(),
                            Some(IdleInhibitChoice { mode: draft.idle_inhibit.clone() }),
                            Message::DraftIdleInhibit,
                        ),
                    ),
                    meta_text("Minimum and maximum size, in pixels. Floating windows only.", 12.0, scale),
                    row![
                        text_input("min width", &draft.min_w).on_input(Message::DraftMinW),
                        text_input("min height", &draft.min_h).on_input(Message::DraftMinH),
                    ]
                    .spacing(spacing::SM),
                    row![
                        text_input("max width", &draft.max_w).on_input(Message::DraftMaxW),
                        text_input("max height", &draft.max_h).on_input(Message::DraftMaxH),
                    ]
                    .spacing(spacing::SM),
                ]
                .spacing(spacing::MD),
            ));

            // How the window comes up, as distinct from how it behaves once
            // it's up — these are applied as it opens.
            let mut monitor_choices = vec![MonitorChoice::any()];
            monitor_choices.extend(self.monitors.iter().map(MonitorChoice::from_monitor));
            let selected_monitor = if draft.monitor.trim().is_empty() {
                MonitorChoice::any()
            } else {
                monitor_choices
                    .iter()
                    .find(|c| c.selector == draft.monitor)
                    .cloned()
                    .unwrap_or_else(|| {
                        let stored = MonitorChoice::from_stored(&draft.monitor);
                        monitor_choices.push(stored.clone());
                        stored
                    })
            };

            body = body.push(section(
                "How it opens",
                scale,
                column![
                    row_field("Open tiled", toggle(draft.tile, scale).on_toggle(Message::DraftTile)),
                    row_field("Open fullscreen", toggle(draft.fullscreen_effect, scale).on_toggle(Message::DraftFullscreenEffect)),
                    row_field("Open maximized", toggle(draft.maximize, scale).on_toggle(Message::DraftMaximize)),
                    row_field("Center it", toggle(draft.center, scale).on_toggle(Message::DraftCenter)),
                    row_field("Pin above workspaces (floating windows only)", toggle(draft.pin, scale).on_toggle(Message::DraftPin)),
                    row_field("Don't focus it when it opens", toggle(draft.no_initial_focus, scale).on_toggle(Message::DraftNoInitialFocus)),
                    row_field(
                        "Monitor",
                        iced::widget::pick_list(
                            monitor_choices,
                            Some(selected_monitor),
                            Message::DraftMonitor,
                        ),
                    ),
                    meta_text(
                        "Matched by description, so it survives a replug. For a whole \
                         workspace rather than one window, use Workspaces on monitors below.",
                        12.0,
                        scale,
                    ),
                    row_field(
                        "Suppress event",
                        text_input(
                            "fullscreen, maximize, activate, activatefocus",
                            &draft.suppress_event,
                        )
                        .on_input(Message::DraftSuppressEvent),
                    ),
                    row_field(
                        "Group",
                        text_input("new, lock, deny, barred", &draft.group)
                            .on_input(Message::DraftGroup),
                    ),
                    meta_text(
                        "Hyprland accepts any text for those two without complaining, \
                         so a value it doesn't recognise does nothing rather than \
                         reporting an error — the listed ones are the ones that act.",
                        12.0,
                        scale,
                    ),
                    row_field(
                        "Refuse close requests for",
                        text_input("no_close_for, in Hyprland's own units", &draft.no_close_for)
                            .on_input(Message::DraftNoCloseFor),
                    ),
                ]
                .spacing(spacing::MD),
            ));
        }

        body = body.push(section("Writes", scale, self.preview_section(draft, scale)));

        // Disabled rather than hidden, with the reason shown above it: a
        // button that vanishes leaves no clue what's missing.
        let blockers = draft.blockers();
        if let Some(first) = blockers.first() {
            body = body.push(
                scaled_text(first.clone(), 12.0, scale).color(hyprforge_ui::theme::warning()),
            );
        }
        let save = primary_button("Save");
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
}

/// Reads the connected monitors. A failure is flattened to an empty list
/// rather than surfaced: the pin section still works without it, and the
/// module already reports a missing `hyprctl` when a save actually needs one.
async fn load_monitors() -> Vec<Monitor> {
    tokio::task::spawn_blocking(hyprforge_windowrules::monitors::list_monitors)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default()
}

/// Reads the open windows off the compositor. Blocking work (it shells out to
/// `hyprctl`), so it goes on the blocking pool rather than stalling the UI
/// thread — same treatment `regenerate_and_reload` gets below.
async fn load_clients() -> Result<Vec<Client>, String> {
    tokio::task::spawn_blocking(hyprforge_windowrules::clients::list_clients)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

async fn regenerate_and_reload(rules: hyprforge_windowrules::storage::Rules) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        hyprforge_windowrules::apply::apply(&hyprforge_core::paths::window_rules_lua_path(), &rules)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}


#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `f` against a module whose config lives in a throwaway directory.
    ///
    /// `WindowRulesModule::new()` reads the canonical TOML and several update
    /// arms write it straight back, so a test that skips this loads — and
    /// *saves over* — the real `~/.config/hyprforge/window-rules.toml`. That
    /// happened: a run wrote its fixture pins into the developer's own
    /// config, and the tests then failed because each one loaded what the
    /// last had left behind.
    ///
    /// A fresh directory per test, because sharing one just moves the
    /// contamination from the real config into a shared temp one. The lock
    /// is what makes that safe: `$XDG_CONFIG_HOME` is process-global and
    /// these tests run on parallel threads, so only one may own it at a time.
    /// Same reasoning as the `ENV_LOCK` in `hyprforge-core`'s `paths` tests.
    fn with_isolated_module(f: impl FnOnce(&mut WindowRulesModule)) {
        // A test that panics while holding the lock poisons it; the next
        // test still needs to run, and there's no shared state to corrupt.
        let _lock = crate::modules::CONFIG_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let dir = tempfile::tempdir().expect("could not make a temp config dir");
        let previous = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", dir.path());

        let mut module = WindowRulesModule::new().0;
        f(&mut module);

        match previous {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
    }

    fn fully_populated_rule() -> Rule {
        Rule {
            name: "hyprforge-test-1".to_string(),
            enabled: true,
            matcher: Matcher {
                class: Some("discord".to_string()),
                title: Some("^Discord$".to_string()),
                initial_class: Some("Discord".to_string()),
                initial_title: Some("Starting".to_string()),
                fullscreen: Some(true),
                // Explicitly false, not unset — the case a plain checkbox
                // can't represent.
                floating: Some(false),
                xwayland: Some(true),
                tag: Some("term".to_string()),
                content: Some("game".to_string()),
            },
            effects: Effects {
                workspace: Workspace { name: "name:coding".to_string(), silent: true },
                tag: Some("+code".to_string()),
                float: Some(true),
                r#move: Some(["cursor_x-(window_w*0.5)".to_string(), "40".to_string()]),
                size: Some(["60%".to_string(), "480".to_string()]),
                opacity: Opacity {
                    active: Some(0.9),
                    inactive: Some(0.7),
                    fullscreen: Some(1.0),
                    is_override: true,
                },
                border_color: Some("rgb(FF0000)".to_string()),
                no_blur: Some(true),
                rounding: Some(8),
                opaque: Some(true),
                no_anim: Some(true),
                no_focus: Some(true),
                stay_focused: Some(true),
                dim_around: Some(true),
                keep_aspect_ratio: Some(true),
                border_size: Some(4),
                min_size: Some([200, 150]),
                max_size: Some([800, 600]),
                animation: Some("popin 80%".to_string()),
                idle_inhibit: Some("focus".to_string()),
                tile: Some(true),
                fullscreen: Some(true),
                maximize: Some(true),
                pin: Some(true),
                center: Some(true),
                no_initial_focus: Some(true),
                monitor: Some("desc:hyprforge-live-probe-monitor".to_string()),
                suppress_event: Some("maximize".to_string()),
                group: Some("deny".to_string()),
                no_close_for: Some(5),
            },
        }
    }

    /// The invariant that keeps editing non-destructive: anything the form
    /// can hold must survive load → save unchanged. A field that's writable
    /// but not readable silently erases itself on the user's next edit.
    #[test]
    fn draft_round_trips_every_field() {
        let rule = fully_populated_rule();
        let (matcher, effects) = RuleDraft::from_rule(0, &rule).into_matcher_effects();
        assert_eq!(matcher, rule.matcher);
        assert_eq!(effects, rule.effects);
    }

    /// `false` is not the same as absent: rules are ordered and a later
    /// `float = false` is how you cancel an earlier `float = true`. A
    /// checkbox can't show the difference, so opening a rule and saving it
    /// unchanged must not quietly turn one into the other.
    #[test]
    fn draft_preserves_explicitly_false_effects() {
        let mut rule = fully_populated_rule();
        rule.effects.float = Some(false);
        rule.effects.no_focus = Some(false);
        rule.effects.opaque = None;

        let (_, effects) = RuleDraft::from_rule(0, &rule).into_matcher_effects();
        assert_eq!(effects.float, Some(false), "an explicit false must survive an untouched edit");
        assert_eq!(effects.no_focus, Some(false));
        assert_eq!(effects.opaque, None, "unset stays unset");
    }

    /// ...but ticking the box still means `true`, and unticking one that was
    /// `true` still removes it. Preserving `false` must not make the
    /// checkbox itself stop working.
    #[test]
    fn toggling_a_preserved_false_effect_still_works() {
        let mut rule = fully_populated_rule();
        rule.effects.float = Some(false);

        let mut draft = RuleDraft::from_rule(0, &rule);
        draft.float = true;
        assert_eq!(draft.clone().into_matcher_effects().1.float, Some(true));

        let mut on = RuleDraft::from_rule(0, &fully_populated_rule());
        assert_eq!(on.stored_effects.float, Some(true));
        on.float = false;
        assert_eq!(
            on.into_matcher_effects().1.float,
            None,
            "unticking a stored true removes the effect rather than storing false"
        );
    }

    #[test]
    fn draft_preserves_explicitly_false_matchers() {
        let mut rule = fully_populated_rule();
        rule.matcher.floating = Some(false);
        rule.matcher.fullscreen = None;

        let (matcher, _) = RuleDraft::from_rule(0, &rule).into_matcher_effects();
        assert_eq!(
            matcher.floating,
            Some(false),
            "floating = false selects tiled windows; collapsing it to None would \
             widen the rule to match everything"
        );
        assert_eq!(matcher.fullscreen, None);
    }

    /// A rule with no match field is stored, listed, and skipped by codegen
    /// — so without this the user gets a rule that looks configured and
    /// never once applies, with nothing anywhere saying why.
    #[test]
    fn a_rule_that_matches_nothing_cannot_be_saved() {
        let mut draft = RuleDraft { float: true, ..Default::default() };
        assert!(
            draft.blockers().iter().any(|b| b.contains("Match at least one")),
            "{:?}",
            draft.blockers()
        );

        draft.class = "discord".to_string();
        assert!(draft.blockers().is_empty(), "{:?}", draft.blockers());
    }

    /// A tri-state matcher counts as matching something, even though it's
    /// not a text field.
    #[test]
    fn a_tri_state_matcher_alone_is_enough_to_match_on() {
        let draft = RuleDraft { floating: Some(true), ..Default::default() };
        assert!(draft.blockers().is_empty(), "{:?}", draft.blockers());
    }

    /// Every numeric field is `parse().ok()` on save, so anything unreadable
    /// vanishes without a word. Blocking is what turns that into a message.
    #[test]
    fn unreadable_numbers_are_reported_rather_than_dropped() {
        let base = RuleDraft { class: "discord".to_string(), ..Default::default() };

        let rounding = RuleDraft { rounding: "10px".to_string(), ..base.clone() };
        assert!(rounding.blockers().iter().any(|b| b.contains("Rounding")), "{:?}", rounding.blockers());

        let opacity = RuleDraft { opacity_active: "ninety".to_string(), ..base.clone() };
        assert!(opacity.blockers().iter().any(|b| b.contains("Active opacity")));

        // A valid one doesn't block.
        let ok = RuleDraft {
            rounding: "10".to_string(),
            opacity_active: "0.9".to_string(),
            ..base
        };
        assert!(ok.blockers().is_empty(), "{:?}", ok.blockers());
    }

    /// Hyprland's `{ a, b }` can't express one half, so a half-filled pair
    /// is dropped entirely on save — including the half the user typed.
    #[test]
    fn a_half_filled_pair_is_reported_rather_than_dropped() {
        let base = RuleDraft { class: "discord".to_string(), ..Default::default() };

        let half_move = RuleDraft { move_x: "100".to_string(), ..base.clone() };
        assert!(half_move.blockers().iter().any(|b| b.contains("Position")), "{:?}", half_move.blockers());

        let half_min = RuleDraft { min_w: "200".to_string(), ..base.clone() };
        assert!(half_min.blockers().iter().any(|b| b.contains("Minimum size")));

        let both = RuleDraft {
            move_x: "100".to_string(),
            move_y: "40".to_string(),
            ..base
        };
        assert!(both.blockers().is_empty(), "{:?}", both.blockers());
    }

    /// Save must actually refuse, not just look disabled.
    #[test]
    fn saving_a_blocked_draft_does_nothing() {
        with_isolated_module(|m| {
            m.draft = Some(RuleDraft { float: true, ..Default::default() });
            let _ = m.update(Message::DraftSave);
            assert!(m.rules.is_empty(), "a rule matching nothing must not be stored");
            assert!(m.draft.is_some(), "and the form stays open to be fixed");
        });
    }

    /// The preview is the real thing, not an approximation — and it names
    /// the rule the same way the save does, so the line shown is the line
    /// written.
    #[test]
    fn the_preview_matches_what_is_saved() {
        with_isolated_module(|m| {
            m.draft = Some(RuleDraft {
                class: "discord".to_string(),
                float: true,
                ..Default::default()
            });
            let draft = m.draft.as_ref().unwrap();
            let preview = draft.preview(&m.name_for(draft));
            assert!(preview.contains("class = [[discord]]"), "got: {preview}");
            assert!(preview.contains("float = true"), "got: {preview}");
            assert!(preview.contains("name = [[hyprforge-discord-1]]"), "got: {preview}");

            m.commit_draft();
            assert_eq!(hyprforge_windowrules::codegen::render_one(&m.rules[0]), preview);
        });
    }

    #[test]
    fn blank_fields_become_none_rather_than_empty_strings() {
        let (matcher, effects) = RuleDraft::default().into_matcher_effects();
        assert!(matcher.is_empty());
        assert_eq!(effects, Effects::default());
    }

    #[test]
    fn whitespace_only_input_counts_as_blank() {
        let draft = RuleDraft {
            class: "   ".to_string(),
            rounding: "  ".to_string(),
            ..Default::default()
        };
        let (matcher, effects) = draft.into_matcher_effects();
        assert_eq!(matcher.class, None);
        assert_eq!(effects.rounding, None);
    }

    #[test]
    fn a_half_filled_move_pair_is_dropped() {
        // Hyprland's move takes { x, y }; there is no way to express "x only",
        // so a partial pair must not be emitted as a bogus rule.
        let draft = RuleDraft {
            move_x: "100".to_string(),
            size_h: "480".to_string(),
            ..Default::default()
        };
        let (_, effects) = draft.into_matcher_effects();
        assert_eq!(effects.r#move, None);
        assert_eq!(effects.size, None);
    }

    #[test]
    fn unparseable_numbers_are_dropped_not_defaulted() {
        // Mid-typing garbage must not silently become 0 — that would apply a
        // real rounding of 0 the user never asked for.
        let draft = RuleDraft {
            rounding: "abc".to_string(),
            opacity_active: "not-a-number".to_string(),
            ..Default::default()
        };
        let (_, effects) = draft.into_matcher_effects();
        assert_eq!(effects.rounding, None);
        assert_eq!(effects.opacity.active, None);
    }

    /// The whole chain the GUI actually exercises: form state -> model ->
    /// generated Lua. Guards the seam between this module and the codegen,
    /// which the per-layer tests on either side don't cover.
    #[test]
    fn an_advanced_draft_reaches_the_generated_lua() {
        let draft = RuleDraft {
            class: "discord".to_string(),
            floating: Some(false),
            initial_class: "Discord".to_string(),
            move_x: "cursor_x-(window_w*0.5)".to_string(),
            move_y: "40".to_string(),
            size_w: "60%".to_string(),
            size_h: "480".to_string(),
            opacity_active: "0.9".to_string(),
            opacity_inactive: "0.7".to_string(),
            opacity_override: true,
            ..Default::default()
        };
        let (matcher, effects) = draft.into_matcher_effects();
        let lua = hyprforge_windowrules::codegen::generate(&[Rule {
            name: "hyprforge-discord-1".to_string(),
            enabled: true,
            matcher,
            effects,
        }], &[]);

        assert!(lua.contains("initial_class = [[Discord]]"), "{lua}");
        // Emitted as `float`, which is what Hyprland calls the matcher —
        // the draft field and TOML key stay `floating`.
        assert!(lua.contains("float = false"), "{lua}");
        // A literal is emitted bare; an expression is quoted.
        assert!(lua.contains("move = { [[cursor_x-(window_w*0.5)]], 40 }"), "{lua}");
        assert!(lua.contains("size = { [[60%]], 480 }"), "{lua}");
        assert!(lua.contains("opacity = [[0.9 override 0.7 override]]"), "{lua}");
    }

    /// The headline case, end to end: what a user types into the form
    /// becomes the Lua that puts Discord on workspace 3 without yanking
    /// them to it.
    #[test]
    fn a_typed_workspace_reaches_the_generated_lua() {
        let draft = RuleDraft {
            class: "discord".to_string(),
            workspace: "3".to_string(),
            workspace_silent: true,
            ..Default::default()
        };
        let (matcher, effects) = draft.into_matcher_effects();
        let lua = hyprforge_windowrules::codegen::generate(&[Rule {
            name: "hyprforge-discord-1".to_string(),
            enabled: true,
            matcher,
            effects,
        }], &[]);
        assert!(lua.contains("workspace = [[3 silent]]"), "{lua}");
    }

    /// The checkbox can be left on with the field cleared. "Silently open on
    /// no workspace at all" isn't a rule, so it must not be written.
    #[test]
    fn silent_without_a_workspace_applies_nothing() {
        let draft = RuleDraft {
            class: "discord".to_string(),
            workspace: "   ".to_string(),
            workspace_silent: true,
            ..Default::default()
        };
        let (matcher, effects) = draft.into_matcher_effects();
        assert!(effects.workspace.is_empty());
        let lua = hyprforge_windowrules::codegen::generate(&[Rule {
            name: "hyprforge-discord-1".to_string(),
            enabled: true,
            matcher,
            effects,
        }], &[]);
        assert!(!lua.contains("workspace"), "{lua}");
    }

    fn client(class: &str, title: &str) -> Client {
        Client {
            class: class.to_string(),
            title: title.to_string(),
            mapped: true,
            ..Default::default()
        }
    }

    /// Opens a draft and shows the picker with two windows, one of which
    /// shares nothing with the other so a mixed-up index is visible.
    fn with_picker(f: impl FnOnce(&mut WindowRulesModule)) {
        with_isolated_module(|m| {
            m.draft = Some(RuleDraft::default());
            m.picker = Some(PickerState::Loaded(vec![
                client("com.mitchellh.ghostty", "hyprforge"),
                client("dev.zed.Zed", "hyprland.lua"),
            ]));
            f(m);
        });
    }

    #[test]
    fn picking_a_window_fills_the_class_and_leaves_the_title_alone() {
        with_picker(|m| {
            let _ = m.update(Message::PickClass(1));
            let draft = m.draft.as_ref().unwrap();
            assert_eq!(draft.class, "dev.zed.Zed");
            assert_eq!(draft.title, "", "the title is a separate, deliberate choice");
        });
    }

    #[test]
    fn adding_the_title_is_a_second_explicit_action() {
        with_picker(|m| {
            let _ = m.update(Message::PickClass(0));
            let _ = m.update(Message::PickTitle(0));
            let draft = m.draft.as_ref().unwrap();
            assert_eq!(draft.class, "com.mitchellh.ghostty");
            assert_eq!(draft.title, "hyprforge");
        });
    }

    /// An index only means anything against the list it was rendered from.
    /// Out of range must be dropped, not panic — indexing a Vec here would
    /// take the whole app down.
    #[test]
    fn a_stale_index_is_ignored_rather_than_panicking() {
        with_picker(|m| {
            let _ = m.update(Message::PickClass(99));
            assert_eq!(m.draft.as_ref().unwrap().class, "");
        });
    }

    #[test]
    fn picking_while_the_picker_is_closed_does_nothing() {
        with_isolated_module(|m| {
            m.draft = Some(RuleDraft::default());
            m.picker = None;
            let _ = m.update(Message::PickClass(0));
            assert_eq!(m.draft.as_ref().unwrap().class, "");
        });
    }

    /// Closing the picker while the hyprctl call is still running must not
    /// have the result reopen it underneath the user.
    #[test]
    fn a_response_arriving_after_close_is_discarded() {
        with_picker(|m| {
            let _ = m.update(Message::ClosePicker);
            let _ = m.update(Message::ClientsLoaded(Ok(vec![client("late", "arrival")])));
            assert!(m.picker.is_none(), "a closed picker must stay closed");
        });
    }

    /// No Hyprland is not an error dialog — the fields still work by hand.
    #[test]
    fn an_unavailable_compositor_leaves_the_form_usable() {
        with_isolated_module(|m| {
            m.draft = Some(RuleDraft::default());
            let _ = m.update(Message::OpenPicker);
            let _ = m.update(Message::ClientsLoaded(Err("could not run hyprctl".into())));
            assert!(matches!(m.picker, Some(PickerState::Unavailable(_))));
            assert!(m.error.is_none(), "this must not surface as a module-level error");
        });
    }

    fn monitor(name: &str, desc: &str) -> Monitor {
        Monitor { name: name.to_string(), description: desc.to_string() }
    }

    /// A pin must store the EDID description, not the connector — that's the
    /// whole reason it survives a replug.
    #[test]
    fn choosing_a_monitor_stores_its_description_not_its_connector() {
        with_isolated_module(|m| {
            m.monitors = vec![monitor("DP-3", "GWD ARZOPA")];
            let _ = m.update(Message::AddWorkspacePin);
            let choice = MonitorChoice::from_monitor(&m.monitors[0]);
            let _ = m.update(Message::PinMonitor(0, choice));
            assert_eq!(m.workspace_rules[0].monitor, "desc:GWD ARZOPA");
        });
    }

    /// A monitor that reports no description can only be named by connector;
    /// a bare `desc:` would match nothing at all.
    #[test]
    fn a_monitor_without_a_description_is_pinned_by_connector() {
        let choice = MonitorChoice::from_monitor(&monitor("HDMI-A-1", ""));
        assert_eq!(choice.selector, "HDMI-A-1");
    }

    /// Unplugging the monitor a pin points at must not make the pin look
    /// blank — editing anything else on the row would then quietly clear it.
    #[test]
    fn a_pin_for_a_disconnected_monitor_still_shows_what_it_points_at() {
        let stored = MonitorChoice::from_stored("desc:GWD ARZOPA");
        assert_eq!(stored.selector, "desc:GWD ARZOPA");
        assert!(stored.to_string().contains("not connected"));
    }

    #[test]
    fn pins_can_be_added_and_removed() {
        with_isolated_module(|m| {
            let _ = m.update(Message::AddWorkspacePin);
            let _ = m.update(Message::AddWorkspacePin);
            assert_eq!(m.workspace_rules.len(), 2);
            let _ = m.update(Message::PinWorkspace(1, "name:gaming".into()));

            // Removing takes two presses, same as a rule.
            let _ = m.update(Message::DeleteWorkspacePin(0));
            assert_eq!(m.workspace_rules.len(), 2, "one press only arms the row");
            let _ = m.update(Message::DeleteWorkspacePinConfirm(0));
            assert_eq!(m.workspace_rules.len(), 1);
            assert_eq!(m.workspace_rules[0].workspace, "name:gaming");
        });
    }

    /// Deleting past the end has to be inert rather than panicking — the
    /// same stale-index hazard the picker has.
    #[test]
    fn editing_a_pin_that_is_gone_does_nothing() {
        with_isolated_module(|m| {
            let _ = m.update(Message::DeleteWorkspacePinConfirm(3));
            let _ = m.update(Message::PinWorkspace(3, "x".into()));
            assert!(m.workspace_rules.is_empty());
        });
    }

    /// The same ordering defect that cost a real user their binds in the
    /// Shortcuts module: nothing may be deleted from the user's config
    /// unless the store was actually written first.
    #[test]
    fn a_failed_save_leaves_the_users_config_untouched() {
        with_isolated_module(|m| {
            let dir = tempfile::tempdir().unwrap();
            let config = dir.path().join("hyprland.lua");
            let source = "hl.window_rule({ match = { class = [[discord]] }, float = true })\n";
            std::fs::write(&config, source).unwrap();

            m.store_unreadable = Some("simulated unreadable store".to_string());
            m.import_review = Some(ImportState::Ready(ImportReview {
                rules: vec![ImportCandidateRule {
                    matcher: Matcher { class: Some("discord".into()), ..Default::default() },
                    effects: Effects::default(),
                    enabled: true,
                    checked: true,
                    already_imported: false,
                    dropped: Vec::new(),
                    source_path: config.clone(),
                    line: Some(1),
                }],
                workspace_rules: Vec::new(),
                failures: Vec::new(),
            }));
            let _ = m.update(Message::ImportConfirm);

            assert_eq!(
                std::fs::read_to_string(&config).unwrap(),
                source,
                "nothing may be removed when the store wasn't written"
            );
            assert!(m.error.is_some(), "and the failure has to be visible");
        });
    }

    /// An unreadable store is not an empty one.
    #[test]
    fn an_unreadable_store_is_never_overwritten() {
        with_isolated_module(|m| {
            let path = hyprforge_core::paths::window_rules_toml_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let precious = "this file did not parse but must survive\n";
            std::fs::write(&path, precious).unwrap();

            m.store_unreadable = Some("expected an equals".to_string());
            m.rules.push(fully_populated_rule());

            assert!(m.persist().is_err());
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                precious,
                "the unparseable file must be left exactly as it was"
            );
        });
    }

    /// A rule is a page of fields to reconstruct from memory, so an
    /// accidental click must not be able to destroy one.
    #[test]
    fn deleting_a_rule_takes_two_presses() {
        with_isolated_module(|m| {
            m.rules = vec![fully_populated_rule()];

            let _ = m.update(Message::Delete(0));
            assert_eq!(m.rules.len(), 1, "one press must not delete anything");
            assert_eq!(m.pending_delete, Some(PendingDelete::Rule(0)));

            let _ = m.update(Message::DeleteCancel);
            assert_eq!(m.rules.len(), 1);
            assert_eq!(m.pending_delete, None, "cancelling disarms the row");

            let _ = m.update(Message::Delete(0));
            let _ = m.update(Message::DeleteConfirm(0));
            assert!(m.rules.is_empty());
            assert_eq!(m.pending_delete, None);
        });
    }

    /// Arming one list must not leave a stale armed row in the other.
    #[test]
    fn arming_a_pin_disarms_an_armed_rule() {
        with_isolated_module(|m| {
            m.rules = vec![fully_populated_rule()];
            let _ = m.update(Message::AddWorkspacePin);
            let _ = m.update(Message::Delete(0));
            let _ = m.update(Message::DeleteWorkspacePin(0));
            assert_eq!(m.pending_delete, Some(PendingDelete::WorkspacePin(0)));
        });
    }

    /// Adding a row before typing into it is the normal flow, so a blank pin
    /// must be storable and simply not emitted.
    #[test]
    fn a_half_filled_pin_is_kept_but_generates_nothing() {
        with_isolated_module(|m| {
            let _ = m.update(Message::AddWorkspacePin);
            let _ = m.update(Message::PinDefault(0, true));
            assert_eq!(m.workspace_rules.len(), 1);
            let lua = hyprforge_windowrules::codegen::generate(&[], &m.workspace_rules);
            assert!(!lua.contains("workspace_rule"), "{lua}");
        });
    }

    #[test]
    fn move_expressions_survive_verbatim_for_the_codegen_to_quote() {
        let draft = RuleDraft {
            move_x: "cursor_x-(window_w*0.5)".to_string(),
            move_y: "100".to_string(),
            ..Default::default()
        };
        let (_, effects) = draft.into_matcher_effects();
        assert_eq!(
            effects.r#move,
            Some(["cursor_x-(window_w*0.5)".to_string(), "100".to_string()])
        );
    }
}

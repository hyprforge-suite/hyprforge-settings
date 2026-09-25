use crate::modules::layout_canvas::{CanvasHead, LayoutCanvas};
use hyprforge_core::geometry::{logical_size, nearest_valid_scale};
use hyprforge_core::displayd_proxy::DisplaydProxy;
use std::time::Duration;
use hyprforge_core::lua_setup::{self, HyprConfig, Placement, SetupPlan};
use hyprforge_ui::theme::{spacing, FontScale, BASE_TEXT_SIZE};
use hyprforge_ui::widgets::{
    confirm_dialog, danger_button, divider, meta_text, primary_button, row_field, scaled_text,
    secondary_button, section,
};
use crate::module::SettingsModule;

/// Monitors goes first among the user's own `require()` calls, same
/// reasoning as window rules: the exact precedence of repeated
/// `hl.monitor()` calls for one output isn't documented, so the safer
/// default is the one that can't silently beat something the user wrote
/// themselves — theirs stays reachable either way `hl.monitor()` actually
/// resolves ties.
const MONITORS_PLACEMENT: Placement = Placement::BeforeUserRequires;

fn monitors_require_line() -> String {
    lua_setup::require_line("monitors")
}
use iced::widget::{checkbox, column, container, row, scrollable, text_input};
use iced::{Element, Length, Subscription, Task};
use serde::Deserialize;

/// Full geometry for one stored profile, fetched on demand for the
/// Monitors editor — `ListProfiles`' summary row doesn't carry per-head
/// detail. Field names/renames mirror `hyprforge_displayd::profile::Profile`
/// exactly, since it's what `GetProfile` serializes; kept as a local,
/// GUI-only type rather than a dependency on the (Wayland-heavy) daemon
/// crate.
#[derive(Debug, Clone, Deserialize)]
pub struct ProfileDetail {
    id: String,
    name: String,
    extra_output_policy: String,
    #[allow(dead_code)]
    head_swaps: Vec<(String, String)>,
    #[serde(rename = "head")]
    heads: Vec<HeadDetail>,
}

#[derive(Debug, Clone, Deserialize)]
struct HeadDetail {
    make: String,
    model: String,
    #[allow(dead_code)]
    serial: String,
    connector_hint: String,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    refresh_mhz: i32,
    scale: f64,
    #[allow(dead_code)]
    transform: String,
    enabled: bool,
}

/// Connector prefixes the kernel uses for internal panels. A laptop's own
/// screen is "Built-in display" to everyone except the DRM subsystem.
const BUILTIN_CONNECTOR_PREFIXES: [&str; 3] = ["edp", "lvds", "dsi"];

impl HeadDetail {
    /// A name a person would recognise on sight.
    ///
    /// `eDP-2` is a connector, not a monitor — it tells the user nothing
    /// about which physical screen they're editing. Every mainstream
    /// display panel (Windows, macOS, GNOME) leads with a friendly name and
    /// demotes the connector to metadata, so this does too. Built-in panels
    /// get a generic label because their EDID model is typically a part
    /// number (`0x0BC9`), which is no more meaningful than the connector.
    /// The size this head occupies in layout space, which is what the
    /// arrangement canvas has to draw.
    ///
    /// `width`/`height` are physical pixels, but `x`/`y` are logical, so
    /// drawing the pixel figures mixes two coordinate systems: a 2560x1600
    /// panel at scale 1.67 covers 1536x960 of layout, not 2560x1600. Sizing
    /// the rectangles in pixels made a heavily-scaled laptop panel look
    /// bigger than a physically larger external, and overlapped monitors
    /// that don't overlap.
    ///
    /// A quarter-turn transform swaps the two, as it does for the
    /// compositor.
    fn logical_size(&self) -> (i32, i32) {
        let w = logical_size(self.width, self.scale);
        let h = logical_size(self.height, self.scale);
        match self.transform.as_str() {
            "Rotate90" | "Rotate270" | "Flipped90" | "Flipped270" => (h, w),
            _ => (w, h),
        }
    }

    fn display_name(&self) -> String {
        let connector = self.connector_hint.to_ascii_lowercase();
        if BUILTIN_CONNECTOR_PREFIXES
            .iter()
            .any(|p| connector.starts_with(p))
        {
            return "Built-in display".to_string();
        }
        let make = self.make.trim();
        let model = self.model.trim();
        match (make.is_empty(), model.is_empty()) {
            // Nothing usable in the EDID — the connector is all we have.
            (_, true) => self.connector_hint.clone(),
            (true, false) => model.to_string(),
            (false, false) => format!("{make} {model}"),
        }
    }
}

/// What to write on a monitor's tile in the arrangement canvas.
///
/// Its name, plus the connector when that name is not unique. Two
/// identical monitors report identical EDIDs — a pair of AOC 2475Ws on
/// one desk both read "AOC 2475W" — so the canvas showed two tiles with
/// the same words on them, and getting them into the right order "took
/// some finagling". The connector (`DP-12`, `HDMI-A-1`) is the one
/// thing that differs, and it names the socket the cable goes into.
///
/// Only when ambiguous: a desk with a laptop screen and one monitor
/// gains nothing from `DP-4` on every tile, and the tiles are small.
fn canvas_label(head: &HeadDetail, all: &[HeadDetail]) -> String {
    let name = head.display_name();
    let shared = all
        .iter()
        .filter(|other| other.connector_hint != head.connector_hint)
        .any(|other| other.display_name() == name);
    match shared {
        true => format!("{name} ({})", head.connector_hint),
        false => name,
    }
}

/// One entry in a monitor picker. Carries the `connector_hint` as the
/// identity (that's what every message and D-Bus call is keyed on) while
/// showing the friendly name.
#[derive(Debug, Clone, PartialEq)]
struct HeadChoice {
    hint: String,
    label: String,
}

impl HeadChoice {
    fn new(h: &HeadDetail) -> Self {
        HeadChoice {
            hint: h.connector_hint.clone(),
            label: h.display_name(),
        }
    }
}

impl std::fmt::Display for HeadChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The connector still earns its place here: it's the only thing
        // that disambiguates two identical monitors in a picker.
        if self.label == self.hint {
            write!(f, "{}", self.label)
        } else {
            write!(f, "{} ({})", self.label, self.hint)
        }
    }
}

/// The scale steps offered in the dropdown, matching what Windows exposes.
///
/// These are only the *targets*: each is moved to the nearest scale the
/// head can actually be set to before it's offered, because most of them
/// don't divide a given resolution cleanly and the compositor would
/// silently substitute one that does (see
/// [`hyprforge_core::geometry::nearest_valid_scale`]).
const SCALE_PRESETS: [f64; 6] = [1.0, 1.25, 1.5, 1.75, 2.0, 2.25];

/// A scale the selected head can genuinely be set to. `percent` is the
/// exact value; the label rounds it for display only — the two differ,
/// since an achievable scale is rarely a whole percentage.
#[derive(Debug, Clone, Copy)]
struct ScaleChoice {
    percent: f64,
}

impl PartialEq for ScaleChoice {
    fn eq(&self, other: &Self) -> bool {
        // Whole-percent tolerance: the pick_list only needs to recognise
        // which entry is selected, and the stored value round-trips through
        // a formatted string.
        (self.percent - other.percent).abs() < 0.5
    }
}

impl Eq for ScaleChoice {}

/// The scale dropdown's contents for a `width`x`height` head, and which
/// entry is selected, given the draft field's percentage.
///
/// Every value offered is one the panel can genuinely take: the standard
/// steps are moved to the nearest achievable scale, and so is the head's
/// current one. Snapping the current value matters as much as snapping the
/// presets — a profile can hold a scale this panel can't take (hand-edited,
/// learned from different hardware, or written by a Hyprforge that had the
/// rule wrong), and offering it unchanged would let someone pick a value the
/// compositor is guaranteed to replace.
///
/// An achievable off-preset scale is still offered, so an existing 160%
/// profile stays representable rather than being dropped to the nearest
/// preset and silently rescaling a working setup.
fn scale_choices(width: i32, height: i32, field_scale: &str) -> (Vec<ScaleChoice>, Option<f64>) {
    let current = field_scale
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|pct| *pct > 0.0)
        .map(|pct| nearest_valid_scale(width, height, pct / 100.0) * 100.0);

    let mut options: Vec<ScaleChoice> = SCALE_PRESETS
        .iter()
        .map(|nominal| ScaleChoice {
            percent: nearest_valid_scale(width, height, *nominal) * 100.0,
        })
        .collect();
    if let Some(current) = current {
        if !options.iter().any(|c| (c.percent - current).abs() < 0.5) {
            options.push(ScaleChoice { percent: current });
        }
    }
    options.sort_by(|a, b| a.percent.total_cmp(&b.percent));
    // Several presets can snap onto the same achievable scale — on a
    // 2560x1600 panel both 150% and 160% land on 1.6 — and the list must not
    // show it twice.
    options.dedup_by(|a, b| (a.percent - b.percent).abs() < 0.5);
    (options, current)
}

impl std::fmt::Display for ScaleChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let rounded = self.percent.round() as i64;
        if rounded == 100 {
            write!(f, "100% (recommended)")
        } else {
            write!(f, "{rounded}%")
        }
    }
}

/// One entry in the resolution/refresh-rate dropdown — populated from the
/// head's *live* supported modes (see `available_modes` on the daemon),
/// since a stored profile only ever remembers the one mode it was saved
/// with, not the full list a physical head supports.
#[derive(Debug, Clone, PartialEq)]
pub struct ModeOption {
    width: i32,
    height: i32,
    refresh_mhz: i32,
    preferred: bool,
}

impl std::fmt::Display for ModeOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} x {} @ {:.0}Hz{}",
            self.width,
            self.height,
            self.refresh_mhz as f64 / 1000.0,
            if self.preferred { " (recommended)" } else { "" }
        )
    }
}

/// A distinct resolution, independent of refresh rate.
///
/// Windows and macOS both split these into two controls: you pick a
/// resolution, then a refresh rate valid *for* it. One combined
/// "2560 x 1600 @ 165Hz" list multiplies out to every mode a monitor
/// supports, which on a modern panel is dozens of near-identical rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolutionOption {
    width: i32,
    height: i32,
    preferred: bool,
}

impl std::fmt::Display for ResolutionOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} × {}{}",
            self.width,
            self.height,
            if self.preferred { " (recommended)" } else { "" }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefreshOption {
    mhz: i32,
}

impl std::fmt::Display for RefreshOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hz = self.mhz as f64 / 1000.0;
        // Rates like 59.94 are real and must not be rounded away, but the
        // common integer case shouldn't read "165.000 Hz".
        if (hz - hz.round()).abs() < 0.01 {
            write!(f, "{:.0} Hz", hz)
        } else {
            write!(f, "{:.2} Hz", hz)
        }
    }
}

/// Orientation options shown in the property panel, in the same order as
/// `Transform`'s wire representation (`GetProfile`'s JSON / `Transform`'s
/// default serde variant names) — index-paired with `TRANSFORM_RAW` so a
/// label picked from the dropdown maps straight back to the string
/// `SetHeadGeometry` expects.
const TRANSFORM_LABELS: [&str; 8] = [
    "Landscape",
    "Portrait (90°)",
    "Landscape (180°)",
    "Portrait (270°)",
    "Landscape (mirrored)",
    "Portrait (90°, mirrored)",
    "Landscape (180°, mirrored)",
    "Portrait (270°, mirrored)",
];
const TRANSFORM_RAW: [&str; 8] = [
    "Normal",
    "Rotate90",
    "Rotate180",
    "Rotate270",
    "Flipped",
    "Flipped90",
    "Flipped180",
    "Flipped270",
];

fn transform_to_label(raw: &str) -> &'static str {
    TRANSFORM_RAW
        .iter()
        .position(|r| *r == raw)
        .map(|i| TRANSFORM_LABELS[i])
        .unwrap_or(TRANSFORM_LABELS[0])
}

fn label_to_transform(label: &str) -> &'static str {
    TRANSFORM_LABELS
        .iter()
        .position(|l| *l == label)
        .map(|i| TRANSFORM_RAW[i])
        .unwrap_or(TRANSFORM_RAW[0])
}

/// The property-panel draft for whichever head is selected — text fields
/// so partial/invalid input (e.g. a bare "-" while typing a negative X)
/// never gets silently reformatted out from under the user, mirroring the
/// same pattern `RuleDraft` uses in the Window Rules module.
fn fields_from_head(h: &HeadDetail) -> (String, String, String, String, String, String, String) {
    (
        h.x.to_string(),
        h.y.to_string(),
        h.width.to_string(),
        h.height.to_string(),
        format!("{:.0}", h.refresh_mhz as f64 / 1000.0),
        // Full precision, not a whole percent: an achievable scale is rarely
        // round (5/3 is 166.796875%), and seeding "167" here meant editing
        // any *other* field committed 1.67 back onto the head — a scale the
        // panel can't take. The dropdown shows this rounded regardless.
        format!("{:.4}", h.scale * 100.0),
        transform_to_label(&h.transform).to_string(),
    )
}

/// Writes the property panel's draft fields straight into the selected
/// head as soon as they parse cleanly, mirroring how dragging on the
/// canvas already updates the head live — so there's no separate "Apply to
/// head" step to remember. Silently no-ops while a field is mid-edit and
/// not yet a valid number (e.g. a bare "-"); the last valid value stays in
/// effect until Save & Apply persists it.
fn commit_selected_head(editor: &mut LayoutEditor) {
    let Some(selected) = editor.selected.clone() else {
        return;
    };
    let parsed = (
        editor.field_x.trim().parse::<i32>(),
        editor.field_y.trim().parse::<i32>(),
        editor.field_width.trim().parse::<i32>(),
        editor.field_height.trim().parse::<i32>(),
        editor.field_refresh.trim().parse::<f64>(),
        editor.field_scale.trim().parse::<f64>(),
    );
    if let (Ok(x), Ok(y), Ok(width), Ok(height), Ok(refresh_hz), Ok(scale_pct)) = parsed {
        if width > 0 && height > 0 && refresh_hz > 0.0 && scale_pct > 0.0 {
            if let Some(head) = editor
                .profile
                .heads
                .iter_mut()
                .find(|h| h.connector_hint == selected)
            {
                head.x = x;
                head.y = y;
                head.width = width;
                head.height = height;
                head.refresh_mhz = (refresh_hz * 1000.0).round() as i32;
                // Snapped at the one point a scale is written, so the head
                // always holds a value the panel can genuinely take. The
                // draft field is text and an achievable scale is rarely
                // round — 175% on a 2560x1600 panel is really 5/3 — so
                // round-tripping through it drifts, and drift is exactly
                // what the compositor answers by silently substituting a
                // scale of its own.
                head.scale = nearest_valid_scale(width, height, scale_pct / 100.0);
                head.transform = label_to_transform(&editor.field_transform).to_string();
            }
        }
    }
}

struct LayoutEditor {
    profile: ProfileDetail,
    selected: Option<String>,
    /// Live modes for `selected`'s connector, if it's currently connected
    /// — empty otherwise, in which case the view falls back to free-text
    /// width/height/refresh fields.
    available_modes: Vec<ModeOption>,
    field_x: String,
    field_y: String,
    field_width: String,
    field_height: String,
    field_refresh: String,
    field_scale: String,
    field_transform: String,
    swap_a: Option<String>,
    swap_b: Option<String>,
    status: Option<String>,
    error: Option<String>,
    /// Edits made here that the compositor has not been told about.
    ///
    /// The page used to apply every edit as it happened, debounced by
    /// 700ms. That reads well in a design document — GNOME's HIG asks
    /// instant-apply pages to have no dismissal button, and the
    /// daemon's revert countdown is a real safety net — and it is
    /// wrong in use: arranging three monitors means dragging one,
    /// looking at it, dragging it again, and the screen rearranging
    /// itself under you between each of those is disorienting enough
    /// that the user asked for it to stop. Nothing is applied now until
    /// this is true and Save & Apply is pressed.
    unapplied: bool,
}

impl LayoutEditor {
    fn new(profile: ProfileDetail) -> Self {
        let selected = profile.heads.first().map(|h| h.connector_hint.clone());
        let (field_x, field_y, field_width, field_height, field_refresh, field_scale, field_transform) =
            profile.heads.first().map(fields_from_head).unwrap_or_default();
        LayoutEditor {
            profile,
            selected,
            available_modes: Vec::new(),
            field_x,
            field_y,
            field_width,
            field_height,
            field_refresh,
            field_scale,
            field_transform,
            swap_a: None,
            swap_b: None,
            unapplied: false,
            status: None,
            error: None,
        }
    }

    fn selected_head(&self) -> Option<&HeadDetail> {
        let hint = self.selected.as_ref()?;
        self.profile.heads.iter().find(|h| &h.connector_hint == hint)
    }
}

#[derive(Debug, Clone)]
pub struct ProfileInfo {
    pub id: String,
    pub name: String,
    pub head_count: u32,
    pub last_used: String,
}

#[derive(Debug, Clone)]
pub struct LoadedState {
    profiles: Vec<ProfileInfo>,
    current_fingerprint: String,
    competing_monitor_rules: Vec<String>,
    /// The daemon's own match, empty when nothing matches. Asked for
    /// directly because a superset/subset match can't be derived from the
    /// fingerprint — only an exact match shares its id with one.
    current_profile: String,
}

#[derive(Debug, Clone)]
pub enum SignalKind {
    ProfileApplied { id: String, name: String, tier: String },
    NewTopologySeen { summary: String },
    RevertPending { seconds: u32 },
    RevertResolved { reverted: bool },
}

#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    Loaded(Result<LoadedState, String>),
    Apply(String),
    Applied(String, Result<(), String>),
    RenameStart(String, String),
    RenameInput(String),
    RenameSubmit,
    RenameCancelled,
    Renamed(Result<(), String>),
    ToggleAdvanced,
    DeleteStart(String),
    DeleteConfirm,
    DeleteCancel,
    Deleted(Result<(), String>),
    KeepLayout,
    RevertLayoutNow,
    RevertActionDone(Result<(), String>),
    /// One tick of the revert countdown.
    RevertTick,
    SignalReceived(SignalKind),
    ToggleOtherProfiles,
    EditLayout(String),
    LayoutLoaded(Result<ProfileDetail, String>),
    SelectHead(String),
    ModesLoaded(String, Vec<(i32, i32, i32, bool)>),
    HeadDragMoved(String, i32, i32),
    FieldX(String),
    FieldY(String),
    FieldWidth(String),
    FieldHeight(String),
    FieldRefresh(String),
    FieldScale(String),
    FieldTransform(String),
    SwapSelectA(String),
    SwapSelectB(String),
    ToggleSwap,
    SwapToggled(Result<ProfileDetail, String>),
    SetPolicy(String),
    PolicySet(Result<ProfileDetail, String>),
    /// Save & Apply: hand the edited layout to the daemon. The only
    /// thing that changes what is on screen — see
    /// [`LayoutEditor::unapplied`].
    ApplyEditor,
    /// Throw the edits away and reload what is on disk.
    DiscardEdits,
    LayoutSaved(Result<(), String>),
    ResolutionSelected(ResolutionOption),
    RefreshSelected(RefreshOption),
    ToggleWarnings,
    ImportFromConfig,
    ImportEvaluated((hyprforge_lua_import::ImportResult, Vec<LiveHead>)),
    /// Tick an entry that resolves to a connected display.
    ImportToggle(usize, bool),
    /// Write the checked entries' geometry into the current profile.
    ImportApply,
    ImportApplied(Result<(), String>),
    ImportClose,
}

/// A profile delete the user has asked for but not yet confirmed.
struct PendingDelete {
    id: String,
    body: String,
    detail: String,
}

pub struct DisplaysModule {
    connected: bool,
    error: Option<String>,
    profiles: Vec<ProfileInfo>,
    current_fingerprint: String,
    /// The profile the daemon actually matched/applied, tracked from the
    /// `ProfileApplied` signal (accurate for superset/subset matches too)
    /// and, as a fallback for the very first load, by comparing
    /// `current_fingerprint` against exact-match profile ids.
    current_profile_id: Option<String>,
    competing_monitor_rules: Vec<String>,
    renaming: Option<(String, String)>,
    /// A delete awaiting confirmation. Holds pre-rendered strings because
    /// `confirm_dialog` borrows them for the dialog's lifetime, and the
    /// wording differs for the connected topology (which gets re-learned).
    deleting: Option<PendingDelete>,
    /// Seconds left before the daemon rolls back a provisional display
    /// change. `None` means nothing is pending. The daemon owns the real
    /// deadline — this is only what the banner counts down, so a stalled or
    /// killed GUI can't keep a bad layout alive.
    revert_seconds_left: Option<u32>,
    /// Whether the rarely-used per-head controls (numeric position, output
    /// policy, head swaps) are expanded. Collapsed on open.
    show_advanced: bool,
    /// Whether the competing-`hl.monitor()` warning is expanded. It's a
    /// standing condition, not news, so it sits collapsed at the bottom
    /// rather than shouting above the controls every visit.
    show_warnings: bool,
    last_event: Option<String>,
    /// Managing every stored profile (not just the one loaded in the
    /// editor) is the power-user case — collapsed by default.
    show_other_profiles: bool,
    /// The profile currently shown in the canvas/property panel. Starts
    /// out following `current_profile_id` automatically; once the user
    /// explicitly picks a different profile to edit (via `EditLayout`),
    /// it stops following so an unrelated apply/auto-learn elsewhere
    /// can't yank their in-progress edit out from under them.
    editor: Option<LayoutEditor>,
    /// Which Hyprland config the user has, for the `monitors.lua` fallback
    /// setup banner. Unlike Window Rules/Shortcuts, nothing here blocks on
    /// this — Displays works fully without it, since the daemon applies
    /// layouts live regardless. This only controls whether the daemon's
    /// generated `monitors.lua` is actually sourced by Hyprland, i.e.
    /// whether a layout survives the daemon simply not running.
    config: HyprConfig,
    /// Only meaningful when `config` is [`HyprConfig::Lua`].
    setup_plan: SetupPlan,
    /// Separate from `error` deliberately: `error` reflects live D-Bus/
    /// daemon connectivity and gets reset on every successful `Loaded`,
    /// which would otherwise wipe out a setup failure noticed once at
    /// startup before the user ever saw it.
    setup_error: Option<String>,
    /// `None` when no import is in progress or under review.
    import_review: Option<ImportState>,
}

/// A hand-written `hl.monitor({...})` call, summarised for display.
///
/// Deliberately a local, GUI-owned type rather than a dependency on
/// `hyprforge-displayd` (which this module otherwise avoids entirely,
/// talking to the daemon only over D-Bus — the same reason
/// [`ProfileDetail`] mirrors that crate's `Profile` fields locally rather
/// than importing them). This is informational only, not a candidate to
/// add anywhere: see [`Message::ImportEvaluated`]'s handler for why a
/// hand-written monitor rule can't safely become a stored `Profile`.
struct ImportedMonitor {
    selector: String,
    disabled: bool,
    mode: Option<String>,
    position: Option<String>,
    scale: Option<String>,
    /// The live connector this entry's `output` selector resolves to, if
    /// that display is plugged in right now.
    ///
    /// This is the whole difference between an entry that can be imported
    /// and one that can only be described. A stored profile is keyed on the
    /// make/model/serial of really-observed hardware, and a `desc:` string
    /// encodes no serial — so a rule for a display that isn't connected
    /// cannot become a profile without inventing an identity. A rule for one
    /// that *is* connected needs no invention at all: the daemon already
    /// learned a profile for it, and the geometry can simply be written into
    /// that profile's head.
    target: Option<String>,
    /// Only meaningful when `target` is set.
    checked: bool,
}

/// One currently-connected output, from `GetCurrentLayout`.
///
/// Only the two fields needed to resolve an `output` selector. Mirrors
/// displayd's `Head` locally rather than depending on that crate, the same
/// as [`ProfileDetail`].
#[derive(Debug, Clone, Deserialize)]
pub struct LiveHead {
    connector: String,
    description: String,
}

impl ImportedMonitor {
    /// Which live connector this entry names, if any.
    ///
    /// Hyprland accepts either a bare connector (`DP-3`) or a
    /// `desc:<description>` selector; both are resolved here against what
    /// the compositor actually reports, so a rule naming a display that
    /// isn't plugged in resolves to nothing rather than to a guess.
    fn resolve(&self, live: &[LiveHead]) -> Option<String> {
        let selector = self.selector.trim();
        match selector.strip_prefix("desc:") {
            Some(desc) => live
                .iter()
                .find(|h| h.description.trim() == desc.trim())
                .map(|h| h.connector.clone()),
            None => live
                .iter()
                .find(|h| h.connector == selector)
                .map(|h| h.connector.clone()),
        }
    }

    /// The geometry this entry would write, as `SetHeadGeometry` wants it.
    ///
    /// `None` for anything that doesn't carry a full mode and position —
    /// those are the two the RPC can't default, and a partial import that
    /// silently filled in zeros would move a display somewhere the user
    /// never asked for.
    fn geometry(&self) -> Option<ImportGeometry> {
        let (width, height, refresh_mhz) = parse_mode(self.mode.as_deref()?)?;
        let (x, y) = parse_position(self.position.as_deref()?)?;
        Some(ImportGeometry {
            x,
            y,
            width,
            height,
            refresh_mhz,
            // Hyprland's own default when a monitor rule omits it.
            scale: self.scale.as_deref().and_then(|s| s.trim().parse().ok()).unwrap_or(1.0),
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct ImportGeometry {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    refresh_mhz: i32,
    scale: f64,
}

/// `"2560x1600@165.00"` → `(2560, 1600, 165000)`. `preferred` and anything
/// else unparseable yields `None` — the caller then treats the entry as
/// describable but not importable rather than inventing a mode.
fn parse_mode(s: &str) -> Option<(i32, i32, i32)> {
    let (dims, refresh) = s.trim().split_once('@')?;
    let (w, h) = dims.split_once('x')?;
    let hz: f64 = refresh.trim().parse().ok()?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?, (hz * 1000.0).round() as i32))
}

/// `"1920x0"` → `(1920, 0)`. `auto` and anything else yields `None`.
fn parse_position(s: &str) -> Option<(i32, i32)> {
    let (x, y) = s.trim().split_once('x')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// Where an "Import from config" run has got to. A two-state enum rather
/// than a nested `Option`, matching the other two modules.
enum ImportState {
    Running,
    Ready(ImportSummary),
}

struct ImportSummary {
    monitors: Vec<ImportedMonitor>,
    failures: Vec<(std::path::PathBuf, String)>,
}

impl ImportSummary {
    /// The entries that can actually be applied: connected, and carrying
    /// enough geometry to write.
    fn importable(&self) -> impl Iterator<Item = (usize, &ImportedMonitor)> {
        self.monitors
            .iter()
            .enumerate()
            .filter(|(_, m)| m.target.is_some() && m.geometry().is_some())
    }
}

impl DisplaysModule {
    pub fn new() -> (Self, Task<Message>) {
        // Through the shared bootstrap, like every other module that
        // owns a require line. This was a hand-copy of it, and the copy
        // had drifted in the one way that matters: it wrote the
        // placeholder with `let _ =`, discarding exactly the error
        // `bootstrap` reports on purpose. `ModuleSetup::generated`'s own
        // doc says writing that file is "not optional" — a require line
        // pointing at a missing file errors on the user's next `hyprctl
        // reload`, and swallowing the failure produced that error with
        // no message anywhere.
        let setup = lua_setup::bootstrap(
            &hyprforge_core::paths::hypr_config_dir(),
            &hyprforge_core::paths::hyprland_lua_path(),
            lua_setup::ModuleSetup {
                require_line: &monitors_require_line(),
                placement: MONITORS_PLACEMENT,
                generated: (
                    hyprforge_core::paths::monitors_lua_path(),
                    // Empty but present: the daemon writes the real
                    // content once it has settled a topology.
                    "-- Generated by Hyprforge. Do not edit by hand — changes will be\n\
                     -- overwritten the next time the display daemon settles a layout.\n"
                        .to_string(),
                ),
            },
        );
        let (config, setup_plan, error) = (setup.config, setup.plan, setup.error);
        (
            DisplaysModule {
                connected: false,
                error: None,
                setup_error: error,
                profiles: Vec::new(),
                current_fingerprint: String::new(),
                current_profile_id: None,
                competing_monitor_rules: Vec::new(),
                renaming: None,
                deleting: None,
                revert_seconds_left: None,
                show_advanced: false,
                show_warnings: false,
                last_event: None,
                show_other_profiles: false,
                editor: None,
                config,
                setup_plan,
                import_review: None,
            },
            Task::perform(load(), Message::Loaded),
        )
    }

    /// Seconds left before the daemon rolls back a provisional change, or
    /// `None` when nothing is pending. The app shell reads this to decide
    /// whether the pinned countdown window should exist.
    pub fn revert_seconds_left(&self) -> Option<u32> {
        self.revert_seconds_left
    }

    /// Applies one edit to the selected head, then records that the
    /// layout on screen is no longer what the compositor is running.
    ///
    /// Six text fields and two dropdowns each did these three steps in
    /// their own arm: write the field, `commit_selected_head`, mark.
    /// Either of the last two is silently omissible, and omitting the
    /// mark means an edit that never offers Save & Apply.
    fn edit(&mut self, change: impl FnOnce(&mut LayoutEditor)) -> Task<Message> {
        if let Some(editor) = &mut self.editor {
            change(editor);
            commit_selected_head(editor);
        }
        self.mark_unapplied()
    }

    /// Records that the layout on screen is no longer what the
    /// compositor is running.
    ///
    /// This used to schedule an apply 700ms after edits stopped — see
    /// [`LayoutEditor::unapplied`] for why it does not any more. It
    /// returns a `Task` so the call sites read unchanged; there is
    /// nothing to do but remember.
    fn mark_unapplied(&mut self) -> Task<Message> {
        if let Some(editor) = &mut self.editor {
            editor.unapplied = true;
            // A stale "applied" line under an edited layout says the
            // opposite of what is true.
            editor.status = None;
        }
        Task::none()
    }

    /// Hands the editor's layout to the daemon — Save & Apply, and the
    /// only thing that changes what is on screen.
    fn apply_editor(&mut self) -> Task<Message> {
        let Some(editor) = &self.editor else {
            return Task::none();
        };
        let heads: Vec<HeadGeometry> = editor
            .profile
            .heads
            .iter()
            .map(|h| {
                (
                    h.connector_hint.clone(),
                    h.x,
                    h.y,
                    h.width,
                    h.height,
                    h.refresh_mhz,
                    h.scale,
                    h.transform.clone(),
                )
            })
            .collect();
        let is_current = self.current_profile_id.as_ref() == Some(&editor.profile.id);
        Task::perform(
            save_geometry(editor.profile.id.clone(), heads, is_current),
            Message::LayoutSaved,
        )
    }

    /// Loads `current_profile_id` into the editor, but only if nothing is
    /// loaded there yet — see the `editor` field doc for why this doesn't
    /// run unconditionally on every update.
    fn maybe_autoload_editor(&self) -> Task<Message> {
        if self.editor.is_none() {
            if let Some(id) = &self.current_profile_id {
                return Task::perform(load_profile(id.clone()), Message::LayoutLoaded);
            }
        }
        Task::none()
    }

    fn profile_row(&self, p: &ProfileInfo, scale: FontScale, is_current: bool) -> Element<'_, Message> {
        let name = if is_current {
            format!("{} (current)", p.name)
        } else {
            p.name.clone()
        };
        let info = column![
            scaled_text(name, BASE_TEXT_SIZE, scale),
            meta_text(
                format!("{} head(s) · last used {}", p.head_count, p.last_used),
                12.0,
                scale,
            ),
        ]
        .spacing(spacing::XS)
        .width(Length::Fill);

        container(
            row![
                info,
                secondary_button("Edit").on_press(Message::EditLayout(p.id.clone())),
                secondary_button("Rename")
                    .on_press(Message::RenameStart(p.id.clone(), p.name.clone())),
                danger_button("Delete", Message::DeleteStart(p.id.clone())),
                primary_button("Apply").on_press(Message::Apply(p.id.clone())),
            ]
            .spacing(spacing::SM)
            .align_y(iced::Alignment::Center),
        )
        .padding([spacing::SM, 0.0])
        .into()
    }

    /// The "Import from config" flow's loading state and summary.
    ///
    /// Unlike Window Rules/Shortcuts' import, this is read-only — there's
    /// nothing to check and add. A hand-written `hl.monitor()` rule only
    /// ever names an `output` selector, never the make/model/serial
    /// triple a stored `Profile` is keyed on, so turning one into a real
    /// profile would mean guessing an identity — a wrong guess is safe
    /// (it just never matches anything) but still not something to do
    /// silently. The common case doesn't need this anyway: displayd
    /// already learns a profile automatically the moment it sees a
    /// display it doesn't recognise.
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
                    "{} file{} couldn't be evaluated:",
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
            body = body.push(section("Couldn't evaluate", scale, failures.spacing(spacing::XS)));
        }

        if review.monitors.is_empty() {
            body = body.push(meta_text(
                "No hand-written hl.monitor() rules found outside Hyprforge's own \
                 generated files.",
                BASE_TEXT_SIZE,
                scale,
            ));
        } else {
            let mut list = column![].spacing(spacing::SM);
            for (i, m) in review.monitors.iter().enumerate() {
                let summary = if m.disabled {
                    format!("{} — disabled", m.selector)
                } else {
                    format!(
                        "{}  mode {}  position {}  scale {}",
                        m.selector,
                        m.mode.as_deref().unwrap_or("preferred"),
                        m.position.as_deref().unwrap_or("auto"),
                        m.scale.as_deref().unwrap_or("auto"),
                    )
                };
                let applicable = m.target.is_some() && m.geometry().is_some();
                let mut info = column![scaled_text(summary, 13.0, scale)]
                    .spacing(spacing::XS)
                    .width(Length::Fill);
                // Say which of the three states this entry is in, because
                // "you can't import this" and "you can" look identical
                // otherwise.
                info = info.push(match (&m.target, applicable) {
                    (Some(connector), true) => meta_text(
                        format!("Connected as {connector} — can be imported"),
                        12.0,
                        scale,
                    ),
                    (Some(connector), false) => meta_text(
                        format!(
                            "Connected as {connector}, but this rule leaves the mode or \
                             position up to Hyprland, so there are no numbers to import."
                        ),
                        12.0,
                        scale,
                    ),
                    (None, _) => meta_text(
                        "That display isn't connected right now, so there's nothing to \
                         import it into.",
                        12.0,
                        scale,
                    ),
                });

                let row_content: Element<'_, Message> = if applicable {
                    row![
                        checkbox(m.checked).on_toggle(move |v| Message::ImportToggle(i, v)),
                        info,
                    ]
                    .spacing(spacing::SM)
                    .align_y(iced::Alignment::Center)
                    .into()
                } else {
                    info.into()
                };
                list = list.push(row_content);
            }
            body = body.push(section("Found in your config", scale, list));

            let can_apply = review.importable().any(|(_, m)| m.checked);
            body = body.push(meta_text(
                "Checked entries write their position, size, refresh and scale into \
                 the profile for the displays you're using now. Transform is left \
                 alone — Hyprland numbers it its own way, and getting that wrong \
                 rotates a screen.\n\nAnything not connected can't be imported: a \
                 profile is keyed on the panel's make/model/serial, and an output \
                 selector doesn't carry that. Plug the display in once and Hyprforge \
                 learns a correct profile from it on its own.",
                12.0,
                scale,
            ));

            body = body.push(
                container(
                    row![
                        secondary_button("Close").on_press(Message::ImportClose),
                        {
                            let b = primary_button("Import checked");
                            if can_apply { b.on_press(Message::ImportApply) } else { b }
                        },
                    ]
                    .spacing(spacing::SM),
                )
                .width(Length::Fill)
                .align_x(iced::alignment::Horizontal::Right),
            );
            return container(body).padding(spacing::LG).into();
        }

        body = body.push(
            container(secondary_button("Close").on_press(Message::ImportClose)).width(Length::Fill),
        );

        container(body).padding(spacing::LG).into()
    }

    /// Unlike Window Rules/Shortcuts, this never blocks anything below it —
    /// Displays works fully without it, since the daemon applies layouts
    /// live regardless of whether Hyprland itself can also fall back to a
    /// static snapshot. `None` once the fallback is already wired up, so
    /// the banner doesn't linger after there's nothing left to do.
    /// `Lua`-with-`setup_plan`-not-`AlreadyPresent` and `Missing` normally
    /// never reach here — `new()` wires up the fallback (or creates a
    /// minimal `hyprland.lua`) automatically — so seeing either means that
    /// attempt failed; `setup_error` has why.
    fn setup_notice(&self, scale: FontScale) -> Option<Element<'_, Message>> {
        let body: Element<'_, Message> = match &self.config {
            HyprConfig::Lua(_) if self.setup_plan == SetupPlan::AlreadyPresent => return None,
            HyprConfig::Lua(_) | HyprConfig::Missing => {
                let reason = self.setup_error.as_deref().unwrap_or("unknown error");
                column![scaled_text(
                    format!(
                        "Hyprforge couldn't set up the display fallback automatically: {reason}"
                    ),
                    13.0,
                    scale,
                )]
                .spacing(spacing::SM)
                .into()
            }
            HyprConfig::ConfOnly(path) => column![
                scaled_text(
                    "You're using Hyprland's hyprland.conf format. The fallback \
                     monitors.lua Hyprforge can keep up to date needs a hyprland.lua to \
                     source it from, which only the Lua config supports — so this won't \
                     modify your .conf.",
                    13.0,
                    scale,
                ),
                meta_text(path.display().to_string(), 12.0, scale),
            ]
            .spacing(spacing::SM)
            .into(),
        };
        Some(section("Display fallback", scale, body))
    }
}

impl SettingsModule for DisplaysModule {
    type Message = Message;

    /// A layout edited on the canvas and not yet handed to the daemon.
    /// One change with no count, so the summary says what it is instead.
    fn pending(&self) -> Option<crate::module::Pending<Message>> {
        self.editor.as_ref().filter(|e| e.unapplied).map(|_| crate::module::Pending {
            summary: "Layout not applied".into(),
            preview: None,
            apply: Message::ApplyEditor,
            discard: Some(Message::DiscardEdits),
        })
    }

    fn subtitle(&self) -> Option<String> {
        (!self.connected).then(|| "hyprforge-displayd isn't running".into())
    }


    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Refresh => Task::perform(load(), Message::Loaded),
            Message::Loaded(Ok(state)) => {
                self.connected = true;
                self.error = None;
                self.profiles = state.profiles;
                self.current_fingerprint = state.current_fingerprint;
                self.competing_monitor_rules = state.competing_monitor_rules;
                // Only covers the exact-match case (profile id == connected
                // fingerprint); superset/subset matches are only known once
                // a ProfileApplied signal arrives. Never clobber an already
                // -known current profile with "unknown" just because this
                // particular load can't derive it.
                if !state.current_profile.is_empty() {
                    self.current_profile_id = Some(state.current_profile.clone());
                } else if let Some(p) = self
                    .profiles
                    .iter()
                    .find(|p| p.id == self.current_fingerprint)
                {
                    self.current_profile_id = Some(p.id.clone());
                }
                self.maybe_autoload_editor()
            }
            Message::Loaded(Err(e)) => {
                self.connected = false;
                self.error = Some(e);
                Task::none()
            }
            Message::Apply(id) => Task::perform(apply(id), |(id, result)| Message::Applied(id, result)),
            Message::Applied(id, Ok(())) => {
                self.current_profile_id = Some(id);
                Task::perform(load(), Message::Loaded)
            }
            Message::Applied(_, Err(e)) => {
                self.error = Some(e);
                Task::none()
            }
            Message::ToggleOtherProfiles => {
                self.show_other_profiles = !self.show_other_profiles;
                Task::none()
            }
            Message::RenameStart(id, current) => {
                self.renaming = Some((id, current));
                Task::none()
            }
            Message::RenameInput(value) => {
                if let Some((_, draft)) = &mut self.renaming {
                    *draft = value;
                }
                Task::none()
            }
            Message::RenameSubmit => {
                if let Some((id, draft)) = self.renaming.take() {
                    Task::perform(rename(id, draft), Message::Renamed)
                } else {
                    Task::none()
                }
            }
            Message::RenameCancelled => {
                self.renaming = None;
                Task::none()
            }
            Message::DeleteStart(id) => {
                let name = self
                    .profiles
                    .iter()
                    .find(|p| p.id == id)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| id.clone());
                // Deleting the profile for what's plugged in right now isn't
                // permanent — auto-learn recreates one on the next topology
                // settle. Say that plainly instead of implying it's gone for
                // good (vision pillar #4: no faith required).
                let is_current = self.current_profile_id.as_deref() == Some(id.as_str());
                let body = if is_current {
                    format!(
                        "\"{name}\" is the profile for your current monitors. Deleting \
                         it discards its saved arrangement; Hyprforge will learn a \
                         fresh one from your live layout the next time your monitors \
                         change. It won't stay deleted while this setup is plugged in."
                    )
                } else {
                    format!(
                        "\"{name}\" will be forgotten, along with its saved arrangement, \
                         head swaps, and output policy. If you plug this set of monitors \
                         back in, Hyprforge will learn them again from scratch."
                    )
                };
                self.deleting = Some(PendingDelete {
                    detail: format!("Profile: {name}\nID: {id}"),
                    id,
                    body,
                });
                Task::none()
            }
            Message::ToggleAdvanced => {
                self.show_advanced = !self.show_advanced;
                Task::none()
            }
            Message::DeleteCancel => {
                self.deleting = None;
                Task::none()
            }
            Message::DeleteConfirm => {
                let Some(pending) = self.deleting.take() else {
                    return Task::none();
                };
                // Drop the editor if it's showing what we just deleted, so
                // the panel can't keep editing a profile that no longer
                // exists (every save would fail with "no such profile").
                if self
                    .editor
                    .as_ref()
                    .is_some_and(|e| e.profile.id == pending.id)
                {
                    self.editor = None;
                }
                Task::perform(delete(pending.id), Message::Deleted)
            }
            Message::Deleted(Ok(())) => {
                self.error = None;
                Task::perform(load(), Message::Loaded)
            }
            Message::Deleted(Err(e)) => {
                self.error = Some(e);
                Task::none()
            }
            Message::Renamed(Ok(())) => Task::perform(load(), Message::Loaded),
            Message::Renamed(Err(e)) => {
                self.error = Some(e);
                Task::none()
            }
            Message::SignalReceived(kind) => {
                self.last_event = Some(match kind {
                    SignalKind::ProfileApplied { id, name, tier } => {
                        self.current_profile_id = Some(id);
                        format!("Applied '{name}' ({tier})")
                    }
                    SignalKind::NewTopologySeen { summary } => format!("New topology: {summary}"),
                    SignalKind::RevertPending { seconds } => {
                        self.revert_seconds_left = Some(seconds);
                        return Task::none();
                    }
                    SignalKind::RevertResolved { reverted } => {
                        self.revert_seconds_left = None;
                        let msg = if reverted {
                            "Display change reverted."
                        } else {
                            "Display change kept."
                        };
                        self.last_event = Some(msg.to_string());
                        // A revert rolls the stored profile back too, so the
                        // editor's copy is stale either way.
                        let reload = Task::perform(load(), Message::Loaded);
                        let refresh_editor = match &self.editor {
                            Some(e) => Task::perform(
                                load_profile(e.profile.id.clone()),
                                Message::LayoutLoaded,
                            ),
                            None => Task::none(),
                        };
                        return Task::batch([reload, refresh_editor]);
                    }
                });
                let reload = Task::perform(load(), Message::Loaded);
                Task::batch([reload, self.maybe_autoload_editor()])
            }
            Message::RevertTick => {
                // Purely cosmetic: the daemon's timer is the one that counts.
                // Floor at 1 rather than 0 so the banner never reads "0s" for
                // however long the round trip to the daemon takes.
                if let Some(left) = self.revert_seconds_left {
                    self.revert_seconds_left = Some(left.saturating_sub(1).max(1));
                }
                Task::none()
            }
            Message::KeepLayout => {
                self.revert_seconds_left = None;
                Task::perform(confirm_layout(), Message::RevertActionDone)
            }
            Message::RevertLayoutNow => {
                self.revert_seconds_left = None;
                Task::perform(revert_layout(), Message::RevertActionDone)
            }
            Message::RevertActionDone(Ok(())) => Task::none(),
            Message::RevertActionDone(Err(e)) => {
                self.error = Some(e);
                Task::none()
            }
            Message::EditLayout(id) => Task::perform(load_profile(id), Message::LayoutLoaded),
            Message::LayoutLoaded(Ok(profile)) => {
                let selected = profile.heads.first().map(|h| h.connector_hint.clone());
                self.editor = Some(LayoutEditor::new(profile));
                match selected {
                    Some(hint) => Task::perform(fetch_modes(hint.clone()), move |modes| {
                        Message::ModesLoaded(hint.clone(), modes)
                    }),
                    None => Task::none(),
                }
            }
            Message::LayoutLoaded(Err(e)) => {
                self.error = Some(e);
                Task::none()
            }
            Message::SelectHead(hint) => {
                if let Some(editor) = &mut self.editor {
                    if let Some(h) = editor.profile.heads.iter().find(|h| h.connector_hint == hint) {
                        let (x, y, w, ht, r, s, t) = fields_from_head(h);
                        editor.field_x = x;
                        editor.field_y = y;
                        editor.field_width = w;
                        editor.field_height = ht;
                        editor.field_refresh = r;
                        editor.field_scale = s;
                        editor.field_transform = t;
                    }
                    editor.available_modes = Vec::new();
                    editor.selected = Some(hint.clone());
                    return Task::perform(fetch_modes(hint.clone()), move |modes| {
                        Message::ModesLoaded(hint.clone(), modes)
                    });
                }
                Task::none()
            }
            Message::ModesLoaded(hint, modes) => {
                if let Some(editor) = &mut self.editor {
                    // Discard if the user already selected a different
                    // head before this (possibly slow) D-Bus round trip
                    // returned.
                    if editor.selected.as_deref() == Some(hint.as_str()) {
                        editor.available_modes = modes
                            .into_iter()
                            .map(|(width, height, refresh_mhz, preferred)| ModeOption {
                                width,
                                height,
                                refresh_mhz,
                                preferred,
                            })
                            .collect();
                    }
                }
                Task::none()
            }
            Message::FieldTransform(label) => {
                if let Some(editor) = &mut self.editor {
                    editor.field_transform = label;
                    commit_selected_head(editor);
                }
                self.mark_unapplied()
            }
            Message::HeadDragMoved(connector_hint, x, y) => {
                if let Some(editor) = &mut self.editor {
                    if let Some(head) = editor
                        .profile
                        .heads
                        .iter_mut()
                        .find(|h| h.connector_hint == connector_hint)
                    {
                        head.x = x;
                        head.y = y;
                    }
                    if editor.selected.as_deref() == Some(connector_hint.as_str()) {
                        editor.field_x = x.to_string();
                        editor.field_y = y.to_string();
                    }
                }
                // Fires per frame while dragging, and none of them
                // applies anything — the whole gesture is one edit, and
                // Save & Apply is what puts it on screen.
                self.mark_unapplied()
            }
            Message::FieldX(v) => self.edit(|editor| editor.field_x = v),
            Message::FieldY(v) => self.edit(|editor| editor.field_y = v),
            Message::FieldWidth(v) => self.edit(|editor| editor.field_width = v),
            Message::FieldHeight(v) => self.edit(|editor| editor.field_height = v),
            Message::FieldRefresh(v) => self.edit(|editor| editor.field_refresh = v),
            Message::FieldScale(v) => self.edit(|editor| editor.field_scale = v),
            Message::SwapSelectA(hint) => {
                if let Some(editor) = &mut self.editor {
                    editor.swap_a = Some(hint);
                }
                Task::none()
            }
            Message::SwapSelectB(hint) => {
                if let Some(editor) = &mut self.editor {
                    editor.swap_b = Some(hint);
                }
                Task::none()
            }
            Message::ToggleSwap => {
                let Some(editor) = &self.editor else {
                    return Task::none();
                };
                let (Some(a), Some(b)) = (editor.swap_a.clone(), editor.swap_b.clone()) else {
                    return Task::none();
                };
                if a == b {
                    return Task::none();
                }
                Task::perform(toggle_swap(editor.profile.id.clone(), a, b), Message::SwapToggled)
            }
            Message::SwapToggled(Ok(profile)) => {
                if let Some(editor) = &mut self.editor {
                    editor.profile = profile;
                    editor.status = Some("Swap updated.".to_string());
                    editor.error = None;
                }
                Task::none()
            }
            Message::SwapToggled(Err(e)) => {
                if let Some(editor) = &mut self.editor {
                    editor.error = Some(e);
                }
                Task::none()
            }
            Message::SetPolicy(policy) => {
                let Some(editor) = &self.editor else {
                    return Task::none();
                };
                Task::perform(
                    set_policy(editor.profile.id.clone(), policy),
                    Message::PolicySet,
                )
            }
            Message::PolicySet(Ok(profile)) => {
                if let Some(editor) = &mut self.editor {
                    editor.profile = profile;
                    editor.status = Some("Policy updated.".to_string());
                    editor.error = None;
                }
                Task::none()
            }
            Message::PolicySet(Err(e)) => {
                if let Some(editor) = &mut self.editor {
                    editor.error = Some(e);
                }
                Task::none()
            }
            Message::ApplyEditor => self.apply_editor(),
            Message::DiscardEdits => {
                // Back to what is on disk, which is what the compositor
                // is running — reloading is the whole undo.
                match self.editor.as_ref().map(|e| e.profile.id.clone()) {
                    Some(id) => Task::perform(load_profile(id), Message::LayoutLoaded),
                    None => Task::none(),
                }
            }
            Message::ResolutionSelected(res) => {
                if let Some(editor) = &mut self.editor {
                    editor.field_width = res.width.to_string();
                    editor.field_height = res.height.to_string();
                    // The old refresh rate may not exist at the new
                    // resolution; snap to the fastest one that does.
                    if let Some(best) = editor
                        .available_modes
                        .iter()
                        .filter(|m| m.width == res.width && m.height == res.height)
                        .map(|m| m.refresh_mhz)
                        .max()
                    {
                        let current = editor
                            .field_refresh
                            .trim()
                            .parse::<f64>()
                            .map(|hz| (hz * 1000.0).round() as i32)
                            .unwrap_or(0);
                        let still_valid = editor.available_modes.iter().any(|m| {
                            m.width == res.width
                                && m.height == res.height
                                && m.refresh_mhz == current
                        });
                        if !still_valid {
                            editor.field_refresh = format!("{:.0}", best as f64 / 1000.0);
                        }
                    }
                    // Which scales are achievable depends on the resolution
                    // — they're the divisors of its gcd — so one that was
                    // valid a moment ago need not be at the new size. Same
                    // reasoning as the refresh rate above.
                    if let Ok(pct) = editor.field_scale.trim().parse::<f64>() {
                        let snapped =
                            nearest_valid_scale(res.width, res.height, pct / 100.0) * 100.0;
                        editor.field_scale = format!("{snapped:.4}");
                    }
                    commit_selected_head(editor);
                }
                self.mark_unapplied()
            }
            Message::RefreshSelected(rate) => {
                if let Some(editor) = &mut self.editor {
                    editor.field_refresh = format!("{:.3}", rate.mhz as f64 / 1000.0);
                    commit_selected_head(editor);
                }
                self.mark_unapplied()
            }
            Message::ToggleWarnings => {
                self.show_warnings = !self.show_warnings;
                Task::none()
            }
            Message::ImportFromConfig => {
                self.import_review = Some(ImportState::Running);
                Task::perform(import_with_layout(), Message::ImportEvaluated)
            }
            Message::ImportEvaluated((result, live)) => {
                let hyprforge_dir = hyprforge_core::paths::hypr_hyprforge_dir();
                let monitors: Vec<ImportedMonitor> = result
                    .calls
                    .iter()
                    // Hyprforge's own generated `monitors.lua` is
                    // `require()`d from hyprland.lua too, so it gets
                    // evaluated right along with the user's own —
                    // excluded here, or the daemon's own fallback would
                    // show up as something to review.
                    .filter(|call| !call.source_path.starts_with(&hyprforge_dir))
                    .filter_map(|call| parse_monitor_call(&call.kind, &call.args))
                    .map(|mut m| {
                        m.target = m.resolve(&live);
                        // Pre-checked only when it can really be applied.
                        m.checked = m.target.is_some() && m.geometry().is_some();
                        m
                    })
                    .collect();
                self.import_review =
                    Some(ImportState::Ready(ImportSummary { monitors, failures: result.failures }));
                Task::none()
            }
            Message::ImportToggle(i, checked) => {
                if let Some(ImportState::Ready(review)) = &mut self.import_review {
                    if let Some(m) = review.monitors.get_mut(i) {
                        // Only an applicable entry can be ticked; the view
                        // doesn't offer a checkbox for the others, and this
                        // guards the message arriving anyway.
                        if m.target.is_some() && m.geometry().is_some() {
                            m.checked = checked;
                        }
                    }
                }
                Task::none()
            }
            Message::ImportApply => {
                let Some(ImportState::Ready(review)) = &self.import_review else {
                    return Task::none();
                };
                // The profile for what's connected right now. Auto-learn
                // guarantees one exists — "nothing matches" is precisely the
                // condition that creates it — so there is nothing to invent.
                let Some(profile_id) = self
                    .current_profile_id
                    .clone()
                    .or_else(|| (!self.current_fingerprint.is_empty()).then(|| self.current_fingerprint.clone()))
                else {
                    self.error = Some(
                        "No profile for the current displays yet — Hyprforge learns one \
                         automatically a moment after it sees them."
                            .to_string(),
                    );
                    return Task::none();
                };
                let edits: Vec<(String, ImportGeometry)> = review
                    .importable()
                    .filter(|(_, m)| m.checked)
                    .filter_map(|(_, m)| Some((m.target.clone()?, m.geometry()?)))
                    .collect();
                if edits.is_empty() {
                    return Task::none();
                }
                self.import_review = None;
                Task::perform(apply_imported_geometry(profile_id, edits), Message::ImportApplied)
            }
            Message::ImportApplied(Ok(())) => {
                self.error = None;
                Task::perform(load(), Message::Loaded)
            }
            Message::ImportApplied(Err(e)) => {
                self.error = Some(e);
                Task::perform(load(), Message::Loaded)
            }
            Message::ImportClose => {
                self.import_review = None;
                Task::none()
            }
            Message::LayoutSaved(Ok(())) => {
                if let Some(editor) = &mut self.editor {
                    editor.status = Some("Saved and applied.".to_string());
                    editor.error = None;
                    // What is on screen is now what is in the editor.
                    editor.unapplied = false;
                }
                Task::perform(load(), Message::Loaded)
            }
            Message::LayoutSaved(Err(e)) => {
                if let Some(editor) = &mut self.editor {
                    editor.error = Some(e);
                }
                Task::none()
            }
        }
    }

    fn view(&self, scale: FontScale) -> Element<'_, Message> {
        if !self.connected {
            return container(
                column![
                    scaled_text(
                        self.error
                            .clone()
                            .unwrap_or_else(|| "hyprforge-displayd is not running.".to_string()),
                        BASE_TEXT_SIZE,
                        scale,
                    ),
                    meta_text(
                        "Start it with: systemctl --user start hyprforge-displayd",
                        13.0,
                        scale,
                    ),
                    container(primary_button("Retry").on_press(Message::Refresh))
                        .width(Length::Fill)
                        .align_x(iced::alignment::Horizontal::Right),
                ]
                .spacing(spacing::SM),
            )
            .padding(spacing::XL)
            .into();
        }

        if let Some(pending) = &self.deleting {
            return container(confirm_dialog(
                "Delete this display profile?",
                &pending.body,
                &pending.detail,
                Message::DeleteConfirm,
                Message::DeleteCancel,
            ))
            .center(Length::Fill)
            .into();
        }

        if let Some(state) = &self.import_review {
            return self.import_review_view(state, scale);
        }

        let mut content = column![].spacing(spacing::LG);

        if let Some(notice) = self.setup_notice(scale) {
            content = content.push(notice);
        }

        // The countdown lives in its own pinned, always-visible window (see
        // `revert_popup_view` in main.rs) rather than here. A change that
        // scrambles a display can leave this window unreadable or on a
        // workspace the user can't see — which is exactly when the prompt
        // matters most.


        match &self.editor {
            Some(editor) => content = content.push(self.editor_body(editor, scale)),
            None => {
                let msg = if self.current_profile_id.is_some() {
                    "Loading current setup…"
                } else {
                    "Hyprforge hasn't matched a saved profile to this display setup yet — \
                     it will learn one automatically."
                };
                content = content.push(section("Current Setup", scale, meta_text(msg, BASE_TEXT_SIZE, scale)));
            }
        }

        // The fingerprint is a 64-char hash — diagnostic output, not
        // something a user acts on, so it lives behind Advanced rather than
        // sitting under the controls on every visit.
        if self.show_advanced {
            content = content.push(meta_text(
                format!("Fingerprint: {}", self.current_fingerprint),
                11.0,
                scale,
            ));
        }
        if let Some(event) = &self.last_event {
            content = content.push(meta_text(event.clone(), 11.0, scale));
        }

        let profiles_button = secondary_button(if self.show_other_profiles {
            "Hide other display profiles"
        } else {
            "Other display profiles"
        })
        .on_press(Message::ToggleOtherProfiles);

        if self.show_other_profiles {
            let mut list = column![].spacing(spacing::SM);
            if self.profiles.is_empty() {
                list = list.push(meta_text(
                    "No profiles yet — connect a display configuration and Hyprforge will learn it.",
                    BASE_TEXT_SIZE,
                    scale,
                ));
            }
            for (i, p) in self.profiles.iter().enumerate() {
                if i > 0 {
                    list = list.push(divider());
                }
                let is_current = Some(&p.id) == self.current_profile_id.as_ref();
                let row_el: Element<'_, Message> = match &self.renaming {
                    Some((id, draft)) if id == &p.id => row![
                        text_input("Profile name", draft)
                            .on_input(Message::RenameInput)
                            .on_submit(Message::RenameSubmit)
                            .width(Length::Fill),
                        primary_button("Save").on_press(Message::RenameSubmit),
                        secondary_button("Cancel").on_press(Message::RenameCancelled),
                    ]
                    .spacing(spacing::SM)
                    .padding([spacing::SM, 0.0])
                    .into(),
                    _ => self.profile_row(p, scale, is_current),
                };
                list = list.push(row_el);
            }
            content = content.push(section(
                "Other Display Profiles",
                scale,
                container(scrollable(list).width(Length::Fill).height(Length::Shrink))
                .max_height(360.0),
            ));
        }

        // A standing condition, not news: it's true on every visit until
        // the user edits their own config, so it sits collapsed at the
        // bottom instead of pushing the actual controls below the fold.
        if !self.competing_monitor_rules.is_empty() {
            let count = self.competing_monitor_rules.len();
            let summary = row![
                meta_text(
                    format!(
                        "{count} config file{} may override Hyprforge's layout.",
                        if count == 1 { "" } else { "s" }
                    ),
                    12.0,
                    scale,
                ),
                secondary_button(if self.show_warnings { "Hide" } else { "Details" })
                    .on_press(Message::ToggleWarnings),
            ]
            .spacing(spacing::SM)
            .align_y(iced::Alignment::Center);

            let block: Element<'_, Message> = if self.show_warnings {
                column![
                    summary,
                    scaled_text(
                        format!(
                            "hl.monitor() rules found in {} — these are re-applied on \
                             every hyprctl reload and may override Hyprforge's \
                             auto-applied layout. Hyprforge will never edit these files \
                             for you.",
                            self.competing_monitor_rules.join(", ")
                        ),
                        13.0,
                        scale,
                    ),
                ]
                .spacing(spacing::SM)
                .into()
            } else {
                summary.into()
            };
            content = content.push(block);
        }

        // One footer row rather than two separately right-aligned buttons
        // stacked on top of each other.
        content = content.push(
            container(
                row![
                    secondary_button("Import from config").on_press(Message::ImportFromConfig),
                    profiles_button,
                    secondary_button("Refresh").on_press(Message::Refresh),
                ]
                .spacing(spacing::SM),
            )
            .width(Length::Fill)
            .align_x(iced::alignment::Horizontal::Right),
        );

        container(content).padding(spacing::LG).into()
    }

    fn subscription(&self) -> Subscription<Message> {
        if !self.connected {
            return Subscription::none();
        }
        let signals = Subscription::run(signal_stream);
        match self.revert_seconds_left {
            Some(_) => Subscription::batch([
                signals,
                iced::time::every(std::time::Duration::from_secs(1))
                    .map(|_| Message::RevertTick),
            ]),
            None => signals,
        }
    }
}

impl DisplaysModule {
    /// The canvas + property panel + policy/swap controls — the "editing a
    /// profile" view, embedded directly in the Monitors screen rather than
    /// behind a separate button (Windows-Display-Settings-style: land on
    /// the diagram, not a summary card).
    fn editor_body<'a>(&'a self, editor: &'a LayoutEditor, scale: FontScale) -> Element<'a, Message> {
        let mut body = column![].spacing(spacing::LG);

        if Some(&editor.profile.id) != self.current_profile_id.as_ref() {
            let mut notice = row![meta_text(
                format!(
                    "Editing '{}' — not the currently-applied setup.",
                    editor.profile.name
                ),
                12.0,
                scale,
            )]
            .spacing(spacing::SM)
            .align_y(iced::Alignment::Center);
            if let Some(current_id) = &self.current_profile_id {
                notice = notice
                    .push(secondary_button("Back to current").on_press(Message::EditLayout(current_id.clone())));
            }
            body = body.push(notice);
        }

        // Above the canvas, not below it: this is the answer to "why is
        // nothing happening", and an explanation that needs scrolling to
        // is not one.
        //
        // The buttons are the shell's pending bar now (see `pending`); what
        // stays here is the one thing that bar has no room to say.
        if editor.unapplied {
            body = body.push(meta_text(
                "These changes are not on screen yet. Applying starts a countdown \
                 that puts the old layout back if you do not confirm — so a setting \
                 that blanks a monitor cannot strand you.",
                BASE_TEXT_SIZE,
                scale,
            ));
        }

        let canvas_heads: Vec<CanvasHead> = editor
            .profile
            .heads
            .iter()
            .map(|h| {
                // Logical, to match x/y — see `HeadDetail::logical_size`.
                let (width, height) = h.logical_size();
                CanvasHead {
                    connector_hint: h.connector_hint.clone(),
                    label: canvas_label(h, &editor.profile.heads),
                    x: h.x,
                    y: h.y,
                    width,
                    height,
                    enabled: h.enabled,
                }
            })
            .collect();

        // The arrangement canvas only earns its space when there's an
        // arrangement to make. With one display it's a 380px box holding a
        // single rectangle you can't meaningfully drag — Windows, macOS and
        // GNOME all hide the diagram entirely below two displays.
        if canvas_heads.len() > 1 {
            let canvas = LayoutCanvas::new(
                canvas_heads,
                editor.selected.clone(),
                Message::SelectHead,
                Message::HeadDragMoved,
            )
            .into_element();
            body = body.push(section(
                "Arrangement — drag a monitor to match your desk",
                scale,
                canvas,
            ));
        }

        let choices: Vec<HeadChoice> = editor.profile.heads.iter().map(HeadChoice::new).collect();
        let selected_choice = editor
            .selected
            .as_ref()
            .and_then(|hint| choices.iter().find(|c| &c.hint == hint).cloned());
        // With a single display there is nothing to pick between, so the
        // picker is just a row restating the name — show it as a heading.
        let monitor_picker: Element<'_, Message> = if choices.len() > 1 {
            row_field(
                "Monitor",
                iced::widget::pick_list(choices, selected_choice, |c: HeadChoice| {
                    Message::SelectHead(c.hint)
                })
                .placeholder("Select a monitor"),
            )
        } else {
            match selected_choice {
                Some(c) => column![
                    scaled_text(c.label.clone(), 16.0, scale),
                    meta_text(c.hint.clone(), 12.0, scale),
                ]
                .spacing(spacing::XS)
                .into(),
                None => meta_text("No monitor selected.", 13.0, scale).into(),
            }
        };

        let properties: Element<'_, Message> = if editor.selected_head().is_some() {
            // The stored mode is always offered, even when the live mode
            // list is unavailable, so there's never a bare text-box
            // fallback on the common path.
            let stored = editor
                .field_width
                .trim()
                .parse::<i32>()
                .ok()
                .zip(editor.field_height.trim().parse::<i32>().ok());
            let stored_refresh = editor
                .field_refresh
                .trim()
                .parse::<f64>()
                .map(|hz| (hz * 1000.0).round() as i32)
                .ok();

            let mut resolutions: Vec<ResolutionOption> = Vec::new();
            for m in &editor.available_modes {
                if let Some(existing) = resolutions
                    .iter_mut()
                    .find(|r| r.width == m.width && r.height == m.height)
                {
                    existing.preferred |= m.preferred;
                } else {
                    resolutions.push(ResolutionOption {
                        width: m.width,
                        height: m.height,
                        preferred: m.preferred,
                    });
                }
            }
            if let Some((w, h)) = stored {
                if !resolutions.iter().any(|r| r.width == w && r.height == h) {
                    resolutions.push(ResolutionOption {
                        width: w,
                        height: h,
                        preferred: false,
                    });
                }
            }
            // Largest first — the order every other display panel uses.
            resolutions.sort_by_key(|r| std::cmp::Reverse((r.width, r.height)));

            let selected_resolution = stored.map(|(w, h)| ResolutionOption {
                width: w,
                height: h,
                preferred: resolutions
                    .iter()
                    .any(|r| r.width == w && r.height == h && r.preferred),
            });

            let mut refresh_rates: Vec<RefreshOption> = editor
                .available_modes
                .iter()
                .filter(|m| stored.is_none_or(|(w, h)| m.width == w && m.height == h))
                .map(|m| RefreshOption { mhz: m.refresh_mhz })
                .collect();
            if let Some(mhz) = stored_refresh {
                if !refresh_rates.iter().any(|r| r.mhz == mhz) {
                    refresh_rates.push(RefreshOption { mhz });
                }
            }
            refresh_rates.sort_by_key(|r| std::cmp::Reverse(r.mhz));
            refresh_rates.dedup();

            let resolution_field: Element<'_, Message> = column![
                row_field(
                    "Resolution",
                    iced::widget::pick_list(
                        resolutions,
                        selected_resolution,
                        Message::ResolutionSelected,
                    )
                    .placeholder("Select a resolution"),
                ),
                row_field(
                    "Refresh rate",
                    iced::widget::pick_list(
                        refresh_rates,
                        stored_refresh.map(|mhz| RefreshOption { mhz }),
                        Message::RefreshSelected,
                    )
                    .placeholder("Select a refresh rate"),
                ),
            ]
            .spacing(spacing::SM)
            .into();

            let orientation_field = row_field(
                "Orientation",
                iced::widget::pick_list(
                    TRANSFORM_LABELS.to_vec(),
                    Some(editor.field_transform.as_str()),
                    |label: &str| Message::FieldTransform(label.to_string()),
                ),
            );

            let (px_w, px_h) = editor
                .selected_head()
                .map(|h| (h.width, h.height))
                .unwrap_or((1920, 1080));
            let (scale_options, current_scale) = scale_choices(px_w, px_h, &editor.field_scale);
            let scale_field = row_field(
                "Scale",
                iced::widget::pick_list(
                    scale_options,
                    current_scale.map(|percent| ScaleChoice { percent }),
                    // Keep the exact value: rounding to a whole percent here
                    // is what made a valid scale invalid again.
                    |c: ScaleChoice| Message::FieldScale(format!("{:.4}", c.percent)),
                )
                .placeholder("Select a scale"),
            );

            // Scale first, then resolution, then orientation — the Windows
            // ordering, and the rough order of how often each is touched.
            column![monitor_picker, scale_field, resolution_field, orientation_field]
                .spacing(spacing::SM)
                .into()
        } else {
            column![monitor_picker, meta_text("This profile has no heads.", 13.0, scale)]
                .spacing(spacing::SM)
                .into()
        };
        body = body.push(section("Selected monitor", scale, properties));

        let swap_hints: Vec<String> = editor
            .profile
            .heads
            .iter()
            .map(|h| h.connector_hint.clone())
            .collect();
        let swap_row = row![
            iced::widget::pick_list(swap_hints.clone(), editor.swap_a.clone(), Message::SwapSelectA)
                .placeholder("Head A"),
            iced::widget::pick_list(swap_hints, editor.swap_b.clone(), Message::SwapSelectB)
                .placeholder("Head B"),
            {
                let ready = matches!(
                    (&editor.swap_a, &editor.swap_b),
                    (Some(a), Some(b)) if a != b
                );
                let btn = secondary_button("Toggle Swap");
                if ready {
                    btn.on_press(Message::ToggleSwap)
                } else {
                    btn
                }
            },
        ]
        .spacing(spacing::SM)
        .align_y(iced::Alignment::Center);

        let policy_button = |label: &'static str, value: &'static str| {
            let active = editor.profile.extra_output_policy == value;
            let btn = if active {
                primary_button(label)
            } else {
                secondary_button(label)
            };
            btn.on_press(Message::SetPolicy(value.to_string()))
        };
        let policy_row = row![
            policy_button("Extend Right", "extend_right"),
            policy_button("Mirror", "mirror"),
            policy_button("Disable", "disable"),
        ]
        .spacing(spacing::SM);

        // Everything below is either rarely touched (numeric position —
        // dragging the canvas is the real interaction, and Windows/macOS
        // don't expose coordinates at all) or a fix for a specific problem
        // you only reach after hitting it. Collapsed so the common path is
        // scale/resolution/orientation and nothing else.
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
            if editor.available_modes.is_empty() && editor.selected_head().is_some() {
                body = body.push(section(
                    "Custom resolution",
                    scale,
                    column![
                        // Deliberately doesn't assert *why*. The mode list is
                        // empty when the head isn't plugged in, but also if
                        // the daemon simply couldn't read it — claiming
                        // "not connected" about a screen the user is looking
                        // at is worse than saying nothing.
                        meta_text(
                            "Supported modes for this monitor couldn't be read, so \
                             only its saved resolution is listed above. Set one \
                             manually here if you need a different mode.",
                            12.0,
                            scale,
                        ),
                        row_field(
                            "Width (px)",
                            text_input("1920", &editor.field_width).on_input(Message::FieldWidth),
                        ),
                        row_field(
                            "Height (px)",
                            text_input("1080", &editor.field_height).on_input(Message::FieldHeight),
                        ),
                        row_field(
                            "Refresh rate (Hz)",
                            text_input("60", &editor.field_refresh).on_input(Message::FieldRefresh),
                        ),
                    ]
                    .spacing(spacing::SM),
                ));
            }

            if editor.selected_head().is_some() {
                // The arrangement canvas is only on screen with 2+ heads, so
                // don't point at something that isn't there.
                let hint = if editor.profile.heads.len() > 1 {
                    "Usually set by dragging on the arrangement above."
                } else {
                    "A single display sits at 0,0 — this only matters once a \
                     second monitor is attached."
                };
                body = body.push(section(
                    "Position",
                    scale,
                    column![
                        meta_text(hint, 12.0, scale),
                        row_field("X", text_input("0", &editor.field_x).on_input(Message::FieldX)),
                        row_field("Y", text_input("0", &editor.field_y).on_input(Message::FieldY)),
                    ]
                    .spacing(spacing::SM),
                ));
            }

            body = body.push(section(
                "Monitors this profile doesn't cover",
                scale,
                column![
                    meta_text(
                        "What to do with a display that's plugged in but isn't part \
                         of this profile.",
                        12.0,
                        scale,
                    ),
                    policy_row,
                ]
                .spacing(spacing::SM),
            ));

            // Only reachable — and only meaningful — with two or more heads.
            if editor.profile.heads.len() > 1 {
                body = body.push(section(
                    "My monitors are swapped",
                    scale,
                    column![
                        meta_text(
                            "If two identical monitors got each other's settings, \
                             Hyprforge can't tell them apart from their EDID alone. \
                             Pick both and swap them.",
                            12.0,
                            scale,
                        ),
                        swap_row,
                    ]
                    .spacing(spacing::SM),
                ));
            }
        }

        if let Some(status) = &editor.status {
            body = body.push(meta_text(status.clone(), 13.0, scale));
        }
        if let Some(err) = &editor.error {
            body = body.push(scaled_text(format!("Error: {err}"), 13.0, scale));
        }

        // The Save & Apply bar is at the top of this body rather than
        // here, and only when there is something unapplied — see
        // `LayoutEditor::unapplied`.
        //
        // This page used to apply as you edited, on the GNOME HIG's
        // reasoning that an instant-apply page needs no dismissal
        // button, with the daemon's timed revert as the undo. The
        // argument is sound and the result was not: arranging three
        // monitors is drag, look, drag again, and having the screen
        // rearrange itself between those is disorienting. The revert
        // countdown stays — it is what makes applying safe — but it is
        // a net for the apply you *asked* for, not a substitute for
        // asking.
        body.into()
    }
}

/// How long any displayd query may take before the app gives up on it.
///
/// zbus has no reply timeout on a `#[proxy]`-generated method, and no
/// attribute to ask for one, so the bound has to be applied at the call
/// site — which is what the `command::TIMEOUT` rule already says for
/// anything that waits on another process. Without it a wedged daemon
/// leaves the screen showing "Applying…" with no way out, which is the
/// dead end pillar #3 rules out.
const CALL_TIMEOUT: Duration = hyprforge_core::command::TIMEOUT;

/// Applying is not a query and must not share a query's bound.
///
/// displayd commits to the compositor and waits for it to settle: three
/// attempts at up to 3s to test plus 3s to apply, so 18s of legitimate
/// work before it gives up on its own. A shorter bound here would abandon
/// applies that were about to succeed — and abandon them *after* the
/// compositor had already changed mode, which is the worst moment to stop
/// listening. This sits above the daemon's own ceiling, so reaching it
/// means displayd is genuinely wedged rather than merely busy.
const APPLY_TIMEOUT: Duration = Duration::from_secs(25);

/// Bounds one displayd call and turns whatever happened into a sentence.
///
/// `what` names the call in the user's terms, since it is what they will
/// read when the daemon doesn't answer.
async fn bounded<T>(
    what: &str,
    within: Duration,
    call: impl std::future::Future<Output = zbus::Result<T>>,
) -> Result<T, String> {
    match tokio::time::timeout(within, call).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(format!(
            "{what} timed out after {}s — the display daemon isn't answering. \
             Check `systemctl --user status hyprforge-displayd`.",
            within.as_secs()
        )),
    }
}

/// The session bus and a proxy on it, both bounded.
///
/// Connecting can hang for the same reason a call can — a bus that
/// accepts the socket and never replies — so the connection is no safer
/// unbounded than the calls that follow it.
async fn connect() -> Result<zbus::Connection, String> {
    match tokio::time::timeout(CALL_TIMEOUT, zbus::Connection::session()).await {
        Ok(result) => result.map_err(|e| e.to_string()),
        Err(_) => Err("the session bus isn't answering".to_string()),
    }
}

async fn displayd(conn: &zbus::Connection) -> Result<DisplaydProxy<'_>, String> {
    bounded("connecting to the display daemon", CALL_TIMEOUT, DisplaydProxy::new(conn)).await
}

async fn load() -> Result<LoadedState, String> {
    let conn = connect().await?;
    let proxy = displayd(&conn).await?;
    let profiles = bounded("listing display profiles", CALL_TIMEOUT, proxy.list_profiles())
        .await?
        .into_iter()
        .map(|(id, name, head_count, last_used)| ProfileInfo {
            id,
            name,
            head_count,
            last_used,
        })
        .collect();
    let current_fingerprint =
        bounded("reading the current displays", CALL_TIMEOUT, proxy.get_current_fingerprint())
            .await?;
    let competing_monitor_rules = bounded(
        "checking for competing monitor rules",
        CALL_TIMEOUT,
        proxy.competing_monitor_rules(),
    )
    .await?;
    let current_profile =
        bounded("reading the active profile", CALL_TIMEOUT, proxy.get_current_profile()).await?;
    Ok(LoadedState {
        profiles,
        current_fingerprint,
        competing_monitor_rules,
        current_profile,
    })
}

async fn apply(id: String) -> (String, Result<(), String>) {
    let result = apply_inner(&id).await;
    (id, result)
}

/// Always the reversible variant here: a GUI apply is exactly the case the
/// confirm/revert window exists for. `ApplyProfile` stays immediate for
/// scripted `displayctl` use, which has no banner to click.
async fn apply_inner(id: &str) -> Result<(), String> {
    let conn = connect().await?;
    let proxy = displayd(&conn).await?;
    bounded("applying the layout", APPLY_TIMEOUT, proxy.apply_profile_reversible(id))
        .await
        .map(|_seconds| ())
}

async fn confirm_layout() -> Result<(), String> {
    let conn = connect().await?;
    let proxy = displayd(&conn).await?;
    bounded("confirming the layout", CALL_TIMEOUT, proxy.confirm_layout()).await
}

async fn revert_layout() -> Result<(), String> {
    let conn = connect().await?;
    let proxy = displayd(&conn).await?;
    bounded("reverting the layout", APPLY_TIMEOUT, proxy.revert_layout()).await
}

async fn rename(id: String, new_name: String) -> Result<(), String> {
    let conn = connect().await?;
    let proxy = displayd(&conn).await?;
    bounded("renaming the profile", CALL_TIMEOUT, proxy.rename_profile(&id, &new_name)).await
}

/// Writes imported geometry into the profile for the currently-connected
/// displays, one head at a time.
///
/// Deliberately reuses `SetHeadGeometry` rather than adding a
/// "create profile from a config file" RPC. Every profile in this system is
/// born from really-observed hardware, and that invariant is what makes
/// matching trustworthy — a profile conjured from a `desc:` string carries
/// no serial and could never match anything. For a display that *is*
/// plugged in, the profile already exists and only its numbers need
/// changing, which is exactly what this RPC is for.
///
/// Transform is left alone: `hl.monitor`'s transform is an integer with
/// Hyprland's own numbering, and mapping it wrong would rotate someone's
/// screen. Position, size, refresh and scale are the fields worth importing.
async fn apply_imported_geometry(
    profile_id: String,
    edits: Vec<(String, ImportGeometry)>,
) -> Result<(), String> {
    let conn = connect().await?;
    let proxy = displayd(&conn).await?;
    let detail: ProfileDetail = serde_json::from_str(
        &bounded("reading the profile", CALL_TIMEOUT, proxy.get_profile(&profile_id)).await?,
    )
    .map_err(|e| e.to_string())?;

    for (connector, geometry) in edits {
        // The stored head to write into. `connector_hint` is the connector
        // the profile was learned on, so for the connected case it is the
        // live connector — but an unmatched name is skipped rather than
        // guessed at, since writing to the wrong head moves the wrong
        // display.
        let Some(head) = detail.heads.iter().find(|h| h.connector_hint == connector) else {
            continue;
        };
        bounded(
            "saving the display position",
            CALL_TIMEOUT,
            proxy.set_head_geometry(
                &profile_id,
                &head.connector_hint,
                geometry.x,
                geometry.y,
                geometry.width,
                geometry.height,
                geometry.refresh_mhz,
                geometry.scale,
                &head.transform,
            ),
        )
        .await?;
    }
    Ok(())
}

async fn delete(id: String) -> Result<(), String> {
    let conn = connect().await?;
    let proxy = displayd(&conn).await?;
    bounded("deleting the profile", CALL_TIMEOUT, proxy.delete_profile(&id)).await
}

async fn load_profile(id: String) -> Result<ProfileDetail, String> {
    let conn = connect().await?;
    let proxy = displayd(&conn).await?;
    let json = bounded("reading the profile", CALL_TIMEOUT, proxy.get_profile(&id)).await?;
    serde_json::from_str(&json).map_err(|e| e.to_string())
}

async fn toggle_swap(profile_id: String, a: String, b: String) -> Result<ProfileDetail, String> {
    let conn = connect().await?;
    let proxy = displayd(&conn).await?;
    bounded("swapping the displays", CALL_TIMEOUT, proxy.swap_heads(&profile_id, &a, &b)).await?;
    load_profile(profile_id).await
}

async fn set_policy(profile_id: String, policy: String) -> Result<ProfileDetail, String> {
    let conn = connect().await?;
    let proxy = displayd(&conn).await?;
    bounded(
        "saving the extra-display policy",
        CALL_TIMEOUT,
        proxy.set_extra_output_policy(&profile_id, &policy),
    )
    .await?;
    load_profile(profile_id).await
}

/// `(connector_hint, x, y, width, height, refresh_mhz, scale, transform)`.
type HeadGeometry = (String, i32, i32, i32, i32, i32, f64, String);

/// Persists head geometry, and — only when `apply` — makes it live.
///
/// Editing a profile that isn't the active one must never switch the user's
/// displays out from under them just because they touched a dropdown. Those
/// edits are saved and take effect the next time that setup is plugged in.
async fn save_geometry(
    profile_id: String,
    heads: Vec<HeadGeometry>,
    apply: bool,
) -> Result<(), String> {
    let conn = connect().await?;
    let proxy = displayd(&conn).await?;
    for (connector_hint, x, y, width, height, refresh_mhz, scale, transform) in heads {
        bounded(
            "saving the display position",
            CALL_TIMEOUT,
            proxy.set_head_geometry(
                &profile_id,
                &connector_hint,
                x,
                y,
                width,
                height,
                refresh_mhz,
                scale,
                &transform,
            ),
        )
        .await?;
    }
    if !apply {
        return Ok(());
    }
    bounded("applying the layout", APPLY_TIMEOUT, proxy.apply_profile_reversible(&profile_id))
        .await
        .map(|_seconds| ())
}

async fn fetch_modes(connector_hint: String) -> Vec<(i32, i32, i32, bool)> {
    let Ok(conn) = connect().await else {
        return Vec::new();
    };
    let Ok(proxy) = displayd(&conn).await else {
        return Vec::new();
    };
    bounded("listing display modes", CALL_TIMEOUT, proxy.get_available_modes(&connector_hint))
        .await
        .unwrap_or_default()
}


/// `None` if this isn't a `monitor` call, or it has no `output` at all
/// (Hyprland requires one).
///
/// Never attempts to reconstruct an `Identity` (make/model/serial) from
/// the `output` selector — see [`ImportedMonitor`]'s doc comment for why
/// that would risk a profile that silently never matches anything. This
/// only extracts what's safe to show as information.
fn parse_monitor_call(kind: &str, args: &[serde_json::Value]) -> Option<ImportedMonitor> {
    if kind != "monitor" {
        return None;
    }
    let table = args.first()?.as_object()?;
    let selector = table.get("output")?.as_str()?.to_string();
    if selector.is_empty() {
        return None;
    }
    let field_as_string = |key: &str| -> Option<String> {
        match table.get(key)? {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        }
    };
    Some(ImportedMonitor {
        selector,
        disabled: table.get("disabled").and_then(serde_json::Value::as_bool).unwrap_or(false),
        mode: field_as_string("mode"),
        position: field_as_string("position"),
        scale: field_as_string("scale"),
        // Filled in once the live layout is known — see
        // `Message::ImportEvaluated`.
        target: None,
        checked: false,
    })
}

/// The currently-connected outputs, for resolving imported `output`
/// selectors. An unreachable daemon yields an empty list, which makes every
/// entry unresolvable and therefore read-only — the honest outcome, since
/// without knowing what's plugged in nothing can be safely applied.
async fn current_layout() -> Vec<LiveHead> {
    let Ok(conn) = connect().await else {
        return Vec::new();
    };
    let Ok(proxy) = displayd(&conn).await else {
        return Vec::new();
    };
    let Ok(json) =
        bounded("reading the current layout", CALL_TIMEOUT, proxy.get_current_layout()).await
    else {
        return Vec::new();
    };
    serde_json::from_str(&json).unwrap_or_default()
}

/// Evaluates the user's config and reads the live layout together, since an
/// imported entry is only meaningful next to what's actually connected.
async fn import_with_layout() -> (hyprforge_lua_import::ImportResult, Vec<LiveHead>) {
    let (result, live) = tokio::join!(super::evaluate_user_config(), current_layout());
    (result, live)
}

fn signal_stream() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(100, |mut output| async move {
        loop {
            if let Err(e) = forward_signals(&mut output).await {
                tracing::warn!(error = %e, "displayd signal stream disconnected; retrying in 3s");
            }
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
    })
}

async fn forward_signals(
    output: &mut iced::futures::channel::mpsc::Sender<Message>,
) -> anyhow::Result<()> {
    use iced::futures::SinkExt;
    use iced::futures::StreamExt;

    // Deliberately *not* bounded by `CALL_TIMEOUT`, unlike every method
    // call above it. These are subscriptions: waiting is what they do, and
    // a signal stream that gave up after five seconds would simply stop
    // reporting that the displays had changed. The bound belongs on calls
    // that owe an answer, not on a stream that owes one only when
    // something happens.
    let conn = zbus::Connection::session().await?;
    let proxy = DisplaydProxy::new(&conn).await?;
    let mut applied = proxy.receive_profile_applied().await?;
    let mut seen = proxy.receive_new_topology_seen().await?;
    let mut revert_pending = proxy.receive_revert_pending().await?;
    let mut revert_resolved = proxy.receive_revert_resolved().await?;

    loop {
        tokio::select! {
            next = revert_pending.next() => {
                let Some(signal) = next else { break };
                let args = signal.args()?;
                let _ = output
                    .send(Message::SignalReceived(SignalKind::RevertPending {
                        seconds: args.seconds,
                    }))
                    .await;
            }
            next = revert_resolved.next() => {
                let Some(signal) = next else { break };
                let args = signal.args()?;
                let _ = output
                    .send(Message::SignalReceived(SignalKind::RevertResolved {
                        reverted: args.reverted,
                    }))
                    .await;
            }
            next = applied.next() => {
                let Some(signal) = next else { break };
                let args = signal.args()?;
                let _ = output
                    .send(Message::SignalReceived(SignalKind::ProfileApplied {
                        id: args.id.clone(),
                        name: args.name.clone(),
                        tier: args.tier.clone(),
                    }))
                    .await;
            }
            next = seen.next() => {
                let Some(signal) = next else { break };
                let args = signal.args()?;
                let _ = output
                    .send(Message::SignalReceived(SignalKind::NewTopologySeen {
                        summary: args.summary.clone(),
                    }))
                    .await;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod call_bounds {
    use super::*;

    /// zbus has no reply timeout on a generated proxy method, so a daemon
    /// that accepts the call and never answers used to leave the screen on
    /// "Applying…" with nothing to click. The bound is what turns that into
    /// an error the user can read.
    #[tokio::test(start_paused = true)]
    async fn a_daemon_that_never_answers_becomes_an_error_not_a_wait() {
        let never = std::future::pending::<zbus::Result<()>>();
        let result = bounded("applying the layout", APPLY_TIMEOUT, never).await;
        let message = result.expect_err("a call that never returns must not resolve to Ok");
        assert!(message.contains("applying the layout"), "{message}");
        assert!(message.contains("isn't answering"), "{message}");
    }

    /// The bound must not truncate work that was going to succeed. displayd
    /// gives the compositor three attempts at 3s to test plus 3s to apply,
    /// so 18s is legitimate; anything at or under that has to survive.
    #[tokio::test(start_paused = true)]
    async fn an_apply_that_takes_the_daemons_full_18_seconds_still_succeeds() {
        let slow = async {
            tokio::time::sleep(Duration::from_secs(18)).await;
            Ok(7u32)
        };
        assert_eq!(bounded("applying the layout", APPLY_TIMEOUT, slow).await, Ok(7));
    }

    /// A query is not an apply and must not inherit its patience: a wedged
    /// read should report back quickly rather than making the screen look
    /// frozen for half a minute.
    #[tokio::test(start_paused = true)]
    async fn a_query_gives_up_far_sooner_than_an_apply() {
        assert!(CALL_TIMEOUT < APPLY_TIMEOUT);
        let never = std::future::pending::<zbus::Result<()>>();
        assert!(bounded("listing display profiles", CALL_TIMEOUT, never).await.is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A verbatim `GetProfile` payload, captured from the daemon running
    /// against its mock backend. Kept literal so a rename on the daemon side
    /// (`heads` vs the serialized `head`, say) fails here instead of
    /// silently producing an editor with no monitors in it.
    const GET_PROFILE_JSON: &str = r#"{
        "id": "eb0c69588ccaf30b4715a073d1d7776b6291232267e08088fa6ccb0e5d81119f",
        "name": "1 display incl. BOE 0x0BC9",
        "last_used": "2026-08-06T01:04:31Z",
        "extra_output_policy": "extend_right",
        "head_swaps": [],
        "head": [{
            "make": "BOE", "model": "0x0BC9", "serial": "",
            "connector_hint": "MOCK-1",
            "x": 0, "y": 0, "width": 1920, "height": 1080,
            "refresh_mhz": 60000, "scale": 1.0,
            "transform": "Normal", "enabled": true
        }]
    }"#;

    fn detail() -> ProfileDetail {
        serde_json::from_str(GET_PROFILE_JSON).expect("GetProfile payload must decode")
    }

    #[test]
    fn decodes_a_real_get_profile_payload() {
        let p = detail();
        assert_eq!(p.name, "1 display incl. BOE 0x0BC9");
        assert_eq!(p.extra_output_policy, "extend_right");
        assert_eq!(p.heads.len(), 1, "the daemon serializes heads as \"head\"");
        assert_eq!(p.heads[0].connector_hint, "MOCK-1");
        assert_eq!(p.heads[0].refresh_mhz, 60000);
    }

    fn head(connector: &str, make: &str, model: &str) -> HeadDetail {
        let mut h = detail().heads.remove(0);
        h.connector_hint = connector.to_string();
        h.make = make.to_string();
        h.model = model.to_string();
        h
    }

    /// Two monitors that report the same EDID are told apart by their
    /// connector — the one thing that differs, and the one printed on
    /// the socket the cable goes into.
    #[test]
    fn identical_monitors_are_told_apart_on_the_canvas() {
        let heads = vec![
            head("DP-11", "AOC", "2475W"),
            head("DP-12", "AOC", "2475W"),
            head("HDMI-A-1", "DELL", "U2720Q"),
        ];
        assert_eq!(canvas_label(&heads[0], &heads), "AOC 2475W (DP-11)");
        assert_eq!(canvas_label(&heads[1], &heads), "AOC 2475W (DP-12)");
        assert_eq!(
            canvas_label(&heads[2], &heads),
            "DELL U2720Q",
            "a name nothing shares is left alone — the tiles are small"
        );
    }

    /// One monitor is never ambiguous with itself.
    #[test]
    fn a_single_monitor_keeps_its_plain_name() {
        let heads = vec![head("DP-4", "DELL", "U2720Q")];
        assert_eq!(canvas_label(&heads[0], &heads), "DELL U2720Q");
    }

    /// Two built-in panels would both be "Built-in display" — rare, but
    /// the rule is about the name being shared, not about EDIDs.
    #[test]
    fn two_built_in_panels_are_told_apart_too() {
        let heads = vec![head("eDP-1", "BOE", "0x0BC9"), head("eDP-2", "AUO", "0x1234")];
        assert_eq!(canvas_label(&heads[0], &heads), "Built-in display (eDP-1)");
    }

    #[test]
    fn internal_panels_get_a_generic_friendly_name() {
        // Their EDID model is a part number, so it's no more useful than
        // the connector it replaces.
        assert_eq!(head("eDP-2", "BOE", "0x0BC9").display_name(), "Built-in display");
        assert_eq!(head("LVDS-1", "", "").display_name(), "Built-in display");
        assert_eq!(head("DSI-1", "", "").display_name(), "Built-in display");
    }

    #[test]
    fn canvas_geometry_is_logical_so_scaled_panels_draw_at_their_real_size() {
        // The real pair that exposed this: a heavily-scaled 2560x1600 laptop
        // panel next to an unscaled 2560x1440 external. In pixels they look
        // near-identical in width, with the laptop *taller*; in layout space
        // the external is much the larger of the two, which is what every
        // other display arranger draws.
        let mut laptop = head("eDP-2", "BOE", "0x0BC9");
        laptop.width = 2560;
        laptop.height = 1600;
        // 5/3 as it arrives over wl_fixed. The compositor lays this panel
        // out as 1536x960; the drawn size must not come out under that, or
        // a monitor snapped to its edge lands inside it.
        laptop.scale = 1.66796875;
        let (lw, lh) = laptop.logical_size();
        assert!(
            (1536..=1539).contains(&lw),
            "width {lw} should cover the compositor's 1536 without overshooting"
        );
        assert!((960..=963).contains(&lh), "height {lh} should cover 960");

        let mut external = head("DP-3", "GWD", "ARZOPA");
        external.width = 2560;
        external.height = 1440;
        external.scale = 1.0;
        assert_eq!(external.logical_size(), (2560, 1440));

        let (lw, _) = laptop.logical_size();
        let (ew, _) = external.logical_size();
        assert!(ew > lw, "the external covers more layout space than the panel");
    }

    #[test]
    fn canvas_geometry_swaps_axes_for_a_quarter_turn() {
        let mut portrait = head("DP-4", "DELL", "U2720Q");
        portrait.width = 2560;
        portrait.height = 1440;
        portrait.scale = 1.0;
        portrait.transform = "Rotate90".to_string();
        assert_eq!(portrait.logical_size(), (1440, 2560));
    }

    #[test]
    fn canvas_geometry_survives_a_zero_scale() {
        let mut h = head("DP-4", "DELL", "U2720Q");
        h.width = 1920;
        h.height = 1080;
        h.scale = 0.0;
        assert_eq!(h.logical_size(), (1920, 1080));
    }

    #[test]
    fn external_monitors_are_named_from_their_edid() {
        assert_eq!(head("DP-4", "DELL", "U2720Q").display_name(), "DELL U2720Q");
        assert_eq!(head("DP-4", "", "U2720Q").display_name(), "U2720Q");
    }

    #[test]
    fn a_blank_edid_falls_back_to_the_connector() {
        // Blank EDID is normal, not a bug — showing an empty name would be
        // worse than showing DP-4.
        assert_eq!(head("DP-4", "", "").display_name(), "DP-4");
        assert_eq!(head("HDMI-A-1", "SOMEMAKE", "").display_name(), "HDMI-A-1");
    }

    #[test]
    fn the_picker_keeps_the_connector_to_disambiguate_identical_monitors() {
        // Two of the same model must not render as two identical entries.
        let choice = HeadChoice::new(&head("DP-4", "DELL", "U2720Q"));
        assert_eq!(choice.to_string(), "DELL U2720Q (DP-4)");
        assert_eq!(choice.hint, "DP-4", "the connector stays the identity");

        // No point repeating it when the name already *is* the connector.
        let bare = HeadChoice::new(&head("DP-4", "", ""));
        assert_eq!(bare.to_string(), "DP-4");
    }

    #[test]
    fn refresh_rates_render_without_losing_fractional_values() {
        // 59.94Hz is a real mode; rounding it to 60 would offer the user a
        // rate their monitor doesn't have.
        assert_eq!(RefreshOption { mhz: 165000 }.to_string(), "165 Hz");
        assert_eq!(RefreshOption { mhz: 59940 }.to_string(), "59.94 Hz");
    }

    /// Every entry in the dropdown has to be one the panel can take, or
    /// picking it hands the compositor something it will swap out.
    #[test]
    fn the_scale_dropdown_only_offers_achievable_scales() {
        let (options, _) = scale_choices(2560, 1600, "100");
        assert!(!options.is_empty());
        for c in &options {
            let s = c.percent / 100.0;
            assert!(
                (nearest_valid_scale(2560, 1600, s) - s).abs() < 1e-9,
                "{}% isn't achievable on a 2560x1600 panel",
                c.percent
            );
        }
    }

    /// 150% doesn't exist on this panel — 180 doesn't divide 38400 — so the
    /// preset has to show up as the 160% it will really be.
    #[test]
    fn a_preset_the_panel_cannot_take_is_offered_as_the_one_it_becomes() {
        let (options, _) = scale_choices(2560, 1600, "100");
        assert!(
            options.iter().any(|c| (c.percent - 160.0).abs() < 0.5),
            "expected a 160% entry, got {:?}",
            options.iter().map(|c| c.percent).collect::<Vec<_>>()
        );
        assert!(
            !options.iter().any(|c| (c.percent - 150.0).abs() < 0.5),
            "150% is not achievable here and must not be offered"
        );
    }

    /// A profile written before the 120ths rule was understood holds
    /// 150.23%. It must come back as the 160% the compositor will actually
    /// run, not as a selectable 150%.
    #[test]
    fn a_stored_scale_the_panel_cannot_take_is_shown_as_what_will_run() {
        let (options, current) = scale_choices(2560, 1600, "150.2347");
        let current = current.expect("a parseable field should select something");
        assert!((current - 160.0).abs() < 0.5, "selected {current}%, expected 160%");
        assert_eq!(
            options.iter().filter(|c| (c.percent - current).abs() < 0.5).count(),
            1,
            "the selected value must appear exactly once"
        );
    }

    /// Presets collapsing onto the same achievable scale must not produce
    /// two identical-looking rows.
    #[test]
    fn snapped_presets_are_not_offered_twice() {
        let (options, _) = scale_choices(2560, 1600, "100");
        let mut labels: Vec<String> = options.iter().map(|c| c.to_string()).collect();
        labels.sort();
        let before = labels.len();
        labels.dedup();
        assert_eq!(before, labels.len(), "duplicate entries: {labels:?}");
    }

    #[test]
    fn a_mid_edit_scale_field_selects_nothing_rather_than_panicking() {
        let (options, current) = scale_choices(2560, 1600, "");
        assert!(current.is_none());
        assert!(!options.is_empty(), "the steps are still offered");
    }

    #[test]
    fn resolutions_mark_the_preferred_mode() {
        let r = ResolutionOption { width: 2560, height: 1600, preferred: true };
        assert_eq!(r.to_string(), "2560 × 1600 (recommended)");
        let plain = ResolutionOption { width: 1920, height: 1080, preferred: false };
        assert_eq!(plain.to_string(), "1920 × 1080");
    }

    #[test]
    fn editor_seeds_its_fields_from_the_first_head() {
        let editor = LayoutEditor::new(detail());
        assert_eq!(editor.selected.as_deref(), Some("MOCK-1"));
        assert_eq!(editor.field_width, "1920");
        // mHz is shown as Hz, and scale as a percentage.
        assert_eq!(editor.field_refresh, "60");
        assert_eq!(editor.field_scale.trim().parse::<f64>().unwrap(), 100.0);
    }

    /// Seeding the scale field used to round it to a whole percent, so
    /// touching an unrelated field wrote a scale the panel can't take back
    /// onto the head — and the compositor would silently run something else.
    #[test]
    fn editing_another_field_leaves_an_off_round_scale_achievable() {
        let mut editor = LayoutEditor::new(detail());
        let head = &mut editor.profile.heads[0];
        head.width = 2560;
        head.height = 1600;
        // What 175% actually becomes on this panel: 5/3, an unroundable
        // 166.67%.
        head.scale = nearest_valid_scale(2560, 1600, 1.75);
        let expected = head.scale;
        let (x, y, w, h, r, s, t) = fields_from_head(&editor.profile.heads[0].clone());
        editor.field_x = x;
        editor.field_y = y;
        editor.field_width = w;
        editor.field_height = h;
        editor.field_refresh = r;
        editor.field_scale = s;
        editor.field_transform = t;

        // Nudge the position, as dragging on the canvas does.
        editor.field_x = "10".to_string();
        commit_selected_head(&mut editor);

        let got = editor.profile.heads[0].scale;
        assert!(
            (got - expected).abs() < 1e-6,
            "committed {got}, which the compositor would replace with {expected}"
        );
        assert!(
            (2560.0 / got).fract().abs() < 1e-9,
            "{got} doesn't divide 2560 cleanly"
        );
    }

    #[test]
    fn a_mid_edit_field_leaves_the_head_untouched() {
        let mut editor = LayoutEditor::new(detail());
        // "-" is what you have after typing the first character of "-100".
        editor.field_x = "-".to_string();
        commit_selected_head(&mut editor);
        assert_eq!(editor.profile.heads[0].x, 0, "a half-typed value must not commit");
    }

    #[test]
    fn zero_or_negative_dimensions_are_rejected() {
        let mut editor = LayoutEditor::new(detail());
        editor.field_width = "0".to_string();
        commit_selected_head(&mut editor);
        assert_eq!(
            editor.profile.heads[0].width, 1920,
            "a zero width would be applied to a real monitor"
        );

        editor.field_width = "1920".to_string();
        editor.field_scale = "0".to_string();
        commit_selected_head(&mut editor);
        assert_eq!(editor.profile.heads[0].scale, 1.0);
    }

    #[test]
    fn valid_fields_commit_with_unit_conversion() {
        let mut editor = LayoutEditor::new(detail());
        editor.field_x = "-1920".to_string();
        editor.field_refresh = "144".to_string();
        editor.field_scale = "150".to_string();
        commit_selected_head(&mut editor);

        let head = &editor.profile.heads[0];
        assert_eq!(head.x, -1920, "negative positions are valid — monitor to the left");
        assert_eq!(head.refresh_mhz, 144000);
        assert_eq!(head.scale, 1.5);
    }

    fn live(connector: &str, description: &str) -> LiveHead {
        LiveHead { connector: connector.to_string(), description: description.to_string() }
    }

    fn imported(selector: &str, mode: Option<&str>, position: Option<&str>) -> ImportedMonitor {
        ImportedMonitor {
            selector: selector.to_string(),
            disabled: false,
            mode: mode.map(str::to_string),
            position: position.map(str::to_string),
            scale: Some("1.6".to_string()),
            target: None,
            checked: false,
        }
    }

    /// Hyprland accepts either form, so both have to resolve — against what
    /// is really connected, never against a guess.
    #[test]
    fn a_selector_resolves_by_description_or_connector() {
        let heads = vec![live("eDP-2", "BOE 0x0BC9"), live("DP-3", "GWD ARZOPA")];

        let by_desc = imported("desc:BOE 0x0BC9", None, None);
        assert_eq!(by_desc.resolve(&heads).as_deref(), Some("eDP-2"));

        let by_connector = imported("DP-3", None, None);
        assert_eq!(by_connector.resolve(&heads).as_deref(), Some("DP-3"));
    }

    /// A rule for a display that isn't plugged in must resolve to nothing.
    /// This is the whole reason import is limited: without the panel
    /// present there is no make/model/serial to key a profile on.
    #[test]
    fn a_selector_for_a_disconnected_display_resolves_to_nothing() {
        let heads = vec![live("eDP-2", "BOE 0x0BC9")];
        assert_eq!(imported("desc:DELL U2720Q", None, None).resolve(&heads), None);
        assert_eq!(imported("HDMI-A-1", None, None).resolve(&heads), None);
        // ...and with nothing connected at all, nothing is importable.
        assert_eq!(imported("desc:BOE 0x0BC9", None, None).resolve(&[]), None);
    }

    #[test]
    fn a_full_rule_yields_geometry() {
        let m = imported("desc:BOE 0x0BC9", Some("2560x1600@165.00"), Some("0x0"));
        let g = m.geometry().expect("a complete rule is importable");
        assert_eq!((g.width, g.height), (2560, 1600));
        assert_eq!(g.refresh_mhz, 165000, "Hz are stored as millihertz");
        assert_eq!((g.x, g.y), (0, 0));
        assert_eq!(g.scale, 1.6);
    }

    /// `preferred`/`auto` are Hyprland deciding for itself. There are no
    /// numbers to import, and inventing them would move someone's display.
    #[test]
    fn a_rule_that_defers_to_hyprland_has_nothing_to_import() {
        assert!(imported("eDP-2", Some("preferred"), Some("0x0")).geometry().is_none());
        assert!(imported("eDP-2", Some("2560x1600@165.00"), Some("auto")).geometry().is_none());
        assert!(imported("eDP-2", None, Some("0x0")).geometry().is_none());
    }

    /// A negative position is ordinary — a monitor to the left of the origin.
    #[test]
    fn negative_positions_parse() {
        let m = imported("eDP-2", Some("1920x1080@60.00"), Some("-1920x0"));
        let g = m.geometry().unwrap();
        assert_eq!((g.x, g.y), (-1920, 0));
    }

    /// Scale is the one field with a safe default: Hyprland's own is 1.
    #[test]
    fn a_missing_scale_defaults_to_one() {
        let mut m = imported("eDP-2", Some("1920x1080@60.00"), Some("0x0"));
        m.scale = None;
        assert_eq!(m.geometry().unwrap().scale, 1.0);
    }

    /// The other half of the `GetProfile` contract.
    ///
    /// [`ProfileDetail`] and [`HeadDetail`] are hand-written mirrors of
    /// `hyprforge_displayd::profile::Profile` and `HeadRecord`, because
    /// depending on the daemon crate would pull Wayland into this GUI's
    /// build. Two declarations of one wire format, in two crates, with
    /// nothing between them that the compiler can see.
    ///
    /// So the daemon has a test asserting it still *sends* these names —
    /// `get_profile_names_every_field_the_displays_screen_reads` in
    /// `hyprforge-displayd/src/dbus.rs` — and this one asserts we can
    /// still *read* them. The JSON below is what that daemon-side test
    /// builds. Renaming a field on either side fails exactly one of the
    /// two, which is how you find out which side moved.
    #[test]
    fn a_profile_detail_parses_the_json_the_daemon_actually_sends() {
        let json = r#"{
            "id": "abc123",
            "name": "Desk",
            "last_used": "2026-09-12T10:00:00Z",
            "extra_output_policy": "extend_right",
            "head_swaps": [["DP-1", "DP-2"]],
            "head": [{
                "make": "Dell",
                "model": "U2720Q",
                "serial": "ABC",
                "connector_hint": "DP-1",
                "x": 0,
                "y": 0,
                "width": 3840,
                "height": 2160,
                "refresh_mhz": 59997,
                "scale": 1.5,
                "transform": "Normal",
                "enabled": true
            }]
        }"#;

        let detail: ProfileDetail =
            serde_json::from_str(json).expect("GetProfile's JSON parses into ProfileDetail");

        assert_eq!(detail.id, "abc123");
        assert_eq!(detail.name, "Desk");
        assert_eq!(detail.extra_output_policy, "extend_right");
        // `head`, not `heads`: the daemon renames the field, and this is
        // the rename actually being exercised rather than assumed.
        assert_eq!(detail.heads.len(), 1);

        let head = &detail.heads[0];
        assert_eq!(head.connector_hint, "DP-1");
        assert_eq!((head.width, head.height), (3840, 2160));
        assert_eq!(head.refresh_mhz, 59997);
        assert_eq!(head.scale, 1.5);
        assert_eq!(head.transform, "Normal");
        assert!(head.enabled);
    }

    /// `last_used` is sent and this screen does not read it. That is
    /// fine — serde ignores unknown fields — but it means the parse
    /// succeeding is not by itself proof the field names line up, which
    /// is why the test above asserts values rather than just `is_ok`.
    #[test]
    fn a_field_the_screen_does_not_use_does_not_break_the_parse() {
        let json = r#"{
            "id": "x", "name": "n", "last_used": "whenever",
            "extra_output_policy": "mirror", "head_swaps": [],
            "head": [], "a_field_added_later": 5
        }"#;
        let detail: ProfileDetail = serde_json::from_str(json).expect("unknown fields are ignored");
        assert_eq!(detail.extra_output_policy, "mirror");
    }

    /// The two fields the `desc:` selector resolution needs out of
    /// `GetCurrentLayout`. Paired with
    /// `get_current_layout_names_the_two_fields_that_resolve_a_monitor_selector`
    /// on the daemon side.
    #[test]
    fn a_live_head_parses_the_layout_json_the_daemon_actually_sends() {
        let json = r#"[{
            "connector": "DP-1",
            "identity": {"make": "Dell", "model": "U2720Q", "serial": "ABC"},
            "description": "Dell U2720Q (DP-1)",
            "modes": [], "current_mode": null,
            "position": [0, 0], "transform": "Normal",
            "scale": 1.0, "enabled": true
        }]"#;
        let live: Vec<LiveHead> =
            serde_json::from_str(json).expect("GetCurrentLayout's JSON parses into LiveHead");
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].connector, "DP-1");
        assert_eq!(live[0].description, "Dell U2720Q (DP-1)");
    }

}

//! Wallpaper, colour temperature and idle behaviour — the Hypr ecosystem
//! daemons.
//!
//! Three tabs rather than three screens: they are one family (separate
//! daemons, hyprlang configs, joined by a `source =` line) and a user
//! thinks of them as "how my desktop behaves when I'm not touching it".
//!
//! The one thing this screen must not smooth over is that a change
//! reaches each daemon differently. hyprpaper and hyprsunset take it
//! live; **hypridle has no IPC at all** and does nothing until it
//! restarts. Every save reports which of those happened rather than
//! saying "Saved." at all three.

use std::path::Path;
use hyprforge_tray::Prefs as TrayPrefs;
use hyprforge_ui::theme::{spacing, FontScale};
use hyprforge_ui::widgets::{
    danger_button, divider, meta_text, primary_button, scaled_text, secondary_button, section,
};
use crate::module::SettingsModule;
use crate::modules::setting_rows::labelled;
use hyprforge_ecosystem::apply::{self, Applied};
use hyprforge_ecosystem::{idle, import, portal, sunset, wallpaper};
use iced::widget::{checkbox, column, container, pick_list, row, scrollable, text_input};
use iced::{Element, Length, Task};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Wallpaper,
    NightLight,
    Idle,
    ScreenSharing,
}

impl Tab {
    const ALL: [Tab; 4] =
        [Tab::Wallpaper, Tab::NightLight, Tab::Idle, Tab::ScreenSharing];

    fn label(self) -> &'static str {
        match self {
            Tab::Wallpaper => "Wallpaper",
            Tab::NightLight => "Night light",
            Tab::Idle => "Idle",
            Tab::ScreenSharing => "Screen sharing",
        }
    }
}

/// Which stored list a message is about. The three tabs each hold a list
/// of blocks and every list needs the same four operations, so they share
/// the messages rather than repeating them.
impl Field {
    /// Which tab's list this field belongs to.
    ///
    /// Needed because drafts from all three tabs share one map: without
    /// it, applying would save only the tab that happened to be open, and
    /// removing a row would drop the wrong rows' drafts.
    fn tab(self) -> Tab {
        match self {
            Field::Monitor
            | Field::Path
            | Field::FitMode
            | Field::Timeout
            | Field::RandomOrder
            | Field::Recursive => Tab::Wallpaper,
            Field::Time | Field::Temperature | Field::Gamma | Field::Identity => Tab::NightLight,
            Field::IdleTimeout
            | Field::OnTimeout
            | Field::OnResume
            | Field::IgnoreInhibit => Tab::Idle,
            Field::MaxFps | Field::PickerBinary | Field::AllowToken | Field::ForceShm => {
                Tab::ScreenSharing
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Field {
    Monitor,
    Path,
    FitMode,
    Timeout,
    RandomOrder,
    Recursive,
    Time,
    Temperature,
    Gamma,
    Identity,
    IdleTimeout,
    OnTimeout,
    OnResume,
    IgnoreInhibit,
    MaxFps,
    PickerBinary,
    AllowToken,
    ForceShm,
}

/// One thing found in a daemon's own config file, offered for adoption.
///
/// The three tabs hold different shapes, so the item is an enum rather
/// than three parallel review types — the review screen, the checkbox
/// handling and the retire bookkeeping are identical for all three and
/// only the label differs.
#[derive(Debug, Clone)]
pub enum AdoptItem {
    Wallpaper(wallpaper::Entry),
    Portal(portal::Settings),
    Profile(sunset::Profile),
    Listener(idle::Listener),
    IdleGeneral(idle::General),
}

impl AdoptItem {
    /// What the user reads in the review list. Says what the setting
    /// *does*, not which struct it is: "after 30 min: systemctl suspend"
    /// is checkable against their own file, `Listener { .. }` is not.
    fn label(&self) -> String {
        match self {
            AdoptItem::Wallpaper(e) => {
                let where_ = if e.monitor.trim().is_empty() {
                    "every display".to_string()
                } else {
                    e.monitor.trim().to_string()
                };
                format!("{where_}: {}", e.path)
            }
            AdoptItem::Profile(p) => {
                format!("from {}: {}K", p.time, p.temperature)
            }
            AdoptItem::Listener(l) => {
                let minutes = l.timeout / 60;
                let when = if minutes >= 1 {
                    format!("after {minutes} min")
                } else {
                    format!("after {}s", l.timeout)
                };
                match (l.on_timeout.trim(), l.on_resume.trim()) {
                    ("", "") => when,
                    (t, "") => format!("{when}: {t}"),
                    (t, r) => format!("{when}: {t} — on wake: {r}"),
                }
            }
            AdoptItem::IdleGeneral(_) => "Session commands (lock, sleep, wake)".to_string(),
            AdoptItem::Portal(p) => {
                let mut said = Vec::new();
                if let Some(fps) = p.max_fps {
                    said.push(if fps == 0 {
                        "no frame rate limit".to_string()
                    } else {
                        format!("up to {fps} fps")
                    });
                }
                if p.allow_token_by_default {
                    said.push("don't re-ask each time".to_string());
                }
                if p.force_shm {
                    said.push("use SHM instead of DMA-BUF".to_string());
                }
                if !p.custom_picker_binary.trim().is_empty() {
                    said.push(format!("picker: {}", p.custom_picker_binary.trim()));
                }
                if p.cursor_mode != portal::CursorMode::Default {
                    said.push(p.cursor_mode.label().to_lowercase());
                }
                if said.is_empty() {
                    "Screen sharing (nothing set)".to_string()
                } else {
                    format!("Screen sharing: {}", said.join(", "))
                }
            }
        }
    }
}

/// One row of the review list.
#[derive(Debug, Clone)]
pub struct AdoptCandidate {
    pub item: AdoptItem,
    pub checked: bool,
    /// Keys the importer does not model. A candidate with any is offered
    /// but never pre-checked, and its original lines are never retired —
    /// adopting it would narrow the setting and then delete the evidence.
    pub dropped: Vec<String>,
    pub line: usize,
    pub end_line: usize,
}

impl AdoptCandidate {
    fn is_faithful(&self) -> bool {
        self.dropped.is_empty()
    }
}

/// What one "import from the daemon's own config" run found.
#[derive(Debug, Clone)]
pub struct AdoptReview {
    pub tab: Tab,
    pub path: PathBuf,
    pub candidates: Vec<AdoptCandidate>,
    /// Lines the parser could not read. Shown, never swallowed: a file
    /// that will not parse is not a file with nothing in it.
    pub problems: Vec<(usize, String)>,
}

#[derive(Debug, Clone)]
pub enum Message {
    TabSelected(Tab),
    Loaded(Loaded),
    // Lists
    WallpaperAdded,
    WallpaperRemoved(usize),
    WallpaperChanged(usize, Field, String),
    WallpaperToggled(usize, Field, bool),
    ProfileAdded,
    ProfileRemoved(usize),
    ProfileChanged(usize, Field, String),
    ProfileToggled(usize, Field, bool),
    ListenerAdded,
    ListenerRemoved(usize),
    ListenerChanged(usize, Field, String),
    ListenerToggled(usize, Field, bool),
    // General
    GeneralCommandChanged(&'static str, String),
    InhibitToggled(&'static str, bool),
    SplashToggled(bool),
    Commit,
    RestartIdle,
    Applied(Tab, Result<Applied, String>),
    PortalChanged(Field, String),
    PortalToggled(Field, bool),
    PortalCursorMode(portal::CursorMode),
    RestartPortal,
    AdoptOpen,
    AdoptToggle(usize, bool),
    AdoptConfirm,
    AdoptCancel,
    /// Whether `hyprforge-trayd` should show the night light icon. Same
    /// shape as `network::Message::TrayToggled` — only reachable from a
    /// loaded [`TrayPrefs`] (see `night_light_tray_row`), guarded again in
    /// `update`. A separate variant from `KeepAwakeTrayToggled` because
    /// this screen owns two of the four tray fields, not one.
    NightLightTrayToggled(bool),
    /// Whether `hyprforge-trayd` should show the keep-awake icon. See
    /// `NightLightTrayToggled`.
    KeepAwakeTrayToggled(bool),
}

/// Everything read from the system when the screen opens.
#[derive(Debug, Clone, Default)]
pub struct Loaded {
    pub images: Vec<String>,
    pub monitors: Vec<String>,
}

pub struct DesktopModule {
    tab: Tab,
    wallpapers: wallpaper::Settings,
    sunset: sunset::Settings,
    idle: idle::Settings,
    portal: portal::Settings,
    /// Text mid-edit, keyed by `(tab, index, field)`. Held rather than
    /// committed per keystroke: every save rewrites a config file and
    /// pokes a daemon.
    drafts: BTreeMap<(usize, Field), String>,
    images: Vec<String>,
    monitors: Vec<String>,
    error: Option<String>,
    status: Option<String>,
    /// Set while the idle config on disk is ahead of the running daemon.
    idle_needs_restart: bool,
    /// Set while the screen-sharing config on disk is ahead of the
    /// running portal.
    portal_needs_restart: bool,
    store_unreadable: Option<String>,
    /// The review shown after "Import from …", before anything is
    /// written. `None` when no import is in progress.
    adopt: Option<AdoptReview>,
    /// Lines to comment out once the adopted settings have actually
    /// reached the daemon.
    ///
    /// Held rather than done at confirm time, and consumed only on a
    /// successful `Applied`. The ordering is the whole safety argument:
    /// the app's own store is written, the generated file produced and
    /// the `source =` line installed *first*, so a crash anywhere in
    /// between leaves the user with a config that is duplicated but
    /// working, rather than one that is retired and missing.
    pending_retire: Option<(PathBuf, Vec<(usize, usize)>)>,
    /// The on-disk `tray.toml`, loaded once at construction. Same shape
    /// and same reasoning as `network::NetworkModule::tray_prefs`: kept as
    /// the whole [`TrayPrefs`] (not just `night_light`/`keep_awake`) so a
    /// toggle here can write back `network` and `bluetooth` unchanged, and
    /// `Err` (a file that exists and would not parse) renders no checkbox
    /// at all rather than guessing a value.
    tray_prefs: Result<TrayPrefs, String>,
}

impl DesktopModule {
    pub fn new() -> (Self, Task<Message>) {
        // A failure here must never look like "you've configured
        // nothing": that reading is what turns one bad parse into a wiped
        // store on the next save.
        let mut unreadable = Vec::new();
        let wallpapers = load_or_note(&wallpaper_toml(), &mut unreadable);
        let sunset = load_or_note(&sunset_toml(), &mut unreadable);
        let idle = load_or_note(&idle_toml(), &mut unreadable);
        let portal = load_or_note(&portal_toml(), &mut unreadable);
        // A missing tray.toml is first run and loads as defaults; a file
        // that exists and will not parse is reported in this screen's own
        // error banner, rather than silently treated as "no tray icons" —
        // the rule in CLAUDE.md about never collapsing "could not read"
        // into "nothing configured".
        let (tray_prefs, tray_error) = match hyprforge_tray::prefs::load() {
            Ok(prefs) => (Ok(prefs), None),
            Err(e) => (Err(e.to_string()), Some(e.to_string())),
        };
        (
            DesktopModule {
                tab: Tab::Wallpaper,
                wallpapers,
                sunset,
                idle,
                portal,
                drafts: BTreeMap::new(),
                images: Vec::new(),
                monitors: Vec::new(),
                error: tray_error,
                status: None,
                idle_needs_restart: false,
                portal_needs_restart: false,
                adopt: None,
                pending_retire: None,
                store_unreadable: (!unreadable.is_empty()).then(|| unreadable.join("; ")),
                tray_prefs,
            },
            Task::perform(load_system(), Message::Loaded),
        )
    }

    fn draft(&self, index: usize, field: Field, current: impl std::fmt::Display) -> String {
        self.drafts
            .get(&(index, field))
            .cloned()
            .unwrap_or_else(|| current.to_string())
    }

    /// Writes the tab's canonical TOML, then applies it.
    ///
    /// Nothing irreversible runs unless the write returned `Ok` — the
    /// ordering rule that exists because doing it the other way round
    /// cost a real user 37 hand-written binds.
    /// Opens the screen on a particular tab.
    ///
    /// For `--screen night-light` and the tray icons that use it: a
    /// setting that lives on a tab is not reachable by naming the screen
    /// alone, and landing someone on Wallpaper when they asked for night
    /// light is the same miss as not deep-linking at all.
    pub fn open_on(&mut self, tab: Tab) {
        self.tab = tab;
    }

    fn save(&mut self, tab: Tab) -> Task<Message> {
        if let Some(reason) = &self.store_unreadable {
            self.error = Some(format!(
                "Not saving — a settings file couldn't be read, and overwriting \
                 it would lose whatever is in it. ({reason})"
            ));
            return Task::none();
        }
        let hypr = hyprforge_core::paths::hypr_config_dir();
        let generated_dir = hyprforge_core::paths::hypr_hyprforge_dir();
        let result = match tab {
            Tab::Wallpaper => hyprforge_ecosystem::storage::save(&wallpaper_toml(), &self.wallpapers),
            Tab::NightLight => hyprforge_ecosystem::storage::save(&sunset_toml(), &self.sunset),
            Tab::Idle => hyprforge_ecosystem::storage::save(&idle_toml(), &self.idle),
            Tab::ScreenSharing => {
                hyprforge_ecosystem::storage::save(&portal_toml(), &self.portal)
            }
        };
        // The wallpaper is also the auth screens' background, so saving
        // it has to reach them too. Doing this only in Appearance is how
        // the lock screen ends up showing last week's wallpaper.
        if result.is_ok() && tab == Tab::Wallpaper {
            crate::look::republish();
        }
        if let Err(e) = result {
            self.error = Some(e.to_string());
            return Task::none();
        }
        self.status = None;
        match tab {
            Tab::Wallpaper => {
                let settings = self.wallpapers.clone();
                Task::perform(
                    apply_wallpapers(generated_dir.join("wallpaper.conf"), hypr.join("hyprpaper.conf"), settings),
                    move |r| Message::Applied(tab, r),
                )
            }
            Tab::NightLight => {
                let settings = self.sunset.clone();
                Task::perform(
                    apply_sunset(generated_dir.join("sunset.conf"), hypr.join("hyprsunset.conf"), settings),
                    move |r| Message::Applied(tab, r),
                )
            }
            Tab::Idle => {
                let settings = self.idle.clone();
                Task::perform(
                    apply_idle(generated_dir.join("idle.conf"), hypr.join("hypridle.conf"), settings),
                    move |r| Message::Applied(tab, r),
                )
            }
            Tab::ScreenSharing => {
                let settings = self.portal.clone();
                Task::perform(
                    apply_portal(generated_dir.join("xdph.conf"), hypr.join("xdph.conf"), settings),
                    move |r| Message::Applied(tab, r),
                )
            }
        }
    }

    /// Which of the daemons' own config files this tab adopts from.
    fn daemon_config_path(tab: Tab) -> PathBuf {
        let hypr = hyprforge_core::paths::hypr_config_dir();
        match tab {
            Tab::Wallpaper => hypr.join("hyprpaper.conf"),
            Tab::NightLight => hypr.join("hyprsunset.conf"),
            Tab::Idle => hypr.join("hypridle.conf"),
            Tab::ScreenSharing => hypr.join("xdph.conf"),
        }
    }

    /// Reads the current tab's daemon config and builds the review.
    ///
    /// Everything Hyprforge itself generated is excluded, by ignoring the
    /// `source =` line's contents entirely — the parser reports sourced
    /// paths rather than following them, so what comes back is only what
    /// the user wrote. Following them would re-import the app's own
    /// output, the same self-import trap the Lua flows avoid by skipping
    /// `hypr_hyprforge_dir()`.
    fn read_daemon_config(&self) -> AdoptReview {
        let tab = self.tab;
        let path = Self::daemon_config_path(tab);
        let contents = std::fs::read_to_string(&path).unwrap_or_default();
        let document = hyprforge_core::hyprlang::parse(&contents);

        let mut candidates = Vec::new();
        let mut problems;
        match tab {
            Tab::Wallpaper => {
                let review = import::wallpaper(&document);
                problems = review.problems;
                for found in review.items {
                    candidates.push(AdoptCandidate {
                        checked: found.is_faithful(),
                        item: AdoptItem::Wallpaper(found.value),
                        dropped: found.dropped,
                        line: found.line,
                        end_line: found.end_line,
                    });
                }
            }
            Tab::ScreenSharing => {
                let review = import::portal(&document);
                problems = review.problems;
                for found in review.items {
                    candidates.push(AdoptCandidate {
                        checked: found.is_faithful(),
                        item: AdoptItem::Portal(found.value),
                        dropped: found.dropped,
                        line: found.line,
                        end_line: found.end_line,
                    });
                }
            }
            Tab::NightLight => {
                let (_, review) = import::sunset(&document);
                problems = review.problems;
                for found in review.items {
                    candidates.push(AdoptCandidate {
                        checked: found.is_faithful(),
                        item: AdoptItem::Profile(found.value),
                        dropped: found.dropped,
                        line: found.line,
                        end_line: found.end_line,
                    });
                }
            }
            Tab::Idle => {
                let (general, review) = import::idle(&document);
                problems = review.problems;
                if let Some(found) = general {
                    candidates.push(AdoptCandidate {
                        checked: found.is_faithful(),
                        item: AdoptItem::IdleGeneral(found.value),
                        dropped: found.dropped,
                        line: found.line,
                        end_line: found.end_line,
                    });
                }
                for found in review.items {
                    candidates.push(AdoptCandidate {
                        checked: found.is_faithful(),
                        item: AdoptItem::Listener(found.value),
                        dropped: found.dropped,
                        line: found.line,
                        end_line: found.end_line,
                    });
                }
            }
        }
        problems.sort_by_key(|(line, _)| *line);
        AdoptReview { tab, path, candidates, problems }
    }

    /// Merges the ticked candidates into the store and saves.
    ///
    /// Appends rather than replaces. The user may already have set
    /// something up in the app before importing, and discarding it
    /// because they pressed Import would be the app deciding their work
    /// was not worth keeping. The exception is the idle `general` block,
    /// which is a single record rather than a list.
    fn adopt_confirmed(&mut self) -> Task<Message> {
        let Some(review) = self.adopt.take() else {
            return Task::none();
        };
        let tab = review.tab;
        let mut ranges = Vec::new();
        let mut adopted = 0;

        for candidate in review.candidates.iter().filter(|c| c.checked) {
            adopted += 1;
            // Only a candidate that round-trips exactly may have its
            // original lines stood down. One with a key this app cannot
            // model is still adoptable — narrower, but the user asked —
            // and its original stays live so nothing is lost.
            if candidate.is_faithful() {
                ranges.push((candidate.line, candidate.end_line));
            }
            match candidate.item.clone() {
                AdoptItem::Wallpaper(entry) => self.wallpapers.entries.push(entry),
                AdoptItem::Profile(profile) => self.sunset.profiles.push(profile),
                AdoptItem::Listener(listener) => self.idle.listeners.push(listener),
                AdoptItem::IdleGeneral(general) => self.idle.general = general,
                // A single settings block, not a list: adopting replaces
                // rather than appends, because there is only ever one.
                AdoptItem::Portal(settings) => self.portal = settings,
            }
        }

        if adopted == 0 {
            self.status = Some("Nothing was ticked, so nothing changed.".into());
            return Task::none();
        }
        self.pending_retire = (!ranges.is_empty()).then_some((review.path, ranges));
        self.save(tab)
    }

    /// Comments out the lines of everything just adopted.
    ///
    /// Refuses unless the `.hyprforge.bak` copy of the file exists and
    /// reads back — the backup is the user's way out, and standing their
    /// settings down without one is not a trade this app gets to make on
    /// their behalf. A failure here is reported and leaves the config
    /// duplicated, which is noisy but working.
    fn retire_adopted(&mut self, path: &Path, ranges: &[(usize, usize)]) {
        let backup = path.with_extension(format!(
            "{}.hyprforge.bak",
            path.extension().and_then(|e| e.to_str()).unwrap_or("conf")
        ));
        if std::fs::read_to_string(&backup).is_err() {
            self.error = Some(format!(
                "Imported, but {} still has the original settings in it —                  there's no backup at {}, and this won't comment them out                  without one. hypridle and friends will see both copies                  until you remove them by hand.",
                path.display(),
                backup.display()
            ));
            return;
        }
        let Ok(contents) = std::fs::read_to_string(path) else {
            self.error = Some(format!("Imported, but couldn't re-read {}.", path.display()));
            return;
        };
        let note = format!(
            "now in Hyprforge Settings → Desktop → {} (original in {})",
            self.tab.label(),
            backup.file_name().and_then(|n| n.to_str()).unwrap_or("the .hyprforge.bak file")
        );
        let retired = hyprforge_core::hyprlang::retire(&contents, ranges, &note);
        if let Err(e) = hyprforge_paths::write_atomic(path, &retired) {
            self.error = Some(format!("Imported, but couldn't update {}: {e}", path.display()));
        }
    }

    /// Parses every pending draft into the store, then saves.
    ///
    /// A field that doesn't parse keeps what was typed and reports it;
    /// the ones that do parse still apply, so one typo doesn't discard
    /// everything else.
    fn commit(&mut self) -> Task<Message> {
        let pending: Vec<((usize, Field), String)> =
            self.drafts.iter().map(|(k, v)| (*k, v.clone())).collect();
        let mut bad = Vec::new();
        let mut touched = Vec::new();
        for ((index, field), raw) in pending {
            if self.apply_draft(index, field, &raw) {
                self.drafts.remove(&(index, field));
                if !touched.contains(&field.tab()) {
                    touched.push(field.tab());
                }
            } else {
                bad.push(raw.trim().to_string());
            }
        }
        self.error = (!bad.is_empty())
            .then(|| format!("Couldn't read: {}. Everything else was saved.", bad.join(", ")));
        // Every tab that changed, not just the one on screen. Typing in
        // Wallpaper, switching to Idle and pressing Apply would otherwise
        // leave the wallpaper edit in memory and never written.
        Task::batch(touched.into_iter().map(|tab| self.save(tab)).collect::<Vec<_>>())
    }

    /// Drops the drafts belonging to a removed row, and shifts the ones
    /// after it down.
    ///
    /// Removing row 1 makes row 2 become row 1, so a draft keyed to index
    /// 2 would reappear against a different row's values. Only this tab's
    /// fields move — the three lists share one map but have separate
    /// indices.
    fn reindex_drafts(&mut self, tab: Tab, removed: usize) {
        let moved: Vec<((usize, Field), String)> = self
            .drafts
            .iter()
            .filter(|((index, field), _)| field.tab() == tab && *index > removed)
            .map(|((index, field), value)| ((*index - 1, *field), value.clone()))
            .collect();
        self.drafts
            .retain(|(index, field), _| field.tab() != tab || *index < removed);
        self.drafts.extend(moved);
    }

    /// `true` if the value was understood and stored.
    fn apply_draft(&mut self, index: usize, field: Field, raw: &str) -> bool {
        let text = raw.trim().to_string();
        match field {
            // Empty is a value here, not a missing one: it means "leave
            // the portal's own default", which is different from every
            // number the field can hold — 0 included, since 0 is "no
            // limit at all".
            Field::MaxFps => {
                if text.is_empty() {
                    self.portal.max_fps = None;
                    return true;
                }
                match text.parse() {
                    Ok(fps) => {
                        self.portal.max_fps = Some(fps);
                        true
                    }
                    Err(_) => false,
                }
            }
            Field::PickerBinary => {
                self.portal.custom_picker_binary = text;
                true
            }
            Field::Monitor | Field::Path => {
                let Some(e) = self.wallpapers.entries.get_mut(index) else {
                    return true;
                };
                if field == Field::Monitor {
                    e.monitor = text;
                } else {
                    e.path = text;
                }
                true
            }
            Field::Timeout => {
                let Some(e) = self.wallpapers.entries.get_mut(index) else {
                    return true;
                };
                if text.is_empty() {
                    e.timeout = None;
                    return true;
                }
                match text.parse::<u32>() {
                    Ok(v) if v > 0 => {
                        e.timeout = Some(v);
                        true
                    }
                    _ => false,
                }
            }
            Field::Time => {
                let Some(p) = self.sunset.profiles.get_mut(index) else {
                    return true;
                };
                if sunset::parse_time(&text).is_none() {
                    return false;
                }
                p.time = text;
                true
            }
            Field::Temperature => {
                let Some(p) = self.sunset.profiles.get_mut(index) else {
                    return true;
                };
                match text.parse::<i64>() {
                    Ok(v) if (sunset::MIN_TEMPERATURE..=sunset::MAX_TEMPERATURE).contains(&v) => {
                        p.temperature = v;
                        true
                    }
                    _ => false,
                }
            }
            Field::Gamma => {
                let Some(p) = self.sunset.profiles.get_mut(index) else {
                    return true;
                };
                match text.parse::<f64>() {
                    Ok(v) if v.is_finite() && v > 0.0 => {
                        p.gamma = v;
                        true
                    }
                    _ => false,
                }
            }
            Field::IdleTimeout => {
                let Some(l) = self.idle.listeners.get_mut(index) else {
                    return true;
                };
                match text.parse::<u32>() {
                    Ok(v) if v > 0 => {
                        l.timeout = v;
                        true
                    }
                    _ => false,
                }
            }
            Field::OnTimeout | Field::OnResume => {
                let Some(l) = self.idle.listeners.get_mut(index) else {
                    return true;
                };
                // Verbatim, not trimmed of interior anything: these are
                // shell commands and Hyprforge never runs them itself.
                if field == Field::OnTimeout {
                    l.on_timeout = raw.trim().to_string();
                } else {
                    l.on_resume = raw.trim().to_string();
                }
                true
            }
            _ => true,
        }
    }
}

impl SettingsModule for DesktopModule {
    type Message = Message;


    fn icon(&self) -> &'static str {
        "🖼"
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::TabSelected(tab) => {
                self.tab = tab;
                Task::none()
            }
            Message::Loaded(loaded) => {
                self.images = loaded.images;
                self.monitors = loaded.monitors;
                Task::none()
            }
            Message::WallpaperAdded => {
                // A new entry starts as the fallback with no image, which
                // `invalid()` reports until it's given one — better than
                // inventing a path the user didn't choose.
                self.wallpapers.entries.push(wallpaper::Entry::default());
                Task::none()
            }
            Message::WallpaperRemoved(i) => {
                if i < self.wallpapers.entries.len() {
                    self.wallpapers.entries.remove(i);
                    self.reindex_drafts(Tab::Wallpaper, i);
                }
                self.save(Tab::Wallpaper)
            }
            Message::WallpaperChanged(i, field, value) => {
                if field == Field::FitMode {
                    if let (Some(e), Some(mode)) = (
                        self.wallpapers.entries.get_mut(i),
                        wallpaper::FitMode::parse(&value),
                    ) {
                        e.fit_mode = mode;
                        return self.save(Tab::Wallpaper);
                    }
                    return Task::none();
                }
                // A picked path or monitor is a complete decision, so it
                // saves; typed text waits for Apply.
                if matches!(field, Field::Path | Field::Monitor) {
                    self.apply_draft(i, field, &value);
                    self.drafts.remove(&(i, field));
                    return self.save(Tab::Wallpaper);
                }
                self.drafts.insert((i, field), value);
                Task::none()
            }
            Message::WallpaperToggled(i, field, on) => {
                if let Some(e) = self.wallpapers.entries.get_mut(i) {
                    match field {
                        Field::RandomOrder => e.random_order = on,
                        Field::Recursive => e.recursive = on,
                        _ => {}
                    }
                }
                self.save(Tab::Wallpaper)
            }
            Message::SplashToggled(on) => {
                self.wallpapers.splash = Some(on);
                self.save(Tab::Wallpaper)
            }
            Message::ProfileAdded => {
                self.sunset.profiles.push(sunset::Profile::default());
                Task::none()
            }
            Message::ProfileRemoved(i) => {
                if i < self.sunset.profiles.len() {
                    self.sunset.profiles.remove(i);
                    self.reindex_drafts(Tab::NightLight, i);
                }
                self.save(Tab::NightLight)
            }
            Message::ProfileChanged(i, field, value) => {
                self.drafts.insert((i, field), value);
                Task::none()
            }
            Message::ProfileToggled(i, field, on) => {
                if let (Some(p), Field::Identity) = (self.sunset.profiles.get_mut(i), field) {
                    p.identity = on;
                }
                self.save(Tab::NightLight)
            }
            Message::ListenerAdded => {
                self.idle.listeners.push(idle::Listener {
                    timeout: 300,
                    ..idle::Listener::default()
                });
                Task::none()
            }
            Message::ListenerRemoved(i) => {
                if i < self.idle.listeners.len() {
                    self.idle.listeners.remove(i);
                    self.reindex_drafts(Tab::Idle, i);
                }
                self.save(Tab::Idle)
            }
            Message::ListenerChanged(i, field, value) => {
                self.drafts.insert((i, field), value);
                Task::none()
            }
            Message::ListenerToggled(i, field, on) => {
                if let (Some(l), Field::IgnoreInhibit) = (self.idle.listeners.get_mut(i), field) {
                    l.ignore_inhibit = on;
                }
                self.save(Tab::Idle)
            }
            Message::GeneralCommandChanged(field, value) => {
                self.idle.general.set_command(field, value);
                // Saved here, like every sibling in this match. It used
                // to return `Task::none()` and record no draft either —
                // and `commit` builds its work list from `self.drafts`
                // alone, so Enter and the Apply button were both no-ops
                // and the text simply sat on screen looking saved.
                //
                // These are the idle daemon's `lock_cmd` and
                // `before_sleep_cmd`. Silently not saving the second one
                // is a machine that suspends unlocked.
                self.save(Tab::Idle)
            }
            Message::InhibitToggled(field, on) => {
                match field {
                    "ignore_dbus_inhibit" => self.idle.general.ignore_dbus_inhibit = on,
                    "ignore_systemd_inhibit" => self.idle.general.ignore_systemd_inhibit = on,
                    "ignore_wayland_inhibit" => self.idle.general.ignore_wayland_inhibit = on,
                    _ => {}
                }
                self.save(Tab::Idle)
            }
            Message::Commit => self.commit(),
            Message::RestartIdle => {
                match apply::restart_idle() {
                    Ok(()) => {
                        self.idle_needs_restart = false;
                        self.status = Some("hypridle restarted — your idle settings are live.".into());
                        self.error = None;
                    }
                    Err(e) => self.error = Some(format!("Couldn't restart hypridle: {e}")),
                }
                Task::none()
            }
            Message::PortalChanged(field, value) => {
                self.drafts.insert((0, field), value);
                Task::none()
            }
            Message::PortalToggled(field, on) => {
                match field {
                    Field::AllowToken => self.portal.allow_token_by_default = on,
                    Field::ForceShm => self.portal.force_shm = on,
                    _ => return Task::none(),
                }
                self.save(Tab::ScreenSharing)
            }
            Message::PortalCursorMode(mode) => {
                self.portal.cursor_mode = mode;
                self.save(Tab::ScreenSharing)
            }
            Message::RestartPortal => {
                match apply::restart_portal() {
                    Ok(()) => {
                        self.portal_needs_restart = false;
                        self.status =
                            Some("Screen sharing restarted — your settings are live.".into());
                        self.error = None;
                    }
                    Err(e) => self.error = Some(format!("Couldn't restart screen sharing: {e}")),
                }
                Task::none()
            }
            Message::AdoptOpen => {
                self.adopt = Some(self.read_daemon_config());
                self.error = None;
                self.status = None;
                Task::none()
            }
            Message::AdoptToggle(index, checked) => {
                if let Some(review) = &mut self.adopt {
                    if let Some(candidate) = review.candidates.get_mut(index) {
                        candidate.checked = checked;
                    }
                }
                Task::none()
            }
            Message::AdoptCancel => {
                self.adopt = None;
                Task::none()
            }
            Message::AdoptConfirm => self.adopt_confirmed(),
            Message::NightLightTrayToggled(shown) => {
                let Ok(prefs) = &self.tray_prefs else {
                    // `night_light_tray_row` renders no checkbox while
                    // `tray_prefs` is `Err`, so a stray message here still
                    // must not turn an unreadable file into a freshly
                    // written default — the exact overwrite this field
                    // exists to prevent.
                    return Task::none();
                };
                let mut updated = *prefs;
                updated.night_light = shown;
                match hyprforge_tray::prefs::save(&updated) {
                    Ok(()) => self.tray_prefs = Ok(updated),
                    Err(e) => self.error = Some(e.to_string()),
                }
                Task::none()
            }
            Message::KeepAwakeTrayToggled(shown) => {
                let Ok(prefs) = &self.tray_prefs else {
                    return Task::none();
                };
                let mut updated = *prefs;
                updated.keep_awake = shown;
                match hyprforge_tray::prefs::save(&updated) {
                    Ok(()) => self.tray_prefs = Ok(updated),
                    Err(e) => self.error = Some(e.to_string()),
                }
                Task::none()
            }
            Message::Applied(tab, Ok(applied)) => {
                self.error = None;
                // Only now, once the store is written, the generated file
                // produced and the `source =` line installed. Retiring
                // before this point would stand the user's own settings
                // down in favour of a file that might not exist.
                if let Some((path, ranges)) = self.pending_retire.take() {
                    self.retire_adopted(&path, &ranges);
                }
                self.idle_needs_restart =
                    tab == Tab::Idle && applied == Applied::NeedsRestart;
                self.portal_needs_restart =
                    tab == Tab::ScreenSharing && applied == Applied::NeedsRestart;
                match applied {
                    // Refusals are an error, not a status: the setting is
                    // saved but the screen did not change, and calling
                    // that "applied" is the failure this project keeps
                    // meeting.
                    Applied::PartlyRefused(refused) => {
                        self.status = None;
                        self.error = Some(format!(
                            "Saved, but the daemon wouldn't take: {}. \
                             Check the file still exists and is readable.",
                            refused.join(", ")
                        ));
                    }
                    Applied::Live => self.status = Some("Saved and applied.".into()),
                    // Named per tab. Two daemons reach this now and they
                    // are restarted differently — one is `hl.exec_cmd`,
                    // the other a systemd unit — so "restart it below"
                    // has to be about the one the user is looking at.
                    Applied::NeedsRestart => {
                        self.status = Some(
                            match tab {
                                Tab::ScreenSharing =>
                                    "Saved. The portal only reads this when it starts — \
                                     restart it below to apply.",
                                _ => "Saved. hypridle has no way to be told — \
                                      restart it below to apply.",
                            }
                            .into(),
                        )
                    }
                    Applied::DaemonNotRunning => {
                        self.status = Some(
                            "Saved. The daemon isn't running, so nothing changed on screen yet."
                                .into(),
                        )
                    }
                    // An error rather than a status, and worded as the
                    // uncertainty it is. "The daemon isn't running" would be
                    // a claim this code cannot make — the check itself is
                    // what failed — and if a daemon *is* running it is now
                    // ignoring settings the user was told were saved.
                    Applied::DaemonUnknown => {
                        self.status = None;
                        self.error = Some(
                            "Saved, but we couldn't tell whether the daemon is running,                              so it may not have picked the change up. Try again, or                              restart it below."
                                .into(),
                        )
                    }
                }
                Task::none()
            }
            Message::Applied(_, Err(e)) => {
                self.status = None;
                self.error = Some(e);
                Task::none()
            }
        }
    }

    fn view(&self, scale: FontScale) -> Element<'_, Message> {
        let mut content = column![].spacing(spacing::LG).width(Length::Fill);

        if let Some(reason) = &self.store_unreadable {
            content = content.push(section(
                "A settings file couldn't be read",
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

        let mut tabs = row![].spacing(spacing::SM);
        for tab in Tab::ALL {
            let button = if tab == self.tab {
                primary_button(tab.label())
            } else {
                secondary_button(tab.label())
            };
            tabs = tabs.push(button.on_press(Message::TabSelected(tab)));
        }
        content = content.push(tabs);

        if !self.drafts.is_empty() {
            content = content.push(
                row![
                    scaled_text(
                        format!("{} field(s) typed but not applied", self.drafts.len()),
                        13.0,
                        scale,
                    ),
                    primary_button("Apply").on_press(Message::Commit),
                ]
                .spacing(spacing::MD)
                .align_y(iced::Alignment::Center),
            );
        }

        // Offered only while no review is open, so the screen never shows
        // an Import button above a list the user is already reviewing.
        if let Some(review) = &self.adopt {
            content = content.push(self.adopt_view(review, scale));
        } else {
            let path = Self::daemon_config_path(self.tab);
            // Only when there is a file to read. Offering to import from
            // a config that doesn't exist is a button whose only outcome
            // is "nothing found".
            if path.is_file() {
                let name =
                    path.file_name().and_then(|n| n.to_str()).unwrap_or("config").to_string();
                content = content.push(
                    row![
                        secondary_button("Import from this daemon's own config")
                            .on_press(Message::AdoptOpen),
                        meta_text(
                            format!("Reads what you wrote by hand in {name}"),
                            12.0,
                            scale,
                        ),
                    ]
                    .spacing(spacing::MD)
                    .align_y(iced::Alignment::Center),
                );
            }
        }

        content = match self.tab {
            Tab::Wallpaper => content.push(self.wallpaper_view(scale)),
            Tab::NightLight => content.push(self.sunset_view(scale)),
            Tab::Idle => self.idle_view(content, scale),
            Tab::ScreenSharing => content.push(self.portal_view(scale)),
        };

        scrollable(container(content).padding(spacing::LG))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

impl DesktopModule {
    fn wallpaper_view(&self, scale: FontScale) -> Element<'_, Message> {
        let problems = self.wallpapers.invalid();
        let mut body = column![meta_text(
            "A wallpaper with no monitor is the fallback: it covers every screen \
             that hasn't got one of its own, which is what makes a setup survive \
             plugging a different display in.",
            12.0,
            scale,
        )]
        .spacing(spacing::SM);

        for (i, entry) in self.wallpapers.entries.iter().enumerate() {
            body = body.push(divider());
            let mut fields = column![].spacing(spacing::SM);

            let monitors: Vec<String> = std::iter::once(ALL_MONITORS.to_string())
                .chain(self.monitors.iter().cloned())
                .collect();
            let selected = Some(if entry.is_fallback() {
                ALL_MONITORS.to_string()
            } else {
                entry.monitor.clone()
            });
            fields = fields.push(labelled(
                "Screen",
                pick_list(monitors, selected, move |choice: String| {
                    let value = if choice == ALL_MONITORS { String::new() } else { choice };
                    Message::WallpaperChanged(i, Field::Monitor, value)
                })
                .into(),
                scale,
            ));

            let images = options_including(&self.images, &entry.path);
            fields = fields.push(labelled(
                "Image or folder",
                pick_list(images, Some(entry.path.clone()).filter(|p| !p.is_empty()), move |choice: String| {
                    Message::WallpaperChanged(i, Field::Path, choice)
                })
                .into(),
                scale,
            ));

            let modes: Vec<String> = wallpaper::FitMode::ALL.iter().map(|m| m.to_string()).collect();
            fields = fields.push(labelled(
                "Fit",
                pick_list(modes, Some(entry.fit_mode.to_string()), move |choice: String| {
                    Message::WallpaperChanged(i, Field::FitMode, choice)
                })
                .into(),
                scale,
            ));

            // Only for a folder: these options mean nothing for one image,
            // and showing them would suggest a single image cycles.
            if entry.is_directory() {
                fields = fields.push(labelled(
                    "Change every (seconds)",
                    text_input("30", &self.draft(i, Field::Timeout, entry.timeout.map(|t| t.to_string()).unwrap_or_default()))
                        .on_input(move |v| Message::WallpaperChanged(i, Field::Timeout, v))
                        .on_submit(Message::Commit)
                        .padding(spacing::SM)
                        .width(Length::Fixed(90.0))
                        .into(),
                    scale,
                ));
                fields = fields.push(
                    row![
                        checkbox(entry.random_order)
                            .on_toggle(move |v| Message::WallpaperToggled(i, Field::RandomOrder, v)),
                        scaled_text("Shuffle", 13.0, scale),
                        checkbox(entry.recursive)
                            .on_toggle(move |v| Message::WallpaperToggled(i, Field::Recursive, v)),
                        scaled_text("Include subfolders", 13.0, scale),
                    ]
                    .spacing(spacing::SM)
                    .align_y(iced::Alignment::Center),
                );
            }

            if let Some((_, problem)) = problems.iter().find(|(index, _)| *index == i) {
                fields = fields.push(scaled_text(problem.clone(), 12.0, scale));
            }
            fields = fields.push(danger_button("Remove", Message::WallpaperRemoved(i)));
            body = body.push(fields);
        }

        body = body.push(divider());
        body = body.push(
            row![
                secondary_button("Add a wallpaper").on_press(Message::WallpaperAdded),
                checkbox(self.wallpapers.splash.unwrap_or(true))
                    .on_toggle(Message::SplashToggled),
                scaled_text("Show the Hyprland splash", 13.0, scale),
            ]
            .spacing(spacing::MD)
            .align_y(iced::Alignment::Center),
        );
        section("Wallpaper", scale, body)
    }

    /// The review shown between pressing Import and anything being
    /// written.
    ///
    /// Nothing here has touched the disk yet. That matters enough to say
    /// on screen: the user is about to have their own config file edited,
    /// and a review they can't tell is provisional isn't a review.
    fn adopt_view<'a>(&'a self, review: &'a AdoptReview, scale: FontScale) -> Element<'a, Message> {
        let name = review
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("the config file")
            .to_string();

        let mut body = column![].spacing(spacing::SM);

        if review.candidates.is_empty() && review.problems.is_empty() {
            body = body.push(scaled_text(
                format!("Nothing to import — {name} has no settings this screen owns."),
                13.0,
                scale,
            ));
            body = body.push(secondary_button("Close").on_press(Message::AdoptCancel));
            return section("Import", scale, body);
        }

        body = body.push(meta_text(
            format!(
                "Found in {name}. Nothing has been changed yet. What you tick is                  copied in here, and the lines it came from are commented out in                  {name} so the daemon doesn't act on both copies."
            ),
            12.0,
            scale,
        ));

        for (index, candidate) in review.candidates.iter().enumerate() {
            let mut entry = column![row![
                checkbox(candidate.checked)
                    .on_toggle(move |v| Message::AdoptToggle(index, v)),
                scaled_text(candidate.item.label(), 13.0, scale),
            ]
            .spacing(spacing::SM)
            .align_y(iced::Alignment::Center)]
            .spacing(spacing::XS);

            // Said plainly, because the consequence is specific: this one
            // is adoptable but would come in narrower than it is now, so
            // its original stays live and the user will have two.
            if !candidate.is_faithful() {
                entry = entry.push(meta_text(
                    format!(
                        "line {}: this screen can't represent {} — import it and the                          original stays in {name} as well, so you'd have both.",
                        candidate.line,
                        candidate.dropped.join(", ")
                    ),
                    12.0,
                    scale,
                ));
            }
            body = body.push(entry);
        }

        if !review.problems.is_empty() {
            body = body.push(divider());
            body = body.push(scaled_text(
                format!("Some of {name} couldn't be read, so it isn't listed above:"),
                13.0,
                scale,
            ));
            for (line, why) in &review.problems {
                body = body.push(meta_text(format!("line {line}: {why}"), 12.0, scale));
            }
        }

        let ticked = review.candidates.iter().filter(|c| c.checked).count();
        body = body.push(divider());
        body = body.push(
            row![
                primary_button("Import what's ticked").on_press(Message::AdoptConfirm),
                secondary_button("Cancel").on_press(Message::AdoptCancel),
                meta_text(format!("{ticked} of {} ticked", review.candidates.len()), 12.0, scale),
            ]
            .spacing(spacing::MD)
            .align_y(iced::Alignment::Center),
        );

        section("Import", scale, body)
    }

    fn portal_view(&self, scale: FontScale) -> Element<'_, Message> {
        let mut body = column![meta_text(
            "What happens when an app asks to capture your screen. These are \
             read by xdg-desktop-portal-hyprland, which is what Firefox, Chrome \
             and Discord actually talk to.",
            12.0,
            scale,
        )]
        .spacing(spacing::SM);

        for (index, problem) in self.portal.invalid() {
            let _ = index;
            body = body.push(scaled_text(problem, 13.0, scale));
        }

        // Empty means "whatever the portal does by default", which is a
        // real choice and has to be reachable — so the placeholder says
        // the number rather than leaving the user to guess what blank does.
        body = body.push(labelled(
            "Maximum frame rate",
            text_input(
                &format!("{} (the portal's default)", portal::DEFAULT_MAX_FPS),
                &self.draft(
                    0,
                    Field::MaxFps,
                    self.portal.max_fps.map(|f| f.to_string()).unwrap_or_default(),
                ),
            )
            .on_input(move |v| Message::PortalChanged(Field::MaxFps, v))
            .into(),
            scale,
        ));
        body = body.push(meta_text(
            "0 means no limit. Lowering it is the usual fix for a share that \
             stutters or heats the machine up.",
            12.0,
            scale,
        ));

        body = body.push(labelled(
            "Share picker",
            text_input(
                portal::DEFAULT_PICKER,
                &self.draft(0, Field::PickerBinary, &self.portal.custom_picker_binary),
            )
            .on_input(move |v| Message::PortalChanged(Field::PickerBinary, v))
            .into(),
            scale,
        ));

        body = body.push(labelled(
            "Pointer in the stream",
            pick_list(portal::CursorMode::ALL, Some(self.portal.cursor_mode), |mode| {
                Message::PortalCursorMode(mode)
            })
            .into(),
            scale,
        ));

        body = body.push(
            row![
                checkbox(self.portal.allow_token_by_default)
                    .on_toggle(|v| Message::PortalToggled(Field::AllowToken, v)),
                scaled_text("Remember my choice, so apps stop asking every time", 13.0, scale),
            ]
            .spacing(spacing::SM)
            .align_y(iced::Alignment::Center),
        );
        body = body.push(
            row![
                checkbox(self.portal.force_shm)
                    .on_toggle(|v| Message::PortalToggled(Field::ForceShm, v)),
                scaled_text("Use the slower, more compatible capture path", 13.0, scale),
            ]
            .spacing(spacing::SM)
            .align_y(iced::Alignment::Center),
        );
        body = body.push(meta_text(
            "Try that one if screen sharing comes out black — it's the documented \
             way around buffer allocation failing on machines with two GPUs.",
            12.0,
            scale,
        ));

        if self.portal_needs_restart {
            body = body.push(divider());
            body = body.push(scaled_text(
                "Saved. The portal only reads this when it starts, so it's still \
                 using the old settings.",
                13.0,
                scale,
            ));
            // Said before they press it, not after. Restarting mid-call
            // is exactly when someone would regret finding out.
            body = body.push(meta_text(
                "Restarting it will end any screen share that's running right now.",
                12.0,
                scale,
            ));
            body = body.push(
                secondary_button("Restart the portal").on_press(Message::RestartPortal),
            );
        }

        section("Screen sharing", scale, body)
    }

    /// The "show in tray" row for the night light icon, appended to the
    /// Night light tab — this is literally that feature's own screen, so
    /// the toggle for its tray icon belongs here rather than anywhere else.
    /// Same shape and reasoning as `network::NetworkModule::tray_row`.
    fn night_light_tray_row(&self, scale: FontScale) -> Element<'_, Message> {
        match &self.tray_prefs {
            Ok(prefs) => row![
                checkbox(prefs.night_light).on_toggle(Message::NightLightTrayToggled),
                scaled_text("Show in tray", 15.0, scale),
            ]
            .spacing(spacing::SM)
            .align_y(iced::Alignment::Center)
            .into(),
            Err(_) => meta_text(
                "Tray setting unavailable — see the error above.",
                13.0,
                scale,
            )
            .into(),
        }
    }

    /// The "show in tray" row for the keep-awake icon, appended to the
    /// Idle tab: keeping the machine awake is the inverse of the timeouts
    /// configured just below it, so it belongs beside them rather than on
    /// a screen of its own.
    fn keep_awake_tray_row(&self, scale: FontScale) -> Element<'_, Message> {
        match &self.tray_prefs {
            Ok(prefs) => row![
                checkbox(prefs.keep_awake).on_toggle(Message::KeepAwakeTrayToggled),
                scaled_text("Show in tray", 15.0, scale),
            ]
            .spacing(spacing::SM)
            .align_y(iced::Alignment::Center)
            .into(),
            Err(_) => meta_text(
                "Tray setting unavailable — see the error above.",
                13.0,
                scale,
            )
            .into(),
        }
    }

    fn sunset_view(&self, scale: FontScale) -> Element<'_, Message> {
        let problems = self.sunset.invalid();
        let mut body = column![
            meta_text(
                "Each entry holds from its time until the next one. Lower temperatures \
                 are warmer; 6500K is neutral daylight.",
                12.0,
                scale,
            ),
            self.night_light_tray_row(scale),
        ]
        .spacing(spacing::SM);

        if self.sunset.has_midnight_gap() {
            body = body.push(scaled_text(
                "No entry starts at 00:00, so the last one of the day carries over \
                 into the morning. Add one at 00:00 if that isn't what you want.",
                13.0,
                scale,
            ));
        }

        for (i, profile) in self.sunset.profiles.iter().enumerate() {
            body = body.push(divider());
            let mut fields = column![].spacing(spacing::SM);
            fields = fields.push(labelled(
                "From",
                text_input("21:00", &self.draft(i, Field::Time, &profile.time))
                    .on_input(move |v| Message::ProfileChanged(i, Field::Time, v))
                    .on_submit(Message::Commit)
                    .padding(spacing::SM)
                    .width(Length::Fixed(90.0))
                    .into(),
                scale,
            ));
            fields = fields.push(labelled(
                "Temperature (K)",
                text_input("6500", &self.draft(i, Field::Temperature, profile.temperature))
                    .on_input(move |v| Message::ProfileChanged(i, Field::Temperature, v))
                    .on_submit(Message::Commit)
                    .padding(spacing::SM)
                    .width(Length::Fixed(90.0))
                    .into(),
                scale,
            ));
            fields = fields.push(labelled(
                "Brightness",
                text_input("1.0", &self.draft(i, Field::Gamma, profile.gamma))
                    .on_input(move |v| Message::ProfileChanged(i, Field::Gamma, v))
                    .on_submit(Message::Commit)
                    .padding(spacing::SM)
                    .width(Length::Fixed(90.0))
                    .into(),
                scale,
            ));
            fields = fields.push(
                row![
                    checkbox(profile.identity)
                        .on_toggle(move |v| Message::ProfileToggled(i, Field::Identity, v)),
                    scaled_text("Brightness only, no colour shift", 13.0, scale),
                ]
                .spacing(spacing::SM)
                .align_y(iced::Alignment::Center),
            );
            if let Some((_, problem)) = problems.iter().find(|(index, _)| *index == i) {
                fields = fields.push(scaled_text(problem.clone(), 12.0, scale));
            }
            fields = fields.push(danger_button("Remove", Message::ProfileRemoved(i)));
            body = body.push(fields);
        }

        body = body.push(divider());
        body = body.push(secondary_button("Add a time").on_press(Message::ProfileAdded));
        section("Night light", scale, body)
    }

    fn idle_view<'a>(
        &'a self,
        mut content: iced::widget::Column<'a, Message>,
        scale: FontScale,
    ) -> iced::widget::Column<'a, Message> {
        // Keeping the screen awake is the inverse of everything else on
        // this tab, and doesn't depend on hypridle itself — the tray icon
        // is a Hyprforge-side inhibitor, not a setting hypridle reads —
        // so it's shown first, ahead of the restart notice below.
        content = content.push(section(
            "Keep awake",
            scale,
            column![
                meta_text(
                    "Adds a tray icon that suspends every timeout below while \
                     it's switched on.",
                    12.0,
                    scale,
                ),
                self.keep_awake_tray_row(scale),
            ]
            .spacing(spacing::SM),
        ));

        // The honest bit. hypridle has no IPC, so a save here genuinely
        // does nothing until it restarts.
        if self.idle_needs_restart {
            content = content.push(section(
                "Restart needed",
                scale,
                column![
                    scaled_text(
                        "hypridle has no way to be told about a config change, so your \
                         saved settings aren't running yet.",
                        13.0,
                        scale,
                    ),
                    meta_text(
                        "Restarting it briefly stops idle tracking — it won't lock or \
                         dim during the moment it takes.",
                        12.0,
                        scale,
                    ),
                    primary_button("Restart hypridle").on_press(Message::RestartIdle),
                ]
                .spacing(spacing::SM),
            ));
        }

        let problems = self.idle.invalid();
        let mut listeners = column![meta_text(
            "Each entry waits for the screen to be idle, then runs a command. \
             Hyprforge stores these and never runs them itself.",
            12.0,
            scale,
        )]
        .spacing(spacing::SM);

        for (i, listener) in self.idle.listeners.iter().enumerate() {
            listeners = listeners.push(divider());
            let mut fields = column![].spacing(spacing::SM);
            fields = fields.push(labelled(
                "After (seconds)",
                text_input("300", &self.draft(i, Field::IdleTimeout, listener.timeout))
                    .on_input(move |v| Message::ListenerChanged(i, Field::IdleTimeout, v))
                    .on_submit(Message::Commit)
                    .padding(spacing::SM)
                    .width(Length::Fixed(90.0))
                    .into(),
                scale,
            ));
            fields = fields.push(labelled(
                "Run",
                text_input("loginctl lock-session", &self.draft(i, Field::OnTimeout, &listener.on_timeout))
                    .on_input(move |v| Message::ListenerChanged(i, Field::OnTimeout, v))
                    .on_submit(Message::Commit)
                    .padding(spacing::SM)
                    .into(),
                scale,
            ));
            fields = fields.push(labelled(
                "On return",
                text_input("", &self.draft(i, Field::OnResume, &listener.on_resume))
                    .on_input(move |v| Message::ListenerChanged(i, Field::OnResume, v))
                    .on_submit(Message::Commit)
                    .padding(spacing::SM)
                    .into(),
                scale,
            ));
            fields = fields.push(
                row![
                    checkbox(listener.ignore_inhibit)
                        .on_toggle(move |v| Message::ListenerToggled(i, Field::IgnoreInhibit, v)),
                    scaled_text("Even while something is blocking idle", 13.0, scale),
                ]
                .spacing(spacing::SM)
                .align_y(iced::Alignment::Center),
            );
            if let Some((_, problem)) = problems.iter().find(|(index, _)| *index == i) {
                fields = fields.push(scaled_text(problem.clone(), 12.0, scale));
            }
            fields = fields.push(danger_button("Remove", Message::ListenerRemoved(i)));
            listeners = listeners.push(fields);
        }
        listeners = listeners.push(divider());
        listeners = listeners.push(secondary_button("Add a timeout").on_press(Message::ListenerAdded));
        content = content.push(section("Idle timeouts", scale, listeners));

        let mut general = column![].spacing(spacing::SM);
        for (field, label, value) in self.idle.general.commands() {
            general = general.push(labelled(
                label,
                text_input("", value)
                    .on_input(move |v| Message::GeneralCommandChanged(field, v))
                    .on_submit(Message::Commit)
                    .padding(spacing::SM)
                    .into(),
                scale,
            ));
        }
        for (field, label, on) in [
            ("ignore_dbus_inhibit", "Ignore app idle blocks (D-Bus)", self.idle.general.ignore_dbus_inhibit),
            ("ignore_systemd_inhibit", "Ignore systemd idle blocks", self.idle.general.ignore_systemd_inhibit),
            ("ignore_wayland_inhibit", "Ignore Wayland idle blocks", self.idle.general.ignore_wayland_inhibit),
        ] {
            general = general.push(
                row![
                    checkbox(on).on_toggle(move |v| Message::InhibitToggled(field, v)),
                    scaled_text(label, 13.0, scale),
                ]
                .spacing(spacing::SM)
                .align_y(iced::Alignment::Center),
            );
        }
        content.push(section("Session commands", scale, general))
    }
}

/// The label a `monitor =` of empty means, spelled out — an empty
/// dropdown entry would read as "not set" rather than "all of them".
const ALL_MONITORS: &str = "All screens";


/// Keeps a stored value that discovery didn't find — an image on another
/// disk, a folder that moved. Dropping it would show nothing selected,
/// and the next pick would replace a working wallpaper.
fn options_including(known: &[String], current: &str) -> Vec<String> {
    let mut options = known.to_vec();
    let current = current.trim();
    if !current.is_empty() && !options.iter().any(|o| o == current) {
        options.insert(0, current.to_string());
    }
    options
}

fn load_or_note<T: serde::de::DeserializeOwned + Default>(
    path: &std::path::Path,
    problems: &mut Vec<String>,
) -> T {
    match hyprforge_ecosystem::storage::load(path) {
        Ok(value) => value,
        Err(e) => {
            problems.push(e.to_string());
            T::default()
        }
    }
}

async fn load_system() -> Loaded {
    tokio::task::spawn_blocking(|| Loaded {
        images: wallpaper::discover_images(),
        monitors: hyprforge_core::monitors::connector_names(),
    })
    .await
    .unwrap_or_default()
}

async fn apply_wallpapers(
    generated: PathBuf,
    target: PathBuf,
    settings: wallpaper::Settings,
) -> Result<Applied, String> {
    tokio::task::spawn_blocking(move || {
        apply::wallpapers(&generated, &target, &settings).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

async fn apply_sunset(
    generated: PathBuf,
    target: PathBuf,
    settings: sunset::Settings,
) -> Result<Applied, String> {
    tokio::task::spawn_blocking(move || {
        // The schedule is applied against the clock now, so the profile
        // that should be holding is the one pushed.
        let now = minutes_now();
        apply::temperature(&generated, &target, &settings, now).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

async fn apply_idle(
    generated: PathBuf,
    target: PathBuf,
    settings: idle::Settings,
) -> Result<Applied, String> {
    tokio::task::spawn_blocking(move || {
        apply::idle(&generated, &target, &settings).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

async fn apply_portal(
    generated: PathBuf,
    target: PathBuf,
    settings: portal::Settings,
) -> Result<Applied, String> {
    tokio::task::spawn_blocking(move || {
        apply::portal(&generated, &target, &settings).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Local minutes past midnight.
///
/// Read from `date` rather than computed from the Unix epoch: the epoch
/// is UTC and the schedule is in the user's local time, and the timezone
/// offset isn't something to reimplement.
fn minutes_now() -> u32 {
    let queried = hyprforge_core::command::output(
        std::process::Command::new("date").arg("+%H:%M"),
        hyprforge_core::command::TIMEOUT,
    );
    let Ok(out) = queried else {
        return 0;
    };
    sunset::parse_time(String::from_utf8_lossy(&out.stdout).trim()).unwrap_or(0)
}

fn wallpaper_toml() -> PathBuf {
    hyprforge_core::paths::hyprforge_config_dir().join("wallpaper.toml")
}

fn sunset_toml() -> PathBuf {
    hyprforge_core::paths::hyprforge_config_dir().join("night-light.toml")
}

fn idle_toml() -> PathBuf {
    hyprforge_core::paths::hyprforge_config_dir().join("idle.toml")
}

fn portal_toml() -> PathBuf {
    hyprforge_core::paths::hyprforge_config_dir().join("screen-sharing.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh DesktopModule against an isolated config home and greeter
    /// export dir — see [`crate::modules::with_temp_env`].
    fn with_temp_config<T>(f: impl FnOnce(&mut DesktopModule) -> T) -> T {
        crate::modules::with_temp_env(|_dir| {
            let (mut module, _) = DesktopModule::new();
            f(&mut module)
        })
    }

    /// Writes a `hypridle.conf` into the isolated config home the way the
    /// user's own would be, so the adopt flow has something real to read.
    fn write_hypridle(body: &str) -> PathBuf {
        let path = DesktopModule::daemon_config_path(Tab::Idle);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        path
    }

    const FIVE_LISTENERS: &str = "\
general {
    lock_cmd = hyprforge-lock
}

listener {
    timeout = 150
    on-timeout = brightnessctl -s set 10
    on-resume = brightnessctl -r
}

listener {
    timeout = 1800
    on-timeout = systemctl suspend
}
";

    #[test]
    fn importing_finds_what_the_user_wrote_by_hand() {
        with_temp_config(|m| {
            write_hypridle(FIVE_LISTENERS);
            m.tab = Tab::Idle;
            let _ = m.update(Message::AdoptOpen);
            let review = m.adopt.as_ref().expect("a review");
            assert_eq!(review.candidates.len(), 3, "general plus two listeners");
            assert!(
                review.candidates.iter().all(|c| c.checked),
                "everything readable should be pre-ticked"
            );
        });
    }

    /// Nothing may reach the disk before the user confirms. A review the
    /// user can't back out of isn't a review.
    #[test]
    fn opening_the_review_changes_nothing() {
        with_temp_config(|m| {
            let path = write_hypridle(FIVE_LISTENERS);
            m.tab = Tab::Idle;
            let _ = m.update(Message::AdoptOpen);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), FIVE_LISTENERS);
            assert!(m.idle.listeners.is_empty(), "nothing adopted yet");
            let _ = m.update(Message::AdoptCancel);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), FIVE_LISTENERS);
            assert!(m.adopt.is_none());
        });
    }

    /// The whole reason retiring exists. Blocks accumulate, so adopting
    /// without standing the originals down leaves hypridle running both
    /// sets — on this machine's real config, ten listeners, two of which
    /// lock the screen.
    #[test]
    fn adopting_stands_the_originals_down_so_the_daemon_sees_one_of_each() {
        with_temp_config(|m| {
            let path = write_hypridle(FIVE_LISTENERS);
            // `install` makes this during the save; the retire step
            // refuses without it, and here it has not happened yet.
            std::fs::write(
                path.with_extension("conf.hyprforge.bak"),
                FIVE_LISTENERS,
            )
            .unwrap();

            m.tab = Tab::Idle;
            let _ = m.update(Message::AdoptOpen);
            let _ = m.update(Message::AdoptConfirm);
            assert_eq!(m.idle.listeners.len(), 2, "adopted into the app");

            // The retire is deferred until the settings actually reached
            // the daemon, which is what `Applied` reports.
            assert!(m.pending_retire.is_some(), "a retire should be pending");
            let _ = m.update(Message::Applied(Tab::Idle, Ok(Applied::NeedsRestart)));

            let after = hyprforge_core::hyprlang::parse(&std::fs::read_to_string(&path).unwrap());
            assert_eq!(after.blocks("listener").count(), 0, "the originals must be stood down");
            assert_eq!(after.blocks("general").count(), 0);
        });
    }

    /// Commented out, never deleted — the difference between a change the
    /// user can read and undo and one that looks like the app ate their
    /// config.
    #[test]
    fn retiring_keeps_every_line_the_user_wrote() {
        with_temp_config(|m| {
            let path = write_hypridle(FIVE_LISTENERS);
            std::fs::write(path.with_extension("conf.hyprforge.bak"), FIVE_LISTENERS).unwrap();
            m.tab = Tab::Idle;
            let _ = m.update(Message::AdoptOpen);
            let _ = m.update(Message::AdoptConfirm);
            let _ = m.update(Message::Applied(Tab::Idle, Ok(Applied::NeedsRestart)));

            let after = std::fs::read_to_string(&path).unwrap();
            for fragment in ["systemctl suspend", "brightnessctl -r", "hyprforge-lock"] {
                assert!(after.contains(fragment), "{fragment} was lost:\n{after}");
            }
        });
    }

    /// The backup is the user's way out. Standing their settings down
    /// without one is not a trade this app gets to make for them.
    #[test]
    fn nothing_is_retired_without_a_backup_to_go_back_to() {
        with_temp_config(|m| {
            let path = write_hypridle(FIVE_LISTENERS);
            m.tab = Tab::Idle;
            let _ = m.update(Message::AdoptOpen);
            let _ = m.update(Message::AdoptConfirm);
            let _ = m.update(Message::Applied(Tab::Idle, Ok(Applied::NeedsRestart)));

            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                FIVE_LISTENERS,
                "the original must be untouched when there is no backup"
            );
            let error = m.error.as_ref().expect("the user must be told");
            assert!(error.contains("both copies"), "{error}");
        });
    }

    /// A candidate this screen cannot represent may still be adopted —
    /// the user asked — but its original stays live, because retiring it
    /// would narrow the setting and then delete the evidence.
    #[test]
    fn a_setting_this_app_cannot_fully_model_keeps_its_original() {
        with_temp_config(|m| {
            let body = "listener {\n    timeout = 5\n    on-timeout = x\n    brand_new_key = 1\n}\n";
            let path = write_hypridle(body);
            std::fs::write(path.with_extension("conf.hyprforge.bak"), body).unwrap();
            m.tab = Tab::Idle;
            let _ = m.update(Message::AdoptOpen);

            let review = m.adopt.as_ref().unwrap();
            assert!(!review.candidates[0].checked, "must not be pre-ticked");
            assert_eq!(review.candidates[0].dropped, vec!["brand_new_key"]);

            // The user ticks it anyway.
            let _ = m.update(Message::AdoptToggle(0, true));
            let _ = m.update(Message::AdoptConfirm);
            let _ = m.update(Message::Applied(Tab::Idle, Ok(Applied::NeedsRestart)));

            assert_eq!(m.idle.listeners.len(), 1, "adopted as asked");
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                body,
                "but its original must stay live, since the import narrowed it"
            );
        });
    }

    /// Importing must not discard what the user already set up in the
    /// app. Appending is the only safe reading of "import".
    #[test]
    fn importing_adds_to_what_is_already_there() {
        with_temp_config(|m| {
            let path = write_hypridle(FIVE_LISTENERS);
            std::fs::write(path.with_extension("conf.hyprforge.bak"), FIVE_LISTENERS).unwrap();
            m.idle.listeners.push(idle::Listener {
                timeout: 42,
                on_timeout: "mine".to_string(),
                ..idle::Listener::default()
            });
            m.tab = Tab::Idle;
            let _ = m.update(Message::AdoptOpen);
            let _ = m.update(Message::AdoptConfirm);

            assert_eq!(m.idle.listeners.len(), 3);
            assert_eq!(m.idle.listeners[0].on_timeout, "mine", "the user's own must survive");
        });
    }

    /// Blank is a value on this field, not a missing one. It means
    /// "whatever the portal does by default" — which is different from
    /// every number the field can hold, 0 included, since 0 means no
    /// limit at all.
    #[test]
    fn an_empty_frame_rate_means_the_portals_default_not_no_limit() {
        with_temp_config(|m| {
            m.tab = Tab::ScreenSharing;
            let _ = m.update(Message::PortalChanged(Field::MaxFps, "60".to_string()));
            let _ = m.update(Message::Commit);
            assert_eq!(m.portal.max_fps, Some(60));

            let _ = m.update(Message::PortalChanged(Field::MaxFps, String::new()));
            let _ = m.update(Message::Commit);
            assert_eq!(m.portal.max_fps, None, "blank must not become Some(0)");
        });
    }

    #[test]
    fn a_frame_rate_that_is_not_a_number_keeps_what_was_typed_and_says_so() {
        with_temp_config(|m| {
            m.tab = Tab::ScreenSharing;
            let _ = m.update(Message::PortalChanged(Field::MaxFps, "sixty".to_string()));
            let _ = m.update(Message::Commit);
            assert_eq!(m.portal.max_fps, None);
            assert_eq!(
                m.drafts.get(&(0, Field::MaxFps)).map(String::as_str),
                Some("sixty"),
                "what was typed must survive"
            );
            assert!(m.error.is_some(), "and be reported");
        });
    }

    /// The portal's own defaults must not be baked into a file. A later
    /// xdph could change one and this file would keep the old value
    /// forever with nothing to say why.
    #[test]
    fn a_screen_sharing_save_writes_only_what_was_changed() {
        with_temp_config(|m| {
            m.tab = Tab::ScreenSharing;
            let _ = m.update(Message::PortalToggled(Field::ForceShm, true));

            // `save` returns the apply as a Task, which no unit test
            // runs, so the generated file is produced here directly —
            // from the module's own state, which is what the wiring
            // under test actually decides.
            let generated = hyprforge_core::paths::hypr_hyprforge_dir().join("xdph.conf");
            let target = hyprforge_core::paths::hypr_config_dir().join("xdph.conf");
            hyprforge_ecosystem::apply::portal(&generated, &target, &m.portal)
                .expect("writing the generated config");
            let out = std::fs::read_to_string(&generated).expect("the generated file");
            assert!(out.contains("force_shm = true"), "{out}");
            assert!(!out.contains("max_fps"), "{out}");
            assert!(!out.contains("cursor_mode"), "{out}");
        });
    }

    /// Importing an `xdph.conf` someone wrote by hand replaces the
    /// block rather than appending — unlike listeners and wallpapers,
    /// `screencopy` is one settings block and there is only ever one.
    #[test]
    fn importing_screen_sharing_settings_replaces_rather_than_appends() {
        with_temp_config(|m| {
            let path = DesktopModule::daemon_config_path(Tab::ScreenSharing);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let body = "screencopy {\n    max_fps = 30\n    force_shm = true\n}\n";
            std::fs::write(&path, body).unwrap();
            std::fs::write(path.with_extension("conf.hyprforge.bak"), body).unwrap();

            m.tab = Tab::ScreenSharing;
            let _ = m.update(Message::AdoptOpen);
            assert_eq!(m.adopt.as_ref().unwrap().candidates.len(), 1);
            let _ = m.update(Message::AdoptConfirm);

            assert_eq!(m.portal.max_fps, Some(30));
            assert!(m.portal.force_shm);
        });
    }

    /// `general:toplevel_dynamic_bind` is real but undocumented, so the
    /// screen won't put a control on it. It still has to be *visible*,
    /// or a user who set it sees a clean import and loses it silently.
    #[test]
    fn an_unmodelled_portal_setting_is_shown_rather_than_swallowed() {
        with_temp_config(|m| {
            let path = DesktopModule::daemon_config_path(Tab::ScreenSharing);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "general {\n    toplevel_dynamic_bind = true\n}\n").unwrap();

            m.tab = Tab::ScreenSharing;
            let _ = m.update(Message::AdoptOpen);
            let review = m.adopt.as_ref().expect("a review");
            assert_eq!(review.problems.len(), 1, "{:?}", review.problems);
            assert!(review.problems[0].1.contains("general"), "{:?}", review.problems);
        });
    }

    #[test]
    fn a_new_module_has_nothing_configured() {
        with_temp_config(|m| {
            assert!(m.wallpapers.is_empty());
            assert!(m.sunset.is_empty());
            assert!(m.idle.is_empty());
        });
    }

    /// Typing a session command and pressing nothing else must still
    /// reach the disk, because there is nothing else to press: the field
    /// submits to `Commit`, and `Commit` only walks `self.drafts`, which
    /// this message never populated. `before_sleep_cmd` is what locks
    /// the screen before suspend.
    #[test]
    fn a_typed_session_command_reaches_the_disk() {
        with_temp_config(|m| {
            let _ = m.update(Message::GeneralCommandChanged(
                "before_sleep_cmd",
                "hyprforge-lock".to_string(),
            ));

            let written: idle::Settings =
                hyprforge_ecosystem::storage::load(&idle_toml())
                    .expect("the idle settings must have been written");
            assert_eq!(
                written.general.before_sleep_cmd, "hyprforge-lock",
                "the command was accepted on screen but never saved"
            );
        });
    }

    /// A new wallpaper starts as the fallback with no image, and
    /// `invalid()` says so until one is chosen — better than inventing a
    /// path nobody picked.
    #[test]
    fn a_new_wallpaper_starts_empty_and_is_reported_until_filled() {
        with_temp_config(|m| {
            let _ = m.update(Message::WallpaperAdded);
            assert_eq!(m.wallpapers.entries.len(), 1);
            assert!(m.wallpapers.entries[0].is_fallback());
            assert_eq!(m.wallpapers.invalid().len(), 1);

            let _ = m.update(Message::WallpaperChanged(0, Field::Path, "/w/a.png".into()));
            assert_eq!(m.wallpapers.entries[0].path, "/w/a.png");
            assert_eq!(m.wallpapers.invalid(), vec![]);
        });
    }

    /// "All screens" is the label for an empty `monitor =`, and it has to
    /// map back to empty — a literal "All screens" would be a monitor
    /// name hyprpaper never matches.
    #[test]
    fn choosing_all_screens_stores_an_empty_monitor() {
        with_temp_config(|m| {
            let _ = m.update(Message::WallpaperAdded);
            let _ = m.update(Message::WallpaperChanged(0, Field::Monitor, "eDP-2".into()));
            assert_eq!(m.wallpapers.entries[0].monitor, "eDP-2");
            let _ = m.update(Message::WallpaperChanged(0, Field::Monitor, String::new()));
            assert!(m.wallpapers.entries[0].is_fallback());
        });
    }

    /// Typed numbers wait for Apply; a picked value doesn't. Committing
    /// per keystroke would rewrite a config file and poke a daemon on
    /// every character.
    #[test]
    fn typed_fields_wait_for_apply_but_picked_ones_do_not() {
        with_temp_config(|m| {
            let _ = m.update(Message::ProfileAdded);
            let _ = m.update(Message::ProfileChanged(0, Field::Temperature, "4000".into()));
            assert_eq!(m.sunset.profiles[0].temperature, 6000, "still a draft");
            let _ = m.update(Message::Commit);
            assert_eq!(m.sunset.profiles[0].temperature, 4000);
            assert!(m.drafts.is_empty());
        });
    }

    /// One bad field must not discard the others — the user typed those
    /// too.
    #[test]
    fn a_bad_value_keeps_what_was_typed_and_lets_the_rest_through() {
        with_temp_config(|m| {
            let _ = m.update(Message::ProfileAdded);
            let _ = m.update(Message::ProfileChanged(0, Field::Time, "25:00".into()));
            let _ = m.update(Message::ProfileChanged(0, Field::Temperature, "4000".into()));
            let _ = m.update(Message::Commit);

            assert_eq!(m.sunset.profiles[0].temperature, 4000, "the good one applied");
            assert_eq!(m.sunset.profiles[0].time, "00:00", "the bad one didn't");
            assert_eq!(
                m.drafts.get(&(0, Field::Time)).map(String::as_str),
                Some("25:00"),
                "what was typed must survive"
            );
            assert!(m.error.is_some(), "and be reported");
        });
    }

    #[test]
    fn an_out_of_range_temperature_is_refused() {
        with_temp_config(|m| {
            let _ = m.update(Message::ProfileAdded);
            for bad in ["500", "99999", "warm"] {
                let _ = m.update(Message::ProfileChanged(0, Field::Temperature, bad.into()));
                let _ = m.update(Message::Commit);
                assert_ne!(m.sunset.profiles[0].temperature.to_string(), bad, "{bad}");
            }
        });
    }

    #[test]
    fn a_zero_idle_timeout_is_refused() {
        with_temp_config(|m| {
            let _ = m.update(Message::ListenerAdded);
            let _ = m.update(Message::ListenerChanged(0, Field::IdleTimeout, "0".into()));
            let _ = m.update(Message::Commit);
            assert_eq!(m.idle.listeners[0].timeout, 300, "the default is kept");
        });
    }

    /// Shell commands are stored exactly as written — Hyprforge never
    /// runs them, and quoting or escaping would change what does.
    #[test]
    fn a_shell_command_is_stored_verbatim() {
        with_temp_config(|m| {
            let _ = m.update(Message::ListenerAdded);
            let command = "pidof hyprlock || hyprlock";
            let _ = m.update(Message::ListenerChanged(0, Field::OnTimeout, command.into()));
            let _ = m.update(Message::Commit);
            assert_eq!(m.idle.listeners[0].on_timeout, command);
        });
    }

    /// Removing a row must take its half-typed drafts with it, or they'd
    /// reappear against whatever row slid into that index.
    #[test]
    fn removing_a_row_discards_its_drafts() {
        with_temp_config(|m| {
            let _ = m.update(Message::ProfileAdded);
            let _ = m.update(Message::ProfileChanged(0, Field::Time, "21:00".into()));
            let _ = m.update(Message::ProfileRemoved(0));
            assert!(m.sunset.profiles.is_empty());
            assert!(m.drafts.is_empty());
        });
    }

    /// The load-bearing honesty on this screen: hypridle has no IPC, so
    /// a save genuinely does nothing until it restarts, and the status
    /// must say which of the three things happened.
    #[test]
    fn each_apply_outcome_gets_its_own_message() {
        with_temp_config(|m| {
            let _ = m.update(Message::Applied(Tab::Idle, Ok(Applied::NeedsRestart)));
            assert!(m.idle_needs_restart);
            let status = m.status.clone().unwrap();
            assert!(status.contains("restart"), "{status}");

            let _ = m.update(Message::Applied(Tab::Wallpaper, Ok(Applied::Live)));
            assert!(!m.idle_needs_restart, "a live apply clears the restart notice");
            assert!(m.status.clone().unwrap().contains("applied"));

            let _ = m.update(Message::Applied(Tab::Wallpaper, Ok(Applied::DaemonNotRunning)));
            assert!(m.status.clone().unwrap().contains("isn't running"));

            let _ = m.update(Message::Applied(
                Tab::Wallpaper,
                Ok(Applied::PartlyRefused(vec!["/w/gone.png".into()])),
            ));
            assert!(m.error.is_some(), "a refusal is an error, not a status");
        });
    }

    /// The rule that keeps the data-loss bug from returning.
    #[test]
    fn an_unreadable_store_blocks_saving() {
        with_temp_config(|m| {
            m.store_unreadable = Some("bad toml".into());
            let _ = m.update(Message::WallpaperAdded);
            let _ = m.update(Message::WallpaperRemoved(0));
            assert!(m.error.is_some(), "the user has to be told why");
        });
    }

    /// An image on another disk, or a folder that moved, would otherwise
    /// vanish from its own picker — and the next pick would replace a
    /// working wallpaper.
    #[test]
    fn a_picker_keeps_a_path_discovery_did_not_find() {
        let known = vec!["/w/a.png".to_string()];
        let options = options_including(&known, "/elsewhere/b.png");
        assert_eq!(options.len(), 2);
        assert_eq!(options[0], "/elsewhere/b.png", "kept, and first so it's visible");
        assert_eq!(options_including(&known, "/w/a.png").len(), 1, "no duplicate");
        assert_eq!(options_including(&known, "").len(), 1);
    }

    /// Typing in one tab, switching, and pressing Apply must still save
    /// the first tab — otherwise the edit sits in memory and is never
    /// written.
    #[test]
    fn applying_saves_every_tab_that_was_edited_not_just_the_open_one() {
        with_temp_config(|m| {
            let _ = m.update(Message::WallpaperAdded);
            let _ = m.update(Message::ProfileAdded);
            let _ = m.update(Message::TabSelected(Tab::Wallpaper));
            let _ = m.update(Message::WallpaperChanged(0, Field::Timeout, "45".into()));
            let _ = m.update(Message::TabSelected(Tab::NightLight));
            let _ = m.update(Message::ProfileChanged(0, Field::Temperature, "4000".into()));

            let _ = m.update(Message::Commit);
            assert_eq!(m.wallpapers.entries[0].timeout, Some(45), "the other tab's edit applied");
            assert_eq!(m.sunset.profiles[0].temperature, 4000);
            assert!(m.drafts.is_empty());
        });
    }

    /// Removing row 1 makes row 2 become row 1, so a draft keyed to index
    /// 2 would reappear against a different row's values.
    #[test]
    fn removing_a_row_shifts_the_later_rows_drafts_down() {
        with_temp_config(|m| {
            for _ in 0..3 {
                let _ = m.update(Message::ProfileAdded);
            }
            let _ = m.update(Message::ProfileChanged(2, Field::Temperature, "4000".into()));
            let _ = m.update(Message::ProfileRemoved(0));

            assert_eq!(m.sunset.profiles.len(), 2);
            assert_eq!(
                m.drafts.get(&(1, Field::Temperature)).map(String::as_str),
                Some("4000"),
                "the draft must follow its row down"
            );
            assert!(!m.drafts.contains_key(&(2, Field::Temperature)));
        });
    }

    /// The three lists share one draft map but have separate indices, so
    /// removing a wallpaper must not disturb a profile's drafts.
    #[test]
    fn removing_a_row_leaves_other_tabs_drafts_alone() {
        with_temp_config(|m| {
            let _ = m.update(Message::WallpaperAdded);
            let _ = m.update(Message::ProfileAdded);
            let _ = m.update(Message::ProfileChanged(0, Field::Temperature, "4000".into()));
            let _ = m.update(Message::WallpaperRemoved(0));
            assert_eq!(
                m.drafts.get(&(0, Field::Temperature)).map(String::as_str),
                Some("4000")
            );
        });
    }

    /// A refused push means the setting is saved but the screen did not
    /// change — calling that "applied" is the failure this project keeps
    /// meeting.
    #[test]
    fn a_refused_push_is_reported_as_an_error_not_a_success() {
        with_temp_config(|m| {
            let _ = m.update(Message::Applied(
                Tab::Wallpaper,
                Ok(Applied::PartlyRefused(vec!["/w/gone.png".into()])),
            ));
            assert!(m.status.is_none(), "it did not succeed");
            let error = m.error.clone().unwrap();
            assert!(error.contains("/w/gone.png"), "{error}");
        });
    }

    /// Builds every tab in each state it can be in.
    #[test]
    fn the_screen_builds_in_every_state() {
        with_temp_config(|m| {
            let scale = FontScale::default();
            for tab in Tab::ALL {
                let _ = m.update(Message::TabSelected(tab));
                let _ = m.view(scale);
            }

            let _ = m.update(Message::Loaded(Loaded {
                images: vec!["/w/a.png".into()],
                monitors: vec!["eDP-2".into()],
            }));
            let _ = m.update(Message::WallpaperAdded);
            let _ = m.update(Message::ProfileAdded);
            let _ = m.update(Message::ListenerAdded);
            m.idle_needs_restart = true;
            m.error = Some("something failed".into());
            m.status = Some("saved".into());
            m.store_unreadable = Some("bad toml".into());
            for tab in Tab::ALL {
                let _ = m.update(Message::TabSelected(tab));
                let _ = m.view(scale);
            }
        });
    }

    // --- tray preferences: keep awake / night light ------------------------

    /// Like `with_temp_config`, but `setup` runs against the temp directory
    /// *before* `DesktopModule::new()` is called — needed for a test that
    /// wants a `tray.toml` already on disk (or already corrupt) for the
    /// constructor itself to read. `with_temp_config` can't do this: its
    /// `f` only runs after the module is built.
    fn with_temp_config_setup<T>(
        setup: impl FnOnce(&std::path::Path),
        f: impl FnOnce(&mut DesktopModule) -> T,
    ) -> T {
        crate::modules::with_temp_env(|dir| {
            setup(dir);
            let (mut module, _) = DesktopModule::new();
            f(&mut module)
        })
    }

    /// A missing `tray.toml` is first run: the two icons this screen owns
    /// default to *off* (unlike network/bluetooth), and nothing about a
    /// missing file is reported as an error — the same distinction
    /// `hyprforge_tray::prefs::load` documents, one layer up.
    #[test]
    fn a_missing_tray_toml_is_first_run_and_both_new_icons_read_as_off() {
        with_temp_config(|m| {
            let prefs = m.tray_prefs.as_ref().expect("a missing file is defaults, not an error");
            assert!(!prefs.keep_awake, "keep_awake defaults to off");
            assert!(!prefs.night_light, "night_light defaults to off");
            assert!(m.error.is_none(), "a first run is not a load failure");
        });
    }

    /// The rule this feature is most likely to break: a `tray.toml` that
    /// exists and will not parse must be reported in the screen's own error
    /// banner, and a subsequent toggle (of either icon this screen owns)
    /// must not then overwrite it with a freshly-guessed default.
    #[test]
    fn an_unreadable_tray_toml_is_reported_and_not_overwritten_by_a_toggle() {
        with_temp_config_setup(
            |dir| {
                let tray_toml = dir.join("hyprforge").join("tray.toml");
                std::fs::create_dir_all(tray_toml.parent().unwrap()).unwrap();
                std::fs::write(&tray_toml, "night_light = yes please\n").unwrap();
            },
            |m| {
                assert!(m.tray_prefs.is_err(), "a malformed file must not be treated as defaults");
                assert!(
                    m.error.as_ref().is_some_and(|e| e.contains("tray.toml")),
                    "the failure must reach the screen's own error banner, got {:?}",
                    m.error
                );

                let tray_toml = hyprforge_tray::prefs::path();
                let before = std::fs::read_to_string(&tray_toml).unwrap();
                let _ = m.update(Message::NightLightTrayToggled(false));
                let _ = m.update(Message::KeepAwakeTrayToggled(true));
                let after = std::fs::read_to_string(&tray_toml).unwrap();
                assert_eq!(
                    before, after,
                    "a toggle must never overwrite a file it could not read"
                );
            },
        );
    }

    /// Toggling the icon this screen owns must leave the *other three*
    /// icons' settings exactly as they were — never re-derived as a
    /// default, or switching keep-awake on would silently also flip
    /// network, bluetooth or night light.
    #[test]
    fn toggling_the_keep_awake_tray_setting_preserves_the_other_three() {
        with_temp_config(|m| {
            let _ = m.update(Message::KeepAwakeTrayToggled(true));
            let prefs = hyprforge_tray::prefs::load().unwrap();
            assert!(prefs.keep_awake, "the icon this screen owns was switched on");
            assert!(prefs.network, "network's setting must survive untouched");
            assert!(prefs.bluetooth, "bluetooth's setting must survive untouched");
            assert!(!prefs.night_light, "night_light's setting must survive untouched");
        });
    }

    /// Same property as above, for the other icon this screen owns.
    #[test]
    fn toggling_the_night_light_tray_setting_preserves_the_other_three() {
        with_temp_config(|m| {
            let _ = m.update(Message::NightLightTrayToggled(true));
            let prefs = hyprforge_tray::prefs::load().unwrap();
            assert!(prefs.night_light, "the icon this screen owns was switched on");
            assert!(prefs.network, "network's setting must survive untouched");
            assert!(prefs.bluetooth, "bluetooth's setting must survive untouched");
            assert!(!prefs.keep_awake, "keep_awake's setting must survive untouched");
        });
    }
}

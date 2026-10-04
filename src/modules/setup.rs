//! The Set up page: the steps that finish an install — services,
//! keybinds, the idle lock, notification blur, default apps, the
//! open/save dialog — each with its state, a switch, and Undo for what
//! setup did.
//!
//! The work is `hyprforge_setup`, the same library `--setup` drives from
//! a terminal (`setup_cli.rs`); this file is the page over it.
//!
//! # Nothing here touches the system on the UI thread
//!
//! Every check and every apply can run `systemctl --user`, `hyprctl` or
//! `pgrep` — each bounded, but bounded at seconds, and eighteen of them in
//! a row would freeze the window for as long as they took. So the module
//! holds no [`System`] at all. [`SetupModule::step`] turns a message into
//! a [`Job`] — plain data saying what to run — and only
//! [`SettingsModule::update`] hands that job to the blocking pool, where
//! it meets [`RealSystem`]. A page that cannot reach a `System` cannot
//! call one from `update`, and the tests check the jobs `step` returns
//! rather than trusting it not to; they run each job themselves against a
//! `MockSystem`, the way the pool would against the real one.
//!
//! # Other pages hold copies of the files setup writes
//!
//! Keybinds, Window rules, Idle & lock and Session each load their TOML
//! once and write the *whole* file back from memory on their next save.
//! An apply here that added a bind and left Keybinds' list as it was
//! would be undone by the next unrelated edit on that page — the
//! `shortcuts.toml` version of the 37-binds lesson. [`reloads`] says
//! which pages a finished job touched, and the shell reloads them before
//! anything else can save.

use crate::module::{NavBadge, SearchEntry, SettingsModule};
use hyprforge_setup::record::Record;
use hyprforge_setup::{Env, Item, Outcome, RealSystem, State, System, ITEMS};
use hyprforge_ui::density;
use hyprforge_ui::theme::{self, spacing, FontScale};
use hyprforge_ui::widgets::{
    chip, hint_text, primary_button, scaled_text, secondary_button, section_label, setting_list,
    setting_row, toggle, Tint,
};
use iced::widget::{column, row, Column};
use iced::{Alignment, Element, Length, Task};
use std::collections::BTreeMap;
use std::time::Duration;

/// How long after the window opens the first check may still move it to
/// this page.
///
/// The check runs off the UI thread so the window never waits for it,
/// which means it can land after the window is up. Normally that is a
/// few hundred milliseconds — a handful of `systemctl is-enabled` and one
/// `hyprctl binds` — and the page is in place before anyone has read the
/// one it replaced. A check slower than this (a `systemctl` that is
/// timing out) would instead yank the window away from a page someone is
/// already using; past this, Settings stays where it opened and the
/// sidebar entry's count says there is something to do.
pub const FIRST_LAUNCH_GRACE: Duration = Duration::from_secs(2);

/// What one check found: each item's state, in [`ITEMS`] order, and what
/// setup has recorded doing.
#[derive(Debug, Clone)]
pub struct Report {
    pub states: Vec<(&'static Item, State)>,
    /// `Err` when `setup.toml` exists and cannot be read. Undo and Apply
    /// both refuse then (the library does), so the page offers neither
    /// and says why.
    pub record: Result<Record, String>,
    /// Whether `setup.toml` exists at all — see [`opens_first`].
    pub has_record: bool,
}

/// What a finished apply, undo or "Turn off for me" did.
#[derive(Debug, Clone)]
pub struct Finished {
    /// The ids the job was asked about, whether or not each worked —
    /// what [`reloads`] reads, because a failed item may still have been
    /// rolled back through the very file another page holds a copy of.
    pub asked: Vec<&'static str>,
    /// `Err` when the record could not be read and nothing was touched.
    pub outcomes: Result<Vec<Outcome>, String>,
    /// A fresh check, taken in the same job, so the page shows the state
    /// the change left rather than the state before it.
    pub report: Report,
}

/// Something to run off the UI thread. See the module doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    Check,
    /// The ticked to-do items, in [`ITEMS`] order.
    Apply(Vec<&'static str>),
    Undo(&'static str),
    TurnOffForMe(&'static str),
}

impl Job {
    /// Runs the job against `sys`. Blocking: the shell calls this on the
    /// blocking pool, the tests call it directly with a `MockSystem`.
    pub fn run(self, env: &Env, sys: &dyn System) -> Message {
        let finished = |asked: Vec<&'static str>, outcomes| {
            Message::Finished(Box::new(Finished { asked, outcomes, report: check(env, sys) }))
        };
        match self {
            Job::Check => Message::Checked(Box::new(check(env, sys))),
            Job::Apply(ids) => {
                let outcomes = hyprforge_setup::apply(env, sys, &ids).map_err(|e| e.to_string());
                finished(ids, outcomes)
            }
            Job::Undo(id) => {
                let outcomes = hyprforge_setup::undo(env, sys, &[id]).map_err(|e| e.to_string());
                finished(vec![id], outcomes)
            }
            Job::TurnOffForMe(id) => {
                let outcomes = hyprforge_setup::turn_off_for_me(env, sys, id)
                    .map(|o| vec![o])
                    .map_err(|e| e.to_string());
                finished(vec![id], outcomes)
            }
        }
    }
}

fn check(env: &Env, sys: &dyn System) -> Report {
    Report {
        states: hyprforge_setup::check_all(env, sys),
        record: hyprforge_setup::record::load(&env.setup_toml()).map_err(|e| e.to_string()),
        has_record: hyprforge_setup::has_record(env),
    }
}

/// Whether the first check should move a newly opened window here: setup
/// has never recorded anything, and something is still to do.
///
/// Both halves matter. "Never recorded" alone would open this page on
/// every launch for someone who looked at it once and ticked nothing;
/// "something to do" alone would open it forever for someone who chose
/// to leave an item off.
pub fn opens_first(report: &Report) -> bool {
    !report.has_record && report.states.iter().any(|(_, s)| s.is_todo())
}

/// [`opens_first`], for the shell: only while it is still `waiting` —
/// nobody asked for a page with `--screen` and nobody has moved — and
/// only within [`FIRST_LAUNCH_GRACE`] of the window opening.
pub fn should_open_on_setup(waiting: bool, elapsed: Duration, report: &Report) -> bool {
    waiting && elapsed <= FIRST_LAUNCH_GRACE && opens_first(report)
}

/// Which other pages hold an in-memory copy of a file a job changed.
/// See the module doc.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Reloads {
    /// `shortcuts.toml` — the `bind-*` items.
    pub shortcuts: bool,
    /// `window-rules.toml` — notification blur's layer rules.
    pub window_rules: bool,
    /// `mimeapps.list` — the Default apps page re-reads the database.
    pub default_apps: bool,
    /// `idle.toml` — the idle lock's `lock_cmd`.
    pub idle: bool,
    /// `session.toml` — `GTK_USE_PORTAL`.
    pub session: bool,
}

impl Reloads {
    /// The pages `ids` reach.
    ///
    /// Wiring, the services, the portal file and the FileManager1 service
    /// file have no page holding a copy: the require lines are each
    /// page's own and installed when it opens, and the other three are
    /// files no page edits.
    pub fn after(ids: &[&str]) -> Reloads {
        let mut r = Reloads::default();
        for id in ids {
            match *id {
                id if id.starts_with("bind-") => r.shortcuts = true,
                "notif-blur" => r.window_rules = true,
                id if id.starts_with("default-") => r.default_apps = true,
                "idle-lock" => r.idle = true,
                "gtk-portal" => r.session = true,
                _ => {}
            }
        }
        r
    }
}

/// What the shell must reload before handing `message` to this page.
pub fn reloads(message: &Message) -> Reloads {
    match message {
        Message::Finished(f) => Reloads::after(&f.asked),
        _ => Reloads::default(),
    }
}

/// The page's groups, in the order they are drawn — which is [`ITEMS`]
/// order, so the page reads top to bottom in the order Apply runs.
fn group(item: &Item) -> &'static str {
    // The opt-in first: an item off by default is "optional" whatever
    // else it is about, and filing it beside the defaults would make it
    // look like one more thing that ought to be on.
    if !item.default_on {
        return "Optional";
    }
    match item.id {
        "wiring" => "Hyprland wiring",
        id if id.starts_with("service-") => "Background services",
        id if id.starts_with("bind-") => "Shortcuts",
        "idle-lock" | "notif-blur" => "Lock & notifications",
        _ => "Default apps & dialogs",
    }
}

/// What a row offers besides its switch.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Offers {
    /// Setup recorded changing this, so it can take it back.
    pub undo: bool,
    /// A service that is on, but not because setup turned it on — the
    /// packages preset it for every user — so the only way off for this
    /// user is a mask. Never offered for one setup enabled: that one's
    /// Undo is the way off.
    pub turn_off: bool,
}

/// See [`Offers`]. `record` is `None` when `setup.toml` could not be
/// read, and then nothing is offered: both would be refused.
pub fn offers(item: &Item, state: &State, record: Option<&Record>) -> Offers {
    let Some(record) = record else { return Offers::default() };
    let recorded = record.items.contains_key(item.id);
    let masked_by_setup = item.unit().is_some_and(|u| record.masked.iter().any(|m| m == u));
    Offers {
        undo: recorded || masked_by_setup,
        turn_off: *state == State::Done && item.unit().is_some() && !recorded,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Busy {
    Checking,
    Applying,
}

pub struct SetupModule {
    env: Env,
    /// `None` until the first check comes back.
    report: Option<Report>,
    /// The switches, by item id. Kept across re-checks so a re-check never
    /// flips back a switch someone turned off; preset to the item's
    /// default the first time it is seen as to do.
    ticked: BTreeMap<&'static str, bool>,
    /// The last job's outcome per item, shown under its row until the
    /// next job.
    outcomes: BTreeMap<&'static str, Result<String, String>>,
    busy: Option<Busy>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Message {
    /// Check every item again.
    Check,
    Checked(Box<Report>),
    Toggled(&'static str, bool),
    Apply,
    Undo(&'static str),
    TurnOffForMe(&'static str),
    Finished(Box<Finished>),
    /// The blocking task itself failed — a panic in the pool. Said, never
    /// swallowed, or the page would sit on "applying…" forever.
    Crashed(String),
}

impl SetupModule {
    /// Starts the first check. The window does not wait for it.
    pub fn new() -> (Self, Task<Message>) {
        let mut module = SetupModule::with_env(Env::from_environment());
        let job = module.step(Message::Check);
        let task = job.map_or_else(Task::none, |job| spawn(module.env.clone(), job));
        (module, task)
    }

    /// A module over `env`, with no check started — for the tests.
    pub fn with_env(env: Env) -> SetupModule {
        SetupModule {
            env,
            report: None,
            ticked: BTreeMap::new(),
            outcomes: BTreeMap::new(),
            busy: None,
            error: None,
        }
    }

    /// The to-do items whose switch is on, in [`ITEMS`] order — what
    /// Apply sends. An item ticked while it was to do and since found
    /// done, unavailable or unknown is not in it: the library would only
    /// re-check and refuse it.
    pub fn to_apply(&self) -> Vec<&'static str> {
        let Some(report) = &self.report else { return Vec::new() };
        report
            .states
            .iter()
            .filter(|(item, state)| state.is_todo() && self.ticked.get(item.id).copied().unwrap_or(false))
            .map(|(item, _)| item.id)
            .collect()
    }

    fn todo_count(&self) -> usize {
        self.report.as_ref().map_or(0, |r| r.states.iter().filter(|(_, s)| s.is_todo()).count())
    }

    fn record(&self) -> Option<&Record> {
        self.report.as_ref().and_then(|r| r.record.as_ref().ok())
    }

    fn adopt(&mut self, report: Report) {
        for (item, state) in &report.states {
            if state.is_todo() {
                self.ticked.entry(item.id).or_insert(item.default_on);
            }
        }
        self.error = report.record.as_ref().err().cloned();
        self.report = Some(report);
    }

    /// The pure half of `update`: changes the page and says what to run,
    /// without running it. See the module doc.
    pub fn step(&mut self, message: Message) -> Option<Job> {
        match message {
            Message::Check => {
                if self.busy.is_some() {
                    return None;
                }
                self.busy = Some(Busy::Checking);
                Some(Job::Check)
            }
            Message::Checked(report) => {
                self.busy = None;
                self.adopt(*report);
                None
            }
            Message::Toggled(id, on) => {
                self.ticked.insert(id, on);
                None
            }
            Message::Apply => {
                let ids = self.to_apply();
                if self.busy.is_some() || ids.is_empty() {
                    return None;
                }
                self.begin();
                Some(Job::Apply(ids))
            }
            Message::Undo(id) => {
                if self.busy.is_some() {
                    return None;
                }
                self.begin();
                Some(Job::Undo(id))
            }
            Message::TurnOffForMe(id) => {
                if self.busy.is_some() {
                    return None;
                }
                self.begin();
                Some(Job::TurnOffForMe(id))
            }
            Message::Finished(finished) => {
                let Finished { outcomes, report, .. } = *finished;
                self.busy = None;
                self.adopt(report);
                match outcomes {
                    Ok(outcomes) => {
                        for o in outcomes {
                            self.outcomes.insert(o.id, o.result);
                        }
                    }
                    // After `adopt`, which clears the error when the record
                    // reads — this one is about the job, and must show.
                    Err(e) => self.error = Some(e),
                }
                None
            }
            Message::Crashed(e) => {
                self.busy = None;
                self.error = Some(format!("Setup stopped partway: {e}. Check again to see where it got to."));
                None
            }
        }
    }

    fn begin(&mut self) {
        self.busy = Some(Busy::Applying);
        self.outcomes.clear();
        self.error = None;
    }
}

/// Runs `job` on the blocking pool against the real system.
fn spawn(env: Env, job: Job) -> Task<Message> {
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || job.run(&env, &RealSystem))
                .await
                .unwrap_or_else(|e| Message::Crashed(e.to_string()))
        },
        std::convert::identity,
    )
}

impl SettingsModule for SetupModule {
    type Message = Message;

    fn update(&mut self, message: Message) -> Task<Message> {
        match self.step(message) {
            Some(job) => spawn(self.env.clone(), job),
            None => Task::none(),
        }
    }

    fn view(&self, scale: FontScale) -> Element<'_, Message> {
        let mut content = column![].spacing(spacing::LG);
        if let Some(e) = &self.error {
            content = content.push(
                scaled_text(e.clone(), density::META_TEXT_BASE, scale).color(theme::warning()),
            );
        }
        match &self.report {
            None => {
                content = content.push(hint_text("Checking what is left to do…", scale));
            }
            Some(report) => {
                let mut groups: Vec<(&'static str, Vec<Element<'_, Message>>)> = Vec::new();
                for (item, state) in &report.states {
                    let name = group(item);
                    if groups.last().is_none_or(|(g, _)| *g != name) {
                        groups.push((name, Vec::new()));
                    }
                    let rows = &mut groups.last_mut().expect("just pushed").1;
                    let index = rows.len();
                    rows.push(self.item_row(index, item, state, scale));
                }
                for (name, rows) in groups {
                    content = content.push(
                        column![section_label(name, scale), setting_list(rows)].spacing(spacing::SM),
                    );
                }
            }
        }
        iced::widget::container(content).padding(spacing::LG).width(Length::Fill).into()
    }

    fn subtitle(&self) -> Option<String> {
        match (self.busy, &self.report) {
            (Some(Busy::Checking), None) => Some("checking…".into()),
            (Some(Busy::Applying), _) => Some("applying…".into()),
            (_, None) => None,
            (_, Some(_)) => Some(match self.todo_count() {
                0 => "nothing left to do".into(),
                n => format!("{n} to do"),
            }),
        }
    }

    fn header_actions(&self, _scale: FontScale) -> Option<Element<'_, Message>> {
        let idle = self.busy.is_none();
        let n = self.to_apply().len();
        let label = match n {
            0 => "Apply".to_string(),
            n => format!("Apply {n}"),
        };
        Some(
            row![
                secondary_button("Check again").on_press_maybe(idle.then_some(Message::Check)),
                primary_button(label).on_press_maybe((idle && n > 0).then_some(Message::Apply)),
            ]
            .spacing(spacing::SM)
            .into(),
        )
    }

    /// Every item, by its label and its id — "clipboard" finds Super+V,
    /// `bind-files` finds Super+E. Revealing one is just opening the page:
    /// the list is short enough to be its own index.
    fn search_entries(&self) -> Vec<SearchEntry<Message>> {
        let state = |id: &str| {
            self.report
                .as_ref()
                .and_then(|r| r.states.iter().find(|(i, _)| i.id == id))
                .map(|(_, s)| s.name().to_string())
        };
        ITEMS
            .iter()
            .map(|item| SearchEntry { label: item.label, key: item.id, value: state(item.id), reveal: Vec::new() })
            .collect()
    }

    /// How many steps are still to do, so the sidebar says so from any
    /// page — the way back here for someone who opened Settings after
    /// [`FIRST_LAUNCH_GRACE`] passed.
    fn nav_badge(&self) -> Option<NavBadge> {
        match self.todo_count() {
            0 => None,
            n => Some(NavBadge::Text(n.to_string())),
        }
    }
}

impl SetupModule {
    /// One item: its label, why, and what it is now (or what the last job
    /// did to it); at the right, its switch or its state, and Undo or
    /// Turn off for me when either applies.
    fn item_row<'a>(&'a self, index: usize, item: &'static Item, state: &'a State, scale: FontScale) -> Element<'a, Message> {
        let idle = self.busy.is_none();
        let mut hint: Column<'a, Message> = column![hint_text(item.why, scale)].spacing(2.0);
        // The last job's word on this item replaces the state's reason:
        // it is newer, and a failure must not be crowded out by a "to do"
        // that only restates it.
        match self.outcomes.get(item.id) {
            Some(Ok(said)) => {
                hint = hint.push(
                    scaled_text(format!("✓ {said}"), density::META_TEXT_BASE, scale).color(theme::success()),
                );
            }
            Some(Err(said)) => {
                hint = hint.push(
                    scaled_text(format!("✗ {said}"), density::META_TEXT_BASE, scale).color(theme::error()),
                );
            }
            None if !state.reason().is_empty() => {
                let line = scaled_text(state.reason().to_string(), density::META_TEXT_BASE, scale);
                // Unknown is the one reason that is a problem rather than a
                // fact: a check that could not run.
                hint = hint.push(match state {
                    State::Unknown { .. } => line.color(theme::warning()),
                    _ => line.color(theme::text_dim()),
                });
            }
            None => {}
        }

        let offers = offers(item, state, self.record());
        let mut control = row![].spacing(spacing::SM).align_y(Alignment::Center);
        if offers.turn_off {
            control = control.push(
                secondary_button("Turn off for me")
                    .on_press_maybe(idle.then_some(Message::TurnOffForMe(item.id))),
            );
        }
        if offers.undo {
            control = control.push(secondary_button("Undo").on_press_maybe(idle.then_some(Message::Undo(item.id))));
        }
        let state_control: Element<'a, Message> = match state {
            State::Todo { .. } => {
                let on = self.ticked.get(item.id).copied().unwrap_or(item.default_on);
                toggle(on, scale)
                    .on_toggle_maybe(idle.then_some(move |on| Message::Toggled(item.id, on)))
                    .into()
            }
            State::Done => chip("✓ done", Tint::Success, scale),
            State::Unavailable { .. } => chip("unavailable", Tint::Dim, scale),
            State::Unknown { .. } => chip("couldn't check", Tint::Warning, scale),
        };
        control = control.push(state_control);
        setting_row(index, item.label, Some(hint.into()), control, scale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyprforge_setup::mock::{MockSystem, MockUnit};

    struct Rig {
        _dir: tempfile::TempDir,
        env: Env,
        sys: MockSystem,
    }

    fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::rooted_at(dir.path());
        std::fs::create_dir_all(env.hypr_dir()).unwrap();
        std::fs::write(env.hyprland_lua(), "-- mine\n").unwrap();
        Rig { _dir: dir, env, sys: MockSystem::with_suite_installed() }
    }

    /// Steps `message` and, if it asked for a job, runs that job the way
    /// the blocking pool would and feeds the result back. Returns the job.
    fn drive(m: &mut SetupModule, r: &Rig, message: Message) -> Option<Job> {
        let job = m.step(message)?;
        let reply = job.clone().run(&r.env, &r.sys);
        assert!(m.step(reply).is_none(), "a reply never starts another job");
        Some(job)
    }

    fn checked(r: &Rig) -> SetupModule {
        let mut m = SetupModule::with_env(r.env.clone());
        assert_eq!(drive(&mut m, r, Message::Check), Some(Job::Check));
        m
    }

    fn report(r: &Rig) -> Report {
        check(&r.env, &r.sys)
    }

    #[test]
    fn a_first_launch_with_nothing_recorded_and_something_to_do_opens_on_set_up() {
        let r = rig();
        assert!(opens_first(&report(&r)));
        assert!(should_open_on_setup(true, Duration::from_millis(300), &report(&r)));
    }

    /// Once setup has recorded anything, it is not a first launch, even
    /// with items left to do — someone chose to leave them.
    #[test]
    fn a_launch_after_setup_recorded_something_opens_as_usual() {
        let r = rig();
        hyprforge_setup::apply(&r.env, &r.sys, &["bind-files"]).unwrap();
        let after = report(&r);
        assert!(after.states.iter().any(|(_, s)| s.is_todo()));
        assert!(!opens_first(&after));
    }

    #[test]
    fn a_launch_with_nothing_to_do_opens_as_usual() {
        let r = rig();
        let mut nothing = report(&r);
        for (_, s) in &mut nothing.states {
            *s = State::Done;
        }
        assert!(!nothing.has_record);
        assert!(!opens_first(&nothing));
    }

    /// `--screen` asked for a page: the first check never overrides it,
    /// and neither does one that came back too late or after the user
    /// moved.
    #[test]
    fn a_requested_screen_or_a_slow_check_keeps_the_window_where_it_is() {
        let r = rig();
        let rep = report(&r);
        assert!(!should_open_on_setup(false, Duration::from_millis(10), &rep), "--screen wins");
        assert!(!should_open_on_setup(true, FIRST_LAUNCH_GRACE + Duration::from_millis(1), &rep));
    }

    /// Apply sends exactly the switched-on to-do items, in apply order —
    /// not the opt-in nobody ticked, not one switched off, and not in the
    /// order they were clicked.
    #[test]
    fn apply_sends_only_ticked_todo_ids_in_items_order() {
        let r = rig();
        let mut m = checked(&r);
        m.step(Message::Toggled("bind-files", false));
        m.step(Message::Toggled("gtk-portal", true));
        let job = m.step(Message::Apply).expect("something to apply");
        let Job::Apply(ids) = job else { panic!("{job:?}") };
        let expected: Vec<&str> = ITEMS
            .iter()
            .filter(|i| (i.default_on && i.id != "bind-files") || i.id == "gtk-portal")
            .filter(|i| m.report.as_ref().unwrap().states.iter().any(|(j, s)| j.id == i.id && s.is_todo()))
            .map(|i| i.id)
            .collect();
        assert_eq!(ids, expected);
        assert!(!ids.contains(&"bind-files"));
        assert!(ids.contains(&"gtk-portal"));
    }

    #[test]
    fn the_opt_in_item_is_presented_switched_off() {
        let r = rig();
        let m = checked(&r);
        assert_eq!(m.ticked.get("gtk-portal"), Some(&false));
        assert_eq!(m.ticked.get("bind-files"), Some(&true));
        assert!(!m.to_apply().contains(&"gtk-portal"));
    }

    /// The structural half of "never on the UI thread": every message that
    /// does work comes back from `step` as a job, and stepping it changes
    /// nothing on disk and calls nothing — the job does that, later,
    /// wherever it is run.
    #[test]
    fn the_page_never_calls_the_system_while_stepping() {
        let r = rig();
        let mut m = checked(&r);
        let calls_before = r.sys.calls();
        for message in [Message::Apply, Message::Undo("bind-files"), Message::TurnOffForMe("service-trayd")] {
            let mut m2 = SetupModule::with_env(r.env.clone());
            m2.report = m.report.clone();
            m2.ticked = m.ticked.clone();
            assert!(m2.step(message.clone()).is_some(), "{message:?} returned no job");
        }
        assert_eq!(r.sys.calls(), calls_before);
        assert!(!r.env.setup_toml().exists(), "stepping wrote the record");
        assert!(!r.env.shortcuts_toml().exists(), "stepping wrote shortcuts");
        // While a job is out, nothing starts a second one over it.
        assert!(m.step(Message::Apply).is_some());
        assert!(m.step(Message::Check).is_none());
        assert!(m.step(Message::Undo("bind-files")).is_none());
    }

    /// Applying through the page records each outcome under its row and
    /// leaves the page showing the state the change produced.
    #[test]
    fn apply_shows_each_outcome_and_rechecks() {
        let r = rig();
        let mut m = checked(&r);
        drive(&mut m, &r, Message::Apply).expect("a job");
        assert!(matches!(m.outcomes.get("bind-files"), Some(Ok(_))), "{:?}", m.outcomes);
        let state = m.report.as_ref().unwrap().states.iter().find(|(i, _)| i.id == "bind-files").unwrap();
        assert_eq!(state.1, State::Done);
        assert!(m.busy.is_none());
        let _ = m.view(FontScale::default());
    }

    #[test]
    fn a_done_item_setup_recorded_offers_undo_and_one_it_did_not_does_not() {
        let r = rig();
        hyprforge_setup::apply(&r.env, &r.sys, &["bind-files"]).unwrap();
        // Bound by the user, not by setup: done, but not setup's to undo.
        r.sys.set_unit("hyprforge-clipd.service", MockUnit { enabled_for_user: true, ..MockUnit::default() });
        let rep = report(&r);
        let state = |id: &str| rep.states.iter().find(|(i, _)| i.id == id).unwrap().1.clone();
        let record = rep.record.as_ref().ok();

        let bind = hyprforge_setup::item("bind-files").unwrap();
        assert_eq!(state("bind-files"), State::Done);
        assert!(offers(bind, &state("bind-files"), record).undo);

        let clipd = hyprforge_setup::item("service-clipd").unwrap();
        assert_eq!(state("service-clipd"), State::Done);
        assert!(!offers(clipd, &state("service-clipd"), record).undo);
    }

    /// A unit the packages enabled for everyone: done, not setup's, so
    /// the off switch is a mask for this user — and once masked, Undo
    /// takes the mask back.
    #[test]
    fn a_preset_enabled_service_offers_turn_off_for_me() {
        let r = rig();
        r.sys.set_unit("hyprforge-trayd.service", MockUnit { enabled_globally: true, active: true, ..MockUnit::default() });
        let mut m = checked(&r);
        let trayd = hyprforge_setup::item("service-trayd").unwrap();
        let state = m.report.as_ref().unwrap().states.iter().find(|(i, _)| i.id == trayd.id).unwrap().1.clone();
        assert_eq!(state, State::Done);
        assert_eq!(offers(trayd, &state, m.record()), Offers { undo: false, turn_off: true });

        assert_eq!(drive(&mut m, &r, Message::TurnOffForMe(trayd.id)), Some(Job::TurnOffForMe(trayd.id)));
        assert!(r.sys.unit("hyprforge-trayd.service").unwrap().masked);
        let state = m.report.as_ref().unwrap().states.iter().find(|(i, _)| i.id == trayd.id).unwrap().1.clone();
        assert!(offers(trayd, &state, m.record()).undo, "a mask setup made is setup's to undo");
    }

    /// A service setup itself enabled is turned off by its Undo; offering
    /// a mask beside it would be two off switches that do different
    /// things.
    #[test]
    fn a_service_setup_enabled_offers_undo_not_turn_off() {
        let r = rig();
        hyprforge_setup::apply(&r.env, &r.sys, &["service-trayd"]).unwrap();
        let rep = report(&r);
        let trayd = hyprforge_setup::item("service-trayd").unwrap();
        let state = rep.states.iter().find(|(i, _)| i.id == trayd.id).unwrap().1.clone();
        assert_eq!(offers(trayd, &state, rep.record.as_ref().ok()), Offers { undo: true, turn_off: false });
    }

    /// Unreadable is not empty: with a `setup.toml` that will not parse
    /// the page says so and offers nothing the library would refuse.
    #[test]
    fn an_unreadable_record_is_shown_and_offers_no_undo() {
        let r = rig();
        std::fs::create_dir_all(r.env.hyprforge_dir()).unwrap();
        std::fs::write(r.env.setup_toml(), "= nope").unwrap();
        let m = checked(&r);
        assert!(m.error.as_deref().is_some_and(|e| e.contains("Fix or remove")), "{:?}", m.error);
        let bind = hyprforge_setup::item("bind-files").unwrap();
        assert_eq!(offers(bind, &State::Done, m.record()), Offers::default());
    }

    #[test]
    fn every_item_has_a_group_and_the_groups_follow_apply_order() {
        let mut seen: Vec<&str> = Vec::new();
        for item in ITEMS.iter() {
            let g = group(item);
            if seen.last() != Some(&g) {
                assert!(!seen.contains(&g), "{g} is split by another group at {}", item.id);
                seen.push(g);
            }
        }
        assert_eq!(
            seen,
            ["Hyprland wiring", "Background services", "Shortcuts", "Lock & notifications", "Default apps & dialogs", "Optional"]
        );
    }

    /// Each file a page keeps a copy of is named by the items that write
    /// it, and nothing else asks for a reload.
    #[test]
    fn each_item_reloads_the_page_holding_its_file() {
        assert_eq!(Reloads::after(&["bind-files"]), Reloads { shortcuts: true, ..Reloads::default() });
        assert_eq!(Reloads::after(&["notif-blur"]), Reloads { window_rules: true, ..Reloads::default() });
        assert_eq!(Reloads::after(&["default-images"]), Reloads { default_apps: true, ..Reloads::default() });
        assert_eq!(Reloads::after(&["idle-lock"]), Reloads { idle: true, ..Reloads::default() });
        assert_eq!(Reloads::after(&["gtk-portal"]), Reloads { session: true, ..Reloads::default() });
        assert_eq!(Reloads::after(&["wiring", "service-trayd", "portal-dialog", "show-in-folder"]), Reloads::default());
    }

    /// The bug the reload exists for, end to end: setup adds a bind while
    /// Keybinds holds its list, and Keybinds' next save must keep it.
    #[test]
    fn applying_a_bind_reloads_keybinds_so_its_next_save_keeps_the_bind() {
        use crate::modules::shortcuts::{Message as ShortcutsMessage, ShortcutsModule};
        use hyprforge_shortcuts::{Action, KeyCombo, Modifier, Shortcut};

        crate::modules::with_temp_env(|_dir| {
            let env = Env::from_environment();
            let sys = MockSystem::with_suite_installed();
            // One bind of the user's own, there before either writer.
            let mine = Shortcut {
                name: "terminal".into(),
                enabled: true,
                combo: KeyCombo { mods: vec![Modifier::Super], key: "T".into() },
                action: Action::with_raw("exec_cmd", "\"kitty\""),
                description: "Terminal".into(),
                flags: Default::default(),
            };
            hyprforge_shortcuts::storage::save(&env.shortcuts_toml(), &[mine]).unwrap();
            let (mut keybinds, _task) = ShortcutsModule::new();

            let mut page = SetupModule::with_env(env.clone());
            let job = page.step(Message::Check).unwrap();
            page.step(job.run(&env, &sys));
            page.ticked.retain(|id, _| *id == "bind-files");
            let job = page.step(Message::Apply).unwrap();
            assert_eq!(job, Job::Apply(vec!["bind-files"]));
            let reply = job.run(&env, &sys);
            let reloads = reloads(&reply);
            assert!(reloads.shortcuts);
            page.step(reply);

            if reloads.shortcuts {
                keybinds.reload_store();
            }
            // Any save from the page: toggling the user's own bind.
            let _ = keybinds.update(ShortcutsMessage::ToggleEnabled(0));
            let stored = hyprforge_shortcuts::storage::load(&env.shortcuts_toml()).unwrap();
            assert_eq!(stored.len(), 2, "{stored:#?}");
            assert!(stored.iter().any(|s| s.combo.key == "E"), "setup's Super+E was lost");
        });
    }
}

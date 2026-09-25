//! What starts with the session, what environment it runs in, what the
//! touchpad does, and what applications are allowed to do.
//!
//! Four tabs over four `hl.*` call types that share a generated file.
//! Two of them behave unlike anything else in the app, and the screen is
//! shaped around those differences:
//!
//! - **Gestures** can't be resolved by ordering. Hyprland *refuses* a
//!   gesture that duplicates one already declared, and a refused call
//!   aborts the whole file — so a clash is caught before writing and
//!   reported, not written and rolled back.
//! - **Environment** holds the most dangerous settings in the app. A
//!   wrong `AQ_DRM_DEVICES` is a session that won't start, edited from a
//!   settings app that is no longer running. Those names are flagged.

use hyprforge_core::lua_setup;
use hyprforge_ui::theme::{spacing, FontScale};
use hyprforge_ui::widgets::{
    danger_button, divider, meta_text, primary_button, scaled_text, secondary_button, section,
};
use crate::modules::setup_notice::setup_notice;
use crate::module::SettingsModule;
use crate::modules::setting_rows::labelled;
use hyprforge_session::storage::Session;
use hyprforge_session::{autostart, environment, gestures, permissions};
use hyprforge_session::setup::{HyprConfig, SetupPlan};
use iced::widget::{checkbox, column, container, pick_list, row, text_input};
use iced::{Element, Length, Task};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tab {
    Autostart,
    Environment,
    Gestures,
    Permissions,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::Autostart, Tab::Environment, Tab::Gestures, Tab::Permissions];

    fn label(self) -> &'static str {
        match self {
            Tab::Autostart => "Autostart",
            Tab::Environment => "Environment",
            Tab::Gestures => "Gestures",
            Tab::Permissions => "Permissions",
        }
    }
}

/// A text field somewhere in the four lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Field {
    Command,
    Note,
    Name,
    Value,
    Fingers,
    Argument,
    Mods,
    Binary,
}

impl Field {
    fn tab(self) -> Tab {
        match self {
            Field::Command | Field::Note => Tab::Autostart,
            Field::Name | Field::Value => Tab::Environment,
            Field::Fingers | Field::Argument | Field::Mods => Tab::Gestures,
            Field::Binary => Tab::Permissions,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    TabSelected(Tab),
    Added(Tab),
    Removed(Tab, usize),
    Changed(usize, Field, String),
    Toggled(Tab, usize, bool),
    /// A dropdown: complete the moment it changes, so it saves.
    Chose(Tab, usize, &'static str, String),
    Commit,
    /// The user's own config, evaluated — for the gestures already
    /// declared there, which ours must not shadow.
    Evaluated(hyprforge_lua_import::ImportResult),
    Reloaded(Result<(), String>),
}

pub struct SessionModule {
    tab: Tab,
    stored: Session,
    drafts: BTreeMap<(usize, Field), String>,
    /// Gestures declared in the user's own config. Ours must not
    /// duplicate one, because Hyprland refuses rather than overrides.
    existing_gestures: Vec<gestures::Gesture>,
    config: HyprConfig,
    setup_plan: SetupPlan,
    error: Option<String>,
    status: Option<String>,
    store_unreadable: Option<String>,
}

impl SessionModule {
    pub fn new() -> (Self, Task<Message>) {
        let (stored, store_unreadable) = match hyprforge_session::storage::load(&session_toml()) {
            Ok(stored) => (stored, None),
            Err(e) => (Session::default(), Some(e.to_string())),
        };
        let setup = lua_setup::bootstrap(
            &hyprforge_core::paths::hypr_config_dir(),
            &hyprforge_core::paths::hyprland_lua_path(),
            lua_setup::ModuleSetup {
                require_line: hyprforge_session::setup::REQUIRE_LINE,
                placement: hyprforge_session::setup::PLACEMENT,
                generated: (
                    session_lua(),
                    hyprforge_session::apply::generate(&Session::default(), &[]),
                ),
            },
        );
        (
            SessionModule {
                tab: Tab::Autostart,
                stored,
                drafts: BTreeMap::new(),
                existing_gestures: Vec::new(),
                config: setup.config,
                setup_plan: setup.plan,
                error: setup.error,
                status: None,
                store_unreadable,
            },
            // Read the user's own gestures so a clash can be reported
            // before it's written rather than after Hyprland refuses it.
            Task::perform(super::evaluate_user_config(), Message::Evaluated),
        )
    }

    fn draft(&self, index: usize, field: Field, current: impl std::fmt::Display) -> String {
        self.drafts
            .get(&(index, field))
            .cloned()
            .unwrap_or_else(|| current.to_string())
    }

    fn persist(&mut self) -> Result<(), String> {
        if let Some(reason) = &self.store_unreadable {
            let message = format!(
                "Not saving — your session.toml couldn't be read, and overwriting \
                 it would lose whatever is in it. ({reason})"
            );
            self.error = Some(message.clone());
            return Err(message);
        }
        hyprforge_session::storage::save(&session_toml(), &self.stored).map_err(|e| {
            self.error = Some(e.to_string());
            e.to_string()
        })
    }

    fn save(&mut self) -> Task<Message> {
        if self.persist().is_err() {
            return Task::none();
        }
        self.status = None;
        if !matches!(self.config, HyprConfig::Lua(_)) {
            self.error = None;
            self.status = Some(
                "Saved. Takes effect once Hyprland setup is finished — see above.".into(),
            );
            return Task::none();
        }
        if self.setup_plan != SetupPlan::AlreadyPresent {
            match hyprforge_session::setup::install(&hyprforge_core::paths::hyprland_lua_path()) {
                Ok(plan) => self.setup_plan = plan,
                Err(e) => {
                    self.error = Some(e.to_string());
                    return Task::none();
                }
            }
        }
        Task::perform(
            regenerate_and_reload(self.stored.clone(), self.existing_gestures.clone()),
            Message::Reloaded,
        )
    }

    fn commit(&mut self) -> Task<Message> {
        let pending: Vec<((usize, Field), String)> =
            self.drafts.iter().map(|(k, v)| (*k, v.clone())).collect();
        let mut bad = Vec::new();
        for ((index, field), raw) in pending {
            if self.apply_draft(index, field, &raw) {
                self.drafts.remove(&(index, field));
            } else {
                bad.push(raw.trim().to_string());
            }
        }
        self.error = (!bad.is_empty())
            .then(|| format!("Couldn't read: {}. Everything else was saved.", bad.join(", ")));
        self.save()
    }

    /// `true` if the value was understood and stored.
    fn apply_draft(&mut self, index: usize, field: Field, raw: &str) -> bool {
        let text = raw.trim().to_string();
        match field {
            // Commands and values are shell and environment content, kept
            // exactly as typed.
            Field::Command => {
                if let Some(p) = self.stored.autostart.programs.get_mut(index) {
                    p.command = text;
                }
                true
            }
            Field::Note => {
                if let Some(p) = self.stored.autostart.programs.get_mut(index) {
                    p.note = text;
                }
                true
            }
            Field::Name => {
                if let Some(v) = self.stored.environment.variables.get_mut(index) {
                    v.name = text;
                }
                true
            }
            Field::Value => {
                if let Some(v) = self.stored.environment.variables.get_mut(index) {
                    // Not trimmed: trailing space can matter in a path list.
                    v.value = raw.to_string();
                }
                true
            }
            Field::Fingers => {
                let Some(g) = self.stored.gestures.gestures.get_mut(index) else {
                    return true;
                };
                match text.parse::<u32>() {
                    Ok(v) if (2..=10).contains(&v) => {
                        g.fingers = v;
                        true
                    }
                    _ => false,
                }
            }
            Field::Argument => {
                if let Some(g) = self.stored.gestures.gestures.get_mut(index) {
                    g.argument = text;
                }
                true
            }
            Field::Mods => {
                if let Some(g) = self.stored.gestures.gestures.get_mut(index) {
                    g.mods = text.to_uppercase();
                }
                true
            }
            Field::Binary => {
                if let Some(r) = self.stored.permissions.rules.get_mut(index) {
                    r.binary = text;
                }
                true
            }
        }
    }

    /// Drops a removed row's drafts and shifts later ones down.
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

    fn problems(&self, tab: Tab) -> Vec<(usize, String)> {
        match tab {
            Tab::Autostart => self.stored.autostart.invalid(),
            Tab::Environment => self.stored.environment.invalid(),
            Tab::Gestures => self.stored.gestures.invalid(&self.existing_gestures),
            Tab::Permissions => self.stored.permissions.invalid(),
        }
    }
}

impl SettingsModule for SessionModule {
    type Message = Message;

    /// No preview and no Discard, for the same reason as Desktop's: the
    /// drafts are fields of list entries, and this page has never offered
    /// to take them back.
    fn pending(&self) -> Option<crate::module::Pending<Message>> {
        (!self.drafts.is_empty()).then(|| crate::module::Pending {
            summary: hyprforge_ui::widgets::pending_label(self.drafts.len()),
            preview: None,
            apply: Message::Commit,
            discard: None,
        })
    }


    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::TabSelected(tab) => {
                self.tab = tab;
                Task::none()
            }
            Message::Evaluated(result) => {
                let hyprforge_dir = hyprforge_core::paths::hypr_hyprforge_dir();
                self.existing_gestures = result
                    .calls
                    .iter()
                    // Hyprforge's own generated file is required from
                    // hyprland.lua too. Counting our own gestures as
                    // "already taken" would make every one of them clash
                    // with itself.
                    .filter(|c| !c.source_path.starts_with(&hyprforge_dir))
                    .filter_map(|c| gesture_from_call(&c.kind, &c.args))
                    .filter_map(|g| g.ok())
                    .collect();

                // A gesture call that couldn't be read is not one that
                // isn't there. Reported alongside the file-level failures
                // below, because from the user's side they are the same
                // problem: something in their config is in effect and this
                // screen isn't counting it.
                let unreadable: Vec<String> = result
                    .calls
                    .iter()
                    .filter(|c| !c.source_path.starts_with(&hyprforge_dir))
                    .filter_map(|c| match gesture_from_call(&c.kind, &c.args) {
                        Some(Err(why)) => {
                            Some(format!("{} ({why})", c.source_path.display()))
                        }
                        _ => None,
                    })
                    .collect();
                if !unreadable.is_empty() {
                    self.error = Some(format!(
                        "Some gestures in your config couldn't be read, so they aren't                          counted as taken: {}",
                        unreadable.join("; ")
                    ));
                }
                // A file that wouldn't evaluate is not a file with no
                // gestures in it. Without this, an unreadable config made
                // every gesture in it look free, and the next one the user
                // added silently clashed with one already there.
                // `appearance` and `shortcuts` both report this already;
                // session was the one screen that stayed quiet.
                if !result.failures.is_empty() {
                    self.error = Some(format!(
                        "Some config files couldn't be read, so gestures they set                          aren't counted as taken: {}",
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
            Message::Added(tab) => {
                match tab {
                    Tab::Autostart => self.stored.autostart.programs.push(Default::default()),
                    Tab::Environment => self.stored.environment.variables.push(Default::default()),
                    Tab::Gestures => self.stored.gestures.gestures.push(Default::default()),
                    Tab::Permissions => self.stored.permissions.rules.push(permissions::Rule {
                        r#type: "screencopy".into(),
                        enabled: true,
                        ..Default::default()
                    }),
                }
                Task::none()
            }
            Message::Removed(tab, i) => {
                match tab {
                    Tab::Autostart if i < self.stored.autostart.programs.len() => {
                        self.stored.autostart.programs.remove(i);
                    }
                    Tab::Environment if i < self.stored.environment.variables.len() => {
                        self.stored.environment.variables.remove(i);
                    }
                    Tab::Gestures if i < self.stored.gestures.gestures.len() => {
                        self.stored.gestures.gestures.remove(i);
                    }
                    Tab::Permissions if i < self.stored.permissions.rules.len() => {
                        self.stored.permissions.rules.remove(i);
                    }
                    _ => return Task::none(),
                }
                self.reindex_drafts(tab, i);
                self.save()
            }
            Message::Changed(i, field, value) => {
                self.drafts.insert((i, field), value);
                Task::none()
            }
            Message::Toggled(tab, i, on) => {
                match tab {
                    Tab::Autostart => {
                        if let Some(p) = self.stored.autostart.programs.get_mut(i) {
                            p.enabled = on;
                        }
                    }
                    Tab::Environment => {
                        if let Some(v) = self.stored.environment.variables.get_mut(i) {
                            v.enabled = on;
                        }
                    }
                    Tab::Gestures => {
                        if let Some(g) = self.stored.gestures.gestures.get_mut(i) {
                            g.enabled = on;
                        }
                    }
                    Tab::Permissions => {
                        if let Some(r) = self.stored.permissions.rules.get_mut(i) {
                            r.enabled = on;
                        }
                    }
                }
                self.save()
            }
            Message::Chose(tab, i, what, value) => {
                match (tab, what) {
                    (Tab::Autostart, "when") => {
                        if let Some(p) = self.stored.autostart.programs.get_mut(i) {
                            p.when = if value.contains("ends") {
                                autostart::When::Shutdown
                            } else {
                                autostart::When::Start
                            };
                        }
                    }
                    (Tab::Gestures, "direction") => {
                        if let Some(g) = self.stored.gestures.gestures.get_mut(i) {
                            g.direction = value;
                        }
                    }
                    (Tab::Gestures, "action") => {
                        if let Some(g) = self.stored.gestures.gestures.get_mut(i) {
                            g.action = value;
                        }
                    }
                    (Tab::Permissions, "type") => {
                        if let Some(r) = self.stored.permissions.rules.get_mut(i) {
                            r.r#type = value;
                        }
                    }
                    (Tab::Permissions, "mode") => {
                        if let (Some(r), Some(mode)) = (
                            self.stored.permissions.rules.get_mut(i),
                            permissions::Mode::parse(&value),
                        ) {
                            r.mode = mode;
                        }
                    }
                    _ => {}
                }
                self.save()
            }
            Message::Commit => self.commit(),
            Message::Reloaded(Ok(())) => {
                self.error = None;
                self.status = Some("Saved.".into());
                Task::none()
            }
            Message::Reloaded(Err(e)) => {
                self.status = None;
                self.error = Some(e);
                Task::none()
            }
        }
    }

    fn view(&self, scale: FontScale) -> Element<'_, Message> {
        let mut content = column![].spacing(spacing::LG).width(Length::Fill);

        if let Some(notice) = setup_notice(&self.config, "session settings", scale) {
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

        content = content.push(match self.tab {
            Tab::Autostart => self.autostart_view(scale),
            Tab::Environment => self.environment_view(scale),
            Tab::Gestures => self.gestures_view(scale),
            Tab::Permissions => self.permissions_view(scale),
        });

        // No scrollable of its own: the shell owns the page's one scroll
        // area, and a second one inside it scrolled the page twice.
        container(content).padding(spacing::LG).width(Length::Fill).into()
    }
}

impl SessionModule {
    fn autostart_view(&self, scale: FontScale) -> Element<'_, Message> {
        let problems = self.problems(Tab::Autostart);
        let mut body = column![meta_text(
            "Run once when you log in. Hyprforge stores these and never runs them \
             itself — Hyprland does, on the session start event.",
            12.0,
            scale,
        )]
        .spacing(spacing::SM);

        for (i, program) in self.stored.autostart.programs.iter().enumerate() {
            body = body.push(divider());
            let whens: Vec<String> =
                autostart::When::ALL.iter().map(|w| w.label().to_string()).collect();
            let mut fields = column![
                labelled(
                    "Command",
                    text_input("waybar", &self.draft(i, Field::Command, &program.command))
                        .on_input(move |v| Message::Changed(i, Field::Command, v))
                        .on_submit(Message::Commit)
                        .padding(spacing::SM)
                        .into(),
                    scale,
                ),
                labelled(
                    "Note",
                    text_input("why this is here", &self.draft(i, Field::Note, &program.note))
                        .on_input(move |v| Message::Changed(i, Field::Note, v))
                        .on_submit(Message::Commit)
                        .padding(spacing::SM)
                        .into(),
                    scale,
                ),
                labelled(
                    "When",
                    pick_list(whens, Some(program.when.label().to_string()), move |v: String| {
                        Message::Chose(Tab::Autostart, i, "when", v)
                    })
                    .into(),
                    scale,
                ),
            ]
            .spacing(spacing::SM);
            fields = fields.push(row_actions(Tab::Autostart, i, program.enabled, &problems, scale));
            body = body.push(fields);
        }
        body = body.push(divider());
        body = body.push(secondary_button("Add a program").on_press(Message::Added(Tab::Autostart)));
        section("Autostart", scale, body)
    }

    fn environment_view(&self, scale: FontScale) -> Element<'_, Message> {
        let problems = self.problems(Tab::Environment);
        let mut body = column![meta_text(
            "Set for everything Hyprland launches. Some of these decide whether the \
             session starts at all — those are marked.",
            12.0,
            scale,
        )]
        .spacing(spacing::SM);

        for (i, variable) in self.stored.environment.variables.iter().enumerate() {
            body = body.push(divider());
            let mut fields = column![
                labelled(
                    "Name",
                    text_input("GTK_THEME", &self.draft(i, Field::Name, &variable.name))
                        .on_input(move |v| Message::Changed(i, Field::Name, v))
                        .on_submit(Message::Commit)
                        .padding(spacing::SM)
                        .into(),
                    scale,
                ),
                labelled(
                    "Value",
                    text_input("", &self.draft(i, Field::Value, &variable.value))
                        .on_input(move |v| Message::Changed(i, Field::Value, v))
                        .on_submit(Message::Commit)
                        .padding(spacing::SM)
                        .into(),
                    scale,
                ),
            ]
            .spacing(spacing::SM);
            // The warning that makes a change to a GPU variable
            // deliberate rather than incidental.
            if let Some(why) = environment::danger(&variable.name) {
                fields = fields.push(scaled_text(why, 12.0, scale));
            }
            fields = fields.push(row_actions(Tab::Environment, i, variable.enabled, &problems, scale));
            body = body.push(fields);
        }
        body = body.push(divider());
        body = body.push(secondary_button("Add a variable").on_press(Message::Added(Tab::Environment)));
        section("Environment", scale, body)
    }

    fn gestures_view(&self, scale: FontScale) -> Element<'_, Message> {
        let problems = self.problems(Tab::Gestures);
        let mut body = column![meta_text(
            "Touchpad swipes and pinches. Hyprland refuses two gestures with the same \
             fingers, direction and modifiers, so a clash has to be resolved rather \
             than layered.",
            12.0,
            scale,
        )]
        .spacing(spacing::SM);

        if !self.existing_gestures.is_empty() {
            body = body.push(meta_text(
                format!(
                    "Your own config already defines {} gesture(s); those are left alone.",
                    self.existing_gestures.len()
                ),
                12.0,
                scale,
            ));
        }

        for (i, gesture) in self.stored.gestures.gestures.iter().enumerate() {
            body = body.push(divider());
            let directions: Vec<String> =
                gestures::DIRECTIONS.iter().map(|(d, _)| d.to_string()).collect();
            let actions: Vec<String> =
                gestures::ACTIONS.iter().map(|(a, _, _)| a.to_string()).collect();
            let mut fields = column![
                labelled(
                    "Fingers",
                    text_input("3", &self.draft(i, Field::Fingers, gesture.fingers))
                        .on_input(move |v| Message::Changed(i, Field::Fingers, v))
                        .on_submit(Message::Commit)
                        .padding(spacing::SM)
                        .width(Length::Fixed(70.0))
                        .into(),
                    scale,
                ),
                labelled(
                    "Direction",
                    pick_list(directions, Some(gesture.direction.clone()), move |v: String| {
                        Message::Chose(Tab::Gestures, i, "direction", v)
                    })
                    .into(),
                    scale,
                ),
                labelled(
                    "Does",
                    pick_list(actions, Some(gesture.action.clone()), move |v: String| {
                        Message::Chose(Tab::Gestures, i, "action", v)
                    })
                    .into(),
                    scale,
                ),
                labelled(
                    "While holding",
                    text_input("SUPER", &self.draft(i, Field::Mods, &gesture.mods))
                        .on_input(move |v| Message::Changed(i, Field::Mods, v))
                        .on_submit(Message::Commit)
                        .padding(spacing::SM)
                        .into(),
                    scale,
                ),
            ]
            .spacing(spacing::SM);
            // Only the actions that take one, so the field can't suggest
            // a setting the action ignores.
            if let Some(argument) = gestures::action_argument(&gesture.action) {
                fields = fields.push(labelled(
                    argument,
                    text_input("", &self.draft(i, Field::Argument, &gesture.argument))
                        .on_input(move |v| Message::Changed(i, Field::Argument, v))
                        .on_submit(Message::Commit)
                        .padding(spacing::SM)
                        .into(),
                    scale,
                ));
            }
            fields = fields.push(row_actions(Tab::Gestures, i, gesture.enabled, &problems, scale));
            body = body.push(fields);
        }
        body = body.push(divider());
        body = body.push(secondary_button("Add a gesture").on_press(Message::Added(Tab::Gestures)));
        section("Gestures", scale, body)
    }

    fn permissions_view(&self, scale: FontScale) -> Element<'_, Message> {
        let problems = self.problems(Tab::Permissions);
        let mut body = column![meta_text(
            "What applications may do without asking. The binary is a pattern matched \
             against the program's full path, so it keeps working across package \
             updates. Order matters: the first matching rule wins.",
            12.0,
            scale,
        )]
        .spacing(spacing::SM);

        for (i, rule) in self.stored.permissions.rules.iter().enumerate() {
            body = body.push(divider());
            let types: Vec<String> = permissions::TYPES.iter().map(|(t, _, _)| t.to_string()).collect();
            let modes: Vec<String> = permissions::Mode::ALL.iter().map(|m| m.to_string()).collect();
            let mut fields = column![
                labelled(
                    "Program",
                    text_input("/usr/bin/grim", &self.draft(i, Field::Binary, &rule.binary))
                        .on_input(move |v| Message::Changed(i, Field::Binary, v))
                        .on_submit(Message::Commit)
                        .padding(spacing::SM)
                        .into(),
                    scale,
                ),
                labelled(
                    "May",
                    pick_list(types, Some(rule.r#type.clone()), move |v: String| {
                        Message::Chose(Tab::Permissions, i, "type", v)
                    })
                    .into(),
                    scale,
                ),
                labelled(
                    "Answer",
                    pick_list(modes, Some(rule.mode.to_string()), move |v: String| {
                        Message::Chose(Tab::Permissions, i, "mode", v)
                    })
                    .into(),
                    scale,
                ),
            ]
            .spacing(spacing::SM);
            if let Some((_, _, why)) = permissions::TYPES.iter().find(|(t, _, _)| *t == rule.r#type) {
                fields = fields.push(meta_text(*why, 12.0, scale));
            }
            fields = fields.push(row_actions(Tab::Permissions, i, rule.enabled, &problems, scale));
            body = body.push(fields);
        }
        body = body.push(divider());
        body = body.push(secondary_button("Add a rule").on_press(Message::Added(Tab::Permissions)));
        section("Permissions", scale, body)
    }
}

/// The enable checkbox, any problem, and Remove — identical for all four
/// lists.
fn row_actions<'a>(
    tab: Tab,
    index: usize,
    enabled: bool,
    problems: &[(usize, String)],
    scale: FontScale,
) -> Element<'a, Message> {
    let mut out = column![].spacing(spacing::XS);
    if let Some((_, problem)) = problems.iter().find(|(i, _)| *i == index) {
        out = out.push(scaled_text(problem.clone(), 12.0, scale));
    }
    out.push(
        row![
            checkbox(enabled).on_toggle(move |v| Message::Toggled(tab, index, v)),
            scaled_text("Enabled", 13.0, scale),
            danger_button("Remove", Message::Removed(tab, index)),
        ]
        .spacing(spacing::SM)
        .align_y(iced::Alignment::Center),
    )
    .into()
}


/// Recovers a gesture from a recorded `hl.gesture({...})` call.
/// `None` when this isn't a gesture call at all; `Some(Err)` when it is
/// one and could not be read.
///
/// The distinction is what stops an unreadable gesture counting as an
/// absent one. `existing_gestures` is the clash check, so a gesture that
/// silently failed to parse left its three-finger swipe looking free, and
/// the next one the user added quietly fought with it.
fn gesture_from_call(
    kind: &str,
    args: &[serde_json::Value],
) -> Option<Result<gestures::Gesture, String>> {
    if kind != "gesture" {
        return None;
    }
    let Some(table) = args.first().and_then(|v| v.as_object()) else {
        return Some(Err("its argument isn't a table".to_string()));
    };
    let Some(fingers) = table.get("fingers").and_then(|v| v.as_u64()) else {
        return Some(Err("it has no readable finger count".to_string()));
    };
    let Some(direction) = table.get("direction").and_then(|v| v.as_str()) else {
        return Some(Err(format!("the {fingers}-finger gesture has no readable direction")));
    };
    Some(Ok(gestures::Gesture {
        fingers: fingers as u32,
        direction: direction.to_string(),
        action: table
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("workspace")
            .to_string(),
        mods: table.get("mods").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        argument: String::new(),
        scale: table.get("scale").and_then(|v| v.as_f64()),
        enabled: true,
    }))
}

async fn regenerate_and_reload(
    session: Session,
    existing: Vec<gestures::Gesture>,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        hyprforge_session::apply::apply(&session_lua(), &session, &existing)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

fn session_toml() -> PathBuf {
    hyprforge_core::paths::hyprforge_config_dir().join("session.toml")
}

fn session_lua() -> PathBuf {
    hyprforge_core::paths::hypr_hyprforge_dir().join("session.lua")
}

#[cfg(test)]
mod gesture_reading {
    use super::*;
    use serde_json::json;

    fn read(args: serde_json::Value) -> Option<Result<gestures::Gesture, String>> {
        gesture_from_call("gesture", args.as_array().unwrap())
    }

    #[test]
    fn a_call_that_is_not_a_gesture_is_not_an_unreadable_gesture() {
        assert!(gesture_from_call("bind", &[json!("SUPER + Q")]).is_none());
    }

    /// `existing_gestures` is the clash check. A gesture that silently
    /// failed to parse left its swipe looking unclaimed, so the next one
    /// the user added quietly fought with a gesture already in effect.
    #[test]
    fn a_gesture_missing_its_direction_is_reported_not_dropped() {
        let out = read(json!([{ "fingers": 3 }]));
        let why = out
            .expect("still a gesture call")
            .expect_err("a gesture with no direction must not read as absent");
        assert!(why.contains("direction"), "{why}");
    }

    #[test]
    fn a_gesture_with_no_readable_finger_count_is_reported() {
        let out = read(json!([{ "direction": "left" }]));
        assert!(out.expect("still a gesture call").is_err());
    }

    #[test]
    fn an_ordinary_gesture_still_reads_cleanly() {
        let g = read(json!([{ "fingers": 3, "direction": "left" }]))
            .expect("a gesture call")
            .expect("should read");
        assert_eq!(g.fingers, 3);
        assert_eq!(g.direction, "left");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh SessionModule against an isolated config home and greeter
    /// export dir — see [`crate::modules::with_temp_env`].
    fn with_temp_config<T>(f: impl FnOnce(&mut SessionModule) -> T) -> T {
        crate::modules::with_temp_env(|_dir| {
            let (mut module, _) = SessionModule::new();
            f(&mut module)
        })
    }

    fn call(kind: &str, body: serde_json::Value, file: &str) -> hyprforge_lua_import::RecordedCall {
        hyprforge_lua_import::RecordedCall {
            kind: kind.to_string(),
            source_path: hyprforge_core::paths::hypr_config_dir().join(file),
            line: Some(1),
            args: vec![body],
        }
    }

    #[test]
    fn a_new_module_has_nothing_configured() {
        with_temp_config(|m| assert!(m.stored.is_empty()));
    }

    #[test]
    fn a_program_is_added_edited_and_written() {
        with_temp_config(|m| {
            let _ = m.update(Message::Added(Tab::Autostart));
            let _ = m.update(Message::Changed(0, Field::Command, "waybar".into()));
            let _ = m.update(Message::Commit);
            assert_eq!(m.stored.autostart.programs[0].command, "waybar");
            let lua = hyprforge_session::apply::generate(&m.stored, &[]);
            assert!(lua.contains("hl.exec_cmd([[waybar]])"), "{lua}");
        });
    }

    /// The user's own gestures come from evaluating their config, and
    /// ours must not duplicate one — Hyprland refuses rather than
    /// overriding, and a refused call aborts the whole file.
    #[test]
    fn a_gesture_clashing_with_the_users_own_is_reported() {
        with_temp_config(|m| {
            let _ = m.update(Message::Evaluated(hyprforge_lua_import::ImportResult {
                calls: vec![call(
                    "gesture",
                    serde_json::json!({ "fingers": 3, "direction": "horizontal", "action": "workspace" }),
                    "hyprland.lua",
                )],
                failures: Vec::new(),
            }));
            assert_eq!(m.existing_gestures.len(), 1);

            // The default new gesture is 3-finger horizontal — exactly
            // what their config already has.
            let _ = m.update(Message::Added(Tab::Gestures));
            let problems = m.problems(Tab::Gestures);
            assert_eq!(problems.len(), 1);
            assert!(problems[0].1.contains("already taken"), "{}", problems[0].1);

            let lua = hyprforge_session::apply::generate(&m.stored, &m.existing_gestures);
            assert!(!lua.contains("hl.gesture"), "it must not be written: {lua}");
        });
    }

    /// Hyprforge's own generated file is required from hyprland.lua too.
    /// Counting our own gestures as "already taken" would make every one
    /// of them clash with itself.
    #[test]
    fn our_own_generated_gestures_are_not_counted_as_taken() {
        with_temp_config(|m| {
            let mut result = hyprforge_lua_import::ImportResult::default();
            result.calls.push(call(
                "gesture",
                serde_json::json!({ "fingers": 3, "direction": "horizontal" }),
                "hyprland.lua",
            ));
            result.calls.push(hyprforge_lua_import::RecordedCall {
                kind: "gesture".into(),
                source_path: session_lua(),
                line: Some(1),
                args: vec![serde_json::json!({ "fingers": 4, "direction": "up" })],
            });
            let _ = m.update(Message::Evaluated(result));
            assert_eq!(m.existing_gestures.len(), 1, "only the user's own counts");
            assert_eq!(m.existing_gestures[0].fingers, 3);
        });
    }

    /// The most dangerous settings in the app get flagged so a change is
    /// deliberate.
    #[test]
    fn gpu_variables_are_flagged_in_the_editor() {
        with_temp_config(|m| {
            let _ = m.update(Message::Added(Tab::Environment));
            let _ = m.update(Message::Changed(0, Field::Name, "AQ_DRM_DEVICES".into()));
            let _ = m.update(Message::Commit);
            assert!(environment::danger(&m.stored.environment.variables[0].name).is_some());
            let _ = m.view(FontScale::default());
        });
    }

    /// A trailing space can matter in a path list, so the value is stored
    /// exactly as typed.
    #[test]
    fn an_environment_value_is_stored_verbatim() {
        with_temp_config(|m| {
            let _ = m.update(Message::Added(Tab::Environment));
            let _ = m.update(Message::Changed(0, Field::Name, "PATHS".into()));
            let _ = m.update(Message::Changed(0, Field::Value, "/a:/b ".into()));
            let _ = m.update(Message::Commit);
            assert_eq!(m.stored.environment.variables[0].value, "/a:/b ");
        });
    }

    #[test]
    fn a_bad_finger_count_is_refused_and_keeps_what_was_typed() {
        with_temp_config(|m| {
            let _ = m.update(Message::Added(Tab::Gestures));
            let _ = m.update(Message::Changed(0, Field::Fingers, "one".into()));
            let _ = m.update(Message::Commit);
            assert_eq!(m.stored.gestures.gestures[0].fingers, 3, "the default is kept");
            assert_eq!(m.drafts.get(&(0, Field::Fingers)).map(String::as_str), Some("one"));
        });
    }

    /// Permissions default to ask, never allow — defaulting to allow
    /// would quietly widen a security setting.
    #[test]
    fn a_new_permission_defaults_to_ask() {
        with_temp_config(|m| {
            let _ = m.update(Message::Added(Tab::Permissions));
            assert_eq!(m.stored.permissions.rules[0].mode, permissions::Mode::Ask);
        });
    }

    #[test]
    fn removing_a_row_shifts_later_drafts_down() {
        with_temp_config(|m| {
            for _ in 0..3 {
                let _ = m.update(Message::Added(Tab::Autostart));
            }
            let _ = m.update(Message::Changed(2, Field::Command, "third".into()));
            let _ = m.update(Message::Removed(Tab::Autostart, 0));
            assert_eq!(
                m.drafts.get(&(1, Field::Command)).map(String::as_str),
                Some("third")
            );
        });
    }

    /// The four lists share one draft map but have separate indices.
    #[test]
    fn removing_a_row_leaves_other_tabs_drafts_alone() {
        with_temp_config(|m| {
            let _ = m.update(Message::Added(Tab::Autostart));
            let _ = m.update(Message::Added(Tab::Environment));
            let _ = m.update(Message::Changed(0, Field::Name, "KEEP".into()));
            let _ = m.update(Message::Removed(Tab::Autostart, 0));
            assert_eq!(m.drafts.get(&(0, Field::Name)).map(String::as_str), Some("KEEP"));
        });
    }

    /// The rule that keeps the data-loss bug from returning.
    #[test]
    fn an_unreadable_store_blocks_saving() {
        with_temp_config(|m| {
            m.store_unreadable = Some("bad toml".into());
            let _ = m.update(Message::Added(Tab::Autostart));
            let _ = m.update(Message::Toggled(Tab::Autostart, 0, false));
            assert!(m.error.is_some());
        });
    }

    #[test]
    fn the_screen_builds_in_every_state() {
        with_temp_config(|m| {
            let scale = FontScale::default();
            for tab in Tab::ALL {
                let _ = m.update(Message::TabSelected(tab));
                let _ = m.view(scale);
            }
            for tab in Tab::ALL {
                let _ = m.update(Message::Added(tab));
            }
            m.error = Some("failed".into());
            m.status = Some("saved".into());
            m.store_unreadable = Some("bad toml".into());
            for tab in Tab::ALL {
                let _ = m.update(Message::TabSelected(tab));
                let _ = m.view(scale);
            }
        });
    }
}

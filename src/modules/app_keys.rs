//! App shortcuts: the keys of the suite's own apps — Files, Media — on one
//! page, beside Keybinds (which are Hyprland's).
//!
//! Settings cannot link against those apps: each is its own component, and
//! a settings app that needed every app installed to build would break the
//! rule that each one runs alone. So each app describes itself
//! (`<app> --list-keys`, a `hyprforge_keys::Listing`) and this page asks
//! whichever are installed. One that is not installed has no section; what
//! is shown is what the installed version binds.
//!
//! Asking is done with no display, no session bus and an empty runtime
//! directory. An app that knows `--list-keys` answers before it needs any
//! of them; one from before it would otherwise open a window (Media reads
//! an unknown flag as nothing to open) or hand the request to the Files
//! window already running — here it fails at once, unseen, and the page
//! says to update it.
//!
//! A change is one line of the app's own `[keys]` table, through
//! `hyprforge_keys::edit`: comments kept, a file that will not parse never
//! written, a missing file created with a line saying who wrote it.

use crate::module::SettingsModule;
use hyprforge_keys::{BareKeys, Combo, KeyPress, Listing};
use hyprforge_ui::theme::{spacing, FontScale};
use hyprforge_ui::widgets::{hint_text, keycap, scaled_text, secondary_button, section_label, setting_list, setting_row};
use iced::widget::{column, container, row, Space};
use iced::{Element, Length, Subscription, Task};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The apps asked, by program name.
const APPS: &[&str] = &["hyprforge-files", "hyprforge-media"];

/// Space kept free at a row's end for the buttons that appear on hover.
const ROW_ACTIONS_WIDTH: f32 = 150.0;

#[derive(Debug, Clone)]
pub enum Message {
    /// Ask the apps again.
    Refresh,
    Loaded(Vec<Asked>),
    /// Start listening for the new shortcut of an action.
    Change(usize, String),
    Captured(KeyPress),
    CaptureCancelled,
    /// Put an action back on its shipped keys.
    Reset(usize, String),
    Filter(String),
}

/// What one app answered.
#[derive(Debug, Clone)]
pub enum Asked {
    Listed(Listing),
    /// Not installed — a state, not a failure.
    Missing,
    /// Installed, and did not answer with a listing: older than
    /// `--list-keys`, or broken.
    NoAnswer(&'static str, String),
}

/// One app's section.
struct App {
    listing: Listing,
    /// What its `[keys]` says now.
    overrides: BTreeMap<String, Vec<String>>,
    /// Why its file cannot be written, when it cannot.
    refused: Option<String>,
}

pub struct AppKeysModule {
    apps: Vec<App>,
    /// Installed apps that could not say what they bind, with why.
    unanswered: Vec<(&'static str, String)>,
    loaded: bool,
    /// The row listening for a new shortcut: (app, action id).
    capturing: Option<(usize, String)>,
    error: Option<String>,
    status: Option<String>,
    /// What the filter field holds: rows whose name or keys contain it.
    filter: String,
}

impl AppKeysModule {
    pub fn new() -> (Self, Task<Message>) {
        (
            AppKeysModule { apps: Vec::new(), unanswered: Vec::new(), loaded: false, capturing: None, error: None, status: None, filter: String::new() },
            Task::perform(ask_all(), Message::Loaded),
        )
    }

    pub fn is_capturing(&self) -> bool {
        self.capturing.is_some()
    }

    fn read_file(path: &str) -> (BTreeMap<String, Vec<String>>, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(text) => match hyprforge_keys::edit::overrides(&text) {
                Ok(o) => (o, None),
                Err(e) => (BTreeMap::new(), Some(format!("{path}: {e}"))),
            },
            // First run: nothing customised, and the first change makes it.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (BTreeMap::new(), None),
            Err(e) => (BTreeMap::new(), Some(format!("{path} couldn't be read: {e}"))),
        }
    }

    /// The keys an action has now: the file's, or the shipped ones.
    fn effective(app: &App, id: &str) -> Vec<String> {
        match app.overrides.get(id) {
            Some(keys) => keys.clone(),
            None => app.listing.actions.iter().find(|a| a.id == id).map(|a| a.defaults.clone()).unwrap_or_default(),
        }
    }

    /// Writes one `[keys]` change to the app's file, read at this moment.
    fn write(&mut self, app: usize, id: &str, keys: Option<&[Combo]>) {
        let Some(a) = self.apps.get(app) else { return };
        let path = PathBuf::from(&a.listing.config);
        let name = a.listing.app.clone();
        let result = (|| -> Result<(), String> {
            let (text, created) = match std::fs::read_to_string(&path) {
                Ok(text) => (text, false),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (String::new(), true),
                Err(e) => return Err(format!("{} couldn't be read: {e}", path.display())),
            };
            let edited = hyprforge_keys::edit::set_keys(&text, id, keys).map_err(|e| format!("{}: {e}", path.display()))?;
            // The header goes on top afterwards: a document of nothing but
            // comments would keep them *after* the table it gained.
            let edited = if created { format!("{}\n{edited}", hyprforge_keys::edit::header(&name)) } else { edited };
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            hyprforge_core::paths::write_atomic(&path, &edited).map_err(|e| format!("{} couldn't be written: {e}", path.display()))
        })();
        match result {
            Ok(()) => {
                self.error = None;
                self.status = Some(format!("Saved. {name} uses it the next time it opens."));
            }
            Err(e) => {
                self.status = None;
                self.error = Some(e);
            }
        }
        let (overrides, refused) = Self::read_file(&self.apps[app].listing.config);
        self.apps[app].overrides = overrides;
        self.apps[app].refused = refused;
    }

    /// Checks a captured press against the app's rules and its other
    /// actions, and saves it — or says why not.
    fn take(&mut self, app: usize, id: String, press: KeyPress) {
        let combo = Combo { key: press.key, mods: press.mods };
        let Some(a) = self.apps.get(app) else { return };
        if a.listing.bare == BareKeys::ReservedForTyping && combo.would_swallow_typing() {
            self.error = Some(format!(
                "{combo} needs Ctrl, Alt or Super in {}: without one, the key types into its search instead.",
                a.listing.app
            ));
            return;
        }
        let taken = a.listing.actions.iter().find(|other| {
            other.id != id
                && Self::effective(a, &other.id).iter().any(|k| Combo::parse(k).is_ok_and(|c| c == combo))
        });
        if let Some(other) = taken {
            self.error = Some(format!("{combo} is already {} in {}. Change that one first.", other.label, a.listing.app));
            return;
        }
        self.write(app, &id, Some(&[combo]));
    }
}

/// Asks every app, off the UI thread.
async fn ask_all() -> Vec<Asked> {
    tokio::task::spawn_blocking(ask_installed).await.unwrap_or_default()
}

/// Asks every app, here and now. Blocking — bounded per app by
/// `command::TIMEOUT`. The render tool calls it directly, since it runs
/// no tasks.
pub(crate) fn ask_installed() -> Vec<Asked> {
    APPS.iter().map(|p| ask(p)).collect()
}

/// `<program> --list-keys`, bounded, in an environment with nowhere to
/// open a window and no one to hand a request to (see the module doc).
fn ask(program: &'static str) -> Asked {
    let mut command = std::process::Command::new(program);
    command.arg("--list-keys").env_clear();
    for var in ["HOME", "XDG_CONFIG_HOME", "PATH", "LANG"] {
        if let Some(v) = std::env::var_os(var) {
            command.env(var, v);
        }
    }
    let scratch = std::env::temp_dir().join(format!("hyprforge-settings-list-keys-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&scratch);
    command.env("XDG_RUNTIME_DIR", &scratch);
    let answer = hyprforge_core::command::output(&mut command, hyprforge_core::command::TIMEOUT);
    let _ = std::fs::remove_dir_all(&scratch);
    match answer {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Asked::Missing,
        Err(e) => Asked::NoAnswer(program, e.to_string()),
        Ok(out) => match Listing::parse(&String::from_utf8_lossy(&out.stdout)) {
            Ok(listing) => Asked::Listed(listing),
            Err(_) => Asked::NoAnswer(program, "it is older than this page — update it to change its shortcuts here".into()),
        },
    }
}

impl SettingsModule for AppKeysModule {
    type Message = Message;

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Refresh => return Task::perform(ask_all(), Message::Loaded),
            Message::Loaded(asked) => {
                self.loaded = true;
                self.apps.clear();
                self.unanswered.clear();
                for a in asked {
                    match a {
                        Asked::Listed(listing) => {
                            let (overrides, refused) = Self::read_file(&listing.config);
                            self.apps.push(App { listing, overrides, refused });
                        }
                        Asked::Missing => {}
                        Asked::NoAnswer(program, why) => self.unanswered.push((program, why)),
                    }
                }
            }
            Message::Change(app, id) => {
                self.error = None;
                self.status = None;
                self.capturing = Some((app, id));
            }
            Message::CaptureCancelled => self.capturing = None,
            Message::Captured(press) => {
                if let Some((app, id)) = self.capturing.take() {
                    self.take(app, id, press);
                }
            }
            Message::Filter(text) => self.filter = text,
            Message::Reset(app, id) => {
                self.capturing = None;
                self.write(app, &id, None);
            }
        }
        Task::none()
    }

    /// While a row listens, every key belongs to it — Escape included,
    /// which cancels. A bare modifier is not a shortcut, so holding Ctrl
    /// on the way to Ctrl+N does not end the capture early.
    fn subscription(&self) -> Subscription<Message> {
        if !self.is_capturing() {
            return Subscription::none();
        }
        iced::keyboard::listen().filter_map(|event| {
            let press = hyprforge_ui::keys::key_press(&event)?;
            if press.key == hyprforge_keys::Key::Escape && !press.mods.any_shortcut_modifier() && !press.mods.shift {
                return Some(Message::CaptureCancelled);
            }
            Some(Message::Captured(press))
        })
    }

    fn subtitle(&self) -> Option<String> {
        self.loaded.then(|| match self.apps.len() {
            1 => "1 app".to_string(),
            n => format!("{n} apps"),
        })
    }

    fn view(&self, scale: FontScale) -> Element<'_, Message> {
        let mut content = column![].spacing(spacing::LG);
        if !self.loaded {
            return container(hint_text("Asking the apps…", scale)).padding(spacing::LG).into();
        }
        if let Some(e) = &self.error {
            content = content.push(hint_text(e.clone(), scale).color(hyprforge_ui::theme::error()));
        } else if let Some(s) = &self.status {
            content = content.push(hint_text(s.clone(), scale));
        }
        if !self.apps.is_empty() {
            content = content.push(
                iced::widget::text_input("Filter by name or key…", &self.filter)
                    .on_input(Message::Filter)
                    .padding(spacing::SM)
                    .style(hyprforge_ui::widgets::inset_input_style),
            );
        }
        let query = self.filter.trim().to_lowercase();
        if self.apps.is_empty() && self.unanswered.is_empty() {
            content = content.push(scaled_text(
                "None of the suite's apps with shortcuts of their own (Files, Media) are installed.",
                13.0,
                scale,
            ));
        }
        for (i, app) in self.apps.iter().enumerate() {
            let mut rows: Vec<Element<'_, Message>> = Vec::new();
            let shown_actions = app.listing.actions.iter().filter(|a| {
                query.is_empty()
                    || a.label.to_lowercase().contains(&query)
                    || Self::effective(app, &a.id).iter().any(|k| k.to_lowercase().contains(&query))
            });
            for (stripe, action) in shown_actions.enumerate() {
                let keys = Self::effective(app, &action.id);
                let changed = app.overrides.contains_key(&action.id);
                let listening = self.capturing.as_ref() == Some(&(i, action.id.clone()));
                let shown: Element<'_, Message> = if listening {
                    hint_text("Press the new shortcut — Esc cancels", scale).into()
                } else if keys.is_empty() {
                    hint_text("None", scale).into()
                } else {
                    row(keys.iter().map(|k| keycap(k.clone(), scale))).spacing(spacing::XS).into()
                };
                let control = row![shown, Space::new().width(Length::Fixed(scale.apply(ROW_ACTIONS_WIDTH)))]
                    .spacing(spacing::MD)
                    .align_y(iced::Alignment::Center);
                let base = setting_row(stripe, action.label.clone(), None, control, scale);
                let element: Element<'_, Message> = if listening || app.refused.is_some() {
                    base
                } else {
                    // Change, and Reset only on a shortcut that was changed.
                    let mut buttons = row![secondary_button("Change").on_press(Message::Change(i, action.id.clone()))]
                        .spacing(spacing::XS);
                    if changed {
                        buttons = buttons.push(secondary_button("Reset").on_press(Message::Reset(i, action.id.clone())));
                    }
                    iced::widget::hover(
                        base,
                        container(buttons)
                            .padding([0.0, spacing::MD])
                            .width(Length::Fill)
                            .height(Length::Fill)
                            .align_x(iced::alignment::Horizontal::Right)
                            .align_y(iced::alignment::Vertical::Center),
                    )
                };
                rows.push(element);
            }
            if rows.is_empty() {
                continue;
            }
            let mut group = column![section_label(&app.listing.app, scale)].spacing(spacing::SM);
            if let Some(why) = &app.refused {
                group = group.push(hint_text(format!("Can't change these until it's fixed — {why}"), scale).color(hyprforge_ui::theme::warning()));
            }
            content = content.push(group.push(setting_list(rows)));
        }
        for (program, why) in &self.unanswered {
            content = content.push(hint_text(format!("{program}: {why}"), scale));
        }
        container(content).padding(spacing::LG).width(Length::Fill).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(config: &str) -> Listing {
        Listing::parse(&format!(
            "hyprforge-keys-listing\t1\napp\tFiles\nconfig\t{config}\nbare\treserved\n\
             action\tnew-tab\tNew Tab\tCtrl+T\naction\ttrash\tMove to Trash\tDelete\n"
        ))
        .unwrap()
    }

    fn module_with(config: &std::path::Path) -> AppKeysModule {
        let (mut m, _) = AppKeysModule::new();
        let _ = m.update(Message::Loaded(vec![Asked::Listed(listing(&config.display().to_string()))]));
        m
    }

    fn press(text: &str) -> KeyPress {
        let c = Combo::parse(text).unwrap();
        KeyPress { key: c.key, mods: c.mods, text: None }
    }

    #[test]
    fn a_new_shortcut_is_written_to_the_apps_own_file_and_reset_takes_it_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("files-config.toml");
        let mut m = module_with(&path);
        let _ = m.update(Message::Change(0, "new-tab".into()));
        assert!(m.is_capturing());
        let _ = m.update(Message::Captured(press("Ctrl+Shift+T")));
        assert!(!m.is_capturing());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Files' own settings"), "{text}");
        assert_eq!(hyprforge_keys::edit::overrides(&text).unwrap()["new-tab"], ["Ctrl+Shift+T"]);
        assert_eq!(AppKeysModule::effective(&m.apps[0], "new-tab"), ["Ctrl+Shift+T"]);

        let _ = m.update(Message::Reset(0, "new-tab".into()));
        assert_eq!(AppKeysModule::effective(&m.apps[0], "new-tab"), ["Ctrl+T"], "back on the shipped key");
    }

    #[test]
    fn a_shortcut_another_action_has_is_refused_and_nothing_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("files-config.toml");
        let mut m = module_with(&path);
        let _ = m.update(Message::Change(0, "trash".into()));
        let _ = m.update(Message::Captured(press("Ctrl+T")));
        assert!(m.error.as_deref().unwrap().contains("New Tab"), "{:?}", m.error);
        assert!(!path.exists());
    }

    /// Files has a search box: a bare letter would type into it, so the
    /// page refuses one there as Files itself would at load.
    #[test]
    fn a_bare_letter_is_refused_for_an_app_that_types() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("files-config.toml");
        let mut m = module_with(&path);
        let _ = m.update(Message::Change(0, "trash".into()));
        let _ = m.update(Message::Captured(press("D")));
        assert!(m.error.as_deref().unwrap().contains("Ctrl, Alt or Super"), "{:?}", m.error);
        assert!(!path.exists());
    }

    /// The 37-binds rule: a file that will not parse is never written.
    #[test]
    fn a_file_that_will_not_parse_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("files-config.toml");
        std::fs::write(&path, "[keys\nnew-tab = ").unwrap();
        let mut m = module_with(&path);
        assert!(m.apps[0].refused.is_some());
        let _ = m.update(Message::Change(0, "new-tab".into()));
        let _ = m.update(Message::Captured(press("Ctrl+Y")));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[keys\nnew-tab = ");
        assert!(m.error.is_some());
    }

    /// An app that is not installed is a state, not a failure: no section,
    /// and nothing said about it.
    #[test]
    fn an_app_that_is_not_installed_has_no_section() {
        let (mut m, _) = AppKeysModule::new();
        let _ = m.update(Message::Loaded(vec![Asked::Missing]));
        assert!(m.apps.is_empty() && m.unanswered.is_empty());
    }
}

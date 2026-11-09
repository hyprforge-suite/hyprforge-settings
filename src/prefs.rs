//! The Settings app's own preferences — how it shows things, never what
//! it writes into anyone's config.
//!
//! One so far: whether rows show their Hyprland name. A settings app is
//! for people who would rather not edit config files, so by default it
//! does not show them the config — no `general:gaps_in = 5` under "Gaps
//! between windows", no "set by hyprforge" chip, no generated Lua at the
//! top of a page. Someone who wants the plumbing turns one switch on, on
//! the Advanced page, and every page shows it again. Search matches a
//! config name either way.
//!
//! Read at draw time from wherever a row is built, rather than threaded
//! through every module's `view`: fourteen modules draw rows, and the
//! answer is the same for all of them. Thread-local, not a process-wide
//! static: the app draws on one thread, and tests that flip it run in
//! parallel on many.

use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Prefs {
    /// Show each setting's Hyprland name, where its value comes from,
    /// and the files a page writes.
    #[serde(default)]
    pub show_config_names: bool,
}

thread_local! {
    static SHOW_CONFIG_NAMES: Cell<bool> = const { Cell::new(false) };
}

/// Whether rows on this thread show their config names.
pub fn show_config_names() -> bool {
    SHOW_CONFIG_NAMES.with(Cell::get)
}

/// Sets what [`show_config_names`] answers on this thread, without
/// saving it.
pub fn set_show_config_names(on: bool) {
    SHOW_CONFIG_NAMES.with(|c| c.set(on));
}

fn path() -> PathBuf {
    hyprforge_paths::hyprforge_config_dir().join("settings-app.toml")
}

/// Reads the preferences and applies them to this thread.
///
/// A missing file is a first run. One that exists and will not read is
/// logged and the defaults used — these only change how the app looks,
/// so refusing to start over them would be the wrong trade — but it is
/// never silently treated as absent (CLAUDE.md), and nothing here writes
/// the defaults back over it until the user changes the switch.
pub fn load() -> Prefs {
    let prefs = match hyprforge_ecosystem::storage::load::<Prefs>(&path()) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "couldn't read the Settings app's own preferences; using the defaults");
            Prefs::default()
        }
    };
    set_show_config_names(prefs.show_config_names);
    prefs
}

/// Saves the preferences and applies them to this thread. Applied even
/// if the save fails, so the switch does what it says for this session;
/// the error is the caller's to show.
pub fn save(prefs: &Prefs) -> Result<(), String> {
    set_show_config_names(prefs.show_config_names);
    hyprforge_ecosystem::storage::save(&path(), prefs).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_names_are_hidden_until_asked_for() {
        assert!(!Prefs::default().show_config_names);
        assert!(!show_config_names());
    }

    #[test]
    fn the_switch_is_saved_and_read_back() {
        crate::modules::with_temp_env(|_dir| {
            assert_eq!(load(), Prefs::default(), "a missing file is a first run");
            save(&Prefs { show_config_names: true }).unwrap();
            set_show_config_names(false);
            assert!(load().show_config_names);
            assert!(show_config_names(), "loading applies it");
        });
    }

    #[test]
    fn a_file_that_will_not_read_is_not_overwritten_by_loading() {
        crate::modules::with_temp_env(|_dir| {
            std::fs::create_dir_all(path().parent().unwrap()).unwrap();
            std::fs::write(path(), "show_config_names = \"perhaps\"").unwrap();
            assert_eq!(load(), Prefs::default());
            assert_eq!(std::fs::read_to_string(path()).unwrap(), "show_config_names = \"perhaps\"");
        });
    }
}

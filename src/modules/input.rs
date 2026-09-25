//! Keyboard, pointer and touchpad settings.
//!
//! The screen itself is [`super::catalog_screen`], shared with System.
//! What's here is what is actually about input: the catalogue, the files,
//! and the pickers built from the XKB data installed on this machine.

use super::catalog_screen::{Catalogued, CatalogScreen, Installed};
use super::setting_rows::DynChoice;
use hyprforge_core::hlconfig::import::Live;
use hyprforge_core::hlconfig::{Catalog, Settings};
use hyprforge_core::lua_setup::{Placement, SetupError, SetupPlan};
use hyprforge_input::catalog::CATALOG;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub use super::catalog_screen::Message;

pub type InputModule = CatalogScreen<Input>;

pub struct Input;

impl Catalogued for Input {
    const SUBJECT: &'static str = "input settings";
    const STORE: &'static str = "input.toml";

    fn catalog() -> &'static Catalog {
        &CATALOG
    }

    fn require_line() -> &'static str {
        hyprforge_input::setup::REQUIRE_LINE
    }

    fn placement() -> Placement {
        hyprforge_input::setup::PLACEMENT
    }

    fn toml_path() -> PathBuf {
        hyprforge_core::paths::input_toml_path()
    }

    fn lua_path() -> PathBuf {
        hyprforge_core::paths::input_lua_path()
    }

    fn generate(settings: &Settings) -> String {
        hyprforge_input::apply::generate(settings)
    }

    fn apply(lua_path: &Path, settings: &Settings) -> Result<(), String> {
        hyprforge_input::apply::apply(lua_path, settings).map_err(|e| e.to_string())
    }

    fn install(hyprland_lua: &Path) -> Result<SetupPlan, SetupError> {
        hyprforge_input::setup::install(hyprland_lua)
    }

    fn discover() -> Installed {
        Installed {
            xkb: hyprforge_input::xkb::catalogue(),
            monitors: hyprforge_core::monitors::connector_names(),
        }
    }

    /// Keyboard layouts, variants and models from the installed XKB data,
    /// and the connected monitors.
    ///
    /// Variants depend on the chosen layout — the rules file lists over
    /// 400 and only a handful belong to any one layout — so this is
    /// rebuilt whenever the layout could have changed rather than
    /// computed once.
    fn choices(
        settings: &Settings,
        live: &BTreeMap<&'static str, Live>,
        installed: &Installed,
    ) -> BTreeMap<&'static str, Vec<DynChoice>> {
        let mut choices = BTreeMap::new();
        if !installed.monitors.is_empty() {
            choices.insert(
                "input:touchdevice:output",
                installed
                    .monitors
                    .iter()
                    .map(|name| DynChoice { value: name.clone(), label: name.clone() })
                    .collect(),
            );
        }
        if installed.xkb.layouts.is_empty() {
            return choices;
        }

        let entries = |list: &[hyprforge_input::xkb::Entry]| -> Vec<DynChoice> {
            list.iter()
                .map(|e| DynChoice {
                    value: e.code.clone(),
                    label: format!("{} \u{2014} {}", e.description, e.code),
                })
                .collect()
        };
        choices.insert("input:kb_layout", entries(&installed.xkb.layouts));
        choices.insert("input:kb_model", entries(&installed.xkb.models));

        let layout = settings
            .get("input:kb_layout")
            .and_then(|v| v.as_text().map(str::to_string))
            .or_else(|| {
                live.get("input:kb_layout")
                    .and_then(|l| l.value.as_text().map(str::to_string))
            })
            .unwrap_or_default();
        let variants = installed.xkb.variants_for(layout.trim());
        // No variants for this layout is a real answer, and an empty
        // dropdown is not a control — the row falls back to a text field.
        if !variants.is_empty() {
            let mut list = entries(&variants);
            // A blank variant is every layout's default and has to be
            // selectable, or the field becomes one-way.
            list.insert(
                0,
                DynChoice { value: String::new(), label: "Default".to_string() },
            );
            choices.insert("input:kb_variant", list);
        }
        choices
    }
}

#[cfg(test)]
mod tests {
    use super::super::catalog_screen::{Candidate, ImportState};
    use super::super::setting_rows::Source;
    use super::*;
    use hyprforge_core::hlconfig::import::Discovered;
    use hyprforge_core::hlconfig::Value;
    use hyprforge_ui::theme::FontScale;
    use crate::module::SettingsModule;
    use hyprforge_input::catalog;

    /// A fresh InputModule against an isolated config home and greeter
    /// export dir — see [`crate::modules::with_temp_env`].
    fn with_temp_config<T>(f: impl FnOnce(&mut InputModule) -> T) -> T {
        crate::modules::with_temp_env(|_dir| {
            let (mut module, _) = InputModule::new();
            f(&mut module)
        })
    }

    /// A typed value that is not applied yet is what the shell's bar
    /// exists to show: the count, the line it will write, and both ways
    /// out. Without a draft there is nothing pending — which is the
    /// header chip's "live" claim.
    #[test]
    fn a_typed_value_is_pending_until_applied() {
        with_temp_config(|m| {
            assert!(m.pending().is_none(), "nothing typed yet");
            let _ = m.update(Message::DraftChanged("input:repeat_rate", "45".into()));
            let pending = m.pending().expect("a draft is pending");
            assert_eq!(pending.summary, "1 pending change");
            assert_eq!(pending.preview.as_deref(), Some("input:repeat_rate = 45"));
            assert!(matches!(pending.apply, Message::ApplyDrafts));
            assert!(matches!(pending.discard, Some(Message::DiscardDrafts)));

            let _ = m.update(Message::ApplyDrafts);
            assert!(m.pending().is_none(), "applied, so nothing is held back");
        });
    }

    #[test]
    fn a_new_module_owns_nothing() {
        with_temp_config(|m| {
            assert!(m.settings.is_empty());
            assert!(!m.rows().owns("input:kb_layout"));
        });
    }

    #[test]
    fn toggling_a_checkbox_takes_ownership_of_the_key() {
        with_temp_config(|m| {
            let _ = m.update(Message::Set("input:numlock_by_default", Value::Bool(true)));
            assert!(m.rows().owns("input:numlock_by_default"));
            assert_eq!(
                m.settings.get("input:numlock_by_default"),
                Some(&Value::Bool(true))
            );
        });
    }

    /// Reset is not "set it to the default" — it stops writing the key, so
    /// Hyprland and the user's own config decide again.
    #[test]
    fn reset_gives_the_key_back_rather_than_writing_the_default() {
        with_temp_config(|m| {
            let _ = m.update(Message::Set("input:repeat_rate", Value::Int(30)));
            assert!(m.rows().owns("input:repeat_rate"));
            let _ = m.update(Message::Reset("input:repeat_rate"));
            assert!(!m.rows().owns("input:repeat_rate"));
            let lua = hyprforge_input::apply::generate(&m.settings);
            assert!(!lua.contains("repeat_rate"), "{lua}");
        });
    }

    /// Typing must not write anything: a keystroke-per-save would reload
    /// Hyprland once per character.
    #[test]
    fn typing_does_not_commit_until_applied() {
        with_temp_config(|m| {
            let _ = m.update(Message::DraftChanged("input:repeat_rate", "45".into()));
            assert!(!m.rows().owns("input:repeat_rate"), "still just a draft");
            let _ = m.update(Message::ApplyDrafts);
            assert_eq!(m.settings.get("input:repeat_rate"), Some(&Value::Int(45)));
            assert!(m.drafts.is_empty());
        });
    }

    #[test]
    fn a_bad_number_reports_next_to_its_own_field_and_keeps_what_was_typed() {
        with_temp_config(|m| {
            let _ = m.update(Message::DraftChanged("input:repeat_rate", "fast".into()));
            let _ = m.update(Message::ApplyDrafts);
            assert!(!m.rows().owns("input:repeat_rate"));
            assert!(m.draft_errors.contains_key("input:repeat_rate"));
            assert_eq!(
                m.drafts.get("input:repeat_rate").map(String::as_str),
                Some("fast"),
                "what the user typed must survive the failed apply"
            );
        });
    }

    /// One bad field must not discard the others — the user typed those too.
    #[test]
    fn a_single_bad_field_does_not_block_the_good_ones() {
        with_temp_config(|m| {
            let _ = m.update(Message::DraftChanged("input:repeat_rate", "oops".into()));
            let _ = m.update(Message::DraftChanged("input:repeat_delay", "450".into()));
            let _ = m.update(Message::ApplyDrafts);
            assert_eq!(m.settings.get("input:repeat_delay"), Some(&Value::Int(450)));
            assert!(!m.rows().owns("input:repeat_rate"));
        });
    }

    /// An out-of-range value is caught by the same validator the file uses,
    /// so the editor and a hand-edit can't disagree about what's allowed.
    #[test]
    fn an_out_of_range_value_is_refused_with_its_limit() {
        with_temp_config(|m| {
            let _ = m.update(Message::DraftChanged("input:sensitivity", "5".into()));
            let _ = m.update(Message::ApplyDrafts);
            assert!(!m.rows().owns("input:sensitivity"));
            let problem = &m.draft_errors["input:sensitivity"];
            assert!(problem.contains("at most 1"), "{problem}");
        });
    }

    #[test]
    fn discarding_drafts_leaves_the_store_untouched() {
        with_temp_config(|m| {
            let _ = m.update(Message::Set("input:repeat_rate", Value::Int(30)));
            let _ = m.update(Message::DraftChanged("input:repeat_rate", "99".into()));
            let _ = m.update(Message::DiscardDrafts);
            assert_eq!(m.settings.get("input:repeat_rate"), Some(&Value::Int(30)));
            assert!(m.drafts.is_empty());
        });
    }

    /// The rule that keeps the data-loss bug from returning: an unreadable
    /// store blocks every write.
    #[test]
    fn an_unreadable_store_blocks_saving() {
        with_temp_config(|m| {
            m.store_unreadable = Some("bad toml".to_string());
            let _ = m.update(Message::Set("input:repeat_rate", Value::Int(30)));
            let saved =
                hyprforge_core::hlconfig::storage::load(&hyprforge_core::paths::input_toml_path()).unwrap();
            assert!(saved.is_empty(), "nothing may be written over a store we can't read");
            assert!(m.error.is_some(), "and the user has to be told why");
        });
    }

    #[test]
    fn a_readable_store_still_saves() {
        with_temp_config(|m| {
            let _ = m.update(Message::Set("input:repeat_rate", Value::Int(30)));
            let saved =
                hyprforge_core::hlconfig::storage::load(&hyprforge_core::paths::input_toml_path()).unwrap();
            assert_eq!(saved.get("input:repeat_rate"), Some(&Value::Int(30)));
        });
    }

    /// Builds an evaluator result holding one `hl.config` call from
    /// `file`, the shape the real import path consumes.
    fn config_result(
        file: &str,
        body: serde_json::Value,
    ) -> hyprforge_lua_import::ImportResult {
        hyprforge_lua_import::ImportResult {
            calls: vec![hyprforge_lua_import::RecordedCall {
                kind: "config".to_string(),
                source_path: hyprforge_core::paths::hypr_config_dir().join(file),
                line: Some(1),
                args: vec![body],
            }],
            failures: Vec::new(),
        }
    }

    /// Values already owned start unticked, so a re-import can't silently
    /// replace a value the user set in the app with one from their file.
    #[test]
    fn an_already_owned_import_candidate_starts_unselected() {
        with_temp_config(|m| {
            let _ = m.update(Message::Set("input:kb_layout", Value::Text("de".into())));
            let _ = m.update(Message::ImportEvaluated(config_result(
                "hyprland.lua",
                serde_json::json!({ "input": { "kb_layout": "us", "repeat_rate": 25 } }),
            )));
            let Some(ImportState::Ready(candidates)) = &m.import_review else {
                panic!("expected a review");
            };
            let layout = candidates.iter().find(|c| c.found.key == "input:kb_layout").unwrap();
            let rate = candidates.iter().find(|c| c.found.key == "input:repeat_rate").unwrap();
            assert!(!layout.selected, "already owned, so not re-imported by default");
            assert!(rate.selected);
        });
    }

    #[test]
    fn importing_writes_only_the_selected_candidates() {
        with_temp_config(|m| {
            let _ = m.update(Message::ImportEvaluated(config_result(
                "hyprland.lua",
                serde_json::json!({ "input": { "kb_layout": "us", "repeat_rate": 25 } }),
            )));
            // Candidates come back in catalog-key order, so kb_layout is
            // first and repeat_rate second.
            let _ = m.update(Message::ImportToggle(1));
            let _ = m.update(Message::ImportConfirm);
            assert!(m.rows().owns("input:kb_layout"));
            assert!(!m.rows().owns("input:repeat_rate"), "it was unticked");
        });
    }

    #[test]
    fn cancelling_an_import_changes_nothing() {
        with_temp_config(|m| {
            let _ = m.update(Message::ImportEvaluated(config_result(
                "hyprland.lua",
                serde_json::json!({ "input": { "kb_layout": "us" } }),
            )));
            let _ = m.update(Message::ImportCancel);
            assert!(m.settings.is_empty());
            assert!(m.import_review.is_none());
        });
    }

    /// The defect that made import read the config instead of the live
    /// compositor. Hyprforge's own generated file is `require()`d from
    /// hyprland.lua, so it evaluates alongside the user's own — and it
    /// holds whatever Hyprforge last wrote. Offering that back as
    /// importable is how a stray `touchdevice:enabled = false` from a test
    /// run got adopted and persisted on a real machine.
    #[test]
    fn hyprforges_own_generated_file_is_never_offered_for_import() {
        with_temp_config(|m| {
            let mut result = config_result(
                "hyprland.lua",
                serde_json::json!({ "input": { "kb_layout": "us" } }),
            );
            result.calls.push(hyprforge_lua_import::RecordedCall {
                kind: "config".to_string(),
                source_path: hyprforge_core::paths::input_lua_path(),
                line: Some(5),
                args: vec![serde_json::json!({
                    "input": {
                        "kb_layout": "us",
                        "touchdevice": { "enabled": false }
                    }
                })],
            });
            let _ = m.update(Message::ImportEvaluated(result));

            let Some(ImportState::Ready(candidates)) = &m.import_review else {
                panic!("expected a review");
            };
            assert!(
                candidates
                    .iter()
                    .all(|c| c.found.key != "input:touchdevice:enabled"),
                "a value only Hyprforge's own file sets must not look importable"
            );
            assert_eq!(candidates.len(), 1);
            assert_eq!(candidates[0].found.key, "input:kb_layout");
        });
    }

    /// A config file that didn't evaluate may be the one holding the
    /// settings the user came to import, so it can't be swallowed.
    #[test]
    fn an_unreadable_config_file_is_reported_rather_than_ignored() {
        with_temp_config(|m| {
            let mut result = config_result(
                "hyprland.lua",
                serde_json::json!({ "input": { "kb_layout": "us" } }),
            );
            result
                .failures
                .push((std::path::PathBuf::from("/tmp/theirs.lua"), "boom".into()));
            let _ = m.update(Message::ImportEvaluated(result));
            let e = m.error.as_ref().expect("the failure must surface");
            assert!(e.contains("theirs.lua"), "{e}");
        });
    }

    /// A value the catalog rejects can only arrive by hand-editing, and
    /// codegen skips it. Without surfacing it the user would see a line in
    /// their TOML doing nothing at all.
    #[test]
    fn an_invalid_stored_value_is_surfaced_rather_than_silently_skipped() {
        with_temp_config(|m| {
            m.settings.set("input:sensitivity", Value::Float(9.0));
            m.invalid = m.settings.validate(&CATALOG);
            assert_eq!(m.invalid.len(), 1);
            assert_eq!(m.invalid[0].key, "input:sensitivity");
        });
    }

    #[test]
    fn the_filter_matches_label_key_and_help() {
        with_temp_config(|m| {
            let tap = catalog::get("input:touchpad:tap_to_click").unwrap();
            m.filter = "tap".to_string();
            assert!(m.matches_filter(tap));
            m.filter = "touchpad".to_string();
            assert!(m.matches_filter(tap));
            m.filter = "three fingers".to_string();
            assert!(m.matches_filter(tap), "help text should match too");
            m.filter = "bluetooth".to_string();
            assert!(!m.matches_filter(tap));
        });
    }

    /// Every catalog entry has to render. A kind the view doesn't handle
    /// would otherwise only show up as a blank row in front of a user.
    #[test]
    fn every_catalogued_setting_produces_a_row() {
        with_temp_config(|m| {
            for setting in CATALOG.settings {
                let _ = m.setting_row(setting, 0, FontScale::default());
            }
        });
    }

    /// Builds the whole screen in each state it can be in. The row test
    /// above covers the controls; this covers the banners, the import
    /// panel and the empty-filter case, none of which any other test
    /// constructs.
    #[test]
    fn the_screen_builds_in_every_state() {
        with_temp_config(|m| {
            let scale = FontScale::default();
            let _ = m.view(scale);

            m.store_unreadable = Some("bad toml".to_string());
            m.error = Some("something failed".to_string());
            m.status = Some("saved".to_string());
            m.settings.set("input:sensitivity", Value::Float(9.0));
            m.invalid = m.settings.validate(&CATALOG);
            let _ = m.view(scale);

            m.store_unreadable = None;
            m.filter = "no such setting anywhere".to_string();
            let _ = m.view(scale);
            m.filter = String::new();

            m.import_review = Some(ImportState::Running);
            let _ = m.view(scale);
            m.import_review = Some(ImportState::Ready(Vec::new()));
            let _ = m.view(scale);
            m.import_review = Some(ImportState::Ready(vec![Candidate {
                found: Discovered {
                    key: "input:kb_layout",
                    label: "Keyboard layout",
                    value: Value::Text("us".into()),
                    already_owned: true,
                    differs: true,
                },
                selected: false,
            }]));
            let _ = m.view(scale);
        });
    }

    fn live_value(key: &'static str, value: Value, set: bool) -> Live {
        Live { key, value, set }
    }

    /// The defect this pass found: an unowned row fell back to the
    /// *catalog* default, so a machine whose config turns numlock on would
    /// see the checkbox unticked while numlock was actually on. The screen
    /// has to report what's running.
    #[test]
    fn an_unowned_row_shows_the_live_value_not_the_catalog_default() {
        with_temp_config(|m| {
            let setting = catalog::get("input:numlock_by_default").unwrap();
            assert_eq!(
                m.rows().effective(setting),
                (Value::Bool(false), Source::Default),
                "catalog default before anything is known"
            );

            let _ = m.update(Message::LiveLoaded(vec![live_value(
                "input:numlock_by_default",
                Value::Bool(true),
                true,
            )]));
            assert_eq!(
                m.rows().effective(setting),
                (Value::Bool(true), Source::UserConfig),
                "the user's config sets it, so that's what must show"
            );
        });
    }

    /// The three sources have to stay distinguishable, because they mean
    /// different things: only one of them is a value this app writes.
    #[test]
    fn the_value_source_is_reported_precisely() {
        with_temp_config(|m| {
            let _ = m.update(Message::LiveLoaded(vec![
                live_value("input:repeat_rate", Value::Int(25), false),
                live_value("input:kb_layout", Value::Text("de".into()), true),
            ]));
            assert_eq!(
                m.rows().effective(catalog::get("input:repeat_rate").unwrap()).1,
                Source::Default
            );
            assert_eq!(
                m.rows().effective(catalog::get("input:kb_layout").unwrap()).1,
                Source::UserConfig
            );

            let _ = m.update(Message::Set("input:kb_layout", Value::Text("us".into())));
            assert_eq!(
                m.rows().effective(catalog::get("input:kb_layout").unwrap()),
                (Value::Text("us".into()), Source::Owned),
                "once owned, what the user chose wins over what's live"
            );
        });
    }

    /// An owned value must keep showing even when the live read disagrees
    /// — between saving and Hyprland reloading, they legitimately differ,
    /// and the control must not flicker back to the old value.
    #[test]
    fn an_owned_value_outranks_a_stale_live_read() {
        with_temp_config(|m| {
            let _ = m.update(Message::Set("input:repeat_rate", Value::Int(45)));
            let _ = m.update(Message::LiveLoaded(vec![live_value(
                "input:repeat_rate",
                Value::Int(25),
                false,
            )]));
            assert_eq!(
                m.rows().effective(catalog::get("input:repeat_rate").unwrap()).0,
                Value::Int(45)
            );
        });
    }

    /// A failed live read must not break the screen — every row still
    /// renders, falling back to the catalog default.
    #[test]
    fn a_failed_live_read_leaves_the_screen_usable() {
        with_temp_config(|m| {
            let _ = m.update(Message::LiveLoaded(Vec::new()));
            let _ = m.view(FontScale::default());
            assert_eq!(
                m.rows().effective(catalog::get("input:repeat_rate").unwrap()).0,
                Value::Int(25)
            );
        });
    }

    /// Text fields go through the same resolver, so a layout the user's
    /// config sets shows up in the box rather than the catalog's "us".
    #[test]
    fn a_text_field_shows_the_live_value_too() {
        with_temp_config(|m| {
            let _ = m.update(Message::LiveLoaded(vec![live_value(
                "input:kb_layout",
                Value::Text("de,fr".into()),
                true,
            )]));
            let setting = catalog::get("input:kb_layout").unwrap();
            assert_eq!(m.rows().shown_text(setting), "de,fr");
        });
    }

    fn xkb_sample() -> hyprforge_input::xkb::Catalogue {
        hyprforge_input::xkb::parse(
            "! layout\n  us  English (US)\n  de  German\n\
             ! variant\n  colemak  us: English (Colemak)\n  neo  de: German (Neo 2)\n\
             ! model\n  pc105  Generic 105-key PC\n",
        )
    }

    /// The value is a code, the thing a user knows is a name, and a wrong
    /// code is accepted silently — XKB falls back and the keyboard simply
    /// doesn't change.
    #[test]
    fn keyboard_fields_become_pickers_once_xkb_is_read() {
        with_temp_config(|m| {
            assert!(m.choices.is_empty(), "a text field until the scan returns");
            let _ = m.update(Message::ChoicesLoaded(Installed { xkb: xkb_sample(), monitors: Vec::new() }));
            let layouts = &m.choices["input:kb_layout"];
            assert!(layouts.iter().any(|c| c.value == "us"));
            assert!(
                layouts.iter().any(|c| c.label.contains("English (US)")),
                "the picker has to show the name, not just the code"
            );
            assert!(m.choices.contains_key("input:kb_model"));
        });
    }

    /// The rules file lists over 400 variants and only a handful belong
    /// to any one layout, so the list follows the chosen layout.
    #[test]
    fn the_variant_list_follows_the_chosen_layout() {
        with_temp_config(|m| {
            let _ = m.update(Message::ChoicesLoaded(Installed { xkb: xkb_sample(), monitors: Vec::new() }));
            let _ = m.update(Message::Set("input:kb_layout", Value::Text("us".into())));
            let variants = &m.choices["input:kb_variant"];
            assert!(variants.iter().any(|c| c.value == "colemak"));
            assert!(!variants.iter().any(|c| c.value == "neo"), "that one is German");

            let _ = m.update(Message::Set("input:kb_layout", Value::Text("de".into())));
            let variants = &m.choices["input:kb_variant"];
            assert!(variants.iter().any(|c| c.value == "neo"));
            assert!(!variants.iter().any(|c| c.value == "colemak"));
        });
    }

    /// A blank variant is every layout's default and has to be
    /// selectable, or the field is one-way.
    #[test]
    fn the_default_variant_is_selectable() {
        with_temp_config(|m| {
            let _ = m.update(Message::ChoicesLoaded(Installed { xkb: xkb_sample(), monitors: Vec::new() }));
            let _ = m.update(Message::Set("input:kb_layout", Value::Text("us".into())));
            assert!(m.choices["input:kb_variant"].iter().any(|c| c.value.is_empty()));
        });
    }

    /// A layout with no variants must not leave an empty dropdown, which
    /// isn't a control at all.
    #[test]
    fn a_layout_without_variants_falls_back_to_a_text_field() {
        with_temp_config(|m| {
            let _ = m.update(Message::ChoicesLoaded(Installed { xkb: xkb_sample(), monitors: Vec::new() }));
            let _ = m.update(Message::Set("input:kb_layout", Value::Text("nonesuch".into())));
            assert!(!m.choices.contains_key("input:kb_variant"));
        });
    }

    /// `kb_layout = "us,cz"` is the switchable-layout pattern, and a
    /// single-select dropdown can't express it — offering one would
    /// replace both layouts with whichever was clicked. The row falls
    /// back to a text field, which can.
    #[test]
    fn a_multi_layout_value_is_not_offered_a_single_select_picker() {
        with_temp_config(|m| {
            let _ = m.update(Message::ChoicesLoaded(Installed { xkb: xkb_sample(), monitors: Vec::new() }));
            let _ = m.update(Message::Set("input:kb_layout", Value::Text("us,de".into())));
            // The choices still exist; the row is what declines to use
            // them, so building the view is what proves it's handled.
            assert!(m.choices.contains_key("input:kb_layout"));
            let _ = m.view(FontScale::default());
            assert_eq!(
                m.settings.get("input:kb_layout"),
                Some(&Value::Text("us,de".into())),
                "both layouts must survive being displayed"
            );
        });
    }

    #[test]
    fn the_mapped_display_field_offers_connected_monitors() {
        with_temp_config(|m| {
            let _ = m.update(Message::ChoicesLoaded(Installed {
                xkb: Default::default(),
                monitors: vec!["eDP-2".into(), "DP-3".into()],
            }));
            let monitors = &m.choices["input:touchdevice:output"];
            assert_eq!(monitors.len(), 2);
            assert!(monitors.iter().any(|c| c.value == "eDP-2"));
        });
    }

    /// The draft bar only exists while something is typed, and the Apply
    /// button is the only way to reach it — a bar that never appears would
    /// strand every text field on the screen.
    #[test]
    fn the_apply_bar_appears_only_while_a_draft_is_pending() {
        with_temp_config(|m| {
            assert!(m.drafts.is_empty());
            let _ = m.update(Message::DraftChanged("input:repeat_rate", "45".into()));
            assert!(!m.drafts.is_empty(), "the bar's condition must now hold");
            let _ = m.update(Message::ApplyDrafts);
            assert!(m.drafts.is_empty(), "and stop holding once applied");
        });
    }
}

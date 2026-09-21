//! Which application opens each kind of file.
//!
//! The screen exists because of a real failure: on the machine this was
//! written on, images opened in a video editor and 3D models opened in
//! Firefox, and neither was anything the user had chosen. Both came from
//! *registration* — whatever application happened to claim the type
//! first — and there was nowhere to look at the list, let alone change
//! it.
//!
//! # Kinds, not types
//!
//! A person thinks "images", not "image/png, image/jpeg, image/gif,
//! image/webp". Setting only the one type they happened to be looking at
//! fixes PNGs and leaves JPEGs where they were, which is worse than not
//! offering the setting: it looks like it did nothing. So a row here
//! owns a *set* of types and writes every one of them.
//!
//! The cost is that the types in a kind can disagree — they were set one
//! at a time by whatever installed them — so the row says when they do,
//! rather than picking one to show and quietly hiding the rest.
//!
//! # What this screen may write
//!
//! `mimeapps.list`, one line per type, through `hyprforge_mime`. Nothing
//! else: this does not install applications, does not edit desktop
//! entries and does not touch the shared MIME database.

use crate::module::SettingsModule;
use hyprforge_ui::theme::{self, spacing, FontScale};
use hyprforge_ui::widgets::{meta_text, scaled_text, section};
use iced::widget::{column, container, pick_list, row};
use iced::{Element, Length, Task};
use std::sync::Arc;

/// One row: a name a person would use, and the types it stands for.
///
/// The first type is the one whose current default the row shows. The
/// rest follow it when the row is changed.
struct Kind {
    label: &'static str,
    types: &'static [&'static str],
}

/// The kinds offered, in the order they appear.
///
/// Deliberately not "every type the database knows": that is tens of
/// thousands of rows and answers no question anyone has. These are the
/// ones people actually go looking for after a fresh install — plus 3D
/// models, which is the kind that started this and which no other
/// desktop's settings app offers at all.
const KINDS: &[Kind] = &[
    Kind { label: "Web pages", types: &["text/html", "x-scheme-handler/http", "x-scheme-handler/https"] },
    Kind { label: "Email", types: &["x-scheme-handler/mailto"] },
    Kind {
        label: "Images",
        types: &["image/png", "image/jpeg", "image/gif", "image/webp", "image/tiff", "image/bmp"],
    },
    Kind { label: "Vector images", types: &["image/svg+xml"] },
    Kind { label: "Audio", types: &["audio/mpeg", "audio/flac", "audio/x-wav", "audio/ogg"] },
    Kind {
        label: "Video",
        types: &["video/mp4", "video/x-matroska", "video/webm", "video/quicktime"],
    },
    Kind { label: "Text", types: &["text/plain", "text/markdown", "text/csv", "text/x-log"] },
    // Everything a person means by "open it in my editor". One row
    // rather than a dozen: nobody wants Python in one editor and Rust
    // in another, and a row per language would bury the rows above.
    //
    // These are the names the database actually uses, checked rather
    // than guessed: a shell script is `text/x-shellscript`, YAML is
    // `application/yaml`, and Rust is `text/rust` with no `x-`.
    // Shell scripts are the reason the row exists — with no default,
    // opening one falls through to a web browser.
    Kind {
        label: "Code and config",
        types: &[
            "text/x-shellscript",
            "application/json",
            "application/yaml",
            "application/toml",
            "application/xml",
            "text/x-python",
            "text/rust",
            "text/javascript",
            "text/css",
            "text/x-csrc",
            "text/x-chdr",
            "text/x-go",
            "application/x-perl",
            "application/x-ruby",
            "application/sql",
            "text/x-makefile",
        ],
    },
    Kind { label: "PDF documents", types: &["application/pdf"] },
    Kind {
        label: "Archives",
        types: &["application/zip", "application/gzip", "application/x-tar", "application/x-7z-compressed"],
    },
    Kind { label: "3D models", types: &["model/stl", "model/3mf", "model/obj"] },
];

/// One entry in a row's list. `Display` is what the list shows, so it
/// carries the "not installed" note rather than the view repeating it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    id: String,
    name: String,
    installed: bool,
}

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.installed {
            true => f.write_str(&self.name),
            // The fstl case: a default pointing at something that has
            // been uninstalled. Shown, and shown as broken, because
            // "why does nothing happen when I open a 3MF" has no other
            // answer on screen.
            false => write!(f, "{} (not installed)", self.name),
        }
    }
}

pub struct DefaultAppsModule {
    /// The machine's own database, reloaded after every write so the
    /// screen shows what it just did rather than what it found at
    /// startup.
    db: Arc<hyprforge_mime::MimeDb>,
    /// What the last save said, if anything went wrong.
    error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Message {
    /// Re-read the database — on entering the screen, since an
    /// application may have been installed since it was last read.
    Refresh,
    /// A kind's application was chosen, by index into [`KINDS`].
    Chosen(usize, Choice),
}

impl DefaultAppsModule {
    pub fn new() -> (Self, Task<Message>) {
        (DefaultAppsModule { db: Arc::new(hyprforge_mime::MimeDb::load()), error: None }, Task::none())
    }

    /// The applications offered for a kind: everything registered for
    /// any of its types, installed only, by name.
    ///
    /// Union rather than intersection. An image viewer that registered
    /// for PNG but not for WebP is still the answer to "what opens
    /// images", and an intersection would hide it for a gap in its own
    /// desktop entry.
    fn choices(&self, kind: &Kind) -> Vec<Choice> {
        let mut choices: Vec<Choice> = Vec::new();
        for mime in kind.types {
            for app in self.db.apps_for(mime) {
                if !choices.iter().any(|c| c.id == app.id) {
                    choices.push(Choice {
                        id: app.id.clone(),
                        name: app.name.clone(),
                        installed: app.installed,
                    });
                }
            }
        }
        choices.sort_by_key(|c| c.name.to_lowercase());
        choices
    }

    /// What the row shows as chosen: the first type's default. `None`
    /// when nothing has been recorded for it.
    fn current(&self, kind: &Kind) -> Option<Choice> {
        let app = self.db.default_for(kind.types.first()?)?;
        Some(Choice { id: app.id.clone(), name: app.name.clone(), installed: app.installed })
    }

    /// The types in this kind whose default differs from the first
    /// one's — the disagreement the module doc explains, named so it can
    /// be seen rather than hidden.
    fn disagreements(&self, kind: &Kind) -> Vec<String> {
        let Some(first) = self.current(kind) else { return Vec::new() };
        kind.types
            .iter()
            .skip(1)
            .filter_map(|mime| {
                let app = self.db.default_for(mime)?;
                (app.id != first.id).then(|| format!("{mime} opens in {}", app.name))
            })
            .collect()
    }
}

impl SettingsModule for DefaultAppsModule {
    type Message = Message;

    fn icon(&self) -> &'static str {
        "\u{1F5C2}" // 🗂 — files and what opens them
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Refresh => {
                self.db = Arc::new(hyprforge_mime::MimeDb::load());
                Task::none()
            }
            Message::Chosen(index, choice) => {
                let Some(kind) = KINDS.get(index) else { return Task::none() };
                // Every type in the kind, so choosing an image viewer
                // does not fix PNGs and leave JPEGs behind.
                self.error = kind
                    .types
                    .iter()
                    .find_map(|mime| hyprforge_mime::set_default(mime, &choice.id).err())
                    .map(|e| format!("Couldn't save that choice: {e}"));
                self.db = Arc::new(hyprforge_mime::MimeDb::load());
                Task::none()
            }
        }
    }

    fn view(&self, scale: FontScale) -> Element<'_, Message> {
        let mut content = column![scaled_text("Default Applications", 22.0, scale)]
            .spacing(spacing::LG);

        if let Some(error) = &self.error {
            content = content.push(scaled_text(error.clone(), 13.0, scale).color(theme::warning()));
        }

        if !self.db.knows_types() {
            content = content.push(section(
                "Nothing to show",
                scale,
                meta_text(
                    "This machine has no shared MIME database, so nothing here knows what \
                     kinds of file exist. Installing shared-mime-info gives every application \
                     — not just this one — something to go on.",
                    13.0,
                    scale,
                ),
            ));
            return content.into();
        }

        let mut rows = column![].spacing(spacing::MD);
        for (index, kind) in KINDS.iter().enumerate() {
            let choices = self.choices(kind);
            let control: Element<'_, Message> = if choices.is_empty() {
                // Not an empty list to click on: a kind with nothing
                // installed is a statement, and an empty dropdown is a
                // puzzle.
                meta_text("Nothing installed opens these", 13.0, scale).into()
            } else {
                pick_list(choices, self.current(kind), move |choice| Message::Chosen(index, choice))
                    .width(Length::Fill)
                    .text_size(scale.apply(13.0))
                    .into()
            };
            let mut cell = column![labelled(kind.label, control, scale)].spacing(spacing::XS);
            for note in self.disagreements(kind) {
                cell = cell.push(meta_text(note, 12.0, scale));
            }
            rows = rows.push(cell);
        }

        content = content.push(section("What opens what", scale, rows));
        content = content.push(meta_text(
            "Changing one of these writes your own mimeapps.list, which every application \
             on the desktop reads — not just Hyprforge's.",
            12.0,
            scale,
        ));
        content.into()
    }
}

fn labelled<'a>(label: &'a str, control: Element<'a, Message>, scale: FontScale) -> Element<'a, Message> {
    row![
        container(scaled_text(label, 13.0, scale)).width(Length::FillPortion(2)),
        container(control).width(Length::FillPortion(3)),
    ]
    .spacing(spacing::MD)
    .align_y(iced::Alignment::Center)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A miniature desktop: two viewers, one of them uninstalled, and
    /// image types that disagree with each other.
    fn module_with_fixture() -> (tempfile::TempDir, DefaultAppsModule) {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(data.join("mime")).unwrap();
        std::fs::create_dir_all(data.join("applications")).unwrap();
        std::fs::write(data.join("mime/globs2"), "50:image/png:*.png\n").unwrap();
        std::fs::write(
            data.join("applications/viewer.desktop"),
            "[Desktop Entry]\nName=Picture Viewer\nExec=sh %f\n",
        )
        .unwrap();
        std::fs::write(
            data.join("applications/editor.desktop"),
            "[Desktop Entry]\nName=Video Editor\nExec=sh %f\n",
        )
        .unwrap();
        std::fs::write(
            data.join("applications/gone.desktop"),
            "[Desktop Entry]\nName=Departed\nExec=departed-xyz %f\n",
        )
        .unwrap();
        std::fs::write(
            data.join("applications/mimeinfo.cache"),
            "[MIME Cache]\n\
             image/png=editor.desktop;viewer.desktop;\n\
             image/jpeg=editor.desktop;viewer.desktop;\n\
             model/stl=gone.desktop;\n",
        )
        .unwrap();
        let mimeapps = dir.path().join("mimeapps.list");
        std::fs::write(
            &mimeapps,
            "[Default Applications]\n\
             image/png=editor.desktop\n\
             image/jpeg=viewer.desktop\n\
             model/stl=gone.desktop\n",
        )
        .unwrap();
        let db = hyprforge_mime::MimeDb::load_from(&[data], &[mimeapps]);
        (dir, DefaultAppsModule { db: Arc::new(db), error: None })
    }

    fn kind(label: &str) -> &'static Kind {
        KINDS.iter().find(|k| k.label == label).expect("a kind with that label")
    }

    /// Every kind offered has at least one type, and the first is the
    /// one the row reports on — a kind with none would render a row
    /// that can never show anything.
    #[test]
    fn every_kind_stands_for_at_least_one_type() {
        assert!(KINDS.iter().all(|k| !k.types.is_empty()));
        assert!(KINDS.iter().all(|k| !k.label.is_empty()));
    }

    /// No type appears in two kinds. It would be a row that silently
    /// undoes the one above it: setting Text and then Code would leave
    /// the shared type pointing wherever the second row was set.
    #[test]
    fn no_type_belongs_to_two_kinds() {
        let mut seen: Vec<&str> = Vec::new();
        for kind in KINDS {
            for mime in kind.types {
                assert!(!seen.contains(mime), "{mime} is in two kinds");
                seen.push(mime);
            }
        }
    }

    /// The types this page exists to cover are the ones with no default
    /// on a fresh machine — a shell script with none opens in a web
    /// browser, which is where this whole piece of work started.
    #[test]
    fn the_types_that_fall_through_to_a_browser_are_offered() {
        let all: Vec<&str> = KINDS.iter().flat_map(|k| k.types.iter().copied()).collect();
        for mime in [
            "text/x-shellscript",
            "application/json",
            "model/stl",
            "model/3mf",
            "image/png",
            "video/mp4",
        ] {
            assert!(all.contains(&mime), "{mime} has no row to set it from");
        }
    }

    /// The union rule: an application registered for any of a kind's
    /// types is offered for the kind.
    #[test]
    fn a_kind_offers_every_application_any_of_its_types_registered() {
        let (_dir, module) = module_with_fixture();
        let names: Vec<String> =
            module.choices(kind("Images")).into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["Picture Viewer", "Video Editor"]);
    }

    /// Types inside one kind can disagree, and the row says so rather
    /// than showing one and hiding the rest.
    #[test]
    fn a_kind_whose_types_disagree_says_which_ones() {
        let (_dir, module) = module_with_fixture();
        assert_eq!(module.current(kind("Images")).unwrap().name, "Video Editor");
        assert_eq!(module.disagreements(kind("Images")), ["image/jpeg opens in Picture Viewer"]);
    }

    /// A default pointing at something uninstalled is still shown, and
    /// shown as broken.
    #[test]
    fn a_departed_application_is_named_as_missing() {
        let (_dir, module) = module_with_fixture();
        let current = module.current(kind("3D models")).expect("the choice is recorded");
        assert!(!current.installed);
        assert_eq!(current.to_string(), "Departed (not installed)");
        assert!(module.choices(kind("3D models")).is_empty(), "and it is not offered as a choice");
    }

    /// A kind nothing registered for shows nothing, rather than an
    /// empty dropdown.
    #[test]
    fn a_kind_with_nothing_installed_offers_nothing() {
        let (_dir, module) = module_with_fixture();
        assert!(module.choices(kind("Audio")).is_empty());
        assert_eq!(module.current(kind("Audio")), None);
        assert!(module.disagreements(kind("Audio")).is_empty());
    }

    /// The machine with no database at all is a state with a sentence,
    /// not a screen of empty rows.
    #[test]
    fn no_database_is_a_state_of_its_own() {
        let db = hyprforge_mime::MimeDb::load_from(&[PathBuf::from("/nonexistent-xyz")], &[]);
        let module = DefaultAppsModule { db: Arc::new(db), error: None };
        assert!(!module.db.knows_types());
    }
}

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
//! # Kinds answer the common question; the rest of the database is
//! still there
//!
//! Eleven rows cannot cover forty thousand types, and the moment
//! somebody wants the twelfth thing they are back to editing
//! `mimeapps.list` in a text editor. So below the kinds is the whole
//! database, searchable — and, with an empty search box, the list of
//! every choice already recorded, which is the one thing no desktop
//! shows anywhere. That list is where a default pointing at an
//! application uninstalled two years ago finally becomes visible.
//!
//! # What this screen may write
//!
//! `mimeapps.list`, one line per type, through `hyprforge_mime`. Nothing
//! else: this does not install applications, does not edit desktop
//! entries and does not touch the shared MIME database.

use crate::module::SettingsModule;
use crate::modules::setting_rows::labelled;
use hyprforge_ui::theme::{self, spacing, FontScale};
use hyprforge_ui::widgets::{meta_text, scaled_text, secondary_button, section};
use iced::widget::{column, pick_list, row, text_input};
use iced::{Element, Length, Task};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
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

/// How many types a search puts on screen at once.
///
/// `view` runs every frame and a row is a `pick_list`, so an
/// unbounded search is not a slow screen — it is one that never draws.
/// Forty is already more than fits on a monitor, so the cap is only
/// reached by a search too broad to be looking for anything in
/// particular, and the count of what was left out is shown rather than
/// the list silently ending.
const SHOWN: usize = 40;

/// One read of the machine: the database, and which defaults are in the
/// file this screen is allowed to edit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    db: hyprforge_mime::MimeDb,
    yours: BTreeSet<String>,
}

/// Reads the database off the UI thread.
fn load() -> Task<Message> {
    Task::perform(
        async {
            tokio::task::spawn_blocking(|| Scan {
                db: hyprforge_mime::MimeDb::load(),
                yours: yours_in(&hyprforge_mime::user_mimeapps_path()),
            })
            .await
            .unwrap_or_default()
        },
        Message::Loaded,
    )
}

/// The types one `mimeapps.list` records a default for.
///
/// Read separately from the database, which merges every file that
/// applies, because *which file a line came from* decides whether this
/// screen can offer to remove it. A default set system-wide is not
/// ours to clear, and a Clear button that silently did nothing would
/// be worse than no button. A file that is not there is a machine
/// where nothing has been chosen yet — an answer, not a failure.
fn yours_in(path: &Path) -> BTreeSet<String> {
    let Ok(text) = std::fs::read_to_string(path) else { return BTreeSet::new() };
    hyprforge_mime::defaults::parse(&text).into_keys().collect()
}

/// Reads the descriptions for the rows now on screen.
///
/// Off the UI thread, and only for what is visible: a description is
/// one small XML file per type, and the database has tens of thousands
/// of them. A row shows its type name until its description arrives,
/// which is why this can be late without being wrong.
fn describe(db: Arc<hyprforge_mime::MimeDb>, mimes: Vec<String>) -> Task<Message> {
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || {
                let dirs = hyprforge_mime::data_dirs();
                mimes
                    .iter()
                    .filter_map(|mime| {
                        let text = hyprforge_mime::types::description_of(
                            &dirs,
                            db.canonical(mime),
                            None,
                        )?;
                        Some((mime.clone(), text))
                    })
                    .collect()
            })
            .await
            .unwrap_or_default()
        },
        Message::Described,
    )
}

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
    /// One prepared row per kind, rebuilt whenever `db` changes.
    ///
    /// `view` runs every frame, and it used to ask the database for all
    /// of this on each one: an `apps_for` per type across thirty types,
    /// a `String` cloned per application, and a sort whose key was a
    /// freshly lowercased name per comparison. None of it changes
    /// between a `Refresh` and a `Chosen`.
    rows: Vec<KindRow>,
    /// What the last save said, if anything went wrong.
    error: Option<String>,
    /// Whether the database has been read yet.
    ///
    /// Distinct from "there is no database": for the moment between
    /// opening the screen and the read returning, an empty database
    /// would otherwise be reported as a machine with no
    /// shared-mime-info — a confident wrong answer where "reading" is
    /// the true one.
    loaded: bool,
    /// What is in the search box. Empty means "show me what I have
    /// chosen", which is a question rather than a blank screen.
    search: String,
    /// The rows the search is showing, at most [`SHOWN`] of them,
    /// rebuilt when the search or the database changes and never in
    /// `view`.
    matches: Vec<TypeRow>,
    /// How many types the search actually matched, so the screen can
    /// say what it left out.
    matched: usize,
    /// Descriptions for types that have been asked about, accumulating
    /// as searches go by. Kept across searches: going back to a
    /// previous search should not re-read the same files.
    descriptions: BTreeMap<String, String>,
    /// The types whose default is in the file this screen writes — the
    /// ones it can offer to clear. See [`yours_in`].
    yours: BTreeSet<String>,
}

/// One searched type's row: what to call it, what it can be opened
/// with, and what opens it now.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct TypeRow {
    mime: String,
    /// The description if one has been read, else the type name. Owned
    /// rather than worked out in `view`, which runs every frame.
    label: String,
    choices: Vec<Choice>,
    current: Option<Choice>,
    /// Whether this screen can clear it — see [`yours_in`].
    clearable: bool,
}

/// What one kind's row shows: the applications to offer, the one
/// currently set, and any types inside the kind that disagree with it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct KindRow {
    choices: Vec<Choice>,
    current: Option<Choice>,
    disagreements: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum Message {
    /// Re-read the database — on entering the screen, since an
    /// application may have been installed since it was last read.
    Refresh,
    /// The database, as just read off the UI thread.
    Loaded(Scan),
    /// A kind's application was chosen, by index into [`KINDS`].
    Chosen(usize, Choice),
    /// What was typed in the search box.
    Searched(String),
    /// An application was chosen for one type, by name.
    TypeChosen(String, Choice),
    /// One type's recorded default was removed.
    TypeCleared(String),
    /// Descriptions for the rows on screen, as just read.
    Described(BTreeMap<String, String>),
}

impl DefaultAppsModule {
    /// The database is read off the UI thread, like every other
    /// backend on this screen's siblings: it scans every `.desktop` on
    /// the machine and walks `PATH` for each one, and `new` runs for
    /// every Settings launch whether or not anyone opens this screen.
    pub fn new() -> (Self, Task<Message>) {
        (
            DefaultAppsModule {
                db: Arc::new(hyprforge_mime::MimeDb::default()),
                rows: Vec::new(),
                error: None,
                loaded: false,
                search: String::new(),
                matches: Vec::new(),
                matched: 0,
                descriptions: BTreeMap::new(),
                yours: BTreeSet::new(),
            },
            load(),
        )
    }

    /// Takes a freshly-read database and prepares what the view draws,
    /// handing back the types whose descriptions are now worth reading.
    fn adopt(&mut self, scan: Scan) -> Vec<String> {
        self.db = Arc::new(scan.db);
        self.yours = scan.yours;
        self.loaded = true;
        self.rows = KINDS
            .iter()
            .map(|kind| KindRow {
                choices: self.choices(kind),
                current: self.current(kind),
                disagreements: self.disagreements(kind),
            })
            .collect();
        self.rebuild_matches()
    }

    /// Works out which types the search is showing, and prepares a row
    /// for each. Returns their names, for [`describe`].
    ///
    /// Matching is on the type's own name (`image/png`), not its
    /// description: a description has to be read from disk before it
    /// can be searched, and reading forty thousand files to answer one
    /// keystroke is not a search box. Descriptions arrive afterwards
    /// and change what a row is *called*, never which rows there are —
    /// so what the box matches stays the same whether or not the reads
    /// have landed.
    fn rebuild_matches(&mut self) -> Vec<String> {
        let query = self.search.trim().to_lowercase();
        let (matched, shown) = {
            let matching: Vec<&str> = match query.is_empty() {
                // An empty box is not an empty screen. It is every
                // choice already recorded — the list this desktop has
                // nowhere else, and where a default pointing at
                // something uninstalled is finally visible.
                true => self.db.chosen_types(),
                false => self
                    .db
                    .known_types()
                    .into_iter()
                    .filter(|mime| mime.contains(&query))
                    .collect(),
            };
            let shown: Vec<String> =
                matching.iter().take(SHOWN).map(|mime| (*mime).to_string()).collect();
            (matching.len(), shown)
        };
        self.matched = matched;
        let rows: Vec<TypeRow> = shown.iter().map(|mime| self.type_row(mime)).collect();
        self.matches = rows;
        shown
    }

    /// One type's row.
    ///
    /// Offered applications come from `candidates`, which includes the
    /// ones registered for a type this one is a *kind of* — a text
    /// editor for a shell script, an archive manager for a 3MF. A
    /// person searching for one specific type is usually there because
    /// nothing registered for it directly, so a list of only the direct
    /// registrations would be empty exactly when it was needed.
    fn type_row(&self, mime: &str) -> TypeRow {
        let mut choices: Vec<Choice> = Vec::new();
        for candidate in self.db.candidates(mime) {
            if !choices.iter().any(|c| c.id == candidate.app.id) {
                choices.push(Choice {
                    id: candidate.app.id.clone(),
                    name: candidate.app.name.clone(),
                    installed: candidate.app.installed,
                });
            }
        }
        TypeRow {
            label: self.descriptions.get(mime).cloned().unwrap_or_else(|| mime.to_string()),
            current: self.db.default_for(mime).map(|app| Choice {
                id: app.id.clone(),
                name: app.name.clone(),
                installed: app.installed,
            }),
            clearable: self.yours.contains(mime),
            mime: mime.to_string(),
            choices,
        }
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
            Message::Refresh => load(),
            Message::Loaded(scan) => {
                let shown = self.adopt(scan);
                describe(self.db.clone(), shown)
            }
            Message::Described(found) => {
                self.descriptions.extend(found);
                // Relabel the rows already on screen rather than
                // rebuilding them: nothing about *which* types matched
                // has changed, only what each one is called, and a
                // rebuild would throw away a pick_list mid-click.
                for row in &mut self.matches {
                    if let Some(text) = self.descriptions.get(&row.mime) {
                        row.label.clone_from(text);
                    }
                }
                Task::none()
            }
            Message::Searched(query) => {
                self.search = query;
                let shown = self.rebuild_matches();
                describe(self.db.clone(), shown)
            }
            Message::TypeChosen(mime, choice) => {
                // One type, unlike a kind: somebody who searched for
                // `text/x-python` asked about that type and nothing
                // else. The kinds above exist precisely so that the
                // broad answer does not have to be assembled here.
                self.error = self
                    .db
                    .set_default(&mime, &choice.id)
                    .err()
                    .map(|e| format!("Couldn't save that choice: {e}"));
                load()
            }
            Message::TypeCleared(mime) => {
                self.error = self
                    .db
                    .clear_default(&mime)
                    .err()
                    .map(|e| format!("Couldn't clear that: {e}"));
                load()
            }
            Message::Chosen(index, choice) => {
                let Some(kind) = KINDS.get(index) else { return Task::none() };
                // Every type in the kind, so choosing an image viewer
                // does not fix PNGs and leave JPEGs behind. Through the
                // database, so an alias is resolved to the name the
                // read side will look for.
                self.error = kind
                    .types
                    .iter()
                    .find_map(|mime| self.db.set_default(mime, &choice.id).err())
                    .map(|e| format!("Couldn't save that choice: {e}"));
                load()
            }
        }
    }

    fn view(&self, scale: FontScale) -> Element<'_, Message> {
        let mut content = column![scaled_text("Default Applications", 22.0, scale)]
            .spacing(spacing::LG);

        if let Some(error) = &self.error {
            content = content.push(scaled_text(error.clone(), 13.0, scale).color(theme::warning()));
        }

        if !self.loaded {
            content = content.push(section(
                "Reading",
                scale,
                meta_text("Looking at what this machine can open\u{2026}", 13.0, scale),
            ));
            return content.into();
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
        for ((index, kind), prepared) in KINDS.iter().enumerate().zip(&self.rows) {
            let choices = prepared.choices.clone();
            let control: Element<'_, Message> = if choices.is_empty() {
                // Not an empty list to click on: a kind with nothing
                // installed is a statement, and an empty dropdown is a
                // puzzle.
                meta_text("Nothing installed opens these", 13.0, scale).into()
            } else {
                pick_list(choices, prepared.current.clone(), move |choice| Message::Chosen(index, choice))
                    .width(Length::Fill)
                    .text_size(scale.apply(13.0))
                    .into()
            };
            let mut cell = column![labelled(kind.label, control, scale)].spacing(spacing::XS);
            for note in prepared.disagreements.clone() {
                cell = cell.push(meta_text(note, 12.0, scale));
            }
            rows = rows.push(cell);
        }

        content = content.push(section("What opens what", scale, rows));
        content = content.push(section("Every file type", scale, self.search_view(scale)));
        content = content.push(meta_text(
            "Changing any of these writes your own mimeapps.list, which every application \
             on the desktop reads — not just Hyprforge's.",
            12.0,
            scale,
        ));
        content.into()
    }
}

impl DefaultAppsModule {
    /// The search box and the rows it found.
    fn search_view(&self, scale: FontScale) -> Element<'_, Message> {
        let mut found = column![text_input("Search all file types\u{2026}", &self.search)
            .on_input(Message::Searched)
            .padding(spacing::SM)]
        .spacing(spacing::MD);

        for prepared in &self.matches {
            let control: Element<'_, Message> = if prepared.choices.is_empty() {
                meta_text("Nothing installed opens this", 13.0, scale).into()
            } else {
                let mime = prepared.mime.clone();
                pick_list(prepared.choices.clone(), prepared.current.clone(), move |choice| {
                    Message::TypeChosen(mime.clone(), choice)
                })
                .width(Length::Fill)
                .text_size(scale.apply(13.0))
                .into()
            };
            let mut line = row![labelled(&prepared.label, control, scale)]
                .spacing(spacing::MD)
                .align_y(iced::Alignment::Center);
            if prepared.clearable {
                // Only for a line in the file this screen writes: see
                // `yours_in`. Clearing is how somebody puts a type back
                // to "whatever registered for it", which choosing a
                // different application cannot express.
                line = line.push(
                    secondary_button("Clear")
                        .on_press(Message::TypeCleared(prepared.mime.clone())),
                );
            }
            // The type's own name under its description, because the
            // description is what a person recognises and the name is
            // what every other tool on the machine will call it.
            let mut cell = column![line].spacing(spacing::XS);
            if prepared.label != prepared.mime {
                cell = cell.push(meta_text(prepared.mime.clone(), 12.0, scale));
            }
            found = found.push(cell);
        }

        if self.matches.is_empty() {
            let empty = match self.search.trim().is_empty() {
                // Not "no results": nothing has been chosen, which is
                // what a machine looks like before anyone changes
                // anything, and is worth saying in those words.
                true => "You haven't chosen an application for any file type yet — everything \
                         opens with whatever registered for it. Search above to set one."
                    .to_string(),
                false => format!("Nothing matches \u{201c}{}\u{201d}.", self.search.trim()),
            };
            found = found.push(meta_text(empty, 13.0, scale));
        } else if self.matched > self.matches.len() {
            found = found.push(meta_text(
                format!(
                    "Showing {} of {} matching types. Type more to narrow it down.",
                    self.matches.len(),
                    self.matched
                ),
                12.0,
                scale,
            ));
        } else if self.search.trim().is_empty() {
            found = found.push(meta_text(
                "These are the choices recorded in your own mimeapps.list. Search to set \
                 one for any other type.",
                12.0,
                scale,
            ));
        }
        found.into()
    }
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
        let mut module = DefaultAppsModule::default_for_test();
        // Through `yours_in`, so the fixture exercises the same
        // "which file was this line in" question the real screen asks.
        let scan = Scan {
            db: hyprforge_mime::MimeDb::load_from(&[data], std::slice::from_ref(&mimeapps)),
            yours: yours_in(&mimeapps),
        };
        module.adopt(scan);
        (dir, module)
    }

    impl DefaultAppsModule {
        /// An empty module for a test to `adopt` a fixture database
        /// into — the same path `new` + `Message::Loaded` take.
        fn default_for_test() -> DefaultAppsModule {
            DefaultAppsModule {
                db: Arc::new(hyprforge_mime::MimeDb::default()),
                rows: Vec::new(),
                error: None,
                loaded: false,
                search: String::new(),
                matches: Vec::new(),
                matched: 0,
                descriptions: BTreeMap::new(),
                yours: BTreeSet::new(),
            }
        }
    }

    /// A module over a database built from `globs2` and `mimeapps`
    /// contents, with one installed application registered for
    /// everything named in the cache.
    fn module_over(globs2: &str, cache: &str, mimeapps: &[&str]) -> (tempfile::TempDir, DefaultAppsModule) {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(data.join("mime")).unwrap();
        std::fs::create_dir_all(data.join("applications")).unwrap();
        std::fs::write(data.join("mime/globs2"), globs2).unwrap();
        std::fs::write(
            data.join("applications/viewer.desktop"),
            "[Desktop Entry]\nName=Picture Viewer\nExec=sh %f\n",
        )
        .unwrap();
        std::fs::write(data.join("applications/mimeinfo.cache"), cache).unwrap();
        let paths: Vec<PathBuf> = mimeapps
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let path = dir.path().join(format!("mimeapps-{index}.list"));
                std::fs::write(&path, text).unwrap();
                path
            })
            .collect();
        let mut module = DefaultAppsModule::default_for_test();
        // The first is "yours" — the one file the screen may write.
        let yours = paths.first().map(|p| yours_in(p)).unwrap_or_default();
        module.adopt(Scan { db: hyprforge_mime::MimeDb::load_from(&[data], &paths), yours });
        (dir, module)
    }

    /// The question the kinds above cannot answer: what have I actually
    /// chosen? An empty search box is that list, not a blank screen.
    #[test]
    fn an_empty_search_shows_the_choices_already_made() {
        let (_dir, module) = module_with_fixture();
        let shown: Vec<&str> = module.matches.iter().map(|r| r.mime.as_str()).collect();
        assert_eq!(shown, vec!["image/jpeg", "image/png", "model/stl"]);
        assert!(
            module.matches.iter().all(|r| r.clearable),
            "every one is in the file this screen writes"
        );
        let stl = module.matches.iter().find(|r| r.mime == "model/stl").unwrap();
        assert_eq!(
            stl.current.as_ref().map(|c| c.installed),
            Some(false),
            "and a default that has been uninstalled is visible here, which is the point"
        );
    }

    /// A type nobody registered for is still findable — that is the
    /// whole reason the search exists beside the eleven kinds.
    #[test]
    fn a_search_finds_types_by_name() {
        let (_dir, mut module) = module_with_fixture();
        module.search = "image".to_string();
        module.rebuild_matches();
        let shown: Vec<&str> = module.matches.iter().map(|r| r.mime.as_str()).collect();
        assert_eq!(shown, vec!["image/jpeg", "image/png"]);

        module.search = "  MODEL ".to_string();
        module.rebuild_matches();
        let shown: Vec<&str> = module.matches.iter().map(|r| r.mime.as_str()).collect();
        assert_eq!(shown, vec!["model/stl"], "trimmed and case-folded, as typing is");

        module.search = "nothing-like-this".to_string();
        module.rebuild_matches();
        assert!(module.matches.is_empty());
        assert_eq!(module.matched, 0);
    }

    /// Clearing edits one file, so only a line in *that* file may be
    /// offered a Clear. A button that silently did nothing would be
    /// worse than no button.
    #[test]
    fn a_default_set_system_wide_is_shown_but_not_offered_a_clear() {
        let (_dir, module) = module_over(
            "50:image/png:*.png\n50:image/gif:*.gif\n",
            "[MIME Cache]\nimage/png=viewer.desktop;\nimage/gif=viewer.desktop;\n",
            &[
                "[Default Applications]\nimage/png=viewer.desktop\n",
                "[Default Applications]\nimage/gif=viewer.desktop\n",
            ],
        );
        let row = |mime: &str| module.matches.iter().find(|r| r.mime == mime).cloned().unwrap();
        assert!(row("image/png").clearable, "this one is in the user's own file");
        assert!(
            row("image/gif").current.is_some(),
            "the system-wide choice is still shown, because it is still in effect"
        );
        assert!(!row("image/gif").clearable, "but this screen cannot remove it");
    }

    /// `view` runs every frame and a row is a `pick_list`. A search
    /// that matched the whole database would not be a slow screen, it
    /// would be one that never draws — so the rows are capped and the
    /// count of what was left out is kept, to be said out loud.
    #[test]
    fn a_broad_search_is_capped_and_says_so() {
        let globs2: String = (0..SHOWN * 3).map(|n| format!("50:test/t{n}:*.t{n}\n")).collect();
        let (_dir, mut module) = module_over(&globs2, "[MIME Cache]\n", &[]);
        module.search = "test/".to_string();
        module.rebuild_matches();
        assert_eq!(module.matches.len(), SHOWN);
        assert_eq!(module.matched, SHOWN * 3, "and it knows how many it did not draw");
    }

    /// A description is a file read, so it arrives after the row does.
    /// Until then the row is called by its type name — never blank, and
    /// never a row that appears once the reading finishes.
    #[test]
    fn a_row_is_called_by_its_type_until_its_description_arrives() {
        let (_dir, mut module) = module_with_fixture();
        let before: Vec<&str> = module.matches.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(before, vec!["image/jpeg", "image/png", "model/stl"]);

        let described = BTreeMap::from([("image/png".to_string(), "PNG image".to_string())]);
        // The task it hands back reads nothing: the descriptions have
        // already arrived, which is what this message is.
        let _ = module.update(Message::Described(described));
        let after: Vec<&str> = module.matches.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(after, vec!["image/jpeg", "PNG image", "model/stl"]);
        assert_eq!(
            module.matches.len(),
            3,
            "a description changes what a row is called, never which rows there are"
        );
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
    /// "Not read yet" and "there is no database" are different states,
    /// and only the second is a machine without shared-mime-info.
    #[test]
    fn reading_is_told_apart_from_having_nothing_to_read() {
        let module = DefaultAppsModule::default_for_test();
        assert!(!module.loaded, "nothing read yet");

        let mut read = DefaultAppsModule::default_for_test();
        read.adopt(Scan {
            db: hyprforge_mime::MimeDb::load_from(&[PathBuf::from("/nonexistent-xyz")], &[]),
            yours: BTreeSet::new(),
        });
        assert!(read.loaded, "read, and there was nothing there");
        assert!(!read.db.knows_types());
    }

    #[test]
    fn no_database_is_a_state_of_its_own() {
        let db = hyprforge_mime::MimeDb::load_from(&[PathBuf::from("/nonexistent-xyz")], &[]);
        let mut module = DefaultAppsModule::default_for_test();
        module.adopt(Scan { db, yours: BTreeSet::new() });
        assert!(!module.db.knows_types());
    }
}

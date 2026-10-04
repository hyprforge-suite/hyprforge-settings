//! `hyprforge-settings --setup`: the Set up page for a terminal, and for
//! the installer.
//!
//! ```text
//! hyprforge-settings --setup              ask about each item that is to do
//! hyprforge-settings --setup --yes        apply every to-do item that is on by default
//! hyprforge-settings --setup --list       what each item's state is, for a person
//! hyprforge-settings --setup --porcelain  the same, one `id<TAB>state<TAB>reason` line each
//! hyprforge-settings --setup --undo [id…] undo what setup recorded (everything, by default)
//! ```
//!
//! Exit status: 0 when everything asked for worked (an item already done
//! counts), 1 when an item failed or `setup.toml` could not be read, 2 for
//! a usage error.
//!
//! **Only the modes that write hand off to a running Settings window.**
//! Interactive, `--yes` and `--undo` take the Settings singleton lock
//! first, and hold it while they write. If a window already has it, they
//! ask that window to show its Set up page and exit 0 having written
//! nothing: two writers to `shortcuts.toml` is the clipboard daemon's
//! lesson again, and the window's in-memory shortcut list would overwrite
//! what this wrote on its next save. `--list` and `--porcelain` only read,
//! so they always answer and never touch the window — `./hyprforge
//! --status` calls them, and a status check must not pop a page up in
//! somebody's session.
//!
//! The work itself is `hyprforge_setup`; this file is arguments, prompts
//! and printing.

use hyprforge_setup::{porcelain_line, Env, Item, State, System, ITEMS};
use std::io::{BufRead, Write};
use std::path::Path;

pub const USAGE: &str =
    "usage: hyprforge-settings --setup [--yes | --list | --porcelain | --undo [id...]]";

/// What `--setup` was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Interactive,
    Yes,
    List,
    Porcelain,
    /// Empty means everything recorded.
    Undo(Vec<String>),
}

impl Mode {
    /// Whether this mode writes, and so must not run beside a window.
    pub fn writes(&self) -> bool {
        matches!(self, Mode::Interactive | Mode::Yes | Mode::Undo(_))
    }
}

/// Reads the arguments after `--setup`. `Err` carries the message for a
/// usage error, which exits 2.
pub fn parse(args: &[String]) -> Result<Mode, String> {
    let mut args = args.iter().map(String::as_str);
    let mode = match args.next() {
        None => return Ok(Mode::Interactive),
        Some("--yes" | "-y") => Mode::Yes,
        Some("--list") => Mode::List,
        Some("--porcelain") => Mode::Porcelain,
        Some("--undo") => {
            let ids: Vec<String> = args.by_ref().map(str::to_string).collect();
            if let Some(unknown) = ids.iter().find(|id| hyprforge_setup::item(id).is_none()) {
                let known: Vec<&str> = ITEMS.iter().map(|i| i.id).collect();
                return Err(format!(
                    "no setup item called {unknown:?}\n  known items: {}",
                    known.join(", ")
                ));
            }
            Mode::Undo(ids)
        }
        Some(other) => return Err(format!("unknown --setup option {other:?}")),
    };
    match args.next() {
        None => Ok(mode),
        Some(extra) => Err(format!("unexpected {extra:?} after {mode:?}")),
    }
}

/// Where `run` reads answers from and writes to — the terminal in real
/// use, buffers in the tests.
pub struct Io<'a> {
    pub input: &'a mut dyn BufRead,
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
}

/// Runs `mode`, returning the exit status.
///
/// `lock_path` is the Settings singleton's lock; `hand_off` asks the
/// window holding it to show Set up. Both are parameters so the tests can
/// hold a lock of their own and see whether a request was sent.
pub fn run(
    mode: &Mode,
    env: &Env,
    sys: &dyn System,
    lock_path: &Path,
    hand_off: &dyn Fn() -> Result<(), String>,
    io: Io<'_>,
) -> i32 {
    // Held until this function returns — for the whole write — so a
    // Settings window opened meanwhile hands off to nothing and waits
    // rather than loading files half-written.
    let _lock = if mode.writes() {
        match crate::singleton::acquire(lock_path) {
            Ok(Some(lock)) => Some(lock),
            Ok(None) => {
                let _ = writeln!(
                    io.out,
                    "Hyprforge Settings is open — finish setting up on its Set up page."
                );
                if let Err(e) = hand_off() {
                    // Still not an error: nothing was written, which is
                    // the whole point of handing off.
                    let _ = writeln!(io.err, "(couldn't ask it to show the page: {e})");
                }
                return 0;
            }
            // A lock that can't even be opened must not block setup, the
            // same decision `main` makes for the window.
            Err(e) => {
                let _ = writeln!(io.err, "couldn't check whether Settings is open ({e}); going ahead");
                None
            }
        }
    } else {
        None
    };

    match mode {
        Mode::Porcelain => {
            for (item, state) in hyprforge_setup::check_all(env, sys) {
                let _ = writeln!(io.out, "{}", porcelain_line(item.id, &state));
            }
            0
        }
        Mode::List => {
            print_list(&hyprforge_setup::check_all(env, sys), io.out);
            0
        }
        Mode::Yes => {
            let states = hyprforge_setup::check_all(env, sys);
            let ids: Vec<&str> = states
                .iter()
                .filter(|(item, state)| item.default_on && state.is_todo())
                .map(|(item, _)| item.id)
                .collect();
            apply(env, sys, &ids, io)
        }
        Mode::Interactive => {
            let states = hyprforge_setup::check_all(env, sys);
            print_list(&states, io.out);
            let mut chosen = Vec::new();
            for (item, state) in &states {
                let State::Todo { what } = state else { continue };
                match ask(item, what, io.input, io.out) {
                    Some(true) => chosen.push(item.id),
                    Some(false) => {}
                    // End of input: nobody is there to answer, so nothing
                    // more is assumed.
                    None => break,
                }
            }
            apply(env, sys, &chosen, io)
        }
        Mode::Undo(ids) => {
            let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
            match hyprforge_setup::undo(env, sys, &ids) {
                Err(e) => {
                    let _ = writeln!(io.err, "hyprforge-settings: {e}");
                    1
                }
                Ok(outcomes) if outcomes.is_empty() => {
                    let _ = writeln!(io.out, "Setup hasn't changed anything, so there is nothing to undo.");
                    0
                }
                Ok(outcomes) => report(&outcomes, io.out),
            }
        }
    }
}

fn apply(env: &Env, sys: &dyn System, ids: &[&str], io: Io<'_>) -> i32 {
    if ids.is_empty() {
        let _ = writeln!(io.out, "Nothing to do.");
        return 0;
    }
    match hyprforge_setup::apply(env, sys, ids) {
        Err(e) => {
            let _ = writeln!(io.err, "hyprforge-settings: {e}");
            1
        }
        Ok(outcomes) => report(&outcomes, io.out),
    }
}

/// One line per outcome; 1 if any failed.
fn report(outcomes: &[hyprforge_setup::Outcome], out: &mut dyn Write) -> i32 {
    for outcome in outcomes {
        let label = hyprforge_setup::item(outcome.id).map_or(outcome.id, |i| i.label);
        let _ = match &outcome.result {
            Ok(message) => writeln!(out, "  ✓ {label}: {message}"),
            Err(message) => writeln!(out, "  ✗ {label}: {message}"),
        };
    }
    i32::from(outcomes.iter().any(|o| !o.ok()))
}

fn mark(state: &State) -> &'static str {
    match state {
        State::Done => "✓",
        State::Todo { .. } => "•",
        State::Unavailable { .. } => "–",
        State::Unknown { .. } => "?",
    }
}

fn print_list(states: &[(&'static Item, State)], out: &mut dyn Write) {
    for (item, state) in states {
        let _ = writeln!(out, "  {} {}", mark(state), item.label);
        if !state.reason().is_empty() {
            let _ = writeln!(out, "      {}", state.reason());
        }
    }
}

/// Asks about one item. `None` at end of input.
fn ask(item: &Item, what: &str, input: &mut dyn BufRead, out: &mut dyn Write) -> Option<bool> {
    let hint = if item.default_on { "[Y/n]" } else { "[y/N]" };
    loop {
        let _ = write!(out, "{}? {what} {hint} ", item.label);
        let _ = out.flush();
        let mut line = String::new();
        match input.read_line(&mut line) {
            Ok(0) | Err(_) => {
                let _ = writeln!(out);
                return None;
            }
            Ok(_) => {}
        }
        match line.trim().to_ascii_lowercase().as_str() {
            "" => return Some(item.default_on),
            "y" | "yes" => return Some(true),
            "n" | "no" => return Some(false),
            _ => {
                let _ = writeln!(out, "  please answer y or n");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyprforge_setup::mock::MockSystem;
    use std::cell::Cell;

    struct Rig {
        dir: tempfile::TempDir,
        env: Env,
        sys: MockSystem,
    }

    fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::rooted_at(dir.path());
        std::fs::create_dir_all(env.hypr_dir()).unwrap();
        std::fs::write(env.hyprland_lua(), "-- mine\n").unwrap();
        Rig { dir, env, sys: MockSystem::with_suite_installed() }
    }

    /// Runs `mode` with `input` as stdin; returns (status, stdout, whether
    /// a hand-off was attempted).
    fn go(r: &Rig, mode: &Mode, input: &str, lock: &Path) -> (i32, String, bool) {
        let handed = Cell::new(false);
        let hand_off = || {
            handed.set(true);
            Ok(())
        };
        let mut input = std::io::Cursor::new(input.as_bytes().to_vec());
        let mut out = Vec::new();
        let mut err = Vec::new();
        let status = run(
            mode,
            &r.env,
            &r.sys,
            lock,
            &hand_off,
            Io { input: &mut input, out: &mut out, err: &mut err },
        );
        (status, String::from_utf8(out).unwrap(), handed.get())
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn setup_flags_parse_into_their_modes() {
        assert_eq!(parse(&[]), Ok(Mode::Interactive));
        assert_eq!(parse(&args(&["--yes"])), Ok(Mode::Yes));
        assert_eq!(parse(&args(&["--list"])), Ok(Mode::List));
        assert_eq!(parse(&args(&["--porcelain"])), Ok(Mode::Porcelain));
        assert_eq!(parse(&args(&["--undo"])), Ok(Mode::Undo(vec![])));
        assert_eq!(
            parse(&args(&["--undo", "bind-files", "wiring"])),
            Ok(Mode::Undo(args(&["bind-files", "wiring"])))
        );
    }

    /// A typo is a usage error (exit 2), not a silent no-op.
    #[test]
    fn an_unknown_flag_or_item_is_a_usage_error() {
        assert!(parse(&args(&["--frobnicate"])).is_err());
        assert!(parse(&args(&["--undo", "bind-everything"])).unwrap_err().contains("known items"));
        assert!(parse(&args(&["--yes", "--list"])).is_err());
    }

    /// The format `./hyprforge --status` parses: one line per item, in
    /// order, `id<TAB>state<TAB>reason`, state one of four words.
    #[test]
    fn porcelain_prints_one_pinned_line_per_item() {
        let r = rig();
        let lock = r.dir.path().join("settings.lock");
        let (status, out, _) = go(&r, &Mode::Porcelain, "", &lock);
        assert_eq!(status, 0);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), ITEMS.len());
        for (line, item) in lines.iter().zip(ITEMS.iter()) {
            let fields: Vec<&str> = line.split('\t').collect();
            assert_eq!(fields.len(), 3, "{line:?}");
            assert_eq!(fields[0], item.id);
            assert!(["done", "todo", "unavailable", "unknown"].contains(&fields[1]), "{line:?}");
        }
        assert!(out.starts_with("wiring\ttodo\t"), "{out}");
    }

    /// The Settings window is open: the writing modes ask it to show Set
    /// up and write nothing at all.
    #[test]
    fn setup_with_settings_open_hands_off_and_writes_nothing() {
        for mode in [Mode::Yes, Mode::Interactive, Mode::Undo(vec![])] {
            let r = rig();
            let lock = r.dir.path().join("settings.lock");
            let _window = crate::singleton::acquire(&lock).unwrap().expect("the window's lock");
            let before = std::fs::read_to_string(r.env.hyprland_lua()).unwrap();

            let (status, _, handed) = go(&r, &mode, "y\n", &lock);
            assert_eq!(status, 0, "{mode:?}");
            assert!(handed, "{mode:?} didn't hand off");
            assert!(!r.env.setup_toml().exists(), "{mode:?} wrote a record");
            assert!(!r.env.shortcuts_toml().exists(), "{mode:?} wrote shortcuts");
            assert_eq!(std::fs::read_to_string(r.env.hyprland_lua()).unwrap(), before);
            assert!(r.sys.calls().is_empty(), "{mode:?} acted: {:?}", r.sys.calls());
        }
    }

    /// Reading never involves the window — a status check must not pop
    /// a page up in somebody's session.
    #[test]
    fn porcelain_and_list_with_settings_open_still_answer_and_send_nothing() {
        for mode in [Mode::Porcelain, Mode::List] {
            let r = rig();
            let lock = r.dir.path().join("settings.lock");
            let _window = crate::singleton::acquire(&lock).unwrap().expect("the window's lock");
            let (status, out, handed) = go(&r, &mode, "", &lock);
            assert_eq!(status, 0);
            assert!(!handed, "{mode:?} sent a request to the window");
            if mode == Mode::Porcelain {
                assert_eq!(out.lines().count(), ITEMS.len(), "{out}");
            } else {
                assert!(out.contains(ITEMS[0].label), "{out}");
            }
        }
    }

    /// `--yes` applies what is ticked by default, and leaves the opt-in
    /// GTK variable alone.
    #[test]
    fn yes_applies_the_defaults_and_not_the_opt_in() {
        let r = rig();
        let lock = r.dir.path().join("settings.lock");
        let (status, out, _) = go(&r, &Mode::Yes, "", &lock);
        assert_eq!(status, 0, "{out}");
        let record = hyprforge_setup::record::load(&r.env.setup_toml()).unwrap();
        assert!(record.items.contains_key("bind-files"), "{out}");
        assert!(!record.items.contains_key("gtk-portal"));

        let (status, out, _) = go(&r, &Mode::Undo(vec![]), "", &lock);
        assert_eq!(status, 0, "{out}");
        assert!(hyprforge_setup::record::load(&r.env.setup_toml()).unwrap().items.is_empty());
    }

    /// Enter takes the default, and the end of input answers nothing more.
    #[test]
    fn interactive_asks_with_the_default_and_stops_at_end_of_input() {
        let r = rig();
        let lock = r.dir.path().join("settings.lock");
        // Wiring: Enter (yes by default). Display daemon: n. Then EOF.
        let (status, out, _) = go(&r, &Mode::Interactive, "\nn\n", &lock);
        assert_eq!(status, 0, "{out}");
        assert!(out.contains("[Y/n]"), "{out}");
        let record = hyprforge_setup::record::load(&r.env.setup_toml()).unwrap();
        let applied: Vec<&String> = record.items.keys().collect();
        assert_eq!(applied, ["wiring"], "{out}");
    }

    /// Unparseable is not empty: undo exits 1 and says so.
    #[test]
    fn an_unreadable_record_fails_the_undo() {
        let r = rig();
        let lock = r.dir.path().join("settings.lock");
        std::fs::create_dir_all(r.env.hyprforge_dir()).unwrap();
        std::fs::write(r.env.setup_toml(), "= nope").unwrap();
        let (status, _, _) = go(&r, &Mode::Undo(vec![]), "", &lock);
        assert_eq!(status, 1);
    }

    #[test]
    fn a_failed_item_exits_one() {
        let r = rig();
        let lock = r.dir.path().join("settings.lock");
        r.sys.reject_lua(Some("Hyprland rejected the generated shortcuts"));
        let (status, out, _) = go(&r, &Mode::Yes, "", &lock);
        assert_eq!(status, 1, "{out}");
        assert!(out.contains("✗"), "{out}");
    }
}

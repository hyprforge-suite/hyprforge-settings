//! A window rule in words: which windows it is about, and what it does to
//! them.
//!
//! The list used to title each rule with its class regex — `.*`, `^$`,
//! `^(steam)$` — and say nothing at all about what the rule did, so a
//! page of eleven rules read as eleven lines of config. The regex is still
//! there for anyone who turns config names on; this is the rest of the
//! page.
//!
//! Not a regex engine. A pattern made of plain names (`^(steam|discord)$`)
//! is read as those names; anything cleverer is shown with its
//! metacharacters softened (`steam_app_…`) rather than guessed at, because
//! a wrong plain-English reading is worse than an honest odd-looking one.

use hyprforge_windowrules::{Effects, Matcher};

/// What an app is called, from its window class: the installed entry's
/// `Name=` (`hyprforge_mime::MimeDb::app_for_window`), or `None` when
/// nothing installed answers to it. A function rather than the database
/// so the tests can say which apps exist.
pub type AppName<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Which windows a rule is about: "Steam — “Friends List”", "Every
/// window", "Windows titled Open File, Save File or Save As".
pub fn which(m: &Matcher, app_name: AppName<'_>) -> String {
    let class = m.class.as_deref().or(m.initial_class.as_deref()).map(str::trim).filter(|s| !s.is_empty());
    let title = m.title.as_deref().or(m.initial_title.as_deref()).map(str::trim).filter(|s| !s.is_empty());

    let mut out = match (class, title) {
        (Some(c), _) if is_anything(c) => "Every window".to_string(),
        (Some(c), _) if is_nothing(c) => "Windows with no app name".to_string(),
        (Some(c), _) => names(c, app_name),
        (None, Some(t)) => match t.strip_prefix("negative:") {
            Some(t) => format!("Windows not titled {}", or_list(t)),
            None => format!("Windows titled {}", or_list(t)),
        },
        (None, None) => match m.tag.as_deref().filter(|t| !t.trim().is_empty()) {
            Some(tag) => format!("Windows tagged {}", tag.trim()),
            None => "Every window".to_string(),
        },
    };
    if let (Some(_), Some(t)) = (class, title) {
        if !is_anything(t) && !is_nothing(t) {
            match t.strip_prefix("negative:") {
                Some(t) => out.push_str(&format!(", except {}", quoted_list(t))),
                None => out.push_str(&format!(" — {}", quoted_list(t))),
            }
        }
    }
    let mut states = Vec::new();
    match m.floating {
        Some(true) => states.push("floating"),
        Some(false) => states.push("tiled"),
        None => {}
    }
    if m.fullscreen == Some(true) {
        states.push("fullscreen");
    }
    if m.xwayland == Some(true) {
        states.push("X11");
    }
    if !states.is_empty() {
        out.push_str(&format!(" ({})", states.join(", ")));
    }
    out
}

/// What a rule does, in a few words each: "Floats · Workspace 3 in the
/// background". Empty when it does nothing this can name.
pub fn what(e: &Effects) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut flag = |on: Option<bool>, words: &str| {
        if on == Some(true) {
            parts.push(words.to_string());
        }
    };
    flag(e.float, "Floats");
    flag(e.tile, "Tiles");
    flag(e.fullscreen, "Fullscreen");
    flag(e.maximize, "Maximised");
    flag(e.pin, "Pinned");
    flag(e.center, "Centred");
    flag(e.no_blur, "No blur");
    flag(e.opaque, "Always opaque");
    flag(e.no_anim, "No animation");
    flag(e.no_focus, "Never focused");
    flag(e.no_initial_focus, "Not focused when it opens");
    flag(e.stay_focused, "Keeps focus");
    flag(e.dim_around, "Dims around it");
    flag(e.keep_aspect_ratio, "Keeps its shape");
    if !e.workspace.is_empty() {
        let name = e.workspace.name.trim();
        let place = match name.strip_prefix("special:") {
            Some(s) => format!("Scratchpad {s}"),
            None => match name.strip_prefix("name:") {
                Some(n) => format!("Workspace “{n}”"),
                None => format!("Workspace {name}"),
            },
        };
        parts.push(if e.workspace.silent { format!("{place} in the background") } else { place });
    }
    if let Some(m) = e.monitor.as_deref().filter(|m| !m.trim().is_empty()) {
        parts.push(format!("On {}", m.trim()));
    }
    if let Some([w, h]) = &e.size {
        parts.push(format!("{w} × {h}"));
    }
    if e.r#move.is_some() {
        parts.push("Placed".into());
    }
    if let Some(a) = e.opacity.active {
        parts.push(format!("{}% opaque", (a * 100.0).round()));
    }
    if let Some(r) = e.rounding {
        parts.push(format!("Corners {r}px"));
    }
    if let Some(i) = e.opacity.inactive {
        parts.push(format!("{}% opaque when not focused", (i * 100.0).round()));
    }
    if let Some(f) = e.opacity.fullscreen {
        parts.push(format!("{}% opaque when fullscreen", (f * 100.0).round()));
    }
    match e.border_size {
        Some(0) => parts.push("No border".into()),
        Some(b) => parts.push(format!("Border {b}px")),
        None => {}
    }
    if e.border_color.is_some() {
        parts.push("Own border colour".into());
    }
    if let Some([w, h]) = e.min_size {
        parts.push(format!("At least {w} × {h}"));
    }
    if let Some([w, h]) = e.max_size {
        parts.push(format!("At most {w} × {h}"));
    }
    if let Some(a) = e.animation.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
        parts.push(animation(a));
    }
    match e.idle_inhibit.as_deref().map(str::trim) {
        Some("always") => parts.push("Keeps the screen awake".into()),
        Some("focus") => parts.push("Keeps the screen awake while focused".into()),
        Some("fullscreen") => parts.push("Keeps the screen awake while fullscreen".into()),
        // `none` is the default said out loud: nothing to report.
        _ => {}
    }
    if let Some(events) = e.suppress_event.as_deref() {
        parts.extend(ignored(events));
    }
    if let Some(g) = e.group.as_deref() {
        parts.extend(grouping(g));
    }
    if let Some(t) = e.tag.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        parts.push(match (t.strip_prefix('+'), t.strip_prefix('-')) {
            (Some(t), _) => format!("Tagged {t}"),
            (_, Some(t)) => format!("Untagged {t}"),
            _ => format!("Tag {t} switched"),
        });
    }
    // Hyprland's own unit: milliseconds, and only against `killactive`.
    if let Some(ms) = e.no_close_for.filter(|ms| *ms > 0) {
        parts.push(format!("Can't be closed for {} after it opens", duration(ms)));
    }
    parts.join(" · ")
}

/// `popin 80%`, `slide left`, `gnomed`: how the window arrives.
fn animation(style: &str) -> String {
    let mut words = style.split_whitespace();
    let first = words.next().unwrap_or_default();
    let rest: Vec<&str> = words.collect();
    match (first, rest.as_slice()) {
        ("popin", _) => "Pops in".into(),
        ("slide", [side]) if matches!(*side, "top" | "bottom" | "left" | "right") => {
            format!("Slides in from the {side}")
        }
        ("slide" | "slidevert", _) => "Slides in".into(),
        ("gnomed", _) => "Unfolds as it opens".into(),
        // A style this does not know is still a style: name it rather
        // than guess what it looks like.
        _ => format!("“{style}” animation"),
    }
}

/// `suppress_event`'s space-separated events, as what the window is no
/// longer allowed to ask for.
fn ignored(events: &str) -> Vec<String> {
    let asked: Vec<&str> = events
        .split_whitespace()
        .map(|e| match e {
            "maximize" => "maximise",
            "fullscreen" => "go fullscreen",
            "maximizefullscreen" => "maximise or go fullscreen",
            "fullscreenoutput" => "go fullscreen on another screen",
            "activate" => "come to the front",
            "activatefocus" => "take focus",
            "x11configurerequest" => "move or resize itself",
            other => other,
        })
        .collect();
    if asked.is_empty() {
        return Vec::new();
    }
    vec![format!("Can't ask to {}", join_or(&asked.iter().map(|s| s.to_string()).collect::<Vec<_>>()))]
}

/// `group`'s options (Hyprland's window-rules page): what happens to the
/// window and tabbed groups. A bare `group` is `set`.
fn grouping(options: &str) -> Vec<String> {
    let options: Vec<&str> = options.split_whitespace().collect();
    if options.is_empty() {
        return vec!["Opens as a group".into()];
    }
    let mut out = Vec::new();
    for o in &options {
        match *o {
            "set" => out.push("Opens as a group".to_string()),
            "new" => out.push("Opens as a new group".to_string()),
            "lock" => out.push("Locks its group".to_string()),
            "barred" => out.push("Not added to the focused group".to_string()),
            "deny" => out.push("Never grouped".to_string()),
            "invade" => out.push("Opens in a locked group".to_string()),
            "unset" => out.push("Group rules cleared".to_string()),
            // Qualifiers on the options around them, not effects.
            "always" | "override" => {}
            other => out.push(format!("Group “{other}”")),
        }
    }
    out
}

/// 500 → "half a second", 3000 → "3 seconds", 1500 → "1.5 seconds".
fn duration(ms: i32) -> String {
    match ms {
        1000 => "a second".into(),
        500 => "half a second".into(),
        ms if ms < 1000 => format!("{ms} ms"),
        ms if ms % 1000 == 0 => format!("{} seconds", ms / 1000),
        ms => format!("{:.1} seconds", f64::from(ms) / 1000.0),
    }
}

fn is_anything(p: &str) -> bool {
    matches!(p, ".*" | "^.*$" | ".+" | "^.+$")
}

fn is_nothing(p: &str) -> bool {
    p == "^$"
}

/// `^(steam|discord)$` → ["steam", "discord"]: the alternatives of an
/// anchored group, each with regex escapes taken out and any remaining
/// pattern softened to "…".
fn alternatives(p: &str) -> Vec<String> {
    let inner = p.strip_prefix('^').unwrap_or(p);
    let inner = inner.strip_suffix('$').unwrap_or(inner);
    let inner = inner.strip_prefix('(').and_then(|s| s.strip_suffix(')')).unwrap_or(inner);
    inner
        .split('|')
        .map(|piece| {
            piece
                .replace(r"\d+", "…")
                .replace(".*", "…")
                .replace(".+", "…")
                .replace(r"\.", ".")
                .replace(r"\-", "-")
                .replace(r"\ ", " ")
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// App names from a class pattern, each as the app is usually known:
/// its installed entry's name when it has one (`com.github.hluk.copyq`
/// is CopyQ), and otherwise the best reading of the class itself.
fn names(p: &str, app_name: AppName<'_>) -> String {
    let names: Vec<String> = alternatives(p)
        .into_iter()
        .map(|n| {
            if let Some(name) = app_name(&n) {
                return name;
            }
            if n == "steam_app_…" {
                // Proton gives each game `steam_app_<its store number>`;
                // `\d+` there means any of them.
                return "Steam games".to_string();
            }
            if is_chrome_app(&n) {
                // Chrome's web apps are `chrome-<32 letters>-<profile>`:
                // the letters are an extension id, meaningless to read,
                // and with no entry installed there is no name to give.
                return "A Chrome web app".to_string();
            }
            if n.matches('.').count() >= 2 && !n.contains(' ') {
                n.rsplit('.').next().unwrap_or(&n).to_string()
            } else {
                n
            }
        })
        .collect();
    join_or(&names)
}

/// `chrome-nngceckbapebfimnlniiiahkandclblb-Default`: an id is 32 of the
/// letters a to p, which is how Chrome spells one.
fn is_chrome_app(class: &str) -> bool {
    let Some(rest) = class.strip_prefix("chrome-").or_else(|| class.strip_prefix("crx_")) else {
        return false;
    };
    let id = rest.split('-').next().unwrap_or_default();
    id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

fn or_list(p: &str) -> String {
    join_or(&alternatives(p))
}

fn quoted_list(p: &str) -> String {
    let quoted: Vec<String> = alternatives(p).into_iter().map(|t| format!("“{t}”")).collect();
    join_or(&quoted)
}

/// "a", "a or b", "a, b or c".
fn join_or(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An installed CopyQ and Steam, and nothing else.
    fn installed(class: &str) -> Option<String> {
        match class {
            "com.github.hluk.copyq" => Some("CopyQ".into()),
            "steam" => Some("Steam".into()),
            _ => None,
        }
    }

    fn which(m: &Matcher) -> String {
        super::which(m, &installed)
    }

    fn matcher(class: Option<&str>, title: Option<&str>) -> Matcher {
        Matcher { class: class.map(Into::into), title: title.map(Into::into), ..Matcher::default() }
    }

    /// The user's own rules, as the list had shown them, each read back.
    #[test]
    fn the_rules_on_this_machine_read_as_words() {
        assert_eq!(which(&matcher(Some(".*"), None)), "Every window");
        assert_eq!(
            which(&Matcher { floating: Some(true), ..matcher(Some("^$"), Some("^$")) }),
            "Windows with no app name (floating)"
        );
        assert_eq!(
            which(&matcher(None, Some("^(Open File|Save File|Open Folder|Save As)$"))),
            "Windows titled Open File, Save File, Open Folder or Save As"
        );
        assert_eq!(which(&matcher(Some("^(com.github.hluk.copyq)$"), None)), "CopyQ");
        assert_eq!(
            which(&matcher(Some("^(nm-connection-editor|blueman-manager)$"), None)),
            "nm-connection-editor or blueman-manager"
        );
        assert_eq!(which(&matcher(Some(r"^(steam_app_\d+)$"), None)), "Steam games");
        assert_eq!(
            which(&matcher(Some("^(steam)$"), Some("^(Friends List)$"))),
            "Steam — “Friends List”"
        );
        assert_eq!(
            which(&matcher(Some("^(steam)$"), Some("negative:^(Friends List|Steam Big Picture Mode)$"))),
            "Steam, except “Friends List” or “Steam Big Picture Mode”"
        );
    }

    #[test]
    fn an_app_with_no_entry_installed_still_reads_as_a_name() {
        let gone = |_: &str| None;
        assert_eq!(super::which(&matcher(Some("^(com.github.hluk.copyq)$"), None), &gone), "copyq");
        assert_eq!(
            super::which(&matcher(Some("^(chrome-nngceckbapebfimnlniiiahkandclblb-Default)$"), None), &gone),
            "A Chrome web app"
        );
        // Not an id: 32 letters, but `z` is not one Chrome uses.
        assert_eq!(
            super::which(&matcher(Some("^(chrome-zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-Default)$"), None), &gone),
            "chrome-zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-Default"
        );
    }

    /// Every effect the editor can set says something in the list: a rule
    /// that read as "Every window" and nothing else was this user's
    /// `suppress_event = "maximize"`.
    #[test]
    fn every_effect_has_words() {
        let one = |e: Effects| what(&e);
        assert_eq!(one(Effects { suppress_event: Some("maximize".into()), ..Effects::default() }), "Can't ask to maximise");
        assert_eq!(
            one(Effects { suppress_event: Some("activate activatefocus".into()), ..Effects::default() }),
            "Can't ask to come to the front or take focus"
        );
        assert_eq!(one(Effects { border_size: Some(0), ..Effects::default() }), "No border");
        assert_eq!(one(Effects { border_size: Some(4), ..Effects::default() }), "Border 4px");
        assert_eq!(one(Effects { min_size: Some([400, 300]), ..Effects::default() }), "At least 400 × 300");
        assert_eq!(one(Effects { max_size: Some([800, 600]), ..Effects::default() }), "At most 800 × 600");
        assert_eq!(one(Effects { animation: Some("popin 80%".into()), ..Effects::default() }), "Pops in");
        assert_eq!(one(Effects { animation: Some("slide left".into()), ..Effects::default() }), "Slides in from the left");
        assert_eq!(one(Effects { animation: Some("whoosh".into()), ..Effects::default() }), "“whoosh” animation");
        assert_eq!(one(Effects { idle_inhibit: Some("fullscreen".into()), ..Effects::default() }), "Keeps the screen awake while fullscreen");
        assert_eq!(one(Effects { idle_inhibit: Some("none".into()), ..Effects::default() }), "");
        assert_eq!(one(Effects { group: Some("set always".into()), ..Effects::default() }), "Opens as a group");
        assert_eq!(one(Effects { group: Some("deny".into()), ..Effects::default() }), "Never grouped");
        assert_eq!(one(Effects { tag: Some("+code".into()), ..Effects::default() }), "Tagged code");
        assert_eq!(one(Effects { no_close_for: Some(3000), ..Effects::default() }), "Can't be closed for 3 seconds after it opens");
        assert_eq!(one(Effects { no_close_for: Some(500), ..Effects::default() }), "Can't be closed for half a second after it opens");
        let mut e = Effects::default();
        e.opacity.inactive = Some(0.8);
        assert_eq!(one(e), "80% opaque when not focused");
    }

    #[test]
    fn what_a_rule_does_is_listed_in_a_few_words() {
        let mut e = Effects { float: Some(true), ..Effects::default() };
        e.workspace.name = "3".into();
        e.workspace.silent = true;
        assert_eq!(what(&e), "Floats · Workspace 3 in the background");
        assert_eq!(what(&Effects::default()), "");
    }
}

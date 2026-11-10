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

/// Which windows a rule is about: "steam — “Friends List”", "Every
/// window", "Windows titled Open File, Save File or Save As".
pub fn which(m: &Matcher) -> String {
    let class = m.class.as_deref().or(m.initial_class.as_deref()).map(str::trim).filter(|s| !s.is_empty());
    let title = m.title.as_deref().or(m.initial_title.as_deref()).map(str::trim).filter(|s| !s.is_empty());

    let mut out = match (class, title) {
        (Some(c), _) if is_anything(c) => "Every window".to_string(),
        (Some(c), _) if is_nothing(c) => "Windows with no app name".to_string(),
        (Some(c), _) => names(c),
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
    if e.border_color.is_some() {
        parts.push("Own border colour".into());
    }
    if e.idle_inhibit.is_some() {
        parts.push("Keeps the screen awake".into());
    }
    parts.join(" · ")
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
/// `com.github.hluk.copyq` is copyq.
fn names(p: &str) -> String {
    let names: Vec<String> = alternatives(p)
        .into_iter()
        .map(|n| {
            if n.matches('.').count() >= 2 && !n.contains(' ') {
                n.rsplit('.').next().unwrap_or(&n).to_string()
            } else {
                n
            }
        })
        .collect();
    join_or(&names)
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
        assert_eq!(which(&matcher(Some("^(com.github.hluk.copyq)$"), None)), "copyq");
        assert_eq!(
            which(&matcher(Some("^(nm-connection-editor|blueman-manager)$"), None)),
            "nm-connection-editor or blueman-manager"
        );
        assert_eq!(which(&matcher(Some(r"^(steam_app_\d+)$"), None)), "steam_app_…");
        assert_eq!(
            which(&matcher(Some("^(steam)$"), Some("^(Friends List)$"))),
            "steam — “Friends List”"
        );
        assert_eq!(
            which(&matcher(Some("^(steam)$"), Some("negative:^(Friends List|Steam Big Picture Mode)$"))),
            "steam, except “Friends List” or “Steam Big Picture Mode”"
        );
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

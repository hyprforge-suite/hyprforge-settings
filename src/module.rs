use hyprforge_ui::theme::FontScale;
use hyprforge_ui::widgets::Tint;
use iced::{Element, Subscription, Task};

/// Shared shape for anything hosted inside the Settings app shell, modeled
/// directly on iced's own `update`/`view` split so a module feels like a
/// miniature iced application rather than a bespoke plugin API.
///
/// The shell draws every page's chrome — the title, the line beside it,
/// the buttons that act on the whole page, the sidebar entry — so a page
/// only draws its body. The methods beyond `update`/`view` are how a page
/// tells the shell what to put there, and each has a default so a page
/// that has nothing to say says nothing.
pub trait SettingsModule {
    type Message: std::fmt::Debug + Send + Clone + 'static;

    fn update(&mut self, message: Self::Message) -> Task<Self::Message>;
    /// `scale` is the app-wide accessibility font scale (vision pillar #7)
    /// — every module receives it rather than reading it from ambient
    /// state, so it's impossible to build a screen that forgets to honor it.
    fn view(&self, scale: FontScale) -> Element<'_, Self::Message>;

    fn subscription(&self) -> Subscription<Self::Message> {
        Subscription::none()
    }

    /// The mono line beside the page title: what the page is talking to,
    /// or what it found — `NetworkManager`, `2 connected`, `14 rules`.
    ///
    /// A fact, never a description of the page. "Configure your
    /// displays" says nothing the title did not; "hyprforge-displayd is
    /// not running" is worth the space.
    fn subtitle(&self) -> Option<String> {
        None
    }

    /// Buttons that act on the whole page — Refresh, Scan — drawn at the
    /// right of the title row, where they sat before the shell took the
    /// title over.
    fn header_actions(&self, _scale: FontScale) -> Option<Element<'_, Self::Message>> {
        None
    }

    /// Changes on this page that are not written yet, if any — the shell
    /// draws one bar for them along the foot of every page, and the
    /// header's chip turns from "live" to "pending" while they exist.
    ///
    /// `None` is a claim: that everything shown on the page is already
    /// what the file says. A page that edits in place and writes at once
    /// returns `None` by default and is right to.
    fn pending(&self) -> Option<Pending<Self::Message>> {
        None
    }

    /// Everything on this page the search palette can find, and how to
    /// bring each one into view.
    ///
    /// Only pages whose settings come from a catalogue list anything:
    /// those are the pages with dozens of settings and a key for each,
    /// which is what searching by config key needs. Every page is found
    /// by its title regardless — the shell does that part.
    fn search_entries(&self) -> Vec<SearchEntry<Self::Message>> {
        Vec::new()
    }

    /// A short mark at the end of this page's sidebar entry: a count, an
    /// interface name, or a dot for a live connection.
    fn nav_badge(&self) -> Option<NavBadge> {
        None
    }
}

/// Changes a page is holding back, and how to write or drop them.
#[derive(Debug, Clone)]
pub struct Pending<M> {
    /// What is waiting — "3 pending changes", "Layout not applied".
    pub summary: String,
    /// The setting as it will be written, when one line can say it —
    /// `input:follow_mouse = 1`. A readable form of the change, not the
    /// generated Lua byte for byte.
    pub preview: Option<String>,
    pub apply: M,
    /// `None` when the page cannot take its drafts back.
    pub discard: Option<M>,
}

impl<M> Pending<M> {
    /// Maps the messages into the shell's own, as `Element::map` does.
    pub fn map<N>(self, f: impl Fn(M) -> N) -> Pending<N> {
        Pending {
            summary: self.summary,
            preview: self.preview,
            apply: f(self.apply),
            discard: self.discard.map(f),
        }
    }
}

/// The preview for a set of typed-but-unapplied values: the first as
/// `key = value`, and how many more follow it.
pub fn drafts_preview<'a>(mut drafts: impl Iterator<Item = (&'a str, &'a str)>) -> Option<String> {
    let (key, value) = drafts.next()?;
    let rest = drafts.count();
    Some(match rest {
        0 => format!("{key} = {value}"),
        n => format!("{key} = {value}  +{n} more"),
    })
}

/// One setting the search palette can offer.
#[derive(Debug, Clone)]
pub struct SearchEntry<M> {
    /// The setting's name, as its row shows it — "Focus follows mouse".
    pub label: &'static str,
    /// Its config key — `input:follow_mouse`.
    pub key: &'static str,
    /// What this page has stored for it, if anything. `None` is "not set
    /// here", never "false".
    pub value: Option<String>,
    /// The messages that bring it into view on its page: switch to the
    /// right tab, narrow the page's own filter to it.
    pub reveal: Vec<M>,
}

impl<M> SearchEntry<M> {
    /// Maps the messages into the shell's own.
    pub fn map<N>(self, f: impl Fn(M) -> N) -> SearchEntry<N> {
        SearchEntry {
            label: self.label,
            key: self.key,
            value: self.value,
            reveal: self.reveal.into_iter().map(f).collect(),
        }
    }
}

/// A stored value as the palette shows it — `true`, `12`, `rgba(...)`.
pub fn value_text(value: &hyprforge_core::hlconfig::Value) -> String {
    use hyprforge_core::hlconfig::Value;
    match value {
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Text(t) => t.clone(),
    }
}

/// What a sidebar entry can carry beside its label.
#[derive(Debug, Clone, PartialEq)]
pub enum NavBadge {
    /// Dim mono text — `14`, `wlan0`.
    Text(String),
    /// A dot in a state colour — a device connected, a radio on.
    Dot(Tint),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bar shows the setting as it will be written, so one draft is
    /// exactly that line and more say how many are hidden behind it —
    /// never a silent subset.
    #[test]
    fn a_preview_names_the_first_draft_and_counts_the_rest() {
        assert_eq!(drafts_preview([("a:b", "1")].into_iter()).as_deref(), Some("a:b = 1"));
        assert_eq!(
            drafts_preview([("a:b", "1"), ("c:d", "x"), ("e:f", "y")].into_iter()).as_deref(),
            Some("a:b = 1  +2 more")
        );
        assert_eq!(drafts_preview(std::iter::empty()), None);
    }
}

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

    /// A short mark at the end of this page's sidebar entry: a count, an
    /// interface name, or a dot for a live connection.
    fn nav_badge(&self) -> Option<NavBadge> {
        None
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

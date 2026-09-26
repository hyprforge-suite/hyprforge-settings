//! Rendering one catalogued `hl.config` setting as an editor row.
//!
//! Shared by every screen built on [`hyprforge_core::hlconfig`] — Input
//! and Appearance today. The row is the same in both because the catalog
//! makes it the same: a kind decides the control, a value decides its
//! state, and where the value came from decides what the row says about
//! itself.
//!
//! The last of those is load-bearing enough to be worth restating.
//! A row shows one of three things, and they mean different things to a
//! user: a value **Hyprforge writes** into the config, one it is merely
//! **reporting back** from their own config, or **Hyprland's default**.
//! Showing the catalog default when the user's config says otherwise is a
//! screen that lies about what is running — it rendered Num Lock as off on
//! a machine where it was on, which is how this distinction got built.

use hyprforge_core::hlconfig::import::Live;
use hyprforge_core::hlconfig::{Kind, Setting, Settings, Value};
use hyprforge_ui::density;
use hyprforge_ui::theme::{self, spacing, surface, FontScale};
use hyprforge_ui::widgets::{
    chip, config_line, dropdown_menu_style, dropdown_style, hint_text, inset_input_style,
    secondary_button, section_label, segmented_choice, setting_list, setting_row, toggle,
    value_slider, Tint,
};
use iced::widget::{column, container, pick_list, row, text_input, tooltip};
use iced::{Element, Length};
use std::collections::BTreeMap;

/// Where the value a row is showing came from. Not cosmetic: it's the
/// difference between a value this app writes and one it is reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Hyprforge writes this key, and it wins over the user's config.
    Owned,
    /// The user's own config sets it; Hyprforge is only displaying it.
    UserConfig,
    /// Nobody set it, so this is what Hyprland does on its own.
    Default,
}

impl Source {
    /// What the chip beside a row's config line says.
    pub fn short_label(self) -> &'static str {
        match self {
            Source::Owned => "set by hyprforge",
            Source::UserConfig => "your config",
            Source::Default => "hyprland default",
        }
    }

    /// Always dim. Where a value came from is not a state — nothing is
    /// wrong with a default — so it gets no state colour, and the words
    /// are what tell the three apart.
    pub fn tint(self) -> Tint {
        Tint::Dim
    }
}

/// One entry in a picker built from something discovered at runtime — an
/// installed keyboard layout, a connected monitor.
///
/// Carries the value and the label separately because the two differ:
/// `us` is what Hyprland wants, "English (US)" is what a user recognises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynChoice {
    pub value: String,
    pub label: String,
}

impl std::fmt::Display for DynChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

/// Everything a row needs, plus how to turn an interaction into the
/// caller's own `Message`.
pub struct RowContext<'a, M> {
    pub settings: &'a Settings,
    pub live: &'a BTreeMap<&'static str, Live>,
    pub drafts: &'a BTreeMap<&'static str, String>,
    pub draft_errors: &'a BTreeMap<&'static str, String>,
    /// Options discovered at runtime, keyed by setting. A setting with
    /// entries here is offered as a picker instead of a text field.
    ///
    /// Supplied by the module rather than the catalog because the
    /// catalog is a compile-time description and these depend on the
    /// machine: which layouts are installed, which monitors are plugged
    /// in.
    pub choices: &'a BTreeMap<&'static str, Vec<DynChoice>>,
    pub on_set: fn(&'static str, Value) -> M,
    pub on_draft: fn(&'static str, String) -> M,
    pub on_reset: fn(&'static str) -> M,
    /// Sent when a text field is submitted with Enter.
    pub on_submit: M,
}

impl<'a, M: Clone + 'static> RowContext<'a, M> {
    /// The value a control should display, and where it came from.
    ///
    /// Owned beats live beats the catalog default. The order matters in
    /// both directions: an owned value that hasn't reached the compositor
    /// yet must still show what the user chose, and an unowned one must
    /// show what's running rather than what Hyprland would do by default.
    pub fn effective(&self, setting: &Setting) -> (Value, Source) {
        if let Some(v) = self.settings.get(setting.key) {
            return (v.clone(), Source::Owned);
        }
        if let Some(live) = self.live.get(setting.key) {
            return (
                live.value.clone(),
                if live.set { Source::UserConfig } else { Source::Default },
            );
        }
        (Value::default_for(&setting.kind), Source::Default)
    }

    pub fn owns(&self, key: &str) -> bool {
        self.settings.get(key).is_some()
    }

    /// The text a field should show: the draft if one is being typed,
    /// otherwise the effective value.
    pub fn shown_text(&self, setting: &Setting) -> String {
        if let Some(draft) = self.drafts.get(setting.key) {
            return draft.clone();
        }
        render_for_edit(&self.effective(setting).0)
    }

    /// One setting as a striped row: its name, the line it writes, where
    /// the value comes from, and a control chosen from its kind.
    ///
    /// `index` is its place in its group, for the alternating stripe.
    pub fn row(&self, setting: &'static Setting, index: usize, scale: FontScale) -> Element<'a, M> {
        let key = setting.key;
        let owned = self.owns(key);
        let (current, source) = self.effective(setting);
        let (on_set, on_draft) = (self.on_set, self.on_draft);

        // A discovered picker wins over the catalog's own idea of the
        // control, except when the stored value is a comma-separated
        // list. `kb_layout = "us,cz"` is the switchable-layout pattern,
        // and a single-select dropdown can't express it — offering one
        // would replace both layouts with whichever the user clicked.
        let multi_valued = current.as_text().is_some_and(|t| t.contains(','));
        if let Some(options) = self.choices.get(key).filter(|_| !multi_valued) {
            let current_text = current.as_text().unwrap_or_default().to_string();
            let options = options_including(options, &current_text);
            let selected = options.iter().find(|c| c.value == current_text).cloned();
            let control = pick_list(options, selected, move |c: DynChoice| {
                on_set(key, Value::Text(c.value))
            })
            .style(dropdown_style)
            .menu_style(dropdown_menu_style)
                        .width(Length::Fixed(scale.apply(TEXT_FIELD_WIDTH)))
            .into();
            return self.wrap(setting, &current, control, source, owned, index, scale);
        }

        let control: Element<'a, M> = match setting.kind {
            Kind::Bool { default } => {
                let current = current.as_bool().unwrap_or(default);
                toggle(current, scale).on_toggle(move |b| on_set(key, Value::Bool(b))).into()
            }
            Kind::IntEnum { default, choices } => {
                let current = current.as_int().unwrap_or(default);
                let options: Vec<Choice> = choices
                    .iter()
                    .map(|(v, label)| Choice { value: *v, label })
                    .collect();
                let selected = options.iter().find(|c| c.value == current).copied();
                if fits_segments(options.iter().map(|c| c.label)) {
                    segmented_choice(&options, selected.as_ref(), |c| c.label.to_string(), move |c| {
                        on_set(key, Value::Int(c.value))
                    }, scale)
                } else {
                    pick_list(options, selected, move |c: Choice| on_set(key, Value::Int(c.value)))
                        .style(dropdown_style)
                        .menu_style(dropdown_menu_style)
                        .width(Length::Fixed(scale.apply(TEXT_FIELD_WIDTH)))
                        .into()
                }
            }
            Kind::TextEnum { default, choices } => {
                let current = current.as_text().unwrap_or(default).to_string();
                let options: Vec<TextChoice> =
                    choices.iter().map(|c| TextChoice { value: c }).collect();
                let selected = options.iter().find(|c| c.value == current.as_str()).copied();
                if fits_segments(options.iter().map(|c| c.value)) {
                    segmented_choice(&options, selected.as_ref(), |c| c.value.to_string(), move |c| {
                        on_set(key, Value::Text(c.value.to_string()))
                    }, scale)
                } else {
                    pick_list(options, selected, move |c: TextChoice| {
                        on_set(key, Value::Text(c.value.to_string()))
                    })
                    .style(dropdown_style)
                    .menu_style(dropdown_menu_style)
                        .width(Length::Fixed(scale.apply(TEXT_FIELD_WIDTH)))
                    .into()
                }
            }
            Kind::Int { min: Some(min), max: Some(max), .. } if slides(min as f64, max as f64) => {
                // A drag drafts, and letting go applies — the same as
                // typing a number and pressing Enter. Writing on every
                // step would rewrite the config once per pixel dragged.
                let shown = self.shown_text(setting).trim().parse::<f64>().ok();
                let value = shown.unwrap_or(current.as_float().unwrap_or(min as f64));
                value_slider(
                    min as f32..=max as f32,
                    value.clamp(min as f64, max as f64) as f32,
                    1.0,
                    move |v| on_draft(key, (v.round() as i64).to_string()),
                    Some(self.on_submit.clone()),
                    format!("{}", value.round() as i64),
                    scale,
                )
            }
            Kind::Float { min: Some(min), max: Some(max), .. } if slides(min, max) => {
                let shown = self.shown_text(setting).trim().parse::<f64>().ok();
                let value = shown.unwrap_or(current.as_float().unwrap_or(min));
                let step = float_step(min, max);
                value_slider(
                    min as f32..=max as f32,
                    value.clamp(min, max) as f32,
                    step as f32,
                    move |v| on_draft(key, format_float(v as f64, step)),
                    Some(self.on_submit.clone()),
                    format_float(value, step),
                    scale,
                )
            }
            _ => text_input(&placeholder_for(&setting.kind), &self.shown_text(setting))
                .on_input(move |raw| on_draft(key, raw))
                .on_submit(self.on_submit.clone())
                .padding(spacing::SM)
                .style(inset_input_style)
                .width(Length::Fixed(scale.apply(TEXT_FIELD_WIDTH)))
                .into(),
        };

        // A colour row shows the colour it names. Reading a hex string and
        // picturing it is not something anyone does reliably, and this is
        // the one control where the value *is* the appearance.
        let control: Element<'a, M> = match setting.kind {
            Kind::Color { .. } | Kind::ColorInt { .. } => row![
                swatch(current.as_text().unwrap_or_default()),
                control,
            ]
            .spacing(spacing::SM)
            .align_y(iced::Alignment::Center)
            .into(),
            _ => control,
        };

        self.wrap(setting, &current, control, source, owned, index, scale)
    }

    /// The row every control sits in: the name (with its help on hover),
    /// the config line it writes, where the value comes from, and the
    /// Reset button when Hyprforge owns the key.
    #[allow(clippy::too_many_arguments)]
    fn wrap(
        &self,
        setting: &'static Setting,
        current: &Value,
        control: Element<'a, M>,
        source: Source,
        owned: bool,
        index: usize,
        scale: FontScale,
    ) -> Element<'a, M> {
        let key = setting.key;
        let on_reset = self.on_reset;

        // The config line and the source, together: the line is what the
        // setting is, the chip says whose it is. Always both — the chip
        // on a default is what stops a row claiming a value is the user's
        // when Hyprland is only doing what it does unasked (see the
        // module doc).
        let mut hint = column![row![
            config_line(config_text(key, current), scale),
            chip(source.short_label(), source.tint(), scale),
        ]
        .spacing(spacing::SM)
        .align_y(iced::Alignment::Center)]
        .spacing(2.0);
        if let Some(problem) = self.draft_errors.get(key) {
            hint = hint.push(hint_text(problem.clone(), scale).color(theme::error()));
        }

        let control: Element<'a, M> = if owned {
            row![control, secondary_button("Reset").on_press(on_reset(key))]
                .spacing(spacing::SM)
                .align_y(iced::Alignment::Center)
                .into()
        } else {
            control
        };

        // The help moves into a tooltip on the row. It was a line of its
        // own under every label, which made a page of fifty settings
        // mostly explanation; the mockup's rows are one line and a config
        // line, and the text is still one hover — or one search — away.
        let body = setting_row(index, setting.label, Some(hint.into()), control, scale);
        if setting.help.trim().is_empty() {
            return body;
        }
        tooltip(body, help_bubble(setting.help, scale), tooltip::Position::Bottom)
            .gap(spacing::XS)
            .into()
    }
}

/// A row's config line: `key = value`, with an empty value written as
/// `""` — a bare `key = ` reads as a line that failed to draw.
fn config_text(key: &str, value: &Value) -> String {
    match render_for_edit(value) {
        v if v.is_empty() => format!("{key} = \"\""),
        v => format!("{key} = {v}"),
    }
}

/// A category: its heading, one line on what it covers, and its rows.
///
/// A heading over a striped stack, not a bordered card with dividers:
/// the stripe is what separates the rows now, and a card around them
/// was a second frame doing the same job.
pub fn category_group<'a, M: 'a>(
    label: &str,
    help: &'a str,
    rows: Vec<Element<'a, M>>,
    scale: FontScale,
) -> Element<'a, M> {
    let mut group = column![section_label(label, scale)].spacing(spacing::SM);
    if !help.trim().is_empty() {
        group = group.push(hint_text(help, scale));
    }
    group.push(setting_list(rows)).into()
}

/// A setting's help, as a small card under the row it explains.
fn help_bubble<'a, M: 'a>(help: &'a str, scale: FontScale) -> Element<'a, M> {
    container(hint_text(help, scale).color(theme::text()))
        .padding([spacing::XS, spacing::SM])
        .max_width(scale.apply(HELP_BUBBLE_WIDTH))
        .style(|_t: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(surface::card())),
            border: iced::Border {
                radius: density::inner_radius().into(),
                width: 1.0,
                color: surface::card_border(),
            },
            ..container::Style::default()
        })
        .into()
}

/// How wide a help tooltip grows before it wraps.
const HELP_BUBBLE_WIDTH: f32 = 420.0;

/// A free-text field's width, and a dropdown's: enough for a path, a
/// comma list or a layout's name, and fixed so a control does not
/// stretch across the row and squeeze its own label.
const TEXT_FIELD_WIDTH: f32 = 240.0;

/// The widest span a number gets a slider for. Past this a pixel of
/// drag is several units, and typing the number is the better control —
/// a timeout in milliseconds wants a field, a gap in pixels a slider.
const SLIDER_MAX_SPAN: f64 = 400.0;

/// Whether a bounded number is better as a slider than a field.
fn slides(min: f64, max: f64) -> bool {
    min.is_finite() && max.is_finite() && max > min && max - min <= SLIDER_MAX_SPAN
}

/// A float slider's step: a hundredth of its span, rounded to a power of
/// ten so the readout shows round numbers — 0.01 over a span of 1, 0.1
/// over a span of 10.
fn float_step(min: f64, max: f64) -> f64 {
    let raw = (max - min) / 100.0;
    10f64.powf(raw.log10().floor())
}

/// A float as its slider step shows it: as many decimals as the step
/// has, so a 0.01 step reads `0.35` and never `0.35000001`.
fn format_float(value: f64, step: f64) -> String {
    let decimals = (-step.log10()).ceil().max(0.0) as usize;
    format!("{value:.decimals$}")
}

/// Whether a set of options reads as a segmented control: three or
/// fewer, each short. A fourth option, or one long label, and the row
/// runs out of room — a dropdown is the honest control then.
fn fits_segments<'s>(labels: impl Iterator<Item = &'s str>) -> bool {
    const MAX_SEGMENTS: usize = 3;
    const MAX_SEGMENT_LABEL: usize = 12;
    let labels: Vec<&str> = labels.collect();
    labels.len() <= MAX_SEGMENTS && labels.iter().all(|l| l.chars().count() <= MAX_SEGMENT_LABEL)
}

/// A settings row: a label on the left, a control on the right.
///
/// Generic over the message type because each module has its own. It
/// lives here because it was written out four times — `desktop.rs`,
/// `session.rs`, `default_apps.rs` and the variant above — identically
/// each time, so a change to the split, the spacing or the label size
/// meant four edits to keep the screens looking like one app. That is
/// the drift CLAUDE.md names when two screens grew their own palettes.
///
/// It and `hyprforge_ui::widgets::row_field` draw the same row now —
/// both are `setting_row` underneath — and differ only in whether the
/// caller passes the scale.
pub fn labelled<'a, M: 'a>(
    label: &'a str,
    control: Element<'a, M>,
    scale: FontScale,
) -> Element<'a, M> {
    // The shared setting row now, like `widgets::row_field`: label at
    // one end, control at the other, capped at a field's width so a
    // stretching control does not squeeze its label. Unstriped, because
    // it is not told its place in a list.
    setting_row(
        1,
        label,
        None,
        container(control).max_width(scale.apply(TEXT_FIELD_WIDTH + 100.0)),
        scale,
    )
}

/// The options a picker should offer, with `current` kept even when it
/// isn't among them.
///
/// A layout or monitor can be perfectly valid and absent from the list —
/// a monitor that's unplugged right now, a layout from a custom XKB
/// tree. A picker that dropped it would show nothing selected, so the
/// next click would replace a working setting with whatever was hit.
fn options_including(known: &[DynChoice], current: &str) -> Vec<DynChoice> {
    let mut options = known.to_vec();
    if !current.is_empty() && !options.iter().any(|c| c.value == current) {
        options.insert(
            0,
            DynChoice {
                value: current.to_string(),
                label: format!("{current} (not installed)"),
            },
        );
    }
    options
}

/// A small filled square of the colour a row names.
///
/// Falls back to nothing drawn rather than to some stand-in colour: a
/// swatch showing the wrong colour would be worse than no swatch, since
/// the whole point of it is to be believed.
fn swatch<'a, M: 'a>(value: &str) -> Element<'a, M> {
    let block = container(column![])
        .width(Length::Fixed(22.0))
        .height(Length::Fixed(22.0));
    match parse_color(value) {
        Some(color) => block
            .style(move |_theme: &iced::Theme| iced::widget::container::Style {
                background: Some(iced::Background::Color(color)),
                border: iced::Border {
                    radius: 4.0.into(),
                    width: 1.0,
                    color: iced::Color::from_rgba(1.0, 1.0, 1.0, 0.2),
                },
                ..Default::default()
            })
            .into(),
        None => block.into(),
    }
}

/// Hyprland's `rgba(rrggbbaa)` / `rgb(rrggbb)` forms. Anything else — a
/// real gradient, `0xAARRGGBB`, a half-typed value — yields `None`.
///
/// Strictness is the point: the shared parser accepts exactly what
/// `hlconfig::model` accepts, so the swatch can never draw a confident
/// colour beside a value the validator is about to refuse.
fn parse_color(value: &str) -> Option<iced::Color> {
    let c = hyprforge_look::Color::parse(value).ok()?;
    Some(iced::Color::from_rgba8(c.r, c.g, c.b, c.a as f32 / 255.0))
}

/// A dropdown entry for a [`Kind::IntEnum`]. Carries the number so the
/// message doesn't have to map a label back to a value — a mapping that
/// breaks the moment two choices share a label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Choice {
    value: i64,
    label: &'static str,
}

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TextChoice {
    value: &'static str,
}

impl std::fmt::Display for TextChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.value.is_empty() {
            f.write_str("Default")
        } else {
            f.write_str(self.value)
        }
    }
}

/// How a value is written into a text field. Floats keep their decimal
/// point so a field showing `1` for a float setting can't be mistaken for
/// an integer one.
pub fn render_for_edit(value: &Value) -> String {
    match value {
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => {
            let s = format!("{f:?}");
            if s.contains('.') {
                s
            } else {
                format!("{s}.0")
            }
        }
        Value::Text(s) => s.clone(),
    }
}

fn placeholder_for(kind: &Kind) -> String {
    match kind {
        Kind::Int { default, .. } | Kind::Gaps { default, .. } => default.to_string(),
        Kind::Float { default, .. } => render_for_edit(&Value::Float(*default)),
        Kind::Text { default: "" } => "not set".to_string(),
        Kind::Text { default } | Kind::Color { default } | Kind::ColorInt { default } => {
            (*default).to_string()
        }
        _ => String::new(),
    }
}

/// Turns what was typed into the type the catalog expects. The error text
/// is what the user sees under the field, so it names the expectation
/// rather than echoing a parser's wording.
pub fn parse_for(kind: &Kind, raw: &str) -> Result<Value, String> {
    let trimmed = raw.trim();
    match kind {
        Kind::Int { .. } | Kind::IntEnum { .. } | Kind::Gaps { .. } => trimmed
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| "expected a whole number".to_string()),
        Kind::Float { .. } => trimmed
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| "expected a number".to_string()),
        Kind::Bool { .. } => trimmed
            .parse::<bool>()
            .map(Value::Bool)
            .map_err(|_| "expected true or false".to_string()),
        // A colour is typed as text; `validate` checks the form, so the
        // same rule applies whether it was typed here or written into the
        // TOML by hand.
        Kind::Color { .. } | Kind::ColorInt { .. } => Ok(Value::Text(trimmed.to_string())),
        // Text keeps its surrounding whitespace: a layout list like
        // "us, cz" is the user's to format.
        Kind::Text { .. } | Kind::TextEnum { .. } => Ok(Value::Text(raw.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_colour_swatch_reads_both_hyprland_forms() {
        let c = parse_color("rgba(bd93f9ff)").unwrap();
        assert!((c.r - 0xbd as f32 / 255.0).abs() < 1e-6);
        assert!((c.a - 1.0).abs() < 1e-6);

        let c = parse_color("rgb(282a36)").unwrap();
        assert!((c.b - 0x36 as f32 / 255.0).abs() < 1e-6);
        assert!((c.a - 1.0).abs() < 1e-6, "rgb() is fully opaque");
    }

    #[test]
    fn alpha_is_read_from_the_last_pair() {
        let c = parse_color("rgba(1a1a1a80)").unwrap();
        assert!((c.a - 128.0 / 255.0).abs() < 0.01, "got {}", c.a);
    }

    /// A swatch showing the wrong colour is worse than no swatch, so
    /// anything unrecognised draws nothing rather than guessing.
    #[test]
    fn an_unparseable_colour_yields_no_swatch() {
        // `rgba(bd93f9)` is six digits where eight are required — the exact
        // mismatch `hlconfig::model` refuses, so the swatch must refuse it too.
        for bad in ["", "rgba(", "rgba(zzzzzzzz)", "0xffbd93f9", "rgba(bd93f9)", "rgb(282a36ff)", "ffbd93f9 0deg"] {
            assert!(parse_color(bad).is_none(), "{bad} should not parse");
        }
    }

    /// A half-typed colour must not panic the view — it renders every
    /// frame, including mid-keystroke.
    #[test]
    fn a_partially_typed_colour_is_harmless() {
        for partial in ["r", "rgba", "rgba(", "rgba(bd", "rgba(bd93f9f"] {
            assert!(parse_color(partial).is_none(), "{partial}");
        }
    }

    #[test]
    fn a_whole_float_keeps_its_decimal_point() {
        assert_eq!(render_for_edit(&Value::Float(1.0)), "1.0");
        assert_eq!(render_for_edit(&Value::Float(0.5)), "0.5");
    }

    #[test]
    fn gaps_parse_as_whole_numbers() {
        let kind = Kind::Gaps { default: 5, min: Some(0), max: Some(200) };
        assert_eq!(parse_for(&kind, " 12 "), Ok(Value::Int(12)));
        assert!(parse_for(&kind, "12.5").is_err());
    }

    /// A monitor that's unplugged right now, or a layout from a custom
    /// XKB tree, is still a valid setting. A picker that dropped it would
    /// show nothing selected, so the next click would replace a working
    /// value with whatever was hit.
    #[test]
    fn a_picker_keeps_a_value_it_does_not_recognise() {
        let known = vec![
            DynChoice { value: "eDP-2".into(), label: "eDP-2".into() },
            DynChoice { value: "DP-3".into(), label: "DP-3".into() },
        ];
        let options = options_including(&known, "HDMI-A-1");
        assert_eq!(options.len(), 3);
        assert_eq!(options[0].value, "HDMI-A-1", "kept, and first so it's visible");
        assert!(options[0].label.contains("not installed"), "and marked as absent");
    }

    #[test]
    fn a_recognised_value_is_not_duplicated_or_relabelled() {
        let known = vec![DynChoice { value: "eDP-2".into(), label: "eDP-2".into() }];
        let options = options_including(&known, "eDP-2");
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].label, "eDP-2");
    }

    #[test]
    fn an_empty_value_adds_no_entry() {
        let known = vec![DynChoice { value: "eDP-2".into(), label: "eDP-2".into() }];
        assert_eq!(options_including(&known, "").len(), 1);
    }

    #[test]
    fn an_empty_value_is_written_as_empty_quotes() {
        assert_eq!(config_text("input:kb_variant", &Value::Text(String::new())), "input:kb_variant = \"\"");
        assert_eq!(config_text("general:gaps_in", &Value::Int(5)), "general:gaps_in = 5");
    }

    /// The three chips share a colour, so their words are the only thing
    /// telling "Hyprforge writes this" from "your config says this" from
    /// "nobody set it" — and a row that blurs those lies about what is
    /// running (see the module doc).
    #[test]
    fn the_three_sources_have_distinct_labels() {
        let labels = [Source::Owned, Source::UserConfig, Source::Default].map(Source::short_label);
        let unique: std::collections::HashSet<_> = labels.iter().collect();
        assert_eq!(unique.len(), 3);
    }

    /// A gap in pixels gets a slider; a timeout in milliseconds, where a
    /// pixel of drag would be several units, keeps its field. So does
    /// anything without both ends, which a slider cannot draw.
    #[test]
    fn only_a_short_bounded_range_gets_a_slider() {
        assert!(slides(0.0, 50.0));
        assert!(slides(-1.0, 1.0));
        assert!(!slides(0.0, 10_000.0));
        assert!(!slides(5.0, 5.0), "no range to slide over");
        assert!(!slides(0.0, f64::INFINITY));
    }

    /// A float slider moves in round steps and says so without float
    /// noise — `0.35`, never `0.35000001`.
    #[test]
    fn a_float_slider_steps_and_reads_in_round_numbers() {
        assert_eq!(float_step(0.0, 1.0), 0.01);
        assert_eq!(float_step(-1.0, 1.0), 0.01);
        assert_eq!(float_step(0.0, 10.0), 0.1);
        assert_eq!(format_float(0.35000001, 0.01), "0.35");
        assert_eq!(format_float(3.0, 0.1), "3.0");
    }

    /// Three short options fit a row as segments; a fourth, or one long
    /// label, and they would not.
    #[test]
    fn only_a_few_short_options_become_segments() {
        assert!(fits_segments(["Off", "Always", "Fullscreen"].into_iter()));
        assert!(!fits_segments(["a", "b", "c", "d"].into_iter()));
        assert!(!fits_segments(["Off", "Keep fullscreen and focus behind it"].into_iter()));
    }
}

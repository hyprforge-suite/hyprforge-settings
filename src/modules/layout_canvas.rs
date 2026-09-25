//! Drag-arrange canvas for the Displays module's layout editor. Renders
//! each head as a rectangle positioned/sized proportionally to its real
//! geometry and lets the user drag it around (with edge-snapping against
//! other heads); every drag frame publishes a `(connector_hint, new_x,
//! new_y)` message so the caller owns the authoritative position (this
//! widget holds no position state of its own beyond "which head is
//! currently grabbed"). Fills the width it's given and computes a
//! fit-to-bounds scale from the heads' own bounding box, rather than a
//! fixed pixels-per-unit constant, so a spread-out or close-together
//! arrangement is always fully visible.
//!
//! That fit is recomputed whenever the arrangement changes — except during
//! a drag, which holds the transform it started with. Refitting mid-drag
//! would rescale the diagram in response to the very movement being made,
//! sliding the grabbed rectangle out from under the pointer.

use hyprforge_ui::theme;
use iced::mouse;
use iced::widget::canvas::{self, Canvas, Path, Text as CanvasText};
use iced::{Element, Length, Point, Rectangle, Renderer, Size, Theme, Vector};

pub const CANVAS_HEIGHT: f32 = 380.0;

/// The canvas's own corner, and a head's: the card radius the rows below
/// use, and a smaller one for something drawn on it.
const CANVAS_RADIUS: f32 = 9.0;
const HEAD_RADIUS: f32 = 6.0;

/// The dot grid's pitch, and a head's number badge.
const GRID_STEP: f32 = 16.0;
const BADGE_SIDE: f32 = 16.0;

/// `top` laid over an opaque `bottom`, as one opaque colour.
///
/// A canvas fill is drawn once, over whatever the canvas already holds —
/// here the dot grid — so a translucent accent would let the dots through
/// the selected monitor. Compositing onto the card colour first draws the
/// tint the selection means and hides the grid under it, as the other
/// cards do.
fn over(top: iced::Color, bottom: iced::Color) -> iced::Color {
    let a = top.a;
    iced::Color {
        r: top.r * a + bottom.r * (1.0 - a),
        g: top.g * a + bottom.g * (1.0 - a),
        b: top.b * a + bottom.b * (1.0 - a),
        a: 1.0,
    }
}
/// Breathing room, in canvas pixels, around the fitted content.
const PADDING: f32 = 24.0;
/// Never zoom in past this many canvas px per logical px, even for a
/// single small/tightly-packed arrangement — keeps a lone 1920x1080 head
/// from filling the entire card at a jarring scale.
const MAX_SCALE: f32 = 0.35;
/// How close (in canvas/screen pixels) a dragged edge must get to another
/// head's edge before it snaps to it.
const SNAP_THRESHOLD_PX: f32 = 10.0;
/// Empty layout space kept around the arrangement, as a fraction of its
/// larger dimension.
///
/// Fitting the bounding box to the viewport exactly would leave a dragged
/// head nowhere to go: it fills the canvas at rest, so every direction is
/// immediately out of bounds, and dragging turns into shoving against a
/// wall. This is the room to rearrange in — and it reads better at rest
/// too, since monitors no longer sit flush against the card's edge.
const SLACK_FRACTION: f32 = 0.2;

#[derive(Debug, Clone, PartialEq)]
pub struct CanvasHead {
    pub connector_hint: String,
    /// What to draw on the rectangle — the monitor's friendly name, not
    /// its connector. `connector_hint` stays the identity used in messages.
    pub label: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub enabled: bool,
}

impl CanvasHead {
    fn rect(&self, t: Transform) -> Rectangle {
        Rectangle::new(
            Point::new(self.x as f32 * t.scale, self.y as f32 * t.scale) + t.offset,
            Size::new(self.width as f32 * t.scale, self.height as f32 * t.scale),
        )
    }
}

/// Computes a scale (logical px -> canvas px) and offset that fits every
/// head's bounding box inside `bounds`, centered with `PADDING` to spare.
fn fit_transform(heads: &[CanvasHead], bounds: Size) -> Transform {
    if heads.is_empty() {
        return Transform {
            scale: 0.1,
            offset: Vector::new(PADDING, PADDING),
        };
    }

    let min_x = heads.iter().map(|h| h.x).min().unwrap() as f32;
    let min_y = heads.iter().map(|h| h.y).min().unwrap() as f32;
    let max_x = heads.iter().map(|h| h.x + h.width).max().unwrap() as f32;
    let max_y = heads.iter().map(|h| h.y + h.height).max().unwrap() as f32;
    let bbox_w = (max_x - min_x).max(1.0);
    let bbox_h = (max_y - min_y).max(1.0);

    // Fit the arrangement *plus* room to move it in, keeping the content
    // centred on the real bounding box rather than the inflated one.
    let slack = bbox_w.max(bbox_h) * SLACK_FRACTION;
    let fit_w = bbox_w + slack * 2.0;
    let fit_h = bbox_h + slack * 2.0;

    let avail_w = (bounds.width - PADDING * 2.0).max(1.0);
    let avail_h = (bounds.height - PADDING * 2.0).max(1.0);

    let scale = (avail_w / fit_w).min(avail_h / fit_h).min(MAX_SCALE);
    let content_w = bbox_w * scale;
    let content_h = bbox_h * scale;
    let offset = Vector::new(
        PADDING + (avail_w - content_w) / 2.0 - min_x * scale,
        PADDING + (avail_h - content_h) / 2.0 - min_y * scale,
    );
    Transform { scale, offset }
}

/// The transform to draw and hit-test with: the frozen one mid-drag, a
/// freshly fitted one otherwise.
fn active_transform(state: &CanvasState, heads: &[CanvasHead], bounds: Size) -> Transform {
    match &state.dragging {
        Some(d) => d.transform,
        None => fit_transform(heads, bounds),
    }
}

/// Snaps `pos`/`pos + size` against every candidate edge derived from
/// `others`' own start/end along one axis, within `threshold` canvas
/// pixels (already converted to logical units by the caller).
fn snap_axis(pos: i32, size: i32, others: &[(i32, i32)], threshold: f32) -> i32 {
    let mut best: Option<(i32, f32)> = None;
    let mut consider = |candidate: i32, distance: i32| {
        let d = distance.unsigned_abs() as f32;
        if d <= threshold && best.is_none_or(|(_, best_d)| d < best_d) {
            best = Some((candidate, d));
        }
    };
    for &(other_pos, other_size) in others {
        let other_end = other_pos + other_size;
        let end = pos + size;
        consider(other_pos, pos - other_pos); // my start <-> their start
        consider(other_end, pos - other_end); // my start <-> their end
        consider(other_pos - size, end - other_pos); // my end <-> their start
        consider(other_end - size, end - other_end); // my end <-> their end
    }
    best.map(|(candidate, _)| candidate).unwrap_or(pos)
}

/// A head's placement in logical units, as `(x, y, width, height)`.
type Placement = (i32, i32, i32, i32);

fn overlaps(a: Placement, b: Placement) -> bool {
    let (ax, ay, aw, ah) = a;
    let (bx, by, bw, bh) = b;
    ax < bx + bw && bx < ax + aw && ay < by + bh && by < ay + ah
}

/// Pushes a placement clear of anything it overlaps, along whichever axis
/// needs the least movement.
///
/// Compositors accept overlapping outputs, and the result is a desktop with
/// a region that exists twice — a pointer crossing it lands somewhere
/// unpredictable and windows straddle the seam. Snapping makes flush easy
/// but does nothing to stop a monitor being dropped on top of another, so
/// this makes overlap unrepresentable rather than merely discouraged.
///
/// Resolving one overlap can create another, so it iterates; the cap keeps
/// a pathological arrangement from spinning, and settling for "still
/// overlapping" is better than not returning.
fn resolve_overlap(mut placement: Placement, others: &[Placement]) -> Placement {
    const MAX_PASSES: usize = 8;
    for _ in 0..MAX_PASSES {
        let Some(&other) = others.iter().find(|&&o| overlaps(placement, o)) else {
            return placement;
        };
        let (x, y, w, h) = placement;
        let (ox, oy, ow, oh) = other;

        // Distance to clear along each direction; pick the smallest, so a
        // monitor nudged slightly into another pops back the way it came
        // rather than leaping to the far side.
        let left = (x + w) - ox; // move left by this
        let right = (ox + ow) - x; // move right by this
        let up = (y + h) - oy;
        let down = (oy + oh) - y;

        let dx = if left <= right { -left } else { right };
        let dy = if up <= down { -up } else { down };

        if dx.abs() <= dy.abs() {
            placement = (x + dx, y, w, h);
        } else {
            placement = (x, y + dy, w, h);
        }
    }
    placement
}

/// Keeps a dragged head inside the visible canvas, in logical units.
///
/// The transform is frozen for the drag, so nothing rescales to bring a
/// head back into view — without this it can be dragged clear off the card
/// and left somewhere the user has to guess at, with the diagram only
/// snapping back to sanity on release.
fn clamp_to_canvas(placement: Placement, t: Transform, bounds: Size) -> Placement {
    let (x, y, w, h) = placement;
    if t.scale <= 0.0 {
        return placement;
    }
    let to_logical_x = |canvas_x: f32| ((canvas_x - t.offset.x) / t.scale).round() as i32;
    let to_logical_y = |canvas_y: f32| ((canvas_y - t.offset.y) / t.scale).round() as i32;

    let min_x = to_logical_x(PADDING);
    let min_y = to_logical_y(PADDING);
    let max_x = to_logical_x(bounds.width - PADDING) - w;
    let max_y = to_logical_y(bounds.height - PADDING) - h;

    // A head larger than the viewport would give max < min; leave it be
    // rather than snapping it to a nonsense coordinate.
    let x = if max_x >= min_x { x.clamp(min_x, max_x) } else { x };
    let y = if max_y >= min_y { y.clamp(min_y, max_y) } else { y };
    (x, y, w, h)
}

/// Logical coordinates where the dragged head's edges coincide exactly with
/// another head's, as `(vertical_xs, horizontal_ys)`.
///
/// Snapping sets exact equality, so an exact match is precisely the
/// "it snapped" condition — no separate flag to keep in sync. Without a
/// guide there is nothing on screen distinguishing "flush" from "one pixel
/// out", which is the difference between a usable desktop and a seam.
fn snap_guides(dragged: &CanvasHead, others: &[&CanvasHead]) -> (Vec<i32>, Vec<i32>) {
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    let (dx0, dx1) = (dragged.x, dragged.x + dragged.width);
    let (dy0, dy1) = (dragged.y, dragged.y + dragged.height);
    for o in others {
        for edge in [o.x, o.x + o.width] {
            if dx0 == edge || dx1 == edge {
                xs.push(edge);
            }
        }
        for edge in [o.y, o.y + o.height] {
            if dy0 == edge || dy1 == edge {
                ys.push(edge);
            }
        }
    }
    xs.sort_unstable();
    xs.dedup();
    ys.sort_unstable();
    ys.dedup();
    (xs, ys)
}

/// Logical-px -> canvas-px scale and the offset that centres the content.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Transform {
    scale: f32,
    offset: Vector,
}

#[derive(Default)]
pub struct CanvasState {
    dragging: Option<Dragging>,
}

struct Dragging {
    connector_hint: String,
    grab_offset: Vector,
    /// The transform as it was when the drag started, held fixed until the
    /// drag ends.
    ///
    /// [`fit_transform`] derives zoom and centring from the heads' bounding
    /// box, so recomputing it mid-drag means moving a head changes the
    /// mapping that positions it. The rectangle then slides out from under
    /// the cursor, the whole diagram rescales as you approach an edge, and
    /// the harder you drag the more it fights back. Freezing it makes the
    /// grabbed point stay exactly under the pointer, which is the entire
    /// contract of a drag.
    transform: Transform,
}

/// Owns its head list and callbacks (rather than borrowing) so a fresh
/// instance can be built from a temporary `Vec` inside `view()` on every
/// redraw without fighting `Element<'a, _>`'s borrow — the same reason
/// `text(String)` variants exist alongside `text(&str)` ones.
pub struct LayoutCanvas<Message> {
    heads: Vec<CanvasHead>,
    selected: Option<String>,
    on_select: Box<dyn Fn(String) -> Message>,
    on_drag: Box<dyn Fn(String, i32, i32) -> Message>,
}

impl<Message: 'static> LayoutCanvas<Message> {
    pub fn new(
        heads: Vec<CanvasHead>,
        selected: Option<String>,
        on_select: impl Fn(String) -> Message + 'static,
        on_drag: impl Fn(String, i32, i32) -> Message + 'static,
    ) -> Self {
        LayoutCanvas {
            heads,
            selected,
            on_select: Box::new(on_select),
            on_drag: Box::new(on_drag),
        }
    }

    pub fn into_element<'a>(self) -> Element<'a, Message>
    where
        Message: 'a,
    {
        Canvas::new(self)
            .width(Length::Fill)
            .height(Length::Fixed(CANVAS_HEIGHT))
            .into()
    }
}

impl<Message> canvas::Program<Message, Theme, Renderer> for LayoutCanvas<Message> {
    type State = CanvasState;

    fn update(
        &self,
        state: &mut Self::State,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let t = fit_transform(&self.heads, bounds.size());
                let cursor_pos = cursor.position_in(bounds)?;
                // Last head drawn is on top, so search in reverse to grab
                // whichever rectangle the user actually sees at the click.
                let head = self
                    .heads
                    .iter()
                    .rev()
                    .find(|h| h.rect(t).contains(cursor_pos))?;
                state.dragging = Some(Dragging {
                    connector_hint: head.connector_hint.clone(),
                    grab_offset: cursor_pos - head.rect(t).position(),
                    transform: t,
                });
                // A press both starts a potential drag and selects the
                // head (Windows Display Settings: clicking a monitor in
                // the diagram both picks it up and shows its properties).
                let message = (self.on_select)(head.connector_hint.clone());
                Some(canvas::Action::publish(message).and_capture())
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let dragging = state.dragging.as_ref()?;
                // The frozen transform, so the grabbed point tracks the
                // pointer instead of drifting as the diagram refits.
                let t = dragging.transform;
                let cursor_pos = cursor.position_in(bounds)?;
                let new_origin = cursor_pos - dragging.grab_offset - t.offset;
                let mut new_x = (new_origin.x / t.scale).round() as i32;
                let mut new_y = (new_origin.y / t.scale).round() as i32;

                if let Some(dragged) = self
                    .heads
                    .iter()
                    .find(|h| h.connector_hint == dragging.connector_hint)
                {
                    let others: Vec<&CanvasHead> = self
                        .heads
                        .iter()
                        .filter(|h| h.connector_hint != dragging.connector_hint)
                        .collect();

                    // Keep it on the canvas first, so the pointer can't
                    // strand it somewhere invisible.
                    let placement = clamp_to_canvas(
                        (new_x, new_y, dragged.width, dragged.height),
                        t,
                        bounds.size(),
                    );
                    let (cx, cy, w, h) = placement;

                    let threshold = SNAP_THRESHOLD_PX / t.scale;
                    let others_x: Vec<(i32, i32)> =
                        others.iter().map(|h| (h.x, h.width)).collect();
                    let others_y: Vec<(i32, i32)> =
                        others.iter().map(|h| (h.y, h.height)).collect();
                    let snapped = (
                        snap_axis(cx, w, &others_x, threshold),
                        snap_axis(cy, h, &others_y, threshold),
                        w,
                        h,
                    );

                    // Overlap-freedom last: it's the invariant worth
                    // holding even if honouring it costs a snap or nudges
                    // slightly past the padding.
                    let placements: Vec<Placement> = others
                        .iter()
                        .map(|h| (h.x, h.y, h.width, h.height))
                        .collect();
                    let (rx, ry, _, _) = resolve_overlap(snapped, &placements);
                    new_x = rx;
                    new_y = ry;
                }

                let message = (self.on_drag)(dragging.connector_hint.clone(), new_x, new_y);
                Some(canvas::Action::publish(message).and_capture())
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.dragging.take().map(|_| canvas::Action::capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let palette = theme.extended_palette();
        let t = active_transform(state, &self.heads, bounds.size());
        let accent = palette.primary.base.color;

        // A recessed plane with a dot grid, the mockup's desk: the dots
        // are what make it read as a surface things are placed on rather
        // than an empty box.
        let background = Path::rounded_rectangle(Point::ORIGIN, bounds.size(), CANVAS_RADIUS.into());
        frame.fill(&background, theme::surface::sidebar());
        let dot = canvas::Fill::from(theme::surface::card_border());
        let mut gy = GRID_STEP / 2.0;
        while gy < bounds.height {
            let mut gx = GRID_STEP / 2.0;
            while gx < bounds.width {
                frame.fill_rectangle(Point::new(gx, gy), Size::new(1.0, 1.0), dot);
                gx += GRID_STEP;
            }
            gy += GRID_STEP;
        }

        for (index, head) in self.heads.iter().enumerate() {
            let rect = head.rect(t);
            let path = Path::rounded_rectangle(rect.position(), rect.size(), HEAD_RADIUS.into());

            let is_dragging = state
                .dragging
                .as_ref()
                .is_some_and(|d| d.connector_hint == head.connector_hint);
            let is_selected = self.selected.as_deref() == Some(head.connector_hint.as_str());

            // Grey cards, one of them the selection. The accent marks
            // *which* monitor the rows below are about — the one meaning
            // purple has here — and not every monitor at once, which is
            // what filling them all with it did. A disabled head is a
            // step darker, so it reads as present but off.
            let fill = if !head.enabled {
                theme::surface::sidebar()
            } else if is_selected || is_dragging {
                over(iced::Color { a: 0.14, ..accent }, theme::surface::row())
            } else {
                theme::surface::row()
            };
            frame.fill(&path, fill);
            frame.stroke(
                &path,
                canvas::Stroke::default()
                    .with_width(if is_selected || is_dragging { 2.0 } else { 1.0 })
                    .with_color(if is_selected || is_dragging { accent } else { theme::surface::card_border() }),
            );

            // The head's number, in a badge at its corner — the "1" and
            // "2" the mockup draws, and the number Identify would show.
            let badge = Size::new(BADGE_SIDE, BADGE_SIDE);
            let badge_at = Point::new(rect.x + 6.0, rect.y + 6.0);
            frame.fill(
                &Path::rounded_rectangle(badge_at, badge, 3.0.into()),
                if is_selected { accent } else { theme::surface::card_border() },
            );
            frame.fill_text(CanvasText {
                content: (index + 1).to_string(),
                position: Point::new(badge_at.x + BADGE_SIDE / 2.0, badge_at.y + BADGE_SIDE / 2.0),
                color: if is_selected { palette.primary.base.text } else { theme::text() },
                size: 11.0.into(),
                align_x: iced::widget::text::Alignment::Center,
                align_y: iced::alignment::Vertical::Center,
                ..CanvasText::default()
            });

            // The name, centred: the mockup's layout, and the one that
            // survives a small rectangle better than a corner label does.
            frame.fill_text(CanvasText {
                content: head.label.clone(),
                position: rect.center(),
                color: if head.enabled { theme::text() } else { theme::text_dim() },
                size: 13.0.into(),
                align_x: iced::widget::text::Alignment::Center,
                align_y: iced::alignment::Vertical::Center,
                ..CanvasText::default()
            });
        }

        // Snap guides on top of the rectangles, so a flush edge is visible
        // rather than something the user has to take on trust. In the
        // accent, faintly — they belong to the head being dragged, which
        // is the selected one — and not the warning colour they used to
        // be drawn in: an edge lining up is the opposite of a problem.
        if let Some(d) = &state.dragging {
            if let Some(dragged) = self
                .heads
                .iter()
                .find(|h| h.connector_hint == d.connector_hint)
            {
                let others: Vec<&CanvasHead> = self
                    .heads
                    .iter()
                    .filter(|h| h.connector_hint != d.connector_hint)
                    .collect();
                let (xs, ys) = snap_guides(dragged, &others);
                let guide = canvas::Stroke::default()
                    .with_width(1.0)
                    .with_color(iced::Color { a: 0.6, ..accent });
                for x in xs {
                    let cx = x as f32 * t.scale + t.offset.x;
                    frame.stroke(
                        &Path::line(Point::new(cx, 0.0), Point::new(cx, bounds.height)),
                        guide,
                    );
                }
                for y in ys {
                    let cy = y as f32 * t.scale + t.offset.y;
                    frame.stroke(
                        &Path::line(Point::new(0.0, cy), Point::new(bounds.width, cy)),
                        guide,
                    );
                }
            }
        }

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.dragging.is_some() {
            return mouse::Interaction::Grabbing;
        }
        let t = active_transform(state, &self.heads, bounds.size());
        if let Some(cursor_pos) = cursor.position_in(bounds) {
            if self.heads.iter().any(|h| h.rect(t).contains(cursor_pos)) {
                return mouse::Interaction::Grab;
            }
        }
        mouse::Interaction::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(hint: &str, x: i32, y: i32, w: i32, h: i32) -> CanvasHead {
        CanvasHead {
            connector_hint: hint.to_string(),
            label: hint.to_string(),
            x,
            y,
            width: w,
            height: h,
            enabled: true,
        }
    }

    const BOUNDS: Size = Size {
        width: 800.0,
        height: CANVAS_HEIGHT,
    };

    /// The reason the drag transform is frozen: moving a head changes the
    /// bounding box the transform is fitted to, so recomputing it mid-drag
    /// changes the mapping that positions the thing being dragged.
    #[test]
    fn refitting_mid_drag_would_move_the_dragged_head_under_the_cursor() {
        let before = vec![head("eDP-2", 0, 0, 1536, 960), head("DP-3", 1536, 0, 2560, 1440)];
        let t0 = fit_transform(&before, BOUNDS);

        // Drag DP-3 right by 400 logical units.
        let after = vec![head("eDP-2", 0, 0, 1536, 960), head("DP-3", 1936, 0, 2560, 1440)];
        let t1 = fit_transform(&after, BOUNDS);

        assert_ne!(
            t0.scale, t1.scale,
            "the fit changes as the arrangement grows — which is exactly why \
             the drag must hold the transform it started with"
        );

        // With the frozen transform the cursor-to-logical mapping is stable:
        // 400 logical units of movement is the same canvas distance before
        // and after.
        let moved_canvas = (1936 - 1536) as f32 * t0.scale;
        assert!(moved_canvas > 0.0);
    }

    #[test]
    fn a_frozen_transform_maps_a_drag_one_to_one() {
        let heads = vec![head("eDP-2", 0, 0, 1536, 960), head("DP-3", 1536, 0, 2560, 1440)];
        let t = fit_transform(&heads, BOUNDS);
        // Grab DP-3's origin and move the pointer 100 canvas px right.
        let grab = heads[1].rect(t).position();
        let cursor = Point::new(grab.x + 100.0, grab.y);
        let origin = cursor - Vector::new(0.0, 0.0) - t.offset;
        let new_x = (origin.x / t.scale).round() as i32;
        assert_eq!(new_x, 1536 + (100.0 / t.scale).round() as i32);
    }

    #[test]
    fn snapping_prefers_the_nearest_edge() {
        // Dragged head is 2560 wide, sitting just short of flush against a
        // neighbour that ends at 1536.
        let others = [(0, 1536)];
        assert_eq!(snap_axis(1530, 2560, &others, 20.0), 1536);
        // Just past it, snaps back the other way.
        assert_eq!(snap_axis(1542, 2560, &others, 20.0), 1536);
        // Outside the threshold, left alone.
        assert_eq!(snap_axis(1400, 2560, &others, 20.0), 1400);
    }

    #[test]
    fn snapping_can_align_far_edges_too() {
        // Top-aligning two heads of different heights: my start to their
        // start.
        let others = [(0, 960)];
        assert_eq!(snap_axis(6, 1440, &others, 20.0), 0);
        // My end to their end: 960 - 1440 = -480.
        assert_eq!(snap_axis(-474, 1440, &others, 20.0), -480);
    }

    #[test]
    fn a_guide_appears_exactly_when_an_edge_is_flush() {
        let neighbour = head("eDP-2", 0, 0, 1536, 960);
        let others = vec![&neighbour];

        let flush = head("DP-3", 1536, 0, 2560, 1440);
        let (xs, ys) = snap_guides(&flush, &others);
        assert_eq!(xs, vec![1536], "the shared vertical edge should be marked");
        assert_eq!(ys, vec![0], "tops are aligned, so that edge is marked too");

        let one_out = head("DP-3", 1537, 1, 2560, 1440);
        let (xs, ys) = snap_guides(&one_out, &others);
        assert!(xs.is_empty() && ys.is_empty(), "not flush, so no guide");
    }

    #[test]
    fn overlap_is_pushed_out_along_the_shorter_axis() {
        // Neighbour occupies 0..1536 x 0..960. Dropping a head mostly on top
        // of it, but only 36 units deep from the right, should pop it back
        // out to the right rather than fling it vertically.
        let others = [(0, 0, 1536, 960)];
        let (x, y, _, _) = resolve_overlap((1500, 0, 2560, 1440), &others);
        assert_eq!((x, y), (1536, 0));
    }

    #[test]
    fn overlap_resolution_leaves_a_clear_placement_alone() {
        let others = [(0, 0, 1536, 960)];
        let placement = (1536, 0, 2560, 1440);
        assert_eq!(resolve_overlap(placement, &others), placement);
    }

    #[test]
    fn a_head_dropped_dead_centre_still_ends_up_clear() {
        // Full containment has no "shorter axis" intuition, but it must
        // still resolve rather than leave the desktop doubled.
        let others = [(0, 0, 3000, 2000)];
        let resolved = resolve_overlap((1000, 800, 500, 400), &others);
        assert!(
            !overlaps(resolved, others[0]),
            "still overlapping after resolution: {resolved:?}"
        );
    }

    #[test]
    fn overlap_resolution_terminates_when_boxed_in() {
        // Surrounded on all sides: the cap must return something rather
        // than loop, even though no placement is clear.
        let others = [
            (0, 0, 100, 100),
            (100, 0, 100, 100),
            (0, 100, 100, 100),
            (100, 100, 100, 100),
        ];
        let resolved = resolve_overlap((50, 50, 100, 100), &others);
        let _ = resolved; // terminating at all is the assertion here.
    }

    #[test]
    fn a_drag_cannot_strand_a_head_off_the_canvas() {
        let heads = vec![head("eDP-2", 0, 0, 1536, 960), head("DP-3", 1536, 0, 2560, 1440)];
        let t = fit_transform(&heads, BOUNDS);
        // Try to fling it far to the right.
        let (x, _, _, _) = clamp_to_canvas((100_000, 0, 2560, 1440), t, BOUNDS);
        let right_edge_canvas = (x + 2560) as f32 * t.scale + t.offset.x;
        assert!(
            right_edge_canvas <= BOUNDS.width - PADDING + 1.0,
            "clamped head still extends past the canvas: {right_edge_canvas}"
        );
    }

    #[test]
    fn the_fit_leaves_room_to_drag_into() {
        // With the arrangement fitted flush there would be nowhere to move
        // to; slack is what makes dragging possible at all.
        //
        // Wide enough that the fit is what limits the scale — a small
        // arrangement is capped by MAX_SCALE instead, which leaves margin
        // for its own reasons and would pass this whether or not slack
        // exists.
        let heads = vec![
            head("eDP-2", 0, 0, 1536, 960),
            head("DP-3", 1536, 0, 2560, 1440),
        ];
        let t = fit_transform(&heads, BOUNDS);
        assert!(t.scale < MAX_SCALE, "test needs a fit-limited scale");

        let left = heads[0].rect(t);
        let right = heads[1].rect(t);
        assert!(
            left.x > PADDING + 1.0,
            "no slack on the left: content starts at {}",
            left.x
        );
        assert!(
            right.x + right.width < BOUNDS.width - PADDING - 1.0,
            "no slack on the right: content ends at {}",
            right.x + right.width
        );
    }

    #[test]
    fn an_empty_arrangement_does_not_divide_by_zero() {
        let t = fit_transform(&[], BOUNDS);
        assert!(t.scale > 0.0);
    }
}

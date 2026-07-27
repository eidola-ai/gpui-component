use gpui::Corners;
use std::{
    ops::Range,
    rc::Rc,
    sync::{Arc, Mutex},
};

use gpui::{
    App, BorderStyle, Bounds, CursorStyle, Edges, Element, ElementId, GlobalElementId, Half,
    HighlightStyle, Hitbox, HitboxBehavior, InspectorElementId, IntoElement, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, SharedString, StyledText,
    TextLayout, Window, point, px, quad,
};

use crate::{
    ActiveTheme, WindowExt as _, global_state::GlobalState, input::Selection,
    text::TextViewMultiClickKind, text::node::LinkMark, text::selection::word_range_at,
};

/// A inline element used to render a inline text and support selectable.
///
/// All text in TextView (including the CodeBlock) used this for text rendering.
pub(super) struct Inline {
    id: ElementId,
    text: SharedString,
    links: Rc<Vec<(Range<usize>, LinkMark)>>,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    styled_text: StyledText,

    state: Arc<Mutex<InlineState>>,
}

/// The inline text state, used RefCell to keep the selection state.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct InlineState {
    hovered_index: Option<usize>,
    /// The text that actually rendering, matched with selection.
    pub(super) text: SharedString,
    pub(super) selection: Option<Selection>,
}

impl InlineState {
    /// Save actually rendered text for selected text to use.
    pub(crate) fn set_text(&mut self, text: SharedString) {
        self.text = text;
    }
}

impl Inline {
    pub(super) fn new(
        id: impl Into<ElementId>,
        state: Arc<Mutex<InlineState>>,
        links: Vec<(Range<usize>, LinkMark)>,
        highlights: Vec<(Range<usize>, HighlightStyle)>,
    ) -> Self {
        let text = state
            .lock()
            .map(|state| state.text.clone())
            .unwrap_or_default();

        Self {
            id: id.into(),
            links: Rc::new(links),
            highlights,
            text: text.clone(),
            styled_text: StyledText::new(text),
            state,
        }
    }

    /// Get link at given mouse position.
    fn link_for_position(
        layout: &TextLayout,
        links: &Vec<(Range<usize>, LinkMark)>,
        position: Point<Pixels>,
    ) -> Option<LinkMark> {
        let offset = layout.index_for_position(position).ok()?;
        for (range, link) in links.iter() {
            if range.contains(&offset) {
                return Some(link.clone());
            }
        }

        None
    }

    /// Paint selected bounds for debug.
    #[allow(unused)]
    fn paint_selected_bounds(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        window.paint_quad(gpui::PaintQuad {
            bounds,
            background: cx.theme().blue.alpha(0.01).into(),
            corner_radii: Corners::default(),
            border_color: gpui::transparent_black(),
            border_style: BorderStyle::default(),
            border_widths: gpui::Edges::all(px(0.)),
        });
    }

    fn layout_selections(
        &self,
        text_layout: &TextLayout,
        bounds: &Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> (bool, bool, Option<Selection>) {
        let Some(text_view_state) = GlobalState::global(cx).text_view_state() else {
            return (false, false, None);
        };

        let text_view_state = text_view_state.read(cx);
        let is_selectable = text_view_state.is_selectable();
        if !is_selectable {
            return (false, false, None);
        }

        if text_view_state.is_all_selected() {
            return (is_selectable, true, Some((0..self.text.len()).into()));
        }

        if let Some(selection) = text_view_state.multi_click_selection() {
            return (
                is_selectable,
                true,
                selection_for_multi_click(
                    &self.text,
                    text_layout,
                    *bounds,
                    selection.pos,
                    selection.kind,
                )
                .map(Selection::from),
            );
        }

        let Some((selection_start, selection_end)) = text_view_state.selection_points(window, cx)
        else {
            return (is_selectable, false, None);
        };
        let line_height = window.line_height();
        let mask_bounds = window.content_mask().bounds;

        // Use for debug selection bounds
        // self.paint_selected_bounds(Bounds::from_corners(selection_start, selection_end), window, cx);

        let selection = scan_selection(
            text_layout,
            &self.text,
            selection_start,
            selection_end,
            line_height,
            mask_bounds,
        );

        (true, true, selection)
    }

    fn text_line_bounds(
        &self,
        text_layout: &TextLayout,
        line_height: Pixels,
        mask_bounds: Bounds<Pixels>,
    ) -> Vec<Bounds<Pixels>> {
        let mut line_bounds = Vec::new();
        let mut current_line_y = None;
        let mut current_bounds: Option<Bounds<Pixels>> = None;
        let mut offset = 0;

        for c in self.text.chars() {
            let next_offset = offset + c.len_utf8();
            let Some(pos) = text_layout.position_for_index(offset) else {
                offset = next_offset;
                continue;
            };

            let mut char_width = line_height.half();
            if let Some(next_pos) = text_layout.position_for_index(next_offset) {
                if next_pos.y == pos.y {
                    char_width = next_pos.x - pos.x;
                }
            }

            let bounds = Bounds::from_corners(pos, point(pos.x + char_width, pos.y + line_height))
                .intersect(&mask_bounds);
            if bounds.size.width > px(0.) && bounds.size.height > px(0.) {
                if current_line_y == Some(pos.y) {
                    if let Some(current) = current_bounds.as_mut() {
                        *current = current.union(&bounds);
                    }
                } else {
                    if let Some(current) = current_bounds.take() {
                        line_bounds.push(current);
                    }
                    current_line_y = Some(pos.y);
                    current_bounds = Some(bounds);
                }
            }

            offset = next_offset;
        }

        if let Some(current) = current_bounds {
            line_bounds.push(current);
        }

        line_bounds
    }

    /// Paint the selection background.
    fn paint_selection(
        selection: &Selection,
        text_layout: &TextLayout,
        bounds: &Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let mut start = selection.start;
        let mut end = selection.end;
        if end < start {
            std::mem::swap(&mut start, &mut end);
        }
        let Some(start_position) = text_layout.position_for_index(start) else {
            return;
        };
        let Some(end_position) = text_layout.position_for_index(end) else {
            return;
        };

        let line_height = text_layout.line_height();
        if start_position.y == end_position.y {
            window.paint_quad(quad(
                Bounds::from_corners(
                    start_position,
                    point(end_position.x, end_position.y + line_height),
                ),
                px(0.),
                cx.theme().selection,
                Edges::default(),
                gpui::transparent_black(),
                BorderStyle::default(),
            ));
        } else {
            window.paint_quad(quad(
                Bounds::from_corners(
                    start_position,
                    point(bounds.right(), start_position.y + line_height),
                ),
                px(0.),
                cx.theme().selection,
                Edges::default(),
                gpui::transparent_black(),
                BorderStyle::default(),
            ));

            if end_position.y > start_position.y + line_height {
                window.paint_quad(quad(
                    Bounds::from_corners(
                        point(bounds.left(), start_position.y + line_height),
                        point(bounds.right(), end_position.y),
                    ),
                    px(0.),
                    cx.theme().selection,
                    Edges::default(),
                    gpui::transparent_black(),
                    BorderStyle::default(),
                ));
            }

            window.paint_quad(quad(
                Bounds::from_corners(
                    point(bounds.left(), end_position.y),
                    point(end_position.x, end_position.y + line_height),
                ),
                px(0.),
                cx.theme().selection,
                Edges::default(),
                gpui::transparent_black(),
                BorderStyle::default(),
            ));
        }
    }
}

impl IntoElement for Inline {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Inline {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_element_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let text_style = window.text_style();

        let mut runs = Vec::new();
        let mut ix = 0;
        for (range, highlight) in self.highlights.iter() {
            if ix < range.start {
                runs.push(text_style.clone().to_run(range.start - ix));
            }
            runs.push(text_style.clone().highlight(*highlight).to_run(range.len()));
            ix = range.end;
        }
        if ix < self.text.len() {
            runs.push(text_style.to_run(self.text.len() - ix));
        }

        self.styled_text = StyledText::new(self.text.clone()).with_runs(runs);
        let (layout_id, _) =
            self.styled_text
                .request_layout(global_element_id, inspector_id, window, cx);

        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.styled_text
            .prepaint(id, inspector_id, bounds, &mut (), window, cx);

        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        hitbox
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let current_view = window.current_view();
        let hitbox = prepaint;
        let Ok(mut state) = self.state.lock() else {
            return;
        };

        let text_layout = self.styled_text.layout().clone();
        self.styled_text
            .paint(global_id, None, bounds, &mut (), &mut (), window, cx);

        // layout selections
        let (is_selectable, is_selection, selection) =
            self.layout_selections(&text_layout, &bounds, window, cx);

        state.selection = selection;

        if is_selection || is_selectable {
            window.set_cursor_style(CursorStyle::IBeam, &hitbox);
        }

        // link cursor pointer
        let mouse_position = window.mouse_position();
        if let Some(_) = Self::link_for_position(&text_layout, &self.links, mouse_position) {
            window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
        }

        if let Some(selection) = &state.selection {
            Self::paint_selection(selection, &text_layout, &bounds, window, cx);
        }

        if is_selectable {
            if let Some(text_view_state) = GlobalState::global(cx).text_view_state().cloned() {
                let text_bounds = self.text_line_bounds(
                    &text_layout,
                    text_layout.line_height(),
                    window.content_mask().bounds,
                );
                crate::Root::register_selectable_text_inline(
                    &text_view_state,
                    text_bounds,
                    window,
                    cx,
                );
            }

            window.on_mouse_event({
                let hitbox = hitbox.clone();
                let text_layout = text_layout.clone();
                let inline_state = self.state.clone();
                let text = self.text.clone();
                let text_view_state = GlobalState::global(cx).text_view_state().cloned();

                move |event: &MouseDownEvent, phase, window, cx| {
                    if !phase.bubble()
                        || !hitbox.is_hovered(window)
                        || event.button != MouseButton::Left
                    {
                        return;
                    }

                    let kind = match event.click_count {
                        2 => TextViewMultiClickKind::Word,
                        3 => TextViewMultiClickKind::Paragraph,
                        _ => return,
                    };

                    let Some(range) = selection_for_multi_click(
                        &text,
                        &text_layout,
                        hitbox.bounds,
                        event.position,
                        kind,
                    ) else {
                        return;
                    };

                    let selected_text = text[range.clone()].to_string();

                    if let Ok(mut inline_state) = inline_state.lock() {
                        inline_state.selection = Some(range.into());
                    }
                    if let Some(text_view_state) = &text_view_state {
                        text_view_state.update(cx, |state, _| {
                            state.set_multi_click_selection(event.position, kind, selected_text);
                        });
                    }
                    cx.notify(current_view);
                }
            });
        }

        // mouse move, update hovered link
        window.on_mouse_event({
            let hitbox = hitbox.clone();
            let text_layout = text_layout.clone();
            let mut hovered_index = state.hovered_index;
            move |event: &MouseMoveEvent, phase, window, cx| {
                if !phase.bubble() || !hitbox.is_hovered(window) {
                    return;
                }

                let current = hovered_index;
                let updated = text_layout.index_for_position(event.position).ok();
                //  notify update when hovering over different links
                if current != updated {
                    hovered_index = updated;
                    cx.notify(current_view);
                }
            }
        });

        if !is_selection {
            // click to open link
            window.on_mouse_event({
                let links = self.links.clone();
                let text_layout = text_layout.clone();
                let hitbox = hitbox.clone();
                let text_view_state = GlobalState::global(cx).text_view_state().cloned();

                move |event: &MouseUpEvent, phase, window, cx| {
                    if !phase.bubble() || !hitbox.is_hovered(window) {
                        return;
                    }
                    if text_view_state
                        .as_ref()
                        .is_some_and(|state| state.read(cx).has_selection(window, cx))
                    {
                        return;
                    }

                    if let Some(link) =
                        Self::link_for_position(&text_layout, &links, event.position)
                    {
                        window.end_text_selection(cx);
                        cx.stop_propagation();
                        cx.open_url(&link.url);
                    }
                }
            });
        }
    }
}

/// The byte range of `text` covered by the window selection, clipped to the
/// content mask.
///
/// Semantically identical to testing every character of `text` with
/// [`point_in_text_selection`] plus `mask_bounds.contains(char_center)` and
/// taking the hull of the hits — which is exactly what this used to do, one
/// [`TextLayout::position_for_index`] call per character. That call rescans
/// the layout from line 0, so the old loop cost O(characters x lines) *per
/// paint*, and it ran on every frame while a selection existed: a 46 KB
/// payload in one `Inline` (the raw request/response bodies in Eidola's
/// Record window) spent ~480 ms per frame in this function.
///
/// Two structural changes remove that cost without changing the result:
///
/// 1. **Skip whole lines.** A character's hit test depends only on its
///    position, and every character of a laid-out line sits inside that
///    line's vertical band. A line whose band misses either the selection's
///    y-span or the content mask cannot contribute a hit, so it is skipped
///    without touching its characters. Because the content mask is the
///    visible viewport, this bounds the scan to the text on screen.
/// 2. **Walk glyphs once per scanned line.** Within a scanned line the
///    per-character positions come from a single ordered pass over the
///    shaped glyphs (character offsets only ever increase), instead of a
///    layout-wide `position_for_index` call per character.
///
/// The per-character predicate itself is unchanged.
fn scan_selection(
    text_layout: &TextLayout,
    text: &str,
    selection_start: Point<Pixels>,
    selection_end: Point<Pixels>,
    line_height: Pixels,
    mask_bounds: Bounds<Pixels>,
) -> Option<Selection> {
    let lines = text_layout.line_layouts();
    let origin = text_layout.bounds().origin;
    // Geometry uses the *layout's* line height (pixel-snapped when the layout
    // was measured), exactly as `TextLayout::position_for_index` does; the
    // hit test below uses the window's, exactly as the old loop did. They can
    // differ by a device pixel, which is enough to shift a hit.
    let layout_line_height = text_layout.line_height();
    let sel_top = selection_start.y.min(selection_end.y);
    let sel_bottom = selection_start.y.max(selection_end.y);
    // Slack that makes the per-line filter a guaranteed superset of the hits.
    let slack = line_height.max(layout_line_height);

    let mut selection: Option<Selection> = None;
    let mut line_origin_y = origin.y;
    let mut line_start_ix = 0usize;

    for line in lines.iter() {
        let line_bottom = line_origin_y + line.size(layout_line_height).height;
        // Positions on this line have `pos.y` in
        // `[line_origin_y, line_bottom - layout_line_height]`.
        // `point_in_text_selection` needs `pos.y + line_height > sel_top &&
        // pos.y <= sel_bottom`; the mask test is on `pos.y + line_height / 2`.
        // Both are widened by `slack` here — an over-inclusive line is merely
        // scanned, never a different answer.
        let in_selection_band = line_bottom + slack >= sel_top && line_origin_y <= sel_bottom;
        let in_mask_band = line_bottom + slack >= mask_bounds.top()
            && line_origin_y <= mask_bounds.bottom() + slack;
        if in_selection_band && in_mask_band {
            scan_line(
                text,
                line,
                line_start_ix,
                point(origin.x, line_origin_y),
                selection_start,
                selection_end,
                line_height,
                layout_line_height,
                mask_bounds,
                &mut selection,
            );
        }
        line_origin_y = line_bottom;
        line_start_ix += line.len() + 1;
    }

    selection
}

/// Run the per-character selection test over one laid-out line, extending
/// `selection` with any hits.
///
/// `line_start_ix` is the line's byte offset in the whole text and
/// `line_origin` its top-left corner. Character positions are reproduced
/// exactly as [`TextLayout::position_for_index`] would report them: the x of
/// the first glyph at or after the byte index (or the line width past the
/// last glyph), relative to the start of the wrapped sub-line the index falls
/// on, and a y of `sub_line_index * line_height`.
#[allow(clippy::too_many_arguments)]
fn scan_line(
    text: &str,
    line: &gpui::WrappedLineLayout,
    line_start_ix: usize,
    line_origin: Point<Pixels>,
    selection_start: Point<Pixels>,
    selection_end: Point<Pixels>,
    line_height: Pixels,
    layout_line_height: Pixels,
    mask_bounds: Bounds<Pixels>,
    selection: &mut Option<Selection>,
) {
    let len = line.len();
    // Glyph (byte index, x) pairs in reading order, and the exclusive end of
    // each wrapped sub-line — the two things `x_for_index` / the sub-line walk
    // in `WrappedLineLayout::position_for_index` consult.
    let glyphs: Vec<(usize, Pixels)> = line
        .runs()
        .iter()
        .flat_map(|run| run.glyphs.iter())
        .map(|glyph| (glyph.index, glyph.position.x))
        .collect();
    let width = line.unwrapped_layout.width;
    let sub_ends: Vec<usize> = line
        .wrap_boundaries()
        .iter()
        .map(|boundary| line.unwrapped_layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix].index)
        .chain([len])
        .collect();

    // `LineLayout::x_for_index`, answered with a cursor instead of a scan: all
    // three query streams below (`i`, `i + 1`, and each sub-line's start) are
    // non-decreasing, so each keeps its own monotone cursor.
    let x_at = |i: usize, cursor: &mut usize| -> Pixels {
        while *cursor < glyphs.len() && glyphs[*cursor].0 < i {
            *cursor += 1;
        }
        glyphs.get(*cursor).map(|(_, x)| *x).unwrap_or(width)
    };
    let mut cur_cursor = 0usize;
    let mut next_cursor = 0usize;
    let mut sub_cursor = 0usize;

    // The wrapped sub-line an index falls on — also non-decreasing.
    let mut sub_ix = 0usize;
    let mut sub_start_x = px(0.);

    // The line's own bytes plus its trailing newline, which the layout places
    // at the end of this line (the old whole-text loop tested it here too).
    let scan_end = (line_start_ix + len + 1).min(text.len());
    let Some(slice) = text.get(line_start_ix..scan_end) else {
        return;
    };

    for (rel, c) in slice.char_indices() {
        let i = rel.min(len);
        while i > sub_ends[sub_ix] && sub_ix + 1 < sub_ends.len() {
            let sub_start = sub_ends[sub_ix];
            sub_ix += 1;
            sub_start_x = x_at(sub_start, &mut sub_cursor);
        }

        let x = x_at(i, &mut cur_cursor) - sub_start_x;
        let pos = point(
            line_origin.x + x,
            line_origin.y + layout_line_height * sub_ix as f32,
        );

        // `position_for_index(offset + 1)` only widened the character when it
        // landed on the same rendered row (`next_pos.y == pos.y`); past a wrap
        // boundary it starts a new row and the half-line-height default stood.
        let char_width = if i + 1 <= sub_ends[sub_ix] {
            x_at(i + 1, &mut next_cursor) - sub_start_x - x
        } else {
            line_height.half()
        };

        let char_center = point(pos.x + char_width.half(), pos.y + line_height.half());
        if mask_bounds.contains(&char_center)
            && point_in_text_selection(pos, char_width, selection_start, selection_end, line_height)
        {
            let offset = line_start_ix + rel;
            if selection.is_none() {
                *selection = Some((offset..offset).into());
            }
            if let Some(selection) = selection.as_mut() {
                selection.end = offset + c.len_utf8();
            }
        }
    }
}

fn selection_for_multi_click(
    text: &str,
    text_layout: &TextLayout,
    bounds: Bounds<Pixels>,
    pos: Point<Pixels>,
    kind: TextViewMultiClickKind,
) -> Option<std::ops::Range<usize>> {
    if !bounds.contains(&pos) {
        return None;
    }

    let offset = text_layout.index_for_position(pos).ok()?;

    match kind {
        TextViewMultiClickKind::Word => word_range_at(text, offset),
        // Known limitation: a paragraph maps to a single Inline run here. When a
        // paragraph embeds an inline image it is split into multiple Inline runs,
        // so triple-click only selects the run on the clicked side of the image.
        TextViewMultiClickKind::Paragraph => (!text.is_empty()).then_some(0..text.len()),
    }
}

/// Check if a `pos` is within a `bounds`, considering multi-line selections.
fn point_in_text_selection(
    pos: Point<Pixels>,
    char_width: Pixels,
    selection_start: Point<Pixels>,
    selection_end: Point<Pixels>,
    line_height: Pixels,
) -> bool {
    let point_in_line = |point: Point<Pixels>| point.y >= pos.y && point.y < pos.y + line_height;
    let top = selection_start.y.min(selection_end.y);
    let bottom = selection_start.y.max(selection_end.y);
    let x = pos.x + char_width.half();

    // Out of the vertical bounds
    if pos.y + line_height <= top || pos.y > bottom {
        return false;
    }

    // Treat the selection as single-line when both drag points fall within the
    // same rendered line, even if their y coordinates differ inside that line.
    if point_in_line(selection_start) && point_in_line(selection_end) {
        let left = selection_start.x.min(selection_end.x);
        let right = selection_start.x.max(selection_end.x);
        return x >= left && x <= right;
    }

    let (top_point, bottom_point) = if selection_start.y < selection_end.y {
        (selection_start, selection_end)
    } else {
        (selection_end, selection_start)
    };
    let is_top_line = point_in_line(top_point);
    let is_bottom_line = point_in_line(bottom_point);

    if is_top_line {
        return x >= top_point.x;
    } else if is_bottom_line {
        return x <= bottom_point.x;
    } else {
        return true;
    }
}

#[cfg(test)]
mod tests {
    use super::point_in_text_selection;
    use gpui::{point, px};

    #[test]
    fn test_point_in_text_selection() {
        let line_height = px(20.);
        let char_width = px(10.);
        let start = point(px(50.), px(50.));
        let end = point(px(150.), px(150.));

        // First line but haft line height, true
        // | p --------|
        // | selection |
        // |-----------|
        assert!(point_in_text_selection(
            point(px(50.), px(40.)),
            char_width,
            start,
            end,
            line_height
        ));

        // First line in selection, true
        // | p --------|
        // | selection |
        // |-----------|
        assert!(point_in_text_selection(
            point(px(50.), px(50.)),
            char_width,
            start,
            end,
            line_height
        ));
        // First line, but left out of selection, false
        // p |-----------|
        //   | selection |
        //   |-----------|
        assert!(!point_in_text_selection(
            point(px(40.), px(50.)),
            char_width,
            start,
            end,
            line_height
        ));
        // First line but right out of selection, true
        // |-----------| p
        // | selection |
        // |-----------|
        assert!(point_in_text_selection(
            point(px(160.), px(50.)),
            char_width,
            start,
            end,
            line_height
        ));

        // Middle line in selection, true
        // |-----------|
        // |     p     |
        // |-----------|
        assert!(point_in_text_selection(
            point(px(100.), px(70.)),
            char_width,
            start,
            end,
            line_height
        ));
        // Middle line, but left out of selection, true
        //   |-----------|
        // p | selection |
        //   |-----------|
        assert!(point_in_text_selection(
            point(px(40.), px(70.)),
            char_width,
            start,
            end,
            line_height
        ));
        // Middle line, but right out of selection, true
        // |-----------|
        // | selection | p
        // |-----------|
        assert!(point_in_text_selection(
            point(px(160.), px(70.)),
            char_width,
            start,
            end,
            line_height
        ));

        // Last line in selection, true
        // |-----------|
        // | selection |
        // |------- p -|
        assert!(point_in_text_selection(
            point(px(100.), px(140.)),
            char_width,
            start,
            end,
            line_height
        ));
        // Last line, but left out of selection, true
        //
        //   |-----------|
        //   | selection |
        // p |-----------|
        assert!(point_in_text_selection(
            point(px(40.), px(140.)),
            char_width,
            start,
            end,
            line_height
        ));
        // Last line, but right out of selection, false
        // |-----------|
        // | selection |
        // |-----------| p
        assert!(!point_in_text_selection(
            point(px(160.), px(140.)),
            char_width,
            start,
            end,
            line_height
        ));

        // Out of vertical bounds (top), false
        //       p
        // |-----------|
        // | selection |
        // |-----------|
        assert!(!point_in_text_selection(
            point(px(100.), px(20.)),
            char_width,
            start,
            end,
            line_height
        ));
        // Out of vertical bounds (bottom), false
        // |-----------|
        // | selection |
        // |-----------|
        //       p
        assert!(!point_in_text_selection(
            point(px(100.), px(160.)),
            char_width,
            start,
            end,
            line_height
        ));
    }

    #[test]
    fn test_point_in_text_selection_reversed_drag_direction() {
        let line_height = px(20.);
        let char_width = px(10.);

        // Mouse down on lower line then drag upward to x=150.
        // Top line should follow current mouse x, bottom line should keep anchor x.
        let start = point(px(80.), px(150.));
        let end = point(px(150.), px(50.));

        // On top line, selection starts from top cursor x (150), so x=140 should be excluded.
        assert!(!point_in_text_selection(
            point(px(140.), px(50.)),
            char_width,
            start,
            end,
            line_height
        ));
        assert!(point_in_text_selection(
            point(px(150.), px(50.)),
            char_width,
            start,
            end,
            line_height
        ));

        // On bottom line, selection ends at anchor x (80), so x=90 should be excluded.
        assert!(point_in_text_selection(
            point(px(75.), px(140.)),
            char_width,
            start,
            end,
            line_height
        ));
        assert!(!point_in_text_selection(
            point(px(80.), px(140.)),
            char_width,
            start,
            end,
            line_height
        ));
    }

    #[test]
    fn test_point_in_text_selection_same_visual_line_with_different_y() {
        let line_height = px(20.);
        let char_width = px(10.);
        let start = point(px(100.), px(55.));
        let end = point(px(60.), px(58.));

        assert!(!point_in_text_selection(
            point(px(40.), px(50.)),
            char_width,
            start,
            end,
            line_height
        ));
        assert!(point_in_text_selection(
            point(px(70.), px(50.)),
            char_width,
            start,
            end,
            line_height
        ));
        assert!(!point_in_text_selection(
            point(px(110.), px(50.)),
            char_width,
            start,
            end,
            line_height
        ));
    }

    #[test]
    fn test_point_in_text_selection_same_visual_line_with_reversed_y() {
        let line_height = px(20.);
        let char_width = px(10.);
        let start = point(px(60.), px(58.));
        let end = point(px(100.), px(55.));

        assert!(!point_in_text_selection(
            point(px(40.), px(50.)),
            char_width,
            start,
            end,
            line_height
        ));
        assert!(point_in_text_selection(
            point(px(70.), px(50.)),
            char_width,
            start,
            end,
            line_height
        ));
        assert!(!point_in_text_selection(
            point(px(110.), px(50.)),
            char_width,
            start,
            end,
            line_height
        ));
    }
}

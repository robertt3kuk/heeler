//! The GPUI element that paints the terminal grid and wires mouse, input-method and ack handling.

use std::rc::Rc;

use gpui::{
    App, Bounds, ContentMask, CursorStyle, DispatchPhase, Element, ElementId, ElementInputHandler,
    Entity, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement,
    LayoutId, ModifiersChangedEvent, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    ScrollWheelEvent, ShapedLine, Size, Style, TextAlign, Window, WindowTextSystem, fill, outline,
    point, px, relative, size,
};
use heeler_protocol::render::{CellFlags, Color, CursorShape};

use crate::model::row_cells;
use crate::shape::{
    Decoration, FontSpec, RowCache, ShapedRow, UnderlineKind, hsla, row_key, shape_row, styled_row,
    text_columns,
};
use crate::theme::Theme;
use crate::view::{Layout, TerminalView};

pub(crate) struct TerminalElement {
    view: Entity<TerminalView>,
}

impl TerminalElement {
    pub fn new(view: Entity<TerminalView>) -> Self {
        Self { view }
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

struct RowPaint {
    shaped: Option<Rc<ShapedRow>>,
    /// Selected columns, end exclusive.
    selected: Option<(u16, u16)>,
}

struct CursorPaint {
    col: u16,
    row: u16,
    cols: u16,
    shape: CursorShape,
    color: Hsla,
    /// The character under a block cursor, drawn in the background color.
    glyph: Option<ShapedLine>,
}

struct MarkedPaint {
    col: u16,
    row: u16,
    cols: u16,
    shaped: ShapedRow,
}

pub(crate) struct Prepaint {
    hitbox: Hitbox,
    font: FontSpec,
    background: Hsla,
    selection: Hsla,
    rows: Vec<RowPaint>,
    cursor: Option<CursorPaint>,
    marked: Option<MarkedPaint>,
    cursor_style: CursorStyle,
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = Style {
            size: Size {
                width: relative(1.).into(),
                height: relative(1.).into(),
            },
            ..Style::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let (family, font_size, line_height) = {
            let settings = &self.view.read(cx).settings;
            (
                settings.font_family.clone(),
                settings.font_size,
                settings.line_height,
            )
        };
        let font = FontSpec::measure(&family, font_size, line_height, window.text_system());
        let cols = ((bounds.size.width / font.cell_width).floor() as u16).max(1);
        let rows = ((bounds.size.height / font.cell_height).floor() as u16).max(1);
        let layout = Layout {
            bounds,
            cell: size(font.cell_width, font.cell_height),
            cols,
            rows,
            scale: window.scale_factor(),
        };
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        self.view.update(cx, |view, cx| {
            view.update_layout(layout, cx);
            prepare(view, font, rows, hitbox, window)
        })
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        state: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.view.read(cx).focus_handle(cx);
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );
        window.set_cursor_style(state.cursor_style, &state.hitbox);
        register_mouse_listeners(&self.view, bounds, window);

        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            paint_grid(bounds, state, window, cx);
        });

        // Ack only what has been painted: the next frame callback runs after this frame was
        // presented, so a flood is paced by the display.
        if let Some(frame) = self.view.update(cx, |view, _| view.take_ack()) {
            let view = self.view.clone();
            window.on_next_frame(move |_, cx| view.read(cx).send_ack(frame));
        }
    }
}

use gpui::Focusable as _;

/// Everything the paint pass needs, computed with the view borrowed.
fn prepare(
    view: &mut TerminalView,
    font: FontSpec,
    grid_rows: u16,
    hitbox: Hitbox,
    window: &mut Window,
) -> Prepaint {
    let focused = view.has_focus(window);
    let text_system = window.text_system().clone();
    let text_system = &*text_system;
    let hover_link = view.hover_link();
    let span = view.selection.and_then(|s| s.span(&view.model));
    let fingerprint = view.theme.fingerprint();
    let model_cols = view.model.cols();

    view.rows.begin_frame();
    let mut rows = Vec::with_capacity(usize::from(grid_rows));
    for r in 0..grid_rows {
        let line = view.line_for_row(r);
        let shaped = view
            .model
            .row_at(line)
            .filter(|row| !row.runs.is_empty())
            .map(|row| {
                let key = row_key(row, fingerprint, &font, hover_link);
                shape_cached(
                    &mut view.rows,
                    key,
                    row,
                    &view.theme,
                    &font,
                    hover_link,
                    text_system,
                )
            });
        rows.push(RowPaint {
            shaped,
            selected: span.and_then(|s| s.columns_on(line, model_cols)),
        });
    }

    let theme = &view.theme;
    let cursor = prepare_cursor(view, focused, &font, grid_rows, text_system);

    let marked = view.marked_for_paint().and_then(|text| {
        let cursor = view.model.cursor();
        if cursor.row >= grid_rows {
            return None;
        }
        let row = styled_row(text, Color::Default, CellFlags::UNDERLINE);
        Some(MarkedPaint {
            col: cursor.col,
            row: cursor.row,
            cols: u16::try_from(text_columns(text)).unwrap_or(u16::MAX),
            shaped: shape_row(&row, theme, &font, None, text_system),
        })
    });

    let cursor_style = if view.hover_link().is_some() && view.cmd_held {
        CursorStyle::PointingHand
    } else if view.model.modes().mouse != heeler_protocol::render::MouseMode::None {
        CursorStyle::Arrow
    } else {
        CursorStyle::IBeam
    };

    Prepaint {
        hitbox,
        font,
        background: hsla(theme.background()),
        selection: hsla(theme.selection()),
        rows,
        cursor,
        marked,
        cursor_style,
    }
}

fn shape_cached(
    cache: &mut RowCache,
    key: u64,
    row: &heeler_protocol::render::Row,
    theme: &Theme,
    font: &FontSpec,
    hover_link: Option<u32>,
    text_system: &WindowTextSystem,
) -> Rc<ShapedRow> {
    cache.get_or_shape(key, || shape_row(row, theme, font, hover_link, text_system))
}

fn prepare_cursor(
    view: &TerminalView,
    focused: bool,
    font: &FontSpec,
    grid_rows: u16,
    text_system: &WindowTextSystem,
) -> Option<CursorPaint> {
    let cursor = view.model.cursor();
    if view.scroll_offset != 0
        || !cursor.visible
        || cursor.row >= grid_rows
        || cursor.col >= view.model.cols().max(1)
    {
        return None;
    }
    let (shape, blinks) = view.cursor_appearance();
    let shape = if focused {
        shape
    } else {
        CursorShape::HollowBlock
    };
    if focused && blinks && !view.blink_on {
        return None;
    }
    let row = view.model.row_at(view.line_for_row(cursor.row));
    let cell = row.and_then(|row| {
        row_cells(row)
            .into_iter()
            .find(|c| c.col <= cursor.col && cursor.col < c.end())
    });
    let cols = cell.map_or(1, |c| u16::from(c.width));
    let glyph = match (shape, cell) {
        (CursorShape::Block, Some(cell))
            if !cell.text.trim().is_empty() && !cell.text.chars().all(char::is_control) =>
        {
            let flags = cell.run.flags & (CellFlags::BOLD | CellFlags::ITALIC | CellFlags::WIDE);
            let bg = view.theme.background();
            let styled = styled_row(
                cell.text,
                Color::Rgb(bg.0, bg.1, bg.2),
                flags - CellFlags::WIDE,
            );
            // `styled_row` re-derives the WIDE flag from the character itself.
            shape_row(&styled, &view.theme, font, None, text_system).line
        }
        _ => None,
    };
    Some(CursorPaint {
        col: cursor.col,
        row: cursor.row,
        cols,
        shape,
        color: hsla(view.theme.cursor()),
        glyph,
    })
}

fn register_mouse_listeners(
    view: &Entity<TerminalView>,
    bounds: Bounds<Pixels>,
    window: &mut Window,
) {
    window.on_mouse_event({
        let view = view.clone();
        move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && bounds.contains(&event.position) {
                view.update(cx, |view, cx| view.mouse_down(event, window, cx));
            }
        }
    });
    window.on_mouse_event({
        let view = view.clone();
        move |event: &MouseUpEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |view, cx| view.mouse_up(event, cx));
            }
        }
    });
    window.on_mouse_event({
        let view = view.clone();
        move |event: &MouseMoveEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble {
                let inside = bounds.contains(&event.position);
                view.update(cx, |view, cx| view.mouse_move(event, inside, cx));
            }
        }
    });
    window.on_mouse_event({
        let view = view.clone();
        move |event: &ScrollWheelEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble && bounds.contains(&event.position) {
                view.update(cx, |view, cx| view.scroll_wheel(event, cx));
            }
        }
    });
    window.on_key_event({
        let view = view.clone();
        move |event: &ModifiersChangedEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |view, cx| view.modifiers_changed(event, cx));
            }
        }
    });
}

fn cell_bounds(
    origin: Point<Pixels>,
    font: &FontSpec,
    row: u16,
    col: u16,
    cols: u16,
) -> Bounds<Pixels> {
    Bounds::new(
        point(
            origin.x + font.cell_width * f32::from(col),
            origin.y + font.cell_height * f32::from(row),
        ),
        size(font.cell_width * f32::from(cols), font.cell_height),
    )
}

fn paint_grid(bounds: Bounds<Pixels>, state: &mut Prepaint, window: &mut Window, cx: &mut App) {
    let origin = bounds.origin;
    let font = state.font.clone();
    window.paint_quad(fill(bounds, state.background));

    // Backgrounds and selection first, so no row's fill covers a neighbour's glyph overhang.
    for (r, row) in state.rows.iter().enumerate() {
        let r = r as u16;
        if let Some(shaped) = &row.shaped {
            for bg in &shaped.backgrounds {
                window.paint_quad(fill(
                    cell_bounds(origin, &font, r, bg.col, bg.cols),
                    bg.color,
                ));
            }
        }
        if let Some((from, to)) = row.selected {
            window.paint_quad(fill(
                cell_bounds(origin, &font, r, from, to - from),
                state.selection,
            ));
        }
    }

    for (r, row) in state.rows.iter().enumerate() {
        let r = r as u16;
        let Some(shaped) = &row.shaped else { continue };
        let top_left = cell_bounds(origin, &font, r, 0, 0).origin;
        if let Some(line) = &shaped.line {
            let _ = line.paint(
                top_left,
                font.cell_height,
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }
        for decoration in &shaped.decorations {
            paint_decoration(decoration, origin, &font, r, window);
        }
    }

    if let Some(marked) = &state.marked {
        let at = cell_bounds(origin, &font, marked.row, marked.col, marked.cols);
        window.paint_quad(fill(at, state.background));
        if let Some(line) = &marked.shaped.line {
            let _ = line.paint(
                at.origin,
                font.cell_height,
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }
        for decoration in &marked.shaped.decorations {
            let shifted = Decoration {
                col: decoration.col + marked.col,
                ..*decoration
            };
            paint_decoration(&shifted, origin, &font, marked.row, window);
        }
    }

    if let Some(cursor) = &state.cursor {
        paint_cursor(cursor, origin, &font, window, cx);
    }
}

fn paint_cursor(
    cursor: &CursorPaint,
    origin: Point<Pixels>,
    font: &FontSpec,
    window: &mut Window,
    cx: &mut App,
) {
    let cell = cell_bounds(origin, font, cursor.row, cursor.col, cursor.cols);
    let thickness = px(2.);
    match cursor.shape {
        CursorShape::Block => {
            window.paint_quad(fill(cell, cursor.color));
            if let Some(glyph) = &cursor.glyph {
                let _ = glyph.paint(
                    cell.origin,
                    font.cell_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
        }
        CursorShape::HollowBlock => {
            window.paint_quad(outline(cell, cursor.color, gpui::BorderStyle::Solid));
        }
        CursorShape::Bar => {
            window.paint_quad(fill(
                Bounds::new(cell.origin, size(thickness, cell.size.height)),
                cursor.color,
            ));
        }
        CursorShape::Underline => {
            window.paint_quad(fill(
                Bounds::new(
                    point(cell.origin.x, cell.origin.y + cell.size.height - thickness),
                    size(cell.size.width, thickness),
                ),
                cursor.color,
            ));
        }
    }
}

fn paint_decoration(
    decoration: &Decoration,
    origin: Point<Pixels>,
    font: &FontSpec,
    row: u16,
    window: &mut Window,
) {
    let cell = cell_bounds(origin, font, row, decoration.col, decoration.cols);
    let one = px(1.);
    let baseline =
        cell.origin.y + (font.cell_height - font.ascent - font.descent) / 2. + font.ascent;
    let floor = cell.origin.y + font.cell_height - one;
    let y = (baseline + (font.descent * 0.5).max(one)).min(floor);
    let x = cell.origin.x;
    let width = cell.size.width;
    let line = |y: Pixels, x: Pixels, w: Pixels, window: &mut Window| {
        window.paint_quad(fill(
            Bounds::new(point(x, y), size(w, one)),
            decoration.color,
        ));
    };
    match decoration.kind {
        UnderlineKind::Single => line(y, x, width, window),
        UnderlineKind::Double => {
            let lower = (y + one).min(floor);
            line(lower, x, width, window);
            line(lower - one * 2., x, width, window);
        }
        UnderlineKind::Dotted => {
            let mut dx = px(0.);
            while dx < width {
                line(y, x + dx, one.min(width - dx), window);
                dx += one * 2.;
            }
        }
        UnderlineKind::Dashed => {
            let mut dx = px(0.);
            while dx < width {
                line(y, x + dx, (one * 3.).min(width - dx), window);
                dx += one * 5.;
            }
        }
    }
}

//! Editors for the values a socket can carry.
//!
//! Which widget a socket gets is decided by its
//! [`ValueType`](wxsl_core::node::ValueType) and nothing else — that is the
//! rule ADR 0004 set and ADR 0013 kept: the core node definition carries no
//! display metadata, so the editor derives everything it needs from the
//! interface the node already describes.
//!
//! | Type | Widget |
//! |---|---|
//! | `bool` | a checkbox |
//! | `i32`, `u32`, `f32` | a drag field |
//! | `vec2f`, `vec3f`, `vec4f` | one drag field per component, plus a swatch when it reads as a colour |
//! | `mat3x3f`, `mat4x4f` | a grid of drag fields, one row per matrix row |
//!
//! A swatch is also a button: clicking it opens a [`color_picker`] under the
//! row, because nobody knows what `vec3f(0.42, 0.19, 0.07)` looks like.

use glam::Vec2;
use wxsl_core::macros::{MacroDef, MacroValue};
use wxsl_core::node::{Value, ValueType};
use wxsl_render::ui::draw::{Color, Rect};

use crate::theme::Theme;
use crate::ui::{Align, Id, Ui};

/// A short, readable form of a value, for a node's row on the canvas.
///
/// Vectors lose their type prefix (the port's colour already says what it
/// is) and components are trimmed, because the whole thing has to fit in
/// half a node's width.
pub fn value_summary(value: &Value) -> String {
    fn components(values: &[f32]) -> String {
        values
            .iter()
            .map(|component| trim(*component))
            .collect::<Vec<_>>()
            .join(", ")
    }
    /// A number short enough for a canvas row.
    fn trim(value: f32) -> String {
        if value == value.trunc() && value.abs() < 1e4 {
            format!("{value:.0}")
        } else {
            format!("{value:.2}")
        }
    }

    match value {
        Value::Bool(value) => value.to_string(),
        Value::I32(value) => value.to_string(),
        Value::U32(value) => value.to_string(),
        Value::F32(value) => trim(*value),
        Value::Vec2(values) => components(values),
        Value::Vec3(values) => components(values),
        Value::Vec4(values) => components(values),
        // Sixteen numbers do not fit anywhere useful; the inspector says the
        // same thing with more room.
        Value::Mat3(_) => "mat3x3f".to_string(),
        Value::Mat4(_) => "mat4x4f".to_string(),
    }
}

/// Whether a value is plausibly a colour, and so worth a swatch.
///
/// A guess from the type and the range, not from metadata: a `vec3f` or
/// `vec4f` whose components are all in `0..=1` is very likely a colour in a
/// shader graph, and a swatch next to the numbers costs nothing when it is
/// wrong.
pub fn looks_like_color(value: &Value) -> bool {
    let components: &[f32] = match value {
        Value::Vec3(values) => values,
        Value::Vec4(values) => values,
        _ => return false,
    };
    components
        .iter()
        .all(|component| (0.0..=1.0).contains(component))
}

/// The drag speed and bounds a socket's type suggests.
///
/// Bounds only where the type itself implies them: an unsigned integer
/// cannot go below zero. Anything else — a roughness that means nothing
/// outside `0..=1`, say — is the *node's* business, and the node model has no
/// way to say so, so the editor does not invent one.
fn drag_settings(ty: ValueType) -> (f32, Option<(f32, f32)>) {
    match ty {
        ValueType::U32 => (0.05, Some((0.0, f32::MAX))),
        ValueType::I32 => (0.05, None),
        _ => (0.005, None),
    }
}

/// The sub-id the colour picker and its swatch take, kept clear of the
/// per-component ids [`vector_editor`] and [`matrix_editor`] hand out (0..16).
const SWATCH: u64 = 64;
const PICKER: u64 = 65;

/// The height [`value_editor`] needs for `ty` at `id`.
///
/// Not a constant row: a matrix is a grid of fields, and a colour whose
/// picker is open carries it underneath. A layout that has to reserve space
/// before drawing — the inspector's scroll area — asks this first.
pub fn value_editor_height(ui: &Ui<'_>, id: Id, ty: ValueType, width: f32) -> f32 {
    let theme = ui.theme();
    let rows = match ty {
        ValueType::Mat3 => 3,
        ValueType::Mat4 => 4,
        _ => 1,
    };
    let mut height =
        theme.metrics.row_height * rows as f32 + theme.metrics.row_gap * (rows - 1) as f32;
    if ui.color_picker_open(id.with(PICKER)) {
        height += theme.metrics.row_gap + color_picker_height(theme, width, ty == ValueType::Vec4);
    }
    height
}

/// Edit `value` in `rect`. Returns whether it changed.
///
/// `rect` has to be as tall as [`value_editor_height`] says: a matrix is a
/// grid of fields, and a colour whose swatch has been clicked carries a
/// [`color_picker`] underneath.
pub fn value_editor(ui: &mut Ui<'_>, id: Id, rect: Rect, value: &mut Value) -> bool {
    // The picker sits below the fields, which keeps the numbers in the same
    // place whether it is open or not.
    let picker_open = looks_like_color(value) && ui.color_picker_open(id.with(PICKER));
    let (rect, picker_rect) = if picker_open {
        let gap = ui.theme().metrics.row_gap;
        let (row, rest) = rect.split_top(ui.theme().metrics.row_height);
        (row, Some(rest.split_top(gap).1))
    } else {
        (rect, None)
    };
    let mut changed = false;
    if let Some(picker_rect) = picker_rect {
        let mut components = match value {
            Value::Vec3(values) => values.to_vec(),
            Value::Vec4(values) => values.to_vec(),
            _ => Vec::new(),
        };
        if color_picker(ui, id.with(PICKER).with(1), picker_rect, &mut components) {
            *value = match components.len() {
                3 => Value::Vec3([components[0], components[1], components[2]]),
                _ => Value::Vec4([components[0], components[1], components[2], components[3]]),
            };
            changed = true;
        }
    }
    changed |= value_fields(ui, id, rect, value);
    changed
}

/// The numeric half of [`value_editor`]: one widget per component.
fn value_fields(ui: &mut Ui<'_>, id: Id, rect: Rect, value: &mut Value) -> bool {
    match value {
        Value::Bool(flag) => {
            let mut current = *flag;
            let response = ui.checkbox(
                id,
                rect,
                if current { "true" } else { "false" },
                &mut current,
            );
            if response.changed {
                *value = Value::Bool(current);
                return true;
            }
            false
        }
        Value::F32(number) => {
            let (speed, range) = drag_settings(ValueType::F32);
            let mut current = *number;
            let changed = ui.drag_value(id, rect, &mut current, speed, range).changed;
            if changed {
                *value = Value::F32(current);
            }
            changed
        }
        Value::I32(number) => {
            let (speed, range) = drag_settings(ValueType::I32);
            let mut current = *number as f32;
            let changed = ui.drag_value(id, rect, &mut current, speed, range).changed;
            if changed {
                *value = Value::I32(current.round() as i32);
            }
            changed
        }
        Value::U32(number) => {
            let (speed, range) = drag_settings(ValueType::U32);
            let mut current = *number as f32;
            let changed = ui.drag_value(id, rect, &mut current, speed, range).changed;
            if changed {
                *value = Value::U32(current.round().max(0.0) as u32);
            }
            changed
        }
        Value::Vec2(components) => {
            let mut current = *components;
            let changed = vector_editor(ui, id, rect, &mut current, false);
            if changed {
                *value = Value::Vec2(current);
            }
            changed
        }
        Value::Vec3(components) => {
            let swatch = looks_like_color(&Value::Vec3(*components));
            let mut current = *components;
            let changed = vector_editor(ui, id, rect, &mut current, swatch);
            if changed {
                *value = Value::Vec3(current);
            }
            changed
        }
        Value::Vec4(components) => {
            let swatch = looks_like_color(&Value::Vec4(*components));
            let mut current = *components;
            let changed = vector_editor(ui, id, rect, &mut current, swatch);
            if changed {
                *value = Value::Vec4(current);
            }
            changed
        }
        Value::Mat3(cells) => {
            let mut current = *cells;
            let changed = matrix_editor::<3, 9>(ui, id, rect, &mut current);
            if changed {
                *value = Value::Mat3(current);
            }
            changed
        }
        Value::Mat4(cells) => {
            let mut current = *cells;
            let changed = matrix_editor::<4, 16>(ui, id, rect, &mut current);
            if changed {
                *value = Value::Mat4(current);
            }
            changed
        }
    }
}

// ---------------------------------------------------------------------------
// Colour picking
// ---------------------------------------------------------------------------

/// How many saturation/value cells across the square is drawn with.
///
/// The interface renderer draws flat rectangles, so a gradient is a grid of
/// them; this is the trade between a smooth square and the instance count of
/// one open picker. At 32 the banding is barely visible at panel width.
const PICKER_CELLS: usize = 32;

/// RGB in `0..=1` to hue (in turns), saturation and value.
fn rgb_to_hsv([r, g, b]: [f32; 3]) -> [f32; 3] {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let span = max - min;
    let hue = if span <= f32::EPSILON {
        0.0
    } else if max == r {
        ((g - b) / span).rem_euclid(6.0)
    } else if max == g {
        (b - r) / span + 2.0
    } else {
        (r - g) / span + 4.0
    };
    let saturation = if max <= f32::EPSILON { 0.0 } else { span / max };
    [hue / 6.0, saturation, max]
}

/// Hue (in turns), saturation and value back to RGB in `0..=1`.
fn hsv_to_rgb([h, s, v]: [f32; 3]) -> [f32; 3] {
    let sector = h.rem_euclid(1.0) * 6.0;
    let index = sector.floor();
    let fraction = sector - index;
    let (p, q, t) = (
        v * (1.0 - s),
        v * (1.0 - s * fraction),
        v * (1.0 - s * (1.0 - fraction)),
    );
    match index as u32 % 6 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

/// The height [`color_picker`] needs at `width`, for a layout that has to
/// reserve it before drawing.
///
/// The square is sized from the width rather than in rows so it stays
/// square-ish at any panel width — a wide, shallow band is hard to aim
/// value at.
pub fn color_picker_height(theme: &Theme, width: f32, alpha: bool) -> f32 {
    let strip = theme.metrics.row_height * 0.6;
    let gap = theme.metrics.row_gap;
    square_height(width) + gap + strip + if alpha { gap + strip } else { 0.0 }
}

/// The saturation/value square's height at `width`.
fn square_height(width: f32) -> f32 {
    (width * 0.7).clamp(48.0, 220.0)
}

/// A saturation/value square over a hue strip, and an alpha strip when the
/// value has a fourth component. Returns whether the colour changed.
///
/// Numbers alone are a poor way to pick a colour — nobody knows what
/// `vec3f(0.42, 0.19, 0.07)` looks like — but they are the only way to
/// *check* one, so this sits below the component fields rather than
/// replacing them, and both edit the same value.
pub fn color_picker(ui: &mut Ui<'_>, id: Id, rect: Rect, components: &mut [f32]) -> bool {
    let theme = *ui.theme();
    let gap = theme.metrics.row_gap;
    let strip_height = theme.metrics.row_height * 0.6;
    let alpha = components.len() > 3;

    let (square, rest) = rect.split_top(square_height(rect.width()));
    let (_, rest) = rest.split_top(gap);
    let (hue_strip, rest) = rest.split_top(strip_height);
    let alpha_strip = alpha.then(|| rest.split_top(gap).1.split_top(strip_height).0);

    let mut hsv = rgb_to_hsv([components[0], components[1], components[2]]);
    let mut changed = false;

    // -- saturation and value ------------------------------------------
    let cell = Vec2::new(
        square.width() / PICKER_CELLS as f32,
        square.height() / PICKER_CELLS as f32,
    );
    for row in 0..PICKER_CELLS {
        for column in 0..PICKER_CELLS {
            let saturation = (column as f32 + 0.5) / PICKER_CELLS as f32;
            let value = 1.0 - (row as f32 + 0.5) / PICKER_CELLS as f32;
            let [r, g, b] = hsv_to_rgb([hsv[0], saturation, value]);
            ui.draw().rect(
                Rect::from_min_size(
                    square.min + Vec2::new(cell.x * column as f32, cell.y * row as f32),
                    // A hair of overlap, so rounding leaves no seams.
                    cell + Vec2::splat(0.75),
                ),
                Color::rgb(r, g, b),
            );
        }
    }
    let square_response = ui.interact(id.with(0), square);
    if square_response.dragging || square_response.pressed {
        let local = ui.input.pointer_or_zero() - square.min;
        hsv[1] = (local.x / square.width()).clamp(0.0, 1.0);
        hsv[2] = 1.0 - (local.y / square.height()).clamp(0.0, 1.0);
        changed = true;
    }
    marker(
        ui,
        Vec2::new(
            square.min.x + hsv[1] * square.width(),
            square.min.y + (1.0 - hsv[2]) * square.height(),
        ),
        theme.metrics.outline_width.max(1.0) * 2.0,
    );
    ui.draw().round_rect_border(
        square,
        theme.metrics.radius,
        theme.metrics.outline_width,
        theme.palette.outline,
    );

    // -- hue -----------------------------------------------------------
    let bars = PICKER_CELLS * 2;
    let bar_width = hue_strip.width() / bars as f32;
    for index in 0..bars {
        let hue = (index as f32 + 0.5) / bars as f32;
        let [r, g, b] = hsv_to_rgb([hue, 1.0, 1.0]);
        ui.draw().rect(
            Rect::from_min_size(
                hue_strip.min + Vec2::new(bar_width * index as f32, 0.0),
                Vec2::new(bar_width + 0.75, hue_strip.height()),
            ),
            Color::rgb(r, g, b),
        );
    }
    let hue_response = ui.interact(id.with(1), hue_strip);
    if hue_response.dragging || hue_response.pressed {
        let local = ui.input.pointer_or_zero().x - hue_strip.min.x;
        hsv[0] = (local / hue_strip.width()).clamp(0.0, 1.0);
        changed = true;
    }
    marker(
        ui,
        Vec2::new(
            hue_strip.min.x + hsv[0] * hue_strip.width(),
            hue_strip.center().y,
        ),
        hue_strip.height() * 0.5,
    );

    if changed {
        let [r, g, b] = hsv_to_rgb(hsv);
        components[0] = r;
        components[1] = g;
        components[2] = b;
    }

    // -- alpha ---------------------------------------------------------
    if let Some(alpha_strip) = alpha_strip {
        let opaque = Color::rgb(components[0], components[1], components[2]);
        for index in 0..bars {
            let fraction = (index as f32 + 0.5) / bars as f32;
            ui.draw().rect(
                Rect::from_min_size(
                    alpha_strip.min + Vec2::new(bar_width * index as f32, 0.0),
                    Vec2::new(bar_width + 0.75, alpha_strip.height()),
                ),
                opaque.with_alpha(fraction),
            );
        }
        let alpha_response = ui.interact(id.with(2), alpha_strip);
        if alpha_response.dragging || alpha_response.pressed {
            let local = ui.input.pointer_or_zero().x - alpha_strip.min.x;
            components[3] = (local / alpha_strip.width()).clamp(0.0, 1.0);
            changed = true;
        }
        marker(
            ui,
            Vec2::new(
                alpha_strip.min.x + components[3] * alpha_strip.width(),
                alpha_strip.center().y,
            ),
            alpha_strip.height() * 0.5,
        );
    }
    changed
}

/// A small ring marking a position inside a gradient, in both a light and a
/// dark outline so it stays visible whatever it sits on.
fn marker(ui: &mut Ui<'_>, at: Vec2, radius: f32) {
    let radius = radius.max(3.0);
    for (inset, color) in [
        (0.0, Color::rgba(0.0, 0.0, 0.0, 0.75)),
        (1.0, Color::rgba(1.0, 1.0, 1.0, 0.9)),
    ] {
        let ring = Rect::from_center_size(at, Vec2::splat((radius - inset) * 2.0));
        ui.draw().round_rect_border(ring, radius, 1.0, color);
    }
}

/// A grid of drag fields, one row of the matrix per row of the grid.
///
/// [`Value::Mat3`]/[`Value::Mat4`] store *columns*, but a matrix is read by
/// rows, so the field at grid position `(row, column)` edits cell
/// `column * N + row`.
fn matrix_editor<const N: usize, const CELLS: usize>(
    ui: &mut Ui<'_>,
    id: Id,
    rect: Rect,
    cells: &mut [f32; CELLS],
) -> bool {
    let gap = ui.theme().metrics.row_gap;
    let side = N as f32;
    let height = ((rect.height() - gap * (side - 1.0)) / side).max(8.0);
    let width = ((rect.width() - gap * (side - 1.0)) / side).max(8.0);
    let (speed, range) = drag_settings(ValueType::F32);
    let mut changed = false;
    for row in 0..N {
        for column in 0..N {
            let field = Rect::from_min_size(
                Vec2::new(
                    rect.min.x + (width + gap) * column as f32,
                    rect.min.y + (height + gap) * row as f32,
                ),
                Vec2::new(width, height),
            );
            let index = column * N + row;
            changed |= ui
                .drag_value(
                    id.with(index as u64),
                    field,
                    &mut cells[index],
                    speed,
                    range,
                )
                .changed;
        }
    }
    changed
}

/// One drag field per component, with an optional colour swatch.
fn vector_editor<const N: usize>(
    ui: &mut Ui<'_>,
    id: Id,
    rect: Rect,
    components: &mut [f32; N],
    swatch: bool,
) -> bool {
    let theme = *ui.theme();
    let gap = theme.metrics.row_gap;
    let mut remaining = rect;

    if swatch {
        let size = rect.height();
        let (patch, rest) = remaining.split_left(size);
        remaining = Rect::from_min_max(rest.min + Vec2::new(gap, 0.0), rest.max);
        let color = Color::rgba(
            components[0],
            *components.get(1).unwrap_or(&0.0),
            *components.get(2).unwrap_or(&0.0),
            1.0,
        );
        ui.draw().round_rect(patch, theme.metrics.radius, color);
        // The swatch is also the way in to the picker: the thing you want to
        // click when a colour is wrong is the colour.
        let response = ui.interact(id.with(SWATCH), patch);
        if response.clicked {
            ui.toggle_color_picker(id.with(PICKER));
        }
        ui.draw().round_rect_border(
            patch,
            theme.metrics.radius,
            theme.metrics.outline_width * if response.hovered { 2.0 } else { 1.0 },
            if response.hovered {
                theme.palette.selection
            } else {
                theme.palette.outline
            },
        );
    }

    let count = components.len() as f32;
    let width = ((remaining.width() - gap * (count - 1.0)) / count).max(8.0);
    let mut changed = false;
    for (index, component) in components.iter_mut().enumerate() {
        let left = remaining.min.x + (width + gap) * index as f32;
        let field = Rect::from_min_size(
            Vec2::new(left, remaining.min.y),
            Vec2::new(width, remaining.height()),
        );
        let (speed, range) = drag_settings(ValueType::F32);
        changed |= ui
            .drag_value(id.with(index as u64), field, component, speed, range)
            .changed;
    }
    changed
}

/// Edit a macro variable. Returns whether it changed.
///
/// Macro variables are not socket values: a flag switches code out of the
/// shader and an integer can be a loop bound, so they are edited here rather
/// than through [`value_editor`], and changing one recompiles.
pub fn macro_editor(
    ui: &mut Ui<'_>,
    id: Id,
    rect: Rect,
    declaration: &MacroDef,
    value: &mut MacroValue,
) -> bool {
    let theme = *ui.theme();
    let label_width = rect.width() * 0.55;
    let (label_rect, editor_rect) = rect.split_left(label_width);
    ui.truncated_label(
        label_rect,
        declaration.name.as_str(),
        theme.palette.text,
        Align::Left,
    );

    match value {
        MacroValue::Flag(flag) => {
            let mut current = *flag;
            let response = ui.checkbox(id, editor_rect, "", &mut current);
            if response.changed {
                *value = MacroValue::Flag(current);
                return true;
            }
            false
        }
        MacroValue::Int(number) => {
            let mut current = *number as f32;
            // Integers a shader uses as a loop bound or an array size: a
            // gentle speed, because every step is a recompile.
            let changed = ui
                .drag_value(id, editor_rect, &mut current, 0.03, None)
                .changed;
            if changed {
                *value = MacroValue::Int(current.round() as i32);
            }
            changed
        }
        MacroValue::Float(number) => {
            let mut current = *number;
            let changed = ui
                .drag_value(id, editor_rect, &mut current, 0.01, None)
                .changed;
            if changed {
                *value = MacroValue::Float(current);
            }
            changed
        }
    }
}

/// A read-only row: a dim name on the left, a value on the right.
pub fn field_row(ui: &mut Ui<'_>, rect: Rect, name: &str, value: &str) {
    let theme = *ui.theme();
    let (name_rect, value_rect) = rect.split_left(rect.width() * 0.45);
    ui.truncated_label(name_rect, name, theme.palette.text_dim, Align::Left);
    ui.truncated_label(value_rect, value, theme.palette.text, Align::Right);
}

/// A value's type and current contents, as one line.
pub fn typed_summary(ty: ValueType, value: Option<&Value>) -> String {
    match value {
        Some(value) => format!("{ty} = {}", value_summary(value)),
        None => ty.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_round_trips_through_rgb() {
        // The picker stores RGB and edits HSV, so a hue drag that does not
        // move saturation or value has to leave them where they were.
        for rgb in [
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.95, 0.36, 0.18],
            [0.07, 0.018, 0.005],
            [0.2, 0.6, 0.35],
            [0.5, 0.5, 0.5],
        ] {
            let [r, g, b] = hsv_to_rgb(rgb_to_hsv(rgb));
            for (before, after) in rgb.iter().zip([r, g, b]) {
                assert!(
                    (before - after).abs() < 1e-5,
                    "{rgb:?} came back as {:?}",
                    [r, g, b]
                );
            }
        }
    }

    #[test]
    fn every_hue_is_a_saturated_colour_of_full_value() {
        // What the hue strip draws: sweeping hue at s = v = 1 has to stay on
        // the outside of the colour solid, or the strip has dark or washed
        // out bands in it.
        for step in 0..24 {
            let hue = step as f32 / 24.0;
            let rgb = hsv_to_rgb([hue, 1.0, 1.0]);
            let max = rgb.iter().copied().fold(f32::MIN, f32::max);
            let min = rgb.iter().copied().fold(f32::MAX, f32::min);
            assert!((max - 1.0).abs() < 1e-5, "hue {hue} peaks at {max}");
            assert!(min.abs() < 1e-5, "hue {hue} bottoms at {min}");
        }
    }

    #[test]
    fn a_colour_picker_is_taller_than_a_row_and_grows_with_alpha() {
        let theme = Theme::default();
        let row = theme.metrics.row_height;
        let rgb = color_picker_height(&theme, 240.0, false);
        let rgba = color_picker_height(&theme, 240.0, true);
        assert!(rgb > row * 2.0, "a picker needs room to aim in");
        assert!(rgba > rgb, "the alpha strip is another strip");
    }

    #[test]
    fn summaries_are_short_enough_for_a_node_row() {
        assert_eq!(value_summary(&Value::F32(1.0)), "1");
        assert_eq!(value_summary(&Value::F32(0.5)), "0.50");
        assert_eq!(value_summary(&Value::Bool(true)), "true");
        assert_eq!(value_summary(&Value::I32(-3)), "-3");
        assert_eq!(
            value_summary(&Value::Vec3([0.8, 0.2, 0.0])),
            "0.80, 0.20, 0"
        );
        // A matrix says what it is rather than listing sixteen numbers.
        assert_eq!(value_summary(&Value::Mat4([0.0; 16])), "mat4x4f");
        for value in [
            Value::Vec4([1.0, 2.0, 3.0, 4.0]),
            Value::Vec2([0.125, 0.25]),
            Value::Mat3([1.0; 9]),
        ] {
            assert!(
                value_summary(&value).len() <= 24,
                "{} is too long for a row",
                value_summary(&value)
            );
        }
    }

    #[test]
    fn a_normalized_vector_reads_as_a_colour_and_others_do_not() {
        assert!(looks_like_color(&Value::Vec3([0.8, 0.2, 0.0])));
        assert!(looks_like_color(&Value::Vec4([0.0, 0.0, 0.0, 1.0])));
        // Out of range: a world-space normal or a position, not a colour.
        assert!(!looks_like_color(&Value::Vec3([1.5, 0.0, 0.0])));
        assert!(!looks_like_color(&Value::Vec3([-1.0, 0.0, 0.0])));
        // Wrong shape.
        assert!(!looks_like_color(&Value::Vec2([0.5, 0.5])));
        assert!(!looks_like_color(&Value::F32(0.5)));
    }

    #[test]
    fn only_the_types_that_imply_bounds_get_them() {
        // A roughness means nothing outside 0..1, but the node model has no
        // way to say so, and inventing a range would silently clamp a value
        // some other node legitimately wants.
        assert_eq!(drag_settings(ValueType::U32).1, Some((0.0, f32::MAX)));
        assert_eq!(drag_settings(ValueType::F32).1, None);
        assert_eq!(drag_settings(ValueType::I32).1, None);
    }

    #[test]
    fn a_typed_summary_names_the_type_with_or_without_a_value() {
        assert_eq!(
            typed_summary(ValueType::Vec3, Some(&Value::Vec3([1.0, 0.0, 0.0]))),
            "vec3f = 1, 0, 0"
        );
        assert_eq!(typed_summary(ValueType::F32, None), "f32");
    }
}

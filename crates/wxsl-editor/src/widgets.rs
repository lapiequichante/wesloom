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
//! | `mat3x3f`, `mat4x4f` | shown, not edited — see [`value_editor`] |

use glam::Vec2;
use wxsl_core::macros::{MacroDef, MacroValue};
use wxsl_core::node::{Value, ValueType};
use wxsl_render::ui::draw::{Color, Rect};

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

/// Edit `value` in `rect`. Returns whether it changed.
///
/// Matrices are shown rather than edited: a `mat4x4f` is sixteen numbers, a
/// grid of sixteen drag fields is unusable at panel width, and no node in the
/// library takes a matrix parameter that is not fed by an edge. When one
/// does, this is the function to grow.
pub fn value_editor(ui: &mut Ui<'_>, id: Id, rect: Rect, value: &mut Value) -> bool {
    let theme = *ui.theme();
    let gap = theme.metrics.row_gap;

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
        Value::Mat3(_) | Value::Mat4(_) => {
            let _ = gap;
            ui.draw()
                .round_rect(rect, theme.metrics.radius, theme.palette.control);
            let text = format!("{} (not editable here)", value_summary(value));
            ui.truncated_label(
                rect.shrink(4.0),
                &text,
                theme.palette.text_dim,
                Align::Center,
            );
            false
        }
    }
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
        ui.draw().round_rect_border(
            patch,
            theme.metrics.radius,
            theme.metrics.outline_width,
            theme.palette.outline,
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

//! Colours and metrics, in one place.
//!
//! Everything the editor draws takes its colour and its size from here, so
//! that a change of palette is one edit rather than a hunt. The defaults are
//! a dark theme, because a shader editor's subject is a lit 3D preview and a
//! bright interface next to it ruins the only thing on screen whose
//! brightness matters.
//!
//! Colours are in the target's colour space, not linear — see
//! [`wxsl_render::ui::Color`]. The UI pass converts nothing, which is what
//! makes a hex value from a palette land on screen as that colour.

use wxsl_render::ui::draw::Rect;
use wxsl_render::ui::Color;

/// The editor's colours.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// Behind everything.
    pub background: Color,
    /// The node canvas's own background, darker than a panel.
    pub canvas: Color,
    /// The canvas grid's minor lines.
    pub grid: Color,
    /// The canvas grid's major lines, every tenth.
    pub grid_major: Color,
    /// A panel's fill.
    pub panel: Color,
    /// A panel's header strip.
    pub panel_header: Color,
    /// Any dividing line or outline.
    pub outline: Color,
    /// A node's body.
    pub node: Color,
    /// A node's title strip.
    pub node_header: Color,
    /// The outline of a selected node.
    pub selection: Color,
    /// Primary text.
    pub text: Color,
    /// Secondary text: socket names, hints, line numbers.
    pub text_dim: Color,
    /// Text on an accented control.
    pub text_on_accent: Color,
    /// The accent: focus rings, the active tab, a pressed button.
    pub accent: Color,
    /// A control's fill.
    pub control: Color,
    /// A control's fill when hovered.
    pub control_hover: Color,
    /// A control's fill when pressed or active.
    pub control_active: Color,
    /// An error message, and an invalid node's outline.
    pub error: Color,
    /// A warning.
    pub warning: Color,
    /// A link between two ports.
    pub link: Color,
    /// A link being dragged, before it lands.
    pub link_pending: Color,
    /// A link the pointer is over, which clicking would cut.
    pub link_hover: Color,
}

impl Palette {
    /// The default dark palette.
    pub const DARK: Palette = Palette {
        background: Color::rgb(0.07, 0.075, 0.09),
        canvas: Color::rgb(0.10, 0.105, 0.125),
        grid: Color::rgb(0.145, 0.15, 0.18),
        grid_major: Color::rgb(0.20, 0.21, 0.25),
        panel: Color::rgb(0.135, 0.14, 0.17),
        panel_header: Color::rgb(0.17, 0.18, 0.21),
        outline: Color::rgb(0.24, 0.25, 0.30),
        node: Color::rgb(0.19, 0.20, 0.24),
        node_header: Color::rgb(0.25, 0.27, 0.33),
        selection: Color::rgb(0.98, 0.75, 0.32),
        text: Color::rgb(0.90, 0.91, 0.94),
        text_dim: Color::rgb(0.60, 0.62, 0.68),
        text_on_accent: Color::rgb(0.06, 0.07, 0.09),
        accent: Color::rgb(0.38, 0.68, 0.98),
        control: Color::rgb(0.23, 0.24, 0.29),
        control_hover: Color::rgb(0.29, 0.31, 0.37),
        control_active: Color::rgb(0.35, 0.38, 0.45),
        error: Color::rgb(0.95, 0.42, 0.42),
        warning: Color::rgb(0.96, 0.76, 0.35),
        link: Color::rgb(0.62, 0.66, 0.74),
        link_pending: Color::rgb(0.98, 0.82, 0.45),
        link_hover: Color::rgb(0.98, 0.55, 0.45),
    };
}

/// Sizes and spacings, in logical pixels before the scale factor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// Body text size.
    pub text_size: f32,
    /// Smaller text: socket labels, hints.
    pub small_text_size: f32,
    /// Monospaced text, for the code panels.
    pub mono_text_size: f32,
    /// Height of a row: a button, a field, a socket.
    pub row_height: f32,
    /// Gap between rows.
    pub row_gap: f32,
    /// Padding inside a panel or a node.
    pub padding: f32,
    /// Corner radius of a panel.
    pub panel_radius: f32,
    /// Corner radius of a node or a control.
    pub radius: f32,
    /// Width of an outline.
    pub outline_width: f32,
    /// Radius of a socket's circle.
    pub port_radius: f32,
    /// Thickness of a link.
    pub link_width: f32,
    /// Width of a node on the canvas, before zoom.
    pub node_width: f32,
    /// Height of a node's title strip.
    pub node_header_height: f32,
    /// Width of the side panels.
    pub side_panel_width: f32,
    /// Height of the bottom panel.
    pub bottom_panel_height: f32,
    /// Height of the status bar.
    pub status_height: f32,
    /// Height of a scrollbar, and the width of a vertical one.
    pub scrollbar_width: f32,
}

impl Metrics {
    /// The default metrics, tuned for a 1x display.
    pub const DEFAULT: Metrics = Metrics {
        text_size: 13.0,
        small_text_size: 11.0,
        mono_text_size: 12.0,
        row_height: 22.0,
        row_gap: 4.0,
        padding: 8.0,
        panel_radius: 6.0,
        radius: 4.0,
        outline_width: 1.0,
        port_radius: 5.0,
        link_width: 2.0,
        node_width: 190.0,
        node_header_height: 24.0,
        side_panel_width: 260.0,
        bottom_panel_height: 240.0,
        status_height: 24.0,
        scrollbar_width: 10.0,
    };

    /// The same metrics scaled by a display's pixel ratio.
    ///
    /// Every field is a length, so scaling is a multiply — which is the
    /// reason the metrics are a plain struct of `f32` rather than a set of
    /// constants scattered through the drawing code.
    pub fn scaled(&self, scale: f32) -> Metrics {
        let scale = scale.max(0.1);
        Metrics {
            text_size: self.text_size * scale,
            small_text_size: self.small_text_size * scale,
            mono_text_size: self.mono_text_size * scale,
            row_height: self.row_height * scale,
            row_gap: self.row_gap * scale,
            padding: self.padding * scale,
            panel_radius: self.panel_radius * scale,
            radius: self.radius * scale,
            outline_width: self.outline_width * scale,
            port_radius: self.port_radius * scale,
            link_width: self.link_width * scale,
            node_width: self.node_width * scale,
            node_header_height: self.node_header_height * scale,
            side_panel_width: self.side_panel_width * scale,
            bottom_panel_height: self.bottom_panel_height * scale,
            status_height: self.status_height * scale,
            scrollbar_width: self.scrollbar_width * scale,
        }
    }
}

/// A palette, the metrics, and the scale they are at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    /// Colours.
    pub palette: Palette,
    /// Sizes, already scaled.
    pub metrics: Metrics,
    /// Physical pixels per logical pixel.
    pub scale: f32,
}

impl Default for Theme {
    fn default() -> Self {
        Theme::new(1.0)
    }
}

impl Theme {
    /// The default theme at a display scale.
    pub fn new(scale: f32) -> Self {
        Theme {
            palette: Palette::DARK,
            metrics: Metrics::DEFAULT.scaled(scale),
            scale: scale.max(0.1),
        }
    }

    /// Re-scale for a display change, keeping the palette.
    pub fn set_scale(&mut self, scale: f32) {
        self.metrics = Metrics::DEFAULT.scaled(scale);
        self.scale = scale.max(0.1);
    }

    /// The colour a socket's type is drawn in.
    ///
    /// Derived from the *type*, never from editor metadata on the node
    /// definition — which is the rule ADR 0004 set and ADR 0013 kept: the
    /// core graph model knows nothing about how it looks.
    pub fn type_color(&self, ty: wxsl_core::node::ValueType) -> Color {
        use wxsl_core::node::ValueType;
        match ty {
            ValueType::Bool => Color::rgb(0.85, 0.45, 0.75),
            ValueType::I32 => Color::rgb(0.55, 0.80, 0.55),
            ValueType::U32 => Color::rgb(0.45, 0.75, 0.65),
            ValueType::F32 => Color::rgb(0.65, 0.70, 0.80),
            ValueType::Vec2 => Color::rgb(0.55, 0.80, 0.95),
            ValueType::Vec3 => Color::rgb(0.98, 0.78, 0.42),
            ValueType::Vec4 => Color::rgb(0.95, 0.55, 0.40),
            ValueType::Mat3 => Color::rgb(0.70, 0.60, 0.95),
            ValueType::Mat4 => Color::rgb(0.60, 0.50, 0.90),
        }
    }

    /// The colour a node's category is drawn in, for its title strip.
    ///
    /// Also derived from data the core model already has — the category
    /// string — rather than from a colour stored per definition.
    pub fn category_color(&self, category: &str) -> Color {
        match category {
            "input" => Color::rgb(0.32, 0.55, 0.42),
            "output" => Color::rgb(0.55, 0.33, 0.38),
            "math" => Color::rgb(0.30, 0.40, 0.58),
            "color" => Color::rgb(0.52, 0.38, 0.58),
            "space" => Color::rgb(0.33, 0.47, 0.55),
            "lighting" => Color::rgb(0.58, 0.48, 0.30),
            "generative" => Color::rgb(0.40, 0.52, 0.35),
            "sdf" => Color::rgb(0.48, 0.40, 0.55),
            "animation" => Color::rgb(0.55, 0.45, 0.35),
            "logic" => Color::rgb(0.45, 0.35, 0.50),
            _ => self.palette.node_header,
        }
    }

    /// The rectangle a panel's contents get, inside its padding.
    pub fn inner(&self, rect: Rect) -> Rect {
        rect.shrink(self.metrics.padding)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wxsl_core::node::ValueType;

    #[test]
    fn scaling_multiplies_every_length() {
        let theme = Theme::new(2.0);
        assert_eq!(theme.metrics.text_size, Metrics::DEFAULT.text_size * 2.0);
        assert_eq!(theme.metrics.node_width, Metrics::DEFAULT.node_width * 2.0);
        assert_eq!(theme.scale, 2.0);
    }

    #[test]
    fn a_degenerate_scale_cannot_collapse_the_interface() {
        let theme = Theme::new(0.0);
        assert!(theme.scale > 0.0);
        assert!(theme.metrics.row_height > 0.0);
    }

    #[test]
    fn every_value_type_has_its_own_port_colour() {
        // Sockets are matched by exact type and there is no implicit
        // conversion, so two types sharing a colour would suggest a
        // connection the graph will refuse.
        let theme = Theme::default();
        let colors: Vec<[f32; 4]> = ValueType::ALL
            .iter()
            .map(|ty| theme.type_color(*ty).to_array())
            .collect();
        for (index, color) in colors.iter().enumerate() {
            for (other_index, other) in colors.iter().enumerate() {
                if index != other_index {
                    assert_ne!(
                        color,
                        other,
                        "{:?} and {:?} share a colour",
                        ValueType::ALL[index],
                        ValueType::ALL[other_index]
                    );
                }
            }
        }
    }

    #[test]
    fn an_unknown_category_falls_back_rather_than_panicking() {
        let theme = Theme::default();
        assert_eq!(
            theme.category_color("something new"),
            theme.palette.node_header
        );
    }
}

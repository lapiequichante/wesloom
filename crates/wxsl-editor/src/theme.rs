//! Colours and metrics, in one place.
//!
//! Everything the editor draws takes its colour and its size from here, so
//! that a change of palette is one edit rather than a hunt. A new [`Theme`]
//! starts dark ([`Palette::DARK`]), because a shader editor's subject is a
//! lit 3D preview and a bright interface next to it ruins the only thing on
//! screen whose brightness matters — but [`Palette::LIGHT`] is there too,
//! and [`Theme::toggle_mode`] (the toolbar's "theme" button) switches
//! between them without touching anything else about the theme.
//!
//! Colours are in the target's colour space, not linear — see
//! [`wxsl_render::ui::Color`]. The UI pass converts nothing, which is what
//! makes a hex value from a palette land on screen as that colour.

use wxsl_core::graph::Node;
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
    /// A keyword, in the WXSL/WGSL code panels (`fn`, `let`, `return`, …).
    pub syntax_keyword: Color,
    /// A built-in type name, in the code panels (`f32`, `vec3f`, `array`, …).
    pub syntax_type: Color,
    /// A numeric literal, in the code panels.
    pub syntax_number: Color,
    /// An attribute, in the code panels (`@fragment`, `@group(0)`, …).
    pub syntax_attribute: Color,
    /// A comment, in the code panels.
    pub syntax_comment: Color,
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
        syntax_keyword: Color::rgb(0.55, 0.62, 0.98),
        syntax_type: Color::rgb(0.45, 0.80, 0.75),
        syntax_number: Color::rgb(0.85, 0.70, 0.45),
        syntax_attribute: Color::rgb(0.90, 0.75, 0.45),
        syntax_comment: Color::rgb(0.48, 0.52, 0.58),
    };

    /// The light palette.
    ///
    /// Not a naive channel inversion of [`Palette::DARK`]: a colour picked
    /// to read clearly on a near-black background (a pale syntax blue, a
    /// light slate link) can all but vanish on a near-white one, so every
    /// colour that carries meaning — text, borders, links, syntax colours,
    /// `error`/`warning` — is deepened for the same contrast on the other
    /// background, rather than mechanically flipped.
    pub const LIGHT: Palette = Palette {
        background: Color::rgb(0.95, 0.955, 0.965),
        canvas: Color::rgb(0.90, 0.905, 0.915),
        grid: Color::rgb(0.855, 0.86, 0.875),
        grid_major: Color::rgb(0.78, 0.785, 0.80),
        panel: Color::rgb(0.985, 0.985, 0.99),
        panel_header: Color::rgb(0.90, 0.905, 0.92),
        outline: Color::rgb(0.78, 0.78, 0.80),
        node: Color::rgb(0.97, 0.97, 0.98),
        node_header: Color::rgb(0.85, 0.855, 0.88),
        selection: Color::rgb(0.85, 0.55, 0.08),
        text: Color::rgb(0.12, 0.13, 0.16),
        text_dim: Color::rgb(0.42, 0.44, 0.50),
        text_on_accent: Color::rgb(0.06, 0.07, 0.09),
        accent: Color::rgb(0.15, 0.47, 0.90),
        control: Color::rgb(0.87, 0.87, 0.90),
        control_hover: Color::rgb(0.79, 0.80, 0.84),
        control_active: Color::rgb(0.70, 0.72, 0.78),
        error: Color::rgb(0.78, 0.16, 0.16),
        warning: Color::rgb(0.72, 0.48, 0.04),
        link: Color::rgb(0.40, 0.44, 0.52),
        link_pending: Color::rgb(0.75, 0.55, 0.10),
        link_hover: Color::rgb(0.80, 0.32, 0.22),
        syntax_keyword: Color::rgb(0.18, 0.28, 0.78),
        syntax_type: Color::rgb(0.03, 0.45, 0.42),
        syntax_number: Color::rgb(0.62, 0.40, 0.06),
        syntax_attribute: Color::rgb(0.58, 0.42, 0.05),
        syntax_comment: Color::rgb(0.45, 0.48, 0.53),
    };
}

/// Which of the two built-in palettes a [`Theme`] is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeMode {
    /// [`Palette::DARK`].
    Dark,
    /// [`Palette::LIGHT`].
    Light,
}

impl ThemeMode {
    /// This mode's palette.
    pub fn palette(self) -> Palette {
        match self {
            ThemeMode::Dark => Palette::DARK,
            ThemeMode::Light => Palette::LIGHT,
        }
    }

    /// The other mode, for a toggle button.
    pub fn toggled(self) -> Self {
        match self {
            ThemeMode::Dark => ThemeMode::Light,
            ThemeMode::Light => ThemeMode::Dark,
        }
    }

    /// A short label for a toggle button: what this mode *is*, not what it
    /// switches to.
    pub fn name(self) -> &'static str {
        match self {
            ThemeMode::Dark => "dark",
            ThemeMode::Light => "light",
        }
    }
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
    /// Colours. Kept in sync with `mode` — always `mode.palette()` — so
    /// every place that reads `theme.palette` (nearly everywhere the editor
    /// draws) does not also have to know about `mode`.
    pub palette: Palette,
    /// Which palette `palette` currently is.
    pub mode: ThemeMode,
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
    /// The default theme (dark) at a display scale.
    pub fn new(scale: f32) -> Self {
        Theme {
            palette: ThemeMode::Dark.palette(),
            mode: ThemeMode::Dark,
            metrics: Metrics::DEFAULT.scaled(scale),
            scale: scale.max(0.1),
        }
    }

    /// Re-scale for a display change, keeping the palette.
    pub fn set_scale(&mut self, scale: f32) {
        self.metrics = Metrics::DEFAULT.scaled(scale);
        self.scale = scale.max(0.1);
    }

    /// Switch to the other of the two built-in palettes.
    pub fn toggle_mode(&mut self) {
        self.mode = self.mode.toggled();
        self.palette = self.mode.palette();
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
            // The resource types, kept together in one hue: a texture
            // edge is a different *kind* of thing from a value edge — it
            // carries a binding, not a number — and reading that at a
            // glance matters more than telling 2D from cube, which the
            // socket's label says anyway.
            ValueType::Texture2d => Color::rgb(0.45, 0.90, 0.70),
            ValueType::TextureCube => Color::rgb(0.35, 0.75, 0.60),
            ValueType::Sampler => Color::rgb(0.60, 0.95, 0.80),
            // The pipeline resources, in the same "not a number" spirit
            // but a distinct hue, so a pipeline document's edges read as
            // resources that are theirs: no shader node ever takes one,
            // and the colour should say so before the canvas is read.
            ValueType::DrawQueue => Color::rgb(0.85, 0.85, 0.60),
            ValueType::ShadowMaps => Color::rgb(0.75, 0.85, 0.45),
            ValueType::GBuffer => Color::rgb(0.90, 0.75, 0.55),
            ValueType::ColorTarget => Color::rgb(0.95, 0.65, 0.50),
            ValueType::DepthTarget => Color::rgb(0.60, 0.70, 0.95),
        }
    }

    /// The colour a node's title strip is drawn in, unless the node itself
    /// says otherwise ([`wxsl_core::graph::Node::color`]).
    ///
    /// One colour for every kind of node, deliberately. Colouring by
    /// category sounds informative and is not: the category is already
    /// written in the inspector and readable off the id, and spending the
    /// canvas's strongest visual channel on it leaves nothing to say the
    /// thing a reader actually wants marked — which part of this graph is
    /// the roughness, which part is the emissive. That is per *node* and
    /// only the author knows it, so the default is uniform and the colour
    /// is theirs to set.
    pub fn node_color(&self, node: &Node) -> Color {
        match node.color {
            Some([r, g, b]) => Color::rgb(r, g, b),
            None => self.palette.node_header,
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
        let types: Vec<ValueType> = ValueType::ALL
            .iter()
            .chain(ValueType::RESOURCES)
            .chain(ValueType::PIPELINE_RESOURCES)
            .copied()
            .collect();
        let colors: Vec<[f32; 4]> = types
            .iter()
            .map(|ty| theme.type_color(*ty).to_array())
            .collect();
        for (index, color) in colors.iter().enumerate() {
            for (other_index, other) in colors.iter().enumerate() {
                if index != other_index {
                    assert_ne!(
                        color, other,
                        "{:?} and {:?} share a colour",
                        types[index], types[other_index]
                    );
                }
            }
        }
    }

    #[test]
    fn a_node_is_the_default_colour_until_it_says_otherwise() {
        let theme = Theme::default();
        assert_eq!(
            theme.node_color(&Node::new("math.add")),
            theme.palette.node_header,
            "every kind of node starts the same colour"
        );
        assert_eq!(
            theme.node_color(&Node::new("math.add").with_color([0.2, 0.4, 0.6])),
            Color::rgb(0.2, 0.4, 0.6)
        );
    }

    #[test]
    fn a_new_theme_starts_dark_with_a_matching_palette() {
        let theme = Theme::default();
        assert_eq!(theme.mode, ThemeMode::Dark);
        assert_eq!(theme.palette, Palette::DARK);
    }

    #[test]
    fn toggling_switches_both_the_mode_and_the_palette() {
        let mut theme = Theme::default();
        theme.toggle_mode();
        assert_eq!(theme.mode, ThemeMode::Light);
        assert_eq!(theme.palette, Palette::LIGHT);
        theme.toggle_mode();
        assert_eq!(theme.mode, ThemeMode::Dark);
        assert_eq!(theme.palette, Palette::DARK);
    }

    #[test]
    fn toggling_keeps_the_metrics_and_scale() {
        // A theme change should not also relayout the interface.
        let mut theme = Theme::new(1.5);
        theme.toggle_mode();
        assert_eq!(theme.scale, 1.5);
        assert_eq!(theme.metrics, Metrics::DEFAULT.scaled(1.5));
    }

    #[test]
    fn the_light_palette_is_actually_light_and_the_dark_one_actually_dark() {
        fn luminance(color: Color) -> f32 {
            (color.r + color.g + color.b) / 3.0
        }
        assert!(luminance(Palette::LIGHT.background) > luminance(Palette::LIGHT.text));
        assert!(luminance(Palette::DARK.text) > luminance(Palette::DARK.background));
        // And the two backgrounds land on opposite sides of the two texts,
        // rather than both palettes drifting to the same mid-grey.
        assert!(luminance(Palette::LIGHT.background) > luminance(Palette::DARK.background));
        assert!(luminance(Palette::LIGHT.text) < luminance(Palette::DARK.text));
    }
}

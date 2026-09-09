//! The draw list: geometry for one frame of interface.
//!
//! A [`DrawList`] is an instance buffer and a list of [`Batch`]es, built up
//! by pushing primitives and consumed by [`crate::ui::UiRenderer`]. Nothing
//! here touches `wgpu`: a draw list is plain data, which is what makes an
//! interface's geometry testable — and it is why the editor's layout code can
//! be exercised without a device.
//!
//! **Every primitive is one instance.** The quad's corners come from
//! `@builtin(vertex_index)` in the shader, so a glyph costs one 68-byte
//! [`UiInstance`] rather than four vertices and six indices, and there is no
//! vertex or index buffer at all — a panel of text is one
//! `draw(0..6, 0..glyphs)`.
//!
//! The fragment stage decides what to do with an instance from its `kind`
//! (`wxsl_core::abi::UI_KINDS`) and from `shape`, which carries a corner
//! radius and a border thickness. That one primitive covers more than it
//! sounds like:
//!
//! | Wanted | How |
//! |---|---|
//! | Rectangle | radius 0 |
//! | Rounded panel | radius > 0 |
//! | Circle (a port, a handle) | radius = half extent |
//! | Outline | border > 0, stroked inwards |
//! | Line, link, hairline | a capsule: `axis` rotates the local frame onto the segment |
//! | Image, material preview | [`UiKind::Texture`] |
//! | Text | [`UiKind::Text`], one instance per glyph |
//!
//! Batches break on a change of texture or clip rectangle, and only then, so
//! a panel full of shapes and text is one draw call.

use std::ops::Range;

use bytemuck::{Pod, Zeroable};
use glam::Vec2;
use wxsl_core::abi;

use crate::ui::atlas::AtlasRegion;
use crate::ui::text::TextLayout;

/// An axis-aligned rectangle in UI pixels, y down from the top left.
///
/// The default is [`Rect::NOTHING`]: empty, at the origin, so a widget that
/// has not been laid out yet draws nothing rather than covering the screen.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    /// Top-left corner.
    pub min: Vec2,
    /// Bottom-right corner.
    pub max: Vec2,
}

impl Rect {
    /// A rectangle covering everything, for an unclipped batch.
    pub const EVERYTHING: Rect = Rect {
        min: Vec2::new(f32::NEG_INFINITY, f32::NEG_INFINITY),
        max: Vec2::new(f32::INFINITY, f32::INFINITY),
    };

    /// An empty rectangle at the origin.
    pub const NOTHING: Rect = Rect {
        min: Vec2::ZERO,
        max: Vec2::ZERO,
    };

    /// From two corners, in either order.
    pub fn from_min_max(min: Vec2, max: Vec2) -> Self {
        Rect {
            min: min.min(max),
            max: min.max(max),
        }
    }

    /// From a top-left corner and a size.
    pub fn from_min_size(min: Vec2, size: Vec2) -> Self {
        Rect {
            min,
            max: min + size.max(Vec2::ZERO),
        }
    }

    /// From a centre and a full size.
    pub fn from_center_size(center: Vec2, size: Vec2) -> Self {
        let half = size.max(Vec2::ZERO) * 0.5;
        Rect {
            min: center - half,
            max: center + half,
        }
    }

    /// From an origin, a width and a height.
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Rect::from_min_size(Vec2::new(x, y), Vec2::new(width, height))
    }

    /// Width in pixels.
    pub fn width(&self) -> f32 {
        self.max.x - self.min.x
    }

    /// Height in pixels.
    pub fn height(&self) -> f32 {
        self.max.y - self.min.y
    }

    /// Size in pixels.
    pub fn size(&self) -> Vec2 {
        self.max - self.min
    }

    /// The centre point.
    pub fn center(&self) -> Vec2 {
        (self.min + self.max) * 0.5
    }

    /// Whether the rectangle has no area.
    pub fn is_empty(&self) -> bool {
        self.max.x <= self.min.x || self.max.y <= self.min.y
    }

    /// Whether `point` is inside, min-inclusive and max-exclusive so that
    /// two abutting rectangles cannot both claim a click.
    pub fn contains(&self, point: Vec2) -> bool {
        point.x >= self.min.x
            && point.x < self.max.x
            && point.y >= self.min.y
            && point.y < self.max.y
    }

    /// The overlap of two rectangles, possibly empty.
    pub fn intersect(&self, other: Rect) -> Rect {
        Rect {
            min: self.min.max(other.min),
            max: self.max.min(other.max),
        }
    }

    /// Whether two rectangles overlap at all.
    pub fn intersects(&self, other: Rect) -> bool {
        !self.intersect(other).is_empty()
    }

    /// Grown by `amount` on every side.
    pub fn expand(&self, amount: f32) -> Rect {
        Rect {
            min: self.min - Vec2::splat(amount),
            max: self.max + Vec2::splat(amount),
        }
    }

    /// Shrunk by `amount` on every side, never past empty.
    pub fn shrink(&self, amount: f32) -> Rect {
        let min = self.min + Vec2::splat(amount);
        Rect {
            min,
            max: (self.max - Vec2::splat(amount)).max(min),
        }
    }

    /// Moved by `offset`.
    pub fn translate(&self, offset: Vec2) -> Rect {
        Rect {
            min: self.min + offset,
            max: self.max + offset,
        }
    }

    /// The leftmost `width` pixels, and the rest.
    pub fn split_left(&self, width: f32) -> (Rect, Rect) {
        let x = (self.min.x + width).clamp(self.min.x, self.max.x);
        (
            Rect::from_min_max(self.min, Vec2::new(x, self.max.y)),
            Rect::from_min_max(Vec2::new(x, self.min.y), self.max),
        )
    }

    /// The rightmost `width` pixels, and the rest.
    pub fn split_right(&self, width: f32) -> (Rect, Rect) {
        let x = (self.max.x - width).clamp(self.min.x, self.max.x);
        (
            Rect::from_min_max(Vec2::new(x, self.min.y), self.max),
            Rect::from_min_max(self.min, Vec2::new(x, self.max.y)),
        )
    }

    /// The topmost `height` pixels, and the rest.
    pub fn split_top(&self, height: f32) -> (Rect, Rect) {
        let y = (self.min.y + height).clamp(self.min.y, self.max.y);
        (
            Rect::from_min_max(self.min, Vec2::new(self.max.x, y)),
            Rect::from_min_max(Vec2::new(self.min.x, y), self.max),
        )
    }

    /// The bottommost `height` pixels, and the rest.
    pub fn split_bottom(&self, height: f32) -> (Rect, Rect) {
        let y = (self.max.y - height).clamp(self.min.y, self.max.y);
        (
            Rect::from_min_max(Vec2::new(self.min.x, y), self.max),
            Rect::from_min_max(self.min, Vec2::new(self.max.x, y)),
        )
    }
}

/// A straight (non-premultiplied) RGBA colour, in the target's colour space.
///
/// The UI pass converts nothing: what is written here is what lands in the
/// framebuffer. This renderer's targets are deliberately non-`*Srgb` (the
/// shading ABI encodes sRGB itself, so that both render paths agree), so a
/// UI colour should be written the way a designer would write it, and it will
/// look that way.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    /// Red, `0..=1`.
    pub r: f32,
    /// Green, `0..=1`.
    pub g: f32,
    /// Blue, `0..=1`.
    pub b: f32,
    /// Alpha, `0..=1`.
    pub a: f32,
}

impl Color {
    /// Fully transparent.
    pub const TRANSPARENT: Color = Color::rgba(0.0, 0.0, 0.0, 0.0);
    /// Opaque white.
    pub const WHITE: Color = Color::rgb(1.0, 1.0, 1.0);
    /// Opaque black.
    pub const BLACK: Color = Color::rgb(0.0, 0.0, 0.0);

    /// An opaque colour.
    pub const fn rgb(r: f32, g: f32, b: f32) -> Self {
        Color { r, g, b, a: 1.0 }
    }

    /// A colour with alpha.
    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Color { r, g, b, a }
    }

    /// An opaque grey.
    pub const fn gray(value: f32) -> Self {
        Color::rgb(value, value, value)
    }

    /// From `0xRRGGBB`, the spelling a palette is written in.
    pub fn hex(value: u32) -> Self {
        Color::rgb(
            ((value >> 16) & 0xff) as f32 / 255.0,
            ((value >> 8) & 0xff) as f32 / 255.0,
            (value & 0xff) as f32 / 255.0,
        )
    }

    /// From `0xRRGGBBAA`.
    pub fn hexa(value: u32) -> Self {
        Color::rgba(
            ((value >> 24) & 0xff) as f32 / 255.0,
            ((value >> 16) & 0xff) as f32 / 255.0,
            ((value >> 8) & 0xff) as f32 / 255.0,
            (value & 0xff) as f32 / 255.0,
        )
    }

    /// The same colour at a different opacity.
    pub fn with_alpha(self, alpha: f32) -> Self {
        Color { a: alpha, ..self }
    }

    /// Scaled towards black or up towards white, alpha untouched. Handy for a
    /// hover or pressed state derived from one base colour.
    pub fn scaled(self, factor: f32) -> Self {
        Color {
            r: (self.r * factor).clamp(0.0, 1.0),
            g: (self.g * factor).clamp(0.0, 1.0),
            b: (self.b * factor).clamp(0.0, 1.0),
            a: self.a,
        }
    }

    /// Linear interpolation, alpha included.
    pub fn lerp(self, other: Color, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        Color {
            r: self.r + (other.r - self.r) * t,
            g: self.g + (other.g - self.g) * t,
            b: self.b + (other.b - self.b) * t,
            a: self.a + (other.a - self.a) * t,
        }
    }

    /// As the array an instance carries.
    pub fn to_array(self) -> [f32; 4] {
        [self.r, self.g, self.b, self.a]
    }
}

/// Which of the shader's primitive kinds an instance is.
///
/// Mirrors `wxsl_core::abi::UI_KINDS`; a test asserts the values line up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiKind {
    /// A signed-distance rounded box, filled or stroked.
    Shape,
    /// A textured quad, tinted and masked by the same rounded box.
    Texture,
    /// An MSDF glyph.
    Text,
}

impl UiKind {
    /// The value the instance carries.
    pub fn value(self) -> u32 {
        match self {
            UiKind::Shape => abi::UI_KIND_SHAPE,
            UiKind::Texture => abi::UI_KIND_TEXTURE,
            UiKind::Text => abi::UI_KIND_TEXT,
        }
    }
}

/// One UI primitive.
///
/// Host-shared with `abi::UI_ATTRIBUTES` and `shaders/wxsl/ui.wxsl`'s
/// `UiInstanceIn`: three views of one layout, edited together (ADR 0013).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct UiInstance {
    /// Centre in physical pixels.
    pub center: [f32; 2],
    /// Half the size, in the primitive's own frame.
    pub half_extent: [f32; 2],
    /// Unit vector the local x axis points along on screen.
    pub axis: [f32; 2],
    /// Corner radius (a glyph's screen-pixel range), and border thickness.
    pub shape: [f32; 2],
    /// Texture coordinate of the top-left corner.
    pub uv_min: [f32; 2],
    /// Texture coordinate of the bottom-right corner.
    pub uv_max: [f32; 2],
    /// Straight RGBA tint.
    pub color: [f32; 4],
    /// Primitive kind, flat-interpolated.
    pub kind: u32,
}

impl UiInstance {
    /// Per-instance attributes, matching `UiInstanceIn`'s locations.
    const ATTRIBUTES: [wgpu::VertexAttribute; 8] = wgpu::vertex_attr_array![
        0 => Float32x2,
        1 => Float32x2,
        2 => Float32x2,
        3 => Float32x2,
        4 => Float32x2,
        5 => Float32x2,
        6 => Float32x4,
        7 => Uint32,
    ];

    /// The buffer layout to hand to the UI pipeline.
    ///
    /// `Instance` step mode: the pipeline has no per-vertex buffer at all,
    /// because the quad's corners come from the vertex index.
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: core::mem::size_of::<UiInstance>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &Self::ATTRIBUTES,
    };
}

/// A texture a batch samples: the atlas, or something registered with the
/// renderer such as an offscreen material preview.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TextureId(pub u32);

impl TextureId {
    /// The glyph and image atlas, which every renderer registers first.
    pub const ATLAS: TextureId = TextureId(0);
}

/// One run of instances sharing a texture and a clip rectangle.
#[derive(Clone, Debug, PartialEq)]
pub struct Batch {
    /// The texture to bind.
    pub texture: TextureId,
    /// The scissor rectangle, in physical pixels.
    pub clip: Rect,
    /// Which instances to draw.
    pub instances: Range<u32>,
}

/// Geometry for one frame of interface.
#[derive(Clone, Debug, Default)]
pub struct DrawList {
    instances: Vec<UiInstance>,
    batches: Vec<Batch>,
    clips: Vec<Rect>,
    texture: TextureId,
}

impl DrawList {
    /// An empty draw list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop everything, keeping the allocations for the next frame.
    pub fn clear(&mut self) {
        self.instances.clear();
        self.batches.clear();
        self.clips.clear();
        self.texture = TextureId::ATLAS;
    }

    /// The instances to upload.
    pub fn instances(&self) -> &[UiInstance] {
        &self.instances
    }

    /// The batches to record, in order.
    pub fn batches(&self) -> &[Batch] {
        &self.batches
    }

    /// Whether there is nothing to draw.
    pub fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }

    /// How many primitives are queued — the number worth putting on a debug
    /// overlay, since it is what a frame costs.
    pub fn len(&self) -> usize {
        self.instances.len()
    }

    /// The clip rectangle in force.
    pub fn clip(&self) -> Rect {
        self.clips.last().copied().unwrap_or(Rect::EVERYTHING)
    }

    /// Clip to `rect`, intersected with whatever is already in force.
    ///
    /// Nesting is the useful behaviour: a scrolled list inside a panel must
    /// not be able to draw outside the panel by pushing a bigger rectangle.
    pub fn push_clip(&mut self, rect: Rect) {
        let clipped = rect.intersect(self.clip());
        self.clips.push(clipped);
    }

    /// Undo the last [`Self::push_clip`].
    pub fn pop_clip(&mut self) {
        self.clips.pop();
    }

    /// A filled rectangle.
    pub fn rect(&mut self, rect: Rect, color: Color) {
        self.round_rect(rect, 0.0, color);
    }

    /// A filled rectangle with rounded corners.
    pub fn round_rect(&mut self, rect: Rect, radius: f32, color: Color) {
        self.shape(rect, radius, 0.0, color);
    }

    /// A rectangle's outline, stroked inwards so it stays inside `rect`.
    pub fn round_rect_border(&mut self, rect: Rect, radius: f32, thickness: f32, color: Color) {
        self.shape(rect, radius, thickness.max(0.01), color);
    }

    /// A filled circle.
    pub fn circle(&mut self, center: Vec2, radius: f32, color: Color) {
        self.shape(
            Rect::from_center_size(center, Vec2::splat(radius * 2.0)),
            radius,
            0.0,
            color,
        );
    }

    /// A circle's outline.
    pub fn circle_border(&mut self, center: Vec2, radius: f32, thickness: f32, color: Color) {
        self.shape(
            Rect::from_center_size(center, Vec2::splat(radius * 2.0)),
            radius,
            thickness.max(0.01),
            color,
        );
    }

    /// A rounded box, filled when `border` is zero and stroked otherwise.
    ///
    /// The primitive everything above funnels into.
    pub fn shape(&mut self, rect: Rect, radius: f32, border: f32, color: Color) {
        if rect.is_empty() || color.a <= 0.0 {
            return;
        }
        let half = rect.size() * 0.5;
        let texture = self.texture;
        self.push(
            texture,
            UiInstance {
                center: rect.center().to_array(),
                half_extent: half.to_array(),
                axis: [1.0, 0.0],
                shape: [radius.min(half.x.min(half.y)), border],
                uv_min: [0.0, 0.0],
                uv_max: [0.0, 0.0],
                color: color.to_array(),
                kind: UiKind::Shape.value(),
            },
        );
    }

    /// A line as a capsule: the local frame rotated onto the segment.
    ///
    /// This is why the shader evaluates its distance in a local frame rather
    /// than in pixels — a rotated capsule is an axis-aligned rounded box in
    /// its own coordinates, so links and hairlines need no extra primitive.
    pub fn line(&mut self, from: Vec2, to: Vec2, thickness: f32, color: Color) {
        if color.a <= 0.0 {
            return;
        }
        let half_thickness = (thickness * 0.5).max(0.01);
        let along = to - from;
        let length = along.length();
        if length <= f32::EPSILON {
            self.circle(from, half_thickness, color);
            return;
        }
        let texture = self.texture;
        self.push(
            texture,
            UiInstance {
                center: ((from + to) * 0.5).to_array(),
                // Extended by the cap radius, so the round ends are inside
                // the quad rather than clipped by it.
                half_extent: [length * 0.5 + half_thickness, half_thickness],
                axis: (along / length).to_array(),
                shape: [half_thickness, 0.0],
                uv_min: [0.0, 0.0],
                uv_max: [0.0, 0.0],
                color: color.to_array(),
                kind: UiKind::Shape.value(),
            },
        );
    }

    /// A connected run of lines.
    pub fn polyline(&mut self, points: &[Vec2], thickness: f32, color: Color) {
        for pair in points.windows(2) {
            self.line(pair[0], pair[1], thickness, color);
        }
    }

    /// A cubic Bézier, flattened into `segments` capsules.
    ///
    /// What a link between two node ports is drawn with. Flattening on the
    /// CPU keeps the fragment stage the same three-branch function for
    /// everything on screen, and a link is only a dozen instances.
    #[allow(clippy::too_many_arguments)]
    pub fn bezier(
        &mut self,
        from: Vec2,
        control_from: Vec2,
        control_to: Vec2,
        to: Vec2,
        thickness: f32,
        color: Color,
        segments: usize,
    ) {
        let segments = segments.max(1);
        let mut previous = from;
        for step in 1..=segments {
            let t = step as f32 / segments as f32;
            let u = 1.0 - t;
            let point = from * (u * u * u)
                + control_from * (3.0 * u * u * t)
                + control_to * (3.0 * u * t * t)
                + to * (t * t * t);
            self.line(previous, point, thickness, color);
            previous = point;
        }
    }

    /// A textured quad, tinted by `color`, with optional rounded corners.
    pub fn image(
        &mut self,
        rect: Rect,
        texture: TextureId,
        uv_min: Vec2,
        uv_max: Vec2,
        color: Color,
        radius: f32,
    ) {
        if rect.is_empty() {
            return;
        }
        let half = rect.size() * 0.5;
        self.push(
            texture,
            UiInstance {
                center: rect.center().to_array(),
                half_extent: half.to_array(),
                axis: [1.0, 0.0],
                shape: [radius.min(half.x.min(half.y)), 0.0],
                uv_min: uv_min.to_array(),
                uv_max: uv_max.to_array(),
                color: color.to_array(),
                kind: UiKind::Texture.value(),
            },
        );
    }

    /// An atlas entry, drawn at `rect`.
    pub fn atlas_image(&mut self, rect: Rect, region: AtlasRegion, color: Color, radius: f32) {
        self.image(
            rect,
            TextureId::ATLAS,
            region.uv_min,
            region.uv_max,
            color,
            radius,
        );
    }

    /// A laid-out string, with its origin at `origin`. One instance per
    /// glyph.
    ///
    /// Glyphs whose quad falls entirely outside the clip rectangle are
    /// dropped here rather than by the GPU — a code panel is thousands of
    /// lines and only tens are on screen, so this is the difference between
    /// scrolling and not.
    pub fn text(&mut self, layout: &TextLayout, origin: Vec2, color: Color) {
        if color.a <= 0.0 {
            return;
        }
        let clip = self.clip();
        for glyph in &layout.glyphs {
            let Some(region) = glyph.region else {
                continue;
            };
            let rect = glyph.rect.translate(origin);
            if !rect.intersects(clip) {
                continue;
            }
            self.push(
                TextureId::ATLAS,
                UiInstance {
                    center: rect.center().to_array(),
                    half_extent: (rect.size() * 0.5).to_array(),
                    axis: [1.0, 0.0],
                    // A glyph's radius slot carries the field's range in
                    // screen pixels instead; its kind skips the rounded-box
                    // mask entirely.
                    shape: [glyph.screen_px_range, 0.0],
                    uv_min: region.uv_min.to_array(),
                    uv_max: region.uv_max.to_array(),
                    color: color.to_array(),
                    kind: UiKind::Text.value(),
                },
            );
        }
    }

    /// Append one instance, extending or opening a batch.
    fn push(&mut self, texture: TextureId, instance: UiInstance) {
        let clip = self.clip();
        if clip.is_empty() {
            return;
        }
        let index = self.instances.len() as u32;
        self.instances.push(instance);
        match self.batches.last_mut() {
            // The common case: same texture, same clip, so this instance
            // joins the batch already open and costs no state change.
            Some(batch) if batch.texture == texture && batch.clip == clip => {
                batch.instances.end = index + 1;
            }
            _ => self.batches.push(Batch {
                texture,
                clip,
                instances: index..index + 1,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::text::{place_run, Glyph, GlyphCache, ShapedChar, TextLayout};

    #[test]
    fn the_instance_layout_matches_the_abi_table() {
        assert_eq!(UiInstance::ATTRIBUTES.len(), abi::UI_ATTRIBUTES.len());
        // Locations are dense and in table order, which is what lets the
        // shader's `@location(n)` line up with the table's index.
        for (index, attribute) in UiInstance::ATTRIBUTES.iter().enumerate() {
            assert_eq!(attribute.shader_location, index as u32);
        }
        // Offsets follow the `#[repr(C)]` field order.
        let expected = [0, 8, 16, 24, 32, 40, 48, 64];
        for (attribute, offset) in UiInstance::ATTRIBUTES.iter().zip(expected) {
            assert_eq!(attribute.offset, offset);
        }
        assert_eq!(core::mem::size_of::<UiInstance>(), 68);
        // `wgpu` requires the stride to be a multiple of four bytes.
        assert_eq!(core::mem::size_of::<UiInstance>() % 4, 0);
        // And the pipeline steps it per instance, not per vertex.
        assert_eq!(UiInstance::LAYOUT.step_mode, wgpu::VertexStepMode::Instance);
    }

    #[test]
    fn the_kinds_agree_with_the_shader_constants() {
        assert_eq!(UiKind::Shape.value(), abi::UI_KIND_SHAPE);
        assert_eq!(UiKind::Texture.value(), abi::UI_KIND_TEXTURE);
        assert_eq!(UiKind::Text.value(), abi::UI_KIND_TEXT);
    }

    #[test]
    fn primitives_sharing_a_texture_and_clip_are_one_batch() {
        let mut list = DrawList::new();
        list.rect(Rect::new(0.0, 0.0, 10.0, 10.0), Color::WHITE);
        list.rect(Rect::new(20.0, 0.0, 10.0, 10.0), Color::WHITE);
        list.circle(Vec2::splat(5.0), 3.0, Color::BLACK);
        assert_eq!(list.batches().len(), 1);
        assert_eq!(list.len(), 3, "three primitives, three instances");
        assert_eq!(list.batches()[0].instances, 0..3);
    }

    #[test]
    fn a_new_clip_or_texture_opens_a_new_batch() {
        let mut list = DrawList::new();
        list.rect(Rect::new(0.0, 0.0, 10.0, 10.0), Color::WHITE);
        list.push_clip(Rect::new(0.0, 0.0, 5.0, 5.0));
        list.rect(Rect::new(0.0, 0.0, 10.0, 10.0), Color::WHITE);
        list.pop_clip();
        list.image(
            Rect::new(0.0, 0.0, 10.0, 10.0),
            TextureId(7),
            Vec2::ZERO,
            Vec2::ONE,
            Color::WHITE,
            0.0,
        );
        assert_eq!(list.batches().len(), 3);
        assert_eq!(list.batches()[1].clip, Rect::new(0.0, 0.0, 5.0, 5.0));
        assert_eq!(list.batches()[2].texture, TextureId(7));
        // The batches partition the instance buffer, in order and with no
        // gaps — a gap would draw something twice or not at all.
        let mut expected = 0;
        for batch in list.batches() {
            assert_eq!(batch.instances.start, expected);
            expected = batch.instances.end;
        }
        assert_eq!(expected as usize, list.len());
    }

    #[test]
    fn clips_nest_rather_than_replace() {
        let mut list = DrawList::new();
        list.push_clip(Rect::new(0.0, 0.0, 100.0, 100.0));
        // A child asking for more than its parent allows gets the overlap.
        list.push_clip(Rect::new(50.0, 50.0, 500.0, 500.0));
        assert_eq!(list.clip(), Rect::new(50.0, 50.0, 50.0, 50.0));
        list.pop_clip();
        assert_eq!(list.clip(), Rect::new(0.0, 0.0, 100.0, 100.0));
        list.pop_clip();
        assert_eq!(list.clip(), Rect::EVERYTHING);
    }

    #[test]
    fn nothing_is_emitted_for_invisible_primitives() {
        let mut list = DrawList::new();
        list.rect(Rect::NOTHING, Color::WHITE);
        list.rect(Rect::new(0.0, 0.0, 10.0, 10.0), Color::TRANSPARENT);
        assert!(list.is_empty());

        // Nor for anything drawn under an empty clip rectangle.
        list.push_clip(Rect::NOTHING);
        list.rect(Rect::new(0.0, 0.0, 10.0, 10.0), Color::WHITE);
        list.pop_clip();
        assert!(list.is_empty(), "an empty clip discards its contents");
    }

    #[test]
    fn a_capsule_is_a_rotated_box_in_its_own_frame() {
        let mut list = DrawList::new();
        list.line(Vec2::ZERO, Vec2::new(10.0, 10.0), 2.0, Color::WHITE);
        let instance = list.instances()[0];
        // The local frame runs along the segment...
        let axis = Vec2::from_array(instance.axis);
        assert!((axis.length() - 1.0).abs() < 1e-5);
        assert!((axis - Vec2::splat(0.5f32.sqrt())).length() < 1e-5);
        // ...and the half extent is the segment's length plus its caps.
        let half_length = (200f32).sqrt() * 0.5 + 1.0;
        assert!((instance.half_extent[0] - half_length).abs() < 1e-4);
        assert_eq!(instance.half_extent[1], 1.0);
        // The corner radius makes it a capsule rather than a rectangle.
        assert_eq!(instance.shape[0], 1.0);
        assert_eq!(instance.center, [5.0, 5.0]);
    }

    #[test]
    fn a_zero_length_line_is_a_dot_rather_than_nothing() {
        let mut list = DrawList::new();
        list.line(Vec2::splat(5.0), Vec2::splat(5.0), 4.0, Color::WHITE);
        assert_eq!(list.len(), 1);
        assert_eq!(
            list.instances()[0].shape[0],
            2.0,
            "radius is half the width"
        );
    }

    #[test]
    fn a_bezier_is_flattened_into_the_requested_number_of_capsules() {
        let mut list = DrawList::new();
        list.bezier(
            Vec2::ZERO,
            Vec2::new(10.0, 0.0),
            Vec2::new(10.0, 10.0),
            Vec2::new(20.0, 10.0),
            2.0,
            Color::WHITE,
            8,
        );
        assert_eq!(list.len(), 8);
        assert_eq!(list.batches().len(), 1, "one batch for the whole link");
    }

    #[test]
    fn a_glyph_is_one_instance_and_carries_its_field_range() {
        let region = AtlasRegion {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
            uv_min: Vec2::ZERO,
            uv_max: Vec2::splat(0.1),
        };
        let mut layout = TextLayout {
            line_height: 10.0,
            ascent: 8.0,
            ..Default::default()
        };
        let item = ShapedChar {
            byte: 0,
            character: 'x',
            glyph: Glyph {
                glyph: Some(1),
                advance: 0.5,
                region: Some(region),
                offset: Vec2::new(0.0, -0.5),
                size: Vec2::splat(0.5),
                range: GlyphCache::RANGE,
            },
            advance: 5.0,
            kern: 0.0,
        };
        place_run(
            &mut layout,
            std::slice::from_ref(&item),
            0,
            0,
            10.0,
            GlyphCache::EM_PIXELS,
        );

        let mut list = DrawList::new();
        list.text(&layout, Vec2::ZERO, Color::WHITE);
        assert_eq!(list.len(), 1, "one instance per glyph");
        let instance = list.instances()[0];
        assert_eq!(instance.kind, abi::UI_KIND_TEXT);
        assert_eq!(instance.uv_min, [0.0, 0.0]);
        assert_eq!(instance.uv_max, [0.1, 0.1]);
        let expected = GlyphCache::RANGE * 10.0 / GlyphCache::EM_PIXELS;
        assert!((instance.shape[0] - expected).abs() < 1e-6);
    }

    #[test]
    fn glyphs_outside_the_clip_rectangle_are_dropped_on_the_cpu() {
        // A code panel is thousands of lines; only the visible ones should
        // reach the instance buffer.
        let region = AtlasRegion {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
            uv_min: Vec2::ZERO,
            uv_max: Vec2::splat(0.1),
        };
        let mut layout = TextLayout {
            line_height: 10.0,
            ascent: 8.0,
            ..Default::default()
        };
        for row in 0..3 {
            let item = ShapedChar {
                byte: row,
                character: 'x',
                glyph: Glyph {
                    glyph: Some(1),
                    advance: 0.5,
                    region: Some(region),
                    offset: Vec2::new(0.0, -0.5),
                    size: Vec2::splat(0.5),
                    range: GlyphCache::RANGE,
                },
                advance: 5.0,
                kern: 0.0,
            };
            place_run(
                &mut layout,
                std::slice::from_ref(&item),
                row,
                row,
                10.0,
                GlyphCache::EM_PIXELS,
            );
        }
        assert_eq!(layout.lines.len(), 3);

        let mut list = DrawList::new();
        // A clip that only admits the first line.
        list.push_clip(Rect::new(0.0, 0.0, 100.0, 9.0));
        list.text(&layout, Vec2::ZERO, Color::WHITE);
        list.pop_clip();
        assert_eq!(list.len(), 1, "one glyph survived the clip");
    }

    #[test]
    fn rect_splitting_partitions_without_overlap_or_gaps() {
        let rect = Rect::new(10.0, 20.0, 100.0, 50.0);
        let (left, rest) = rect.split_left(30.0);
        assert_eq!(left.width(), 30.0);
        assert_eq!(rest.width(), 70.0);
        assert_eq!(left.max.x, rest.min.x);
        assert_eq!(left.height(), rect.height());

        let (top, below) = rect.split_top(20.0);
        assert_eq!(top.height(), 20.0);
        assert_eq!(top.max.y, below.min.y);

        // Asking for more than there is clamps instead of inverting.
        let (all, nothing) = rect.split_left(1000.0);
        assert_eq!(all.width(), rect.width());
        assert!(nothing.is_empty());
    }

    #[test]
    fn every_split_returns_the_strip_first_and_the_remainder_second() {
        // One convention for all four, because a caller reading them the
        // other way round lays its whole interface out in a 20px strip.
        let rect = Rect::new(0.0, 0.0, 100.0, 100.0);
        let (strip, rest) = rect.split_left(20.0);
        assert_eq!(strip.width(), 20.0);
        assert_eq!(rest.width(), 80.0);
        let (strip, rest) = rect.split_right(20.0);
        assert_eq!(strip.width(), 20.0);
        assert_eq!(strip.max.x, rect.max.x);
        assert_eq!(rest.width(), 80.0);
        let (strip, rest) = rect.split_top(20.0);
        assert_eq!(strip.height(), 20.0);
        assert_eq!(rest.height(), 80.0);
        let (strip, rest) = rect.split_bottom(20.0);
        assert_eq!(strip.height(), 20.0);
        assert_eq!(strip.max.y, rect.max.y);
        assert_eq!(rest.height(), 80.0);
        assert_eq!(rest.min.y, rect.min.y);
    }

    #[test]
    fn abutting_rectangles_cannot_both_claim_a_point() {
        let (left, right) = Rect::new(0.0, 0.0, 20.0, 10.0).split_left(10.0);
        let boundary = Vec2::new(10.0, 5.0);
        assert!(!left.contains(boundary));
        assert!(right.contains(boundary));
    }

    #[test]
    fn shrinking_a_rectangle_past_empty_stops_at_empty() {
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0).shrink(20.0);
        assert!(rect.is_empty());
        assert!(rect.width() >= 0.0 && rect.height() >= 0.0);
    }

    #[test]
    fn colors_come_out_of_hex_the_way_they_went_in() {
        let color = Color::hex(0x336699);
        assert!((color.r - 0.2).abs() < 0.01);
        assert!((color.g - 0.4).abs() < 0.01);
        assert!((color.b - 0.6).abs() < 0.01);
        assert_eq!(color.a, 1.0);
        assert_eq!(Color::hexa(0x00000080).a, 128.0 / 255.0);
        assert_eq!(Color::WHITE.scaled(0.5), Color::gray(0.5));
        assert_eq!(Color::BLACK.lerp(Color::WHITE, 0.5), Color::gray(0.5));
    }
}

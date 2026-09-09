//! Fonts: outlines in, metrics and distance fields out.
//!
//! This crate embeds no font and reads no file — the application hands the
//! bytes in, exactly as it hands in the shader library
//! ([ADR 0014](../../../../docs/adr/0014-msdf-text-with-an-own-generator-and-app-supplied-fonts.md),
//! ADR 0009). [`Font`] is then an immutable view of those bytes: metrics,
//! character lookup, advances, kerning, and glyph outlines as
//! [`crate::ui::msdf::Shape`]s.
//!
//! Everything a caller sees is in **em units**, i.e. the font's own units
//! divided by its units-per-em. That is the coordinate system a distance
//! field is generated in and the one a layout scales by the text size, so
//! keeping it out of the API's vocabulary would only mean every caller
//! dividing by the same number.
//!
//! # Why the face is parsed on demand
//!
//! `ttf_parser::Face` borrows the font's bytes, and a struct that owns both
//! is self-referential. Rather than reach for `unsafe` or a second crate,
//! [`Font`] keeps the bytes and parses a face when it needs one — which is
//! only when a glyph is first measured or rasterized, because
//! [`GlyphCache`](crate::ui::text::GlyphCache) caches everything a frame
//! actually reads. Parsing a face is a table walk, not a font load.

use std::sync::Arc;

use glam::Vec2;
use ttf_parser::{Face, GlyphId, OutlineBuilder};

use crate::error::RenderError;
use crate::ui::msdf::{self, ColoredEdge, Contour, MsdfRequest, MsdfTransform, Segment, Shape};

/// The vertical metrics of a font, in em units.
///
/// `ascent` is positive above the baseline and `descent` positive below it,
/// which is not how the font stores them (the descender is negative there) —
/// but it is how every layout calculation wants them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontMetrics {
    /// How far the tallest glyphs reach above the baseline.
    pub ascent: f32,
    /// How far the deepest glyphs reach below it.
    pub descent: f32,
    /// Extra space the font asks for between lines.
    pub line_gap: f32,
}

impl FontMetrics {
    /// The distance between two baselines the font recommends, in em units.
    pub fn line_height(&self) -> f32 {
        self.ascent + self.descent + self.line_gap
    }
}

/// A font: its bytes, and the metrics worth caching from them.
#[derive(Clone)]
pub struct Font {
    data: Arc<Vec<u8>>,
    index: u32,
    units_per_em: f32,
    metrics: FontMetrics,
    name: String,
    monospaced: bool,
}

impl Font {
    /// Parse `bytes` as a font, taking face `index` from a collection.
    ///
    /// The bytes are kept (shared, so cloning a `Font` is cheap): outlines
    /// are read from them on demand.
    pub fn from_bytes(bytes: Vec<u8>, index: u32) -> Result<Self, RenderError> {
        let data = Arc::new(bytes);
        let face = Face::parse(&data, index).map_err(|error| RenderError::InvalidFont {
            reason: error.to_string(),
        })?;
        let units_per_em = f32::from(face.units_per_em());
        if units_per_em <= 0.0 {
            return Err(RenderError::InvalidFont {
                reason: "the font declares no units per em".to_string(),
            });
        }
        let metrics = FontMetrics {
            ascent: f32::from(face.ascender()) / units_per_em,
            // Positive downwards; fonts store it as a negative number.
            descent: -f32::from(face.descender()) / units_per_em,
            line_gap: f32::from(face.line_gap()) / units_per_em,
        };
        let name = face
            .names()
            .into_iter()
            .find(|name| name.name_id == ttf_parser::name_id::FULL_NAME)
            .and_then(|name| name.to_string())
            .unwrap_or_else(|| "unnamed".to_string());
        let monospaced = face.is_monospaced();

        Ok(Font {
            data,
            index,
            units_per_em,
            metrics,
            name,
            monospaced,
        })
    }

    /// The font's full name, for a status line or an error message.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Vertical metrics, in em units.
    pub fn metrics(&self) -> FontMetrics {
        self.metrics
    }

    /// Whether the font declares itself monospaced.
    ///
    /// The editor uses it as a sanity check on the font it was given for the
    /// code panels: proportional code is legible but the columns will not
    /// line up, and it is better to say so than to leave the user wondering.
    pub fn is_monospaced(&self) -> bool {
        self.monospaced
    }

    /// The font's design grid, for callers converting to and from font units.
    pub fn units_per_em(&self) -> f32 {
        self.units_per_em
    }

    /// Run `body` with a parsed face.
    ///
    /// The bytes parsed once at construction, so this cannot fail; the
    /// `expect` documents that rather than pushing a `Result` onto every
    /// caller.
    fn with_face<T>(&self, body: impl FnOnce(&Face<'_>) -> T) -> T {
        let face = Face::parse(&self.data, self.index)
            .expect("the font parsed at construction, so it parses now");
        body(&face)
    }

    /// Look up several characters at once: glyph id and advance in em units.
    ///
    /// Batched because the face is parsed per call, so a caller filling a
    /// cache should ask for everything it is missing in one go.
    pub fn lookup(&self, characters: impl IntoIterator<Item = char>) -> Vec<CharMetrics> {
        self.with_face(|face| {
            characters
                .into_iter()
                .map(|character| {
                    let glyph = face.glyph_index(character);
                    let advance = glyph
                        .and_then(|glyph| face.glyph_hor_advance(glyph))
                        .map(|advance| f32::from(advance) / self.units_per_em)
                        .unwrap_or(0.0);
                    CharMetrics {
                        character,
                        glyph: glyph.map(|glyph| glyph.0),
                        advance,
                    }
                })
                .collect()
        })
    }

    /// The kerning adjustment for each `(left, right)` glyph pair, in em
    /// units.
    ///
    /// Batched for the same reason [`Self::lookup`] is: the face is parsed
    /// per call, and a caller filling a cache has a list of pairs, not one.
    /// Asking per pair inside a layout pass would parse the face once per
    /// character on screen per frame.
    ///
    /// Only the `kern` table, and only its horizontal, non-state-machine
    /// subtables: that covers the pair kerning in the fonts a UI is likely to
    /// be handed. `GPOS` kerning is not read — see ADR 0014 on shaping being
    /// out of scope.
    pub fn kerning_pairs(&self, pairs: &[(u16, u16)]) -> Vec<f32> {
        self.with_face(|face| {
            let Some(table) = face.tables().kern else {
                return vec![0.0; pairs.len()];
            };
            pairs
                .iter()
                .map(|(left, right)| {
                    let adjustment: i32 = table
                        .subtables
                        .into_iter()
                        .filter(|subtable| subtable.horizontal && !subtable.variable)
                        .filter_map(|subtable| {
                            subtable.glyphs_kerning(GlyphId(*left), GlyphId(*right))
                        })
                        .map(i32::from)
                        .sum();
                    adjustment as f32 / self.units_per_em
                })
                .collect()
        })
    }

    /// The kerning adjustment between one pair of glyphs, in em units.
    ///
    /// Parses the face, so use [`Self::kerning_pairs`] from anything that
    /// runs per frame.
    pub fn kerning(&self, left: u16, right: u16) -> f32 {
        self.kerning_pairs(&[(left, right)])[0]
    }

    /// The outline of `glyph`, in em units, oriented so that inside is
    /// positive.
    ///
    /// `None` when the glyph has no outline at all — a space, or a character
    /// the font draws with a bitmap or an SVG table.
    pub fn outline(&self, glyph: u16) -> Option<Shape> {
        self.with_face(|face| {
            let mut sink = OutlineSink::new(1.0 / self.units_per_em);
            face.outline_glyph(GlyphId(glyph), &mut sink)?;
            let mut shape = sink.finish();
            shape.clean();
            if shape.is_empty() {
                return None;
            }
            shape.normalize_orientation();
            Some(shape)
        })
    }

    /// Everything needed to generate `glyph`'s distance field, and to place
    /// it when drawing.
    ///
    /// `em_pixels` is the resolution the field is generated at and `range`
    /// its spread in those pixels; both are properties of the atlas, not of
    /// the text size it will be drawn at.
    pub fn glyph_field(&self, glyph: u16, em_pixels: f32, range: f32) -> Option<GlyphField> {
        let shape = self.outline(glyph)?;
        let (min, max) = shape.bounds()?;
        let edges = msdf::color_edges(&shape);
        if edges.is_empty() {
            return None;
        }

        // Pad by half the range, plus a pixel so that the outermost texel is
        // fully outside the field's reach and cannot clip.
        let padding = range * 0.5 + 1.0;
        let scale = em_pixels;
        let width = ((max.x - min.x) * scale + padding * 2.0).ceil().max(1.0);
        let height = ((max.y - min.y) * scale + padding * 2.0).ceil().max(1.0);
        // A bitmap point is `(shape + translate) * scale`, and the shape's
        // lower-left corner has to land `padding` pixels in.
        let translate = Vec2::new(padding / scale - min.x, padding / scale - min.y);

        Some(GlyphField {
            edges,
            width: width as u32,
            height: height as u32,
            transform: MsdfTransform { scale, translate },
            range,
            // In em units, y down from the pen position on the baseline: the
            // bitmap's top-left corner.
            offset: Vec2::new(min.x - padding / scale, -(max.y + padding / scale)),
            size: Vec2::new(width, height) / em_pixels,
        })
    }
}

impl core::fmt::Debug for Font {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Font")
            .field("name", &self.name)
            .field("bytes", &self.data.len())
            .field("units_per_em", &self.units_per_em)
            .finish()
    }
}

/// What a character resolves to in a font.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharMetrics {
    /// The character asked about.
    pub character: char,
    /// The glyph the font maps it to, or `None` if it has none.
    pub glyph: Option<u16>,
    /// How far the pen advances, in em units.
    pub advance: f32,
}

/// A glyph's distance field request, and where the result goes on screen.
#[derive(Clone, Debug)]
pub struct GlyphField {
    /// The coloured outline, ready for either MSDF backend.
    pub edges: Vec<ColoredEdge>,
    /// Bitmap width in pixels.
    pub width: u32,
    /// Bitmap height in pixels.
    pub height: u32,
    /// Where the outline sits in that bitmap.
    pub transform: MsdfTransform,
    /// The field's spread, in bitmap pixels.
    pub range: f32,
    /// Offset from the pen position on the baseline to the bitmap's top-left
    /// corner, in em units, y down.
    pub offset: Vec2,
    /// The bitmap's size in em units, so that multiplying by a text size
    /// gives the rectangle to draw.
    pub size: Vec2,
}

impl GlyphField {
    /// This field as a generation request.
    pub fn request(&self) -> MsdfRequest<'_> {
        MsdfRequest {
            edges: &self.edges,
            width: self.width,
            height: self.height,
            transform: self.transform,
            range: self.range,
        }
    }
}

/// Collects `ttf_parser`'s outline callbacks into a [`Shape`].
///
/// Scales font units to em units as it goes, and closes any contour the font
/// left open — an unclosed contour is common in the wild, and a distance
/// field needs a loop.
struct OutlineSink {
    scale: f32,
    shape: Shape,
    segments: Vec<Segment>,
    position: Vec2,
    start: Vec2,
}

impl OutlineSink {
    fn new(scale: f32) -> Self {
        OutlineSink {
            scale,
            shape: Shape::new(),
            segments: Vec::new(),
            position: Vec2::ZERO,
            start: Vec2::ZERO,
        }
    }

    fn point(&self, x: f32, y: f32) -> Vec2 {
        Vec2::new(x, y) * self.scale
    }

    /// Close the contour being built, if any, and start a new one.
    fn flush(&mut self) {
        if self.segments.is_empty() {
            return;
        }
        if self.position.distance_squared(self.start) > 1e-12 {
            self.segments
                .push(Segment::Line([self.position, self.start]));
        }
        self.shape.contours.push(Contour {
            segments: core::mem::take(&mut self.segments),
        });
    }

    fn finish(mut self) -> Shape {
        self.flush();
        self.shape
    }
}

impl OutlineBuilder for OutlineSink {
    fn move_to(&mut self, x: f32, y: f32) {
        self.flush();
        self.position = self.point(x, y);
        self.start = self.position;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let to = self.point(x, y);
        self.segments.push(Segment::Line([self.position, to]));
        self.position = to;
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let control = self.point(x1, y1);
        let to = self.point(x, y);
        self.segments
            .push(Segment::Quad([self.position, control, to]));
        self.position = to;
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let first = self.point(x1, y1);
        let second = self.point(x2, y2);
        let to = self.point(x, y);
        self.segments
            .push(Segment::Cubic([self.position, first, second, to]));
        self.position = to;
    }

    fn close(&mut self) {
        self.flush();
        self.position = self.start;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_outline_sink_closes_a_contour_the_font_left_open() {
        let mut sink = OutlineSink::new(1.0);
        sink.move_to(0.0, 0.0);
        sink.line_to(10.0, 0.0);
        sink.line_to(10.0, 10.0);
        // No `close()`, and the pen is not back at the start.
        let shape = sink.finish();
        assert_eq!(shape.contours.len(), 1);
        let segments = &shape.contours[0].segments;
        assert_eq!(segments.len(), 3, "a closing line was added");
        assert_eq!(segments[2].end(), Vec2::ZERO);
    }

    #[test]
    fn a_closed_contour_gains_no_extra_segment() {
        let mut sink = OutlineSink::new(1.0);
        sink.move_to(0.0, 0.0);
        sink.line_to(10.0, 0.0);
        sink.line_to(10.0, 10.0);
        sink.line_to(0.0, 0.0);
        sink.close();
        let shape = sink.finish();
        assert_eq!(shape.contours.len(), 1);
        assert_eq!(shape.contours[0].segments.len(), 3);
    }

    #[test]
    fn every_curve_kind_survives_the_sink_at_the_right_scale() {
        let mut sink = OutlineSink::new(0.5);
        sink.move_to(0.0, 0.0);
        sink.line_to(2.0, 0.0);
        sink.quad_to(4.0, 0.0, 4.0, 2.0);
        sink.curve_to(4.0, 4.0, 2.0, 4.0, 0.0, 4.0);
        sink.close();
        let shape = sink.finish();
        let segments = &shape.contours[0].segments;
        assert!(matches!(segments[0], Segment::Line(_)));
        assert!(matches!(segments[1], Segment::Quad(_)));
        assert!(matches!(segments[2], Segment::Cubic(_)));
        // Scaled: the line's end was (2, 0) in font units.
        assert_eq!(segments[0].end(), Vec2::new(1.0, 0.0));
        // And closed back to the origin.
        assert_eq!(segments.last().expect("segments").end(), Vec2::ZERO);
    }

    #[test]
    fn several_contours_become_several_loops() {
        let mut sink = OutlineSink::new(1.0);
        for offset in [0.0, 20.0] {
            sink.move_to(offset, 0.0);
            sink.line_to(offset + 10.0, 0.0);
            sink.line_to(offset + 10.0, 10.0);
            sink.close();
        }
        let shape = sink.finish();
        assert_eq!(shape.contours.len(), 2);
    }

    #[test]
    fn rubbish_bytes_are_reported_as_an_unreadable_font() {
        let error = Font::from_bytes(vec![0; 64], 0).expect_err("not a font");
        assert!(matches!(error, RenderError::InvalidFont { .. }));
        assert!(error.to_string().starts_with("cannot read the font"));
    }

    #[test]
    fn metrics_are_in_em_units_with_a_positive_descent() {
        let metrics = FontMetrics {
            ascent: 0.8,
            descent: 0.2,
            line_gap: 0.1,
        };
        assert!((metrics.line_height() - 1.1).abs() < 1e-6);
    }
}

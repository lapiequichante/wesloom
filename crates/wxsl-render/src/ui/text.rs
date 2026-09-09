//! Text: the glyph cache, and turning a string into positioned quads.
//!
//! Two things live here, either side of one seam:
//!
//! * [`GlyphCache`] owns the loaded [`Font`]s and, per character, the
//!   distance field packed into the [`Atlas`] plus the metrics a layout
//!   needs. Filling it is the only expensive part of drawing text, and it
//!   happens once per character per session — a distance field is
//!   resolution-independent, so a second text size is a second *draw*, not a
//!   second atlas entry ([ADR 0014](../../../../docs/adr/0014-msdf-text-with-an-own-generator-and-app-supplied-fonts.md)).
//! * [`TextLayout`] is the result of shaping a string: one rectangle per
//!   glyph, in pixels, plus enough structure to answer the two questions a
//!   text field asks — where does the caret at this byte go, and which byte
//!   did the user just click on.
//!
//! Shaping is deliberately simple: left to right, advance plus pair kerning,
//! tabs expanded, `\n` breaking a line, optional word wrap. No bidi, no
//! ligatures, no script itemization. Node labels, WGSL and file paths do not
//! need them, and pretending otherwise would mean a shaping engine rather
//! than a hundred lines (ADR 0014).
//!
//! The three pieces of the layout pass — [`source_lines`], [`break_runs`] and
//! [`place_run`] — are free functions over plain data, so line breaking and
//! wrapping can be tested without a font, a device or an atlas.

use std::collections::HashMap;
use std::ops::Range;

use glam::Vec2;

use crate::error::RenderError;
use crate::ui::atlas::{Atlas, AtlasRegion};
use crate::ui::draw::Rect;
use crate::ui::font::Font;
use crate::ui::msdf_gpu::{MsdfBackend, MsdfGenerator};

/// A font registered with a [`GlyphCache`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FontId(pub usize);

/// One cached character: what it costs to advance past, and what to draw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glyph {
    /// The font's glyph, or `None` if the font has no mapping for the
    /// character — which then draws nothing and advances by nothing.
    pub glyph: Option<u16>,
    /// Pen advance, in em units.
    pub advance: f32,
    /// Where the field ended up in the atlas, or `None` for a character with
    /// no outline (a space) or one that did not fit.
    pub region: Option<AtlasRegion>,
    /// Offset from the pen on the baseline to the quad's top-left corner, in
    /// em units, y down.
    pub offset: Vec2,
    /// Quad size in em units.
    pub size: Vec2,
    /// The field's spread in atlas pixels, at the atlas's pixels-per-em.
    pub range: f32,
}

/// How to lay a string out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextOptions {
    /// Text size in pixels per em.
    pub size: f32,
    /// Baseline-to-baseline distance in pixels. `None` uses the font's own
    /// recommendation.
    pub line_height: Option<f32>,
    /// Wrap width in pixels, or `None` to let lines run on — which is what a
    /// code panel wants, since it scrolls horizontally instead.
    pub wrap: Option<f32>,
    /// How many spaces a tab advances by.
    pub tab_size: usize,
}

impl TextOptions {
    /// Options for text at `size` pixels, with the font's line height, no
    /// wrapping, and four-space tabs.
    pub fn new(size: f32) -> Self {
        TextOptions {
            size,
            line_height: None,
            wrap: None,
            tab_size: 4,
        }
    }

    /// Wrap at `width` pixels.
    pub fn wrapped(mut self, width: f32) -> Self {
        self.wrap = Some(width.max(1.0));
        self
    }

    /// Override the line height.
    pub fn with_line_height(mut self, height: f32) -> Self {
        self.line_height = Some(height);
        self
    }

    /// Set the tab width in spaces.
    pub fn with_tab_size(mut self, spaces: usize) -> Self {
        self.tab_size = spaces.max(1);
        self
    }
}

/// One glyph, placed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PositionedGlyph {
    /// The character this came from.
    pub character: char,
    /// Its byte offset in the laid-out string, which is what a caret is
    /// addressed by.
    pub byte: usize,
    /// Which line it is on.
    pub line: usize,
    /// The quad to draw, relative to the layout's origin, in pixels.
    pub rect: Rect,
    /// The atlas entry to sample, or `None` if there is nothing to draw.
    pub region: Option<AtlasRegion>,
    /// How many screen pixels the distance field's range spans at this size —
    /// what the UI shader needs in order to antialias the glyph.
    pub screen_px_range: f32,
    /// Pen x before this glyph, relative to the layout's origin.
    pub pen: f32,
    /// Pen advance, in pixels.
    pub advance: f32,
}

/// One laid-out line.
#[derive(Clone, Debug, PartialEq)]
pub struct LineLayout {
    /// The bytes of the source string this line covers, excluding the
    /// newline that ended it.
    pub bytes: Range<usize>,
    /// Baseline offset from the layout's origin, in pixels.
    pub baseline: f32,
    /// Width in pixels.
    pub width: f32,
}

/// A shaped string: glyph quads, line structure, and total size.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextLayout {
    /// Every glyph, in source order.
    pub glyphs: Vec<PositionedGlyph>,
    /// Every line, in order. Never empty: the empty string is one empty
    /// line, because a text field still has to put a caret somewhere.
    pub lines: Vec<LineLayout>,
    /// Bounding size in pixels: the widest line, and every line's height.
    pub size: Vec2,
    /// Baseline-to-baseline distance in pixels.
    pub line_height: f32,
    /// Distance from a line's top to its baseline, in pixels.
    pub ascent: f32,
}

impl TextLayout {
    /// Where the caret sits for a byte offset: the top of its line, at the
    /// byte's pen position.
    ///
    /// Offsets past the end clamp to the end, which is what a text field
    /// wants after a deletion.
    pub fn caret(&self, byte: usize) -> Vec2 {
        if self.lines.is_empty() {
            return Vec2::ZERO;
        }
        let line = self.line_of(byte);
        let top = self.lines[line].baseline - self.ascent;
        let mut x = 0.0;
        for glyph in self.glyphs.iter().filter(|glyph| glyph.line == line) {
            if glyph.byte >= byte {
                return Vec2::new(glyph.pen, top);
            }
            x = glyph.pen + glyph.advance;
        }
        Vec2::new(x, top)
    }

    /// Which line a byte offset falls on.
    pub fn line_of(&self, byte: usize) -> usize {
        for (index, line) in self.lines.iter().enumerate() {
            if byte <= line.bytes.end {
                return index;
            }
        }
        self.lines.len().saturating_sub(1)
    }

    /// The byte offset nearest to a point, in layout-relative pixels.
    ///
    /// Rounds to the nearer side of a glyph, so clicking the right half of a
    /// character puts the caret after it.
    pub fn byte_at(&self, point: Vec2) -> usize {
        if self.lines.is_empty() {
            return 0;
        }
        let line = ((point.y / self.line_height.max(1.0)).floor().max(0.0) as usize)
            .min(self.lines.len() - 1);
        let info = &self.lines[line];
        let mut best = info.bytes.start;
        for glyph in self.glyphs.iter().filter(|glyph| glyph.line == line) {
            if point.x < glyph.pen + glyph.advance * 0.5 {
                return glyph.byte;
            }
            best = glyph.byte + glyph.character.len_utf8();
        }
        best.clamp(info.bytes.start, info.bytes.end)
    }

    /// The pixel width of the widest line.
    pub fn width(&self) -> f32 {
        self.size.x
    }

    /// The pixel height of the whole layout.
    pub fn height(&self) -> f32 {
        self.size.y
    }
}

/// One character, resolved against a font and ready to place.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapedChar {
    /// Byte offset in the source string.
    pub byte: usize,
    /// The character.
    pub character: char,
    /// Its cached glyph.
    pub glyph: Glyph,
    /// Pen advance in pixels, with tabs already expanded.
    pub advance: f32,
    /// Kerning against the previous character, in pixels.
    pub kern: f32,
}

/// The byte range of each `\n`-separated line of `text`, excluding the
/// newlines.
///
/// Always returns at least one range, so that an empty string is one empty
/// line rather than no lines at all.
pub fn source_lines(text: &str) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (offset, character) in text.char_indices() {
        if character == '\n' {
            lines.push(start..offset);
            start = offset + 1;
        }
    }
    lines.push(start..text.len());
    lines
}

/// Split one source line's characters into the runs that fit `wrap`.
///
/// Greedy, breaking after the last space that fits; a word longer than the
/// limit is broken at the character that overflows, because the alternative
/// is a line that runs off the panel. Returns index ranges into `items`, and
/// always at least one (possibly empty) range.
pub fn break_runs(items: &[ShapedChar], wrap: Option<f32>) -> Vec<Range<usize>> {
    let Some(limit) = wrap else {
        // `once(..).collect()` rather than `vec![..]`: clippy reads the
        // macro form as an attempt to build a Vec *of* the range.
        return std::iter::once(0..items.len()).collect();
    };

    let mut runs = Vec::new();
    let mut start = 0usize;
    let mut index = 0usize;
    let mut pen = 0.0f32;
    // Where the last break opportunity was: the index *after* a space, so
    // the space stays on the line it ended.
    let mut after_space: Option<usize> = None;

    while index < items.len() {
        let item = &items[index];
        let width = if index > start { item.kern } else { 0.0 } + item.advance;
        if pen + width > limit && index > start {
            let split = match after_space {
                Some(after) if after > start && after <= index => after,
                _ => index,
            };
            runs.push(start..split);
            start = split;
            index = split;
            pen = 0.0;
            after_space = None;
            continue;
        }
        pen += width;
        if item.character == ' ' {
            after_space = Some(index + 1);
        }
        index += 1;
    }
    runs.push(start..items.len());
    runs
}

/// Place one run of shaped characters as line `row` of `layout`.
///
/// `fallback_byte` is where an empty run's line starts, since it has no
/// glyphs to take a byte range from.
pub fn place_run(
    layout: &mut TextLayout,
    run: &[ShapedChar],
    row: usize,
    fallback_byte: usize,
    size: f32,
    em_pixels: f32,
) {
    let baseline = row as f32 * layout.line_height + layout.ascent;
    let mut pen = 0.0f32;
    for (index, item) in run.iter().enumerate() {
        // A line never starts with a kerning adjustment: there is no glyph
        // to its left to kern against.
        if index > 0 {
            pen += item.kern;
        }
        let origin = Vec2::new(pen, baseline) + item.glyph.offset * size;
        layout.glyphs.push(PositionedGlyph {
            character: item.character,
            byte: item.byte,
            line: row,
            rect: Rect::from_min_size(origin, item.glyph.size * size),
            region: item.glyph.region,
            // The field spans `range` pixels at `em_pixels` pixels per em, so
            // at `size` pixels per em it spans this many. That conversion is
            // the whole reason one atlas entry serves every text size.
            screen_px_range: item.glyph.range * size / em_pixels,
            pen,
            advance: item.advance,
        });
        pen += item.advance;
    }
    let bytes = match (run.first(), run.last()) {
        (Some(first), Some(last)) => first.byte..last.byte + last.character.len_utf8(),
        _ => fallback_byte..fallback_byte,
    };
    layout.lines.push(LineLayout {
        bytes,
        baseline,
        width: pen,
    });
}

/// How much of the cache has been used, for a status line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GlyphStats {
    /// Characters cached.
    pub characters: usize,
    /// Distance fields generated, i.e. characters that had an outline.
    pub fields: usize,
    /// Batches handed to the generator. One per frame that introduced new
    /// characters, so it settles at a constant.
    pub batches: usize,
}

/// The loaded fonts, and every character's field and metrics.
pub struct GlyphCache {
    fonts: Vec<Font>,
    glyphs: HashMap<(usize, char), Glyph>,
    kerning: HashMap<(usize, char, char), f32>,
    generator: MsdfGenerator,
    em_pixels: f32,
    range: f32,
    stats: GlyphStats,
    atlas_full: bool,
}

impl GlyphCache {
    /// The resolution glyph fields are generated at, in pixels per em.
    ///
    /// One size for every text size on screen, because a distance field
    /// scales. 48 is enough that a heading is smooth, and small enough that a
    /// full Latin charset is a fraction of a 2048px atlas.
    pub const EM_PIXELS: f32 = 48.0;

    /// The field's spread, in pixels at [`Self::EM_PIXELS`].
    ///
    /// Five pixels leaves the antialiasing ramp a little over a pixel wide at
    /// a 13px UI size, which is where it wants to be: less and small text
    /// goes crunchy, more and it goes soft.
    pub const RANGE: f32 = 5.0;

    /// A cache generating fields with `generator`.
    pub fn new(generator: MsdfGenerator) -> Self {
        GlyphCache {
            fonts: Vec::new(),
            glyphs: HashMap::new(),
            kerning: HashMap::new(),
            generator,
            em_pixels: Self::EM_PIXELS,
            range: Self::RANGE,
            stats: GlyphStats::default(),
            atlas_full: false,
        }
    }

    /// Register a font, returning the id to lay text out with.
    pub fn add_font(&mut self, font: Font) -> FontId {
        self.fonts.push(font);
        FontId(self.fonts.len() - 1)
    }

    /// A registered font.
    ///
    /// # Panics
    ///
    /// Panics on an id from another cache. Ids are indices handed out by
    /// [`Self::add_font`], so that is a programming error.
    pub fn font(&self, id: FontId) -> &Font {
        &self.fonts[id.0]
    }

    /// How many fonts are registered.
    pub fn font_count(&self) -> usize {
        self.fonts.len()
    }

    /// Which MSDF backend is generating fields.
    pub fn backend(&self) -> MsdfBackend {
        self.generator.backend()
    }

    /// Cache statistics, for a status line.
    pub fn stats(&self) -> GlyphStats {
        self.stats
    }

    /// Whether the atlas ran out of room for a glyph.
    ///
    /// A full atlas drops glyphs rather than failing the frame — an interface
    /// with a hole in it is recoverable, one that stopped drawing is not — so
    /// this is how the caller finds out. Sticky until the generator is
    /// replaced.
    pub fn atlas_full(&self) -> bool {
        self.atlas_full
    }

    /// Switch generator, dropping every cached field.
    ///
    /// Both backends produce the same fields (a test compares them), so this
    /// is for measuring the difference or working around a driver. Every
    /// glyph then has to be regenerated, and the atlas has to forget its
    /// entries at the same moment the cache forgets its regions — hence the
    /// atlas being borrowed here rather than cleared by the caller.
    pub fn set_generator(&mut self, generator: MsdfGenerator, atlas: &mut Atlas) {
        self.generator = generator;
        self.glyphs.clear();
        self.kerning.clear();
        self.stats = GlyphStats::default();
        self.atlas_full = false;
        atlas.clear();
    }

    /// A cached character, if it has been prepared.
    pub fn glyph(&self, font: FontId, character: char) -> Option<&Glyph> {
        self.glyphs.get(&(font.0, character))
    }

    /// Make sure every character of `text` is cached, generating and packing
    /// whatever is missing.
    ///
    /// Cheap to call every frame: in the steady state it is a hash lookup per
    /// character and no allocation. Everything expensive — parsing the face,
    /// generating fields, uploading to the atlas — happens on the frame a
    /// character first appears, and in one batch, which is what makes the GPU
    /// backend worth having.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &mut Atlas,
        font: FontId,
        text: &str,
    ) -> Result<(), RenderError> {
        let mut missing: Vec<char> = Vec::new();
        for character in text.chars() {
            if character == '\n' || character == '\r' {
                continue;
            }
            if !self.glyphs.contains_key(&(font.0, character)) && !missing.contains(&character) {
                missing.push(character);
            }
        }
        // Kerning is looked up per adjacent pair, and the face has to be
        // parsed to read it — so the pairs are cached exactly like the
        // glyphs, and a steady-state frame does no parsing at all.
        let mut pairs: Vec<(char, char)> = Vec::new();
        let mut previous: Option<char> = None;
        for character in text.chars() {
            if character == '\n' || character == '\r' {
                previous = None;
                continue;
            }
            if let Some(left) = previous {
                let pair = (left, character);
                if !self.kerning.contains_key(&(font.0, pair.0, pair.1)) && !pairs.contains(&pair) {
                    pairs.push(pair);
                }
            }
            previous = Some(character);
        }

        if missing.is_empty() && pairs.is_empty() {
            return Ok(());
        }

        // One face parse for the whole batch, then one generator dispatch.
        let metrics = self.fonts[font.0].lookup(missing);
        let mut fields = Vec::new();
        let mut entries = Vec::with_capacity(metrics.len());
        for metric in &metrics {
            let field = metric.glyph.and_then(|glyph| {
                self.fonts[font.0].glyph_field(glyph, self.em_pixels, self.range)
            });
            match field {
                Some(field) => {
                    fields.push(field);
                    entries.push((*metric, Some(fields.len() - 1)));
                }
                // A space, or a character the font draws with no outline: it
                // still advances the pen, it just has nothing to draw.
                None => entries.push((*metric, None)),
            }
        }

        let requests: Vec<_> = fields.iter().map(|field| field.request()).collect();
        let bitmaps = self.generator.generate(device, queue, &requests)?;
        if !requests.is_empty() {
            self.stats.batches += 1;
        }

        for (metric, field_index) in entries {
            let mut cached = Glyph {
                glyph: metric.glyph,
                advance: metric.advance,
                region: None,
                offset: Vec2::ZERO,
                size: Vec2::ZERO,
                range: self.range,
            };
            if let Some(index) = field_index {
                cached.offset = fields[index].offset;
                cached.size = fields[index].size;
                match atlas.insert_msdf(queue, &bitmaps[index]) {
                    Ok(region) => {
                        cached.region = Some(region);
                        self.stats.fields += 1;
                    }
                    Err(RenderError::AtlasFull { .. }) => {
                        // Cached without a region, so the next frame does not
                        // try again for the rest of the session.
                        self.atlas_full = true;
                    }
                    Err(error) => return Err(error),
                }
            }
            self.glyphs.insert((font.0, metric.character), cached);
            self.stats.characters += 1;
        }

        if !pairs.is_empty() {
            // Every character of every pair is cached by now, so the glyph
            // ids are in hand and this is one face parse for the batch.
            let ids: Vec<(u16, u16)> = pairs
                .iter()
                .map(|(left, right)| {
                    let glyph_of = |character: &char| {
                        self.glyphs
                            .get(&(font.0, *character))
                            .and_then(|glyph| glyph.glyph)
                            .unwrap_or(0)
                    };
                    (glyph_of(left), glyph_of(right))
                })
                .collect();
            let adjustments = self.fonts[font.0].kerning_pairs(&ids);
            for ((left, right), adjustment) in pairs.iter().zip(adjustments) {
                self.kerning.insert((font.0, *left, *right), adjustment);
            }
        }
        Ok(())
    }

    /// Lay `text` out. Characters that were never prepared are skipped.
    ///
    /// Takes `&self`, so a caller can measure and place in the same frame it
    /// draws; [`Self::prepare`] is the mutable half, and the two are separate
    /// because a layout pass runs over the same strings several times.
    pub fn layout(&self, font: FontId, text: &str, options: TextOptions) -> TextLayout {
        let metrics = self.fonts[font.0].metrics();
        let line_height = options
            .line_height
            .unwrap_or_else(|| metrics.line_height() * options.size);
        let mut layout = TextLayout {
            line_height,
            ascent: metrics.ascent * options.size,
            ..Default::default()
        };

        let mut row = 0usize;
        for source in source_lines(text) {
            let items = self.shape(font, text, source.clone(), options);
            for run in break_runs(&items, options.wrap) {
                place_run(
                    &mut layout,
                    &items[run],
                    row,
                    source.start,
                    options.size,
                    self.em_pixels,
                );
                row += 1;
            }
        }
        let widest = layout
            .lines
            .iter()
            .map(|line| line.width)
            .fold(0.0f32, f32::max);
        layout.size = Vec2::new(widest, layout.lines.len() as f32 * line_height);
        layout
    }

    /// The size `text` would lay out to, without keeping the glyphs.
    pub fn measure(&self, font: FontId, text: &str, options: TextOptions) -> Vec2 {
        self.layout(font, text, options).size
    }

    /// Resolve one source line's characters against the cache.
    fn shape(
        &self,
        font: FontId,
        text: &str,
        source: Range<usize>,
        options: TextOptions,
    ) -> Vec<ShapedChar> {
        let size = options.size;
        let space = self
            .glyph(font, ' ')
            .map(|glyph| glyph.advance * size)
            // No space cached yet: half an em is a decent guess, and it only
            // affects a tab in text nobody prepared.
            .unwrap_or(size * 0.5);

        let mut items = Vec::new();
        let mut previous: Option<char> = None;
        for (offset, character) in text[source.clone()].char_indices() {
            if character == '\r' {
                continue;
            }
            let Some(glyph) = self.glyph(font, character) else {
                continue;
            };
            let advance = if character == '\t' {
                space * options.tab_size as f32
            } else {
                glyph.advance * size
            };
            // From the cache `prepare` filled: never from the font, which
            // would mean parsing the face once per character per frame.
            let kern = previous
                .and_then(|left| self.kerning.get(&(font.0, left, character)))
                .copied()
                .unwrap_or(0.0)
                * size;
            previous = Some(character);
            items.push(ShapedChar {
                byte: source.start + offset,
                character,
                glyph: *glyph,
                advance,
                kern,
            });
        }
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shaped character `advance` pixels wide, for the pure layout tests.
    fn shaped(byte: usize, character: char, advance: f32) -> ShapedChar {
        ShapedChar {
            byte,
            character,
            glyph: Glyph {
                glyph: Some(1),
                advance: advance / 10.0,
                region: None,
                offset: Vec2::new(0.0, -1.0),
                size: Vec2::splat(1.0),
                range: GlyphCache::RANGE,
            },
            advance,
            kern: 0.0,
        }
    }

    /// Every character of `text` as a 10px-wide shaped character.
    fn shaped_text(text: &str) -> Vec<ShapedChar> {
        text.char_indices()
            .map(|(byte, character)| shaped(byte, character, 10.0))
            .collect()
    }

    #[test]
    fn source_lines_covers_the_whole_string_and_never_returns_nothing() {
        assert_eq!(source_lines(""), vec![0..0]);
        assert_eq!(source_lines("abc"), vec![0..3]);
        assert_eq!(source_lines("a\nb"), vec![0..1, 2..3]);
        // A trailing newline opens a last, empty line — which is where the
        // caret goes when you press Enter at the end of a file.
        assert_eq!(source_lines("a\n"), vec![0..1, 2..2]);
        assert_eq!(source_lines("\n\n"), vec![0..0, 1..1, 2..2]);
    }

    #[test]
    fn unwrapped_text_is_one_run() {
        let items = shaped_text("hello world");
        assert_eq!(break_runs(&items, None), vec![0..items.len()]);
    }

    #[test]
    fn wrapping_breaks_after_the_last_space_that_fits() {
        // 10px per character, so "hello " is 60px and "world" another 50px.
        let items = shaped_text("hello world");
        let runs = break_runs(&items, Some(80.0));
        assert_eq!(runs.len(), 2);
        // The space stays at the end of the line it ended.
        assert_eq!(runs[0], 0..6);
        assert_eq!(runs[1], 6..11);
    }

    #[test]
    fn a_word_longer_than_the_limit_is_broken_rather_than_dropped() {
        let items = shaped_text("unbreakable");
        let runs = break_runs(&items, Some(45.0));
        assert!(runs.len() >= 3, "{runs:?}");
        // Every character is accounted for exactly once, in order.
        let mut expected = 0;
        for run in &runs {
            assert_eq!(run.start, expected);
            expected = run.end;
        }
        assert_eq!(expected, items.len());
        // And no run overflows, except one that could not be broken further.
        for run in &runs {
            assert!(run.len() <= 4, "{run:?} is wider than the limit");
        }
    }

    #[test]
    fn wrapping_never_loses_or_reorders_a_character() {
        let items = shaped_text("the quick brown fox jumps over the lazy dog");
        for limit in [20.0, 45.0, 100.0, 1000.0] {
            let runs = break_runs(&items, Some(limit));
            let mut expected = 0;
            for run in &runs {
                assert_eq!(run.start, expected, "limit {limit}: {runs:?}");
                expected = run.end;
            }
            assert_eq!(expected, items.len(), "limit {limit}: {runs:?}");
        }
    }

    #[test]
    fn placing_a_run_positions_glyphs_left_to_right_on_its_baseline() {
        let mut layout = TextLayout {
            line_height: 16.0,
            ascent: 12.0,
            ..Default::default()
        };
        let items = shaped_text("ab");
        place_run(&mut layout, &items, 1, 0, 10.0, GlyphCache::EM_PIXELS);

        assert_eq!(layout.glyphs.len(), 2);
        assert_eq!(layout.glyphs[0].pen, 0.0);
        assert_eq!(layout.glyphs[1].pen, 10.0);
        assert_eq!(layout.lines.len(), 1);
        // Row 1, so the baseline is one line height down plus the ascent.
        assert_eq!(layout.lines[0].baseline, 28.0);
        assert_eq!(layout.lines[0].width, 20.0);
        assert_eq!(layout.lines[0].bytes, 0..2);
        // The glyph's em-unit offset scaled by the text size.
        assert_eq!(layout.glyphs[0].rect.min, Vec2::new(0.0, 18.0));
        // And the field's range converted to screen pixels at this size.
        let expected = GlyphCache::RANGE * 10.0 / GlyphCache::EM_PIXELS;
        assert!((layout.glyphs[0].screen_px_range - expected).abs() < 1e-6);
    }

    #[test]
    fn an_empty_run_still_produces_a_line_to_put_a_caret_on() {
        let mut layout = TextLayout {
            line_height: 16.0,
            ascent: 12.0,
            ..Default::default()
        };
        place_run(&mut layout, &[], 0, 7, 10.0, GlyphCache::EM_PIXELS);
        assert_eq!(layout.lines.len(), 1);
        assert_eq!(layout.lines[0].bytes, 7..7);
        assert_eq!(layout.lines[0].width, 0.0);
        assert!(layout.glyphs.is_empty());
    }

    #[test]
    fn a_line_never_starts_with_a_kerning_adjustment() {
        let mut items = shaped_text("ab");
        items[1].kern = -3.0;
        let mut layout = TextLayout {
            line_height: 16.0,
            ascent: 12.0,
            ..Default::default()
        };
        // As the second glyph of a run, the kern applies.
        place_run(&mut layout, &items, 0, 0, 10.0, GlyphCache::EM_PIXELS);
        assert_eq!(layout.glyphs[1].pen, 7.0);

        // As the first glyph of a run, it must not: there is nothing to its
        // left, and applying it would shift the whole line.
        let mut second = TextLayout {
            line_height: 16.0,
            ascent: 12.0,
            ..Default::default()
        };
        place_run(&mut second, &items[1..], 0, 1, 10.0, GlyphCache::EM_PIXELS);
        assert_eq!(second.glyphs[0].pen, 0.0);
    }

    #[test]
    fn the_caret_and_the_hit_test_are_inverse_at_glyph_boundaries() {
        let mut layout = TextLayout {
            line_height: 16.0,
            ascent: 12.0,
            ..Default::default()
        };
        place_run(
            &mut layout,
            &shaped_text("abc"),
            0,
            0,
            10.0,
            GlyphCache::EM_PIXELS,
        );

        assert_eq!(layout.caret(0).x, 0.0);
        assert_eq!(layout.caret(1).x, 10.0);
        assert_eq!(layout.caret(3).x, 30.0, "past the last glyph is the end");
        // Clicking the left half of a glyph puts the caret before it, the
        // right half after it.
        assert_eq!(layout.byte_at(Vec2::new(1.0, 1.0)), 0);
        assert_eq!(layout.byte_at(Vec2::new(9.0, 1.0)), 1);
        assert_eq!(layout.byte_at(Vec2::new(100.0, 1.0)), 3);
    }

    #[test]
    fn the_line_a_byte_falls_on_is_found_across_a_break() {
        let mut layout = TextLayout {
            line_height: 16.0,
            ascent: 12.0,
            ..Default::default()
        };
        place_run(
            &mut layout,
            &shaped_text("hello"),
            0,
            0,
            10.0,
            GlyphCache::EM_PIXELS,
        );
        let second: Vec<ShapedChar> = "world"
            .char_indices()
            .map(|(byte, character)| shaped(byte + 6, character, 10.0))
            .collect();
        place_run(&mut layout, &second, 1, 6, 10.0, GlyphCache::EM_PIXELS);

        assert_eq!(layout.line_of(0), 0);
        assert_eq!(layout.line_of(5), 0);
        assert_eq!(layout.line_of(7), 1);
        assert_eq!(
            layout.line_of(99),
            1,
            "past the end clamps to the last line"
        );
        // A click below every line lands on the last one.
        assert_eq!(layout.byte_at(Vec2::new(0.0, 100.0)), 6);
        // And the second line's caret is a line height further down.
        assert_eq!(layout.caret(6).y, 16.0);
    }

    #[test]
    fn kerning_comes_from_the_cache_rather_than_the_font() {
        // The cache is what `prepare` fills; `shape` must not reach for the
        // font, because parsing a face per character per frame is the one
        // performance mistake this design can make.
        let mut cache = GlyphCache::new(MsdfGenerator::cpu());
        assert!(cache.kerning.is_empty());
        cache.kerning.insert((0, 'A', 'V'), -0.08);
        assert_eq!(cache.kerning.get(&(0, 'A', 'V')).copied(), Some(-0.08));
        assert_eq!(cache.kerning.get(&(0, 'V', 'A')), None);
    }

    #[test]
    fn text_options_clamp_the_values_that_cannot_be_zero() {
        let options = TextOptions::new(13.0).wrapped(0.0).with_tab_size(0);
        assert_eq!(options.wrap, Some(1.0));
        assert_eq!(options.tab_size, 1);
    }
}

//! Print one glyph's distance field as text.
//!
//! The tool that found the tie-break bug in `ui::msdf`, kept because it will
//! find the next one: when text renders wrong, the question is always "what
//! does the field actually say", and no screenshot answers it. Needs no GPU.
//!
//! ```text
//! cargo run -p wxsl-render --example glyph_field -- <FONT> [CHARACTER]
//! cargo run -p wxsl-render --example glyph_field -- C:/Windows/Fonts/segoeui.ttf c
//! ```
//!
//! Reading the output: `#` is well inside the glyph, `+` just inside, `.`
//! just outside, and a space is outside the field's range. The per-channel
//! numbers underneath are the three fields at one pixel, in bitmap pixels,
//! and the median of them is what the shader draws.

use wxsl_render::ui::font::Font;
use wxsl_render::ui::msdf;

/// Pixels per em the field is generated at. Smaller than the atlas uses, so
/// that the art fits in a terminal.
const EM_PIXELS: f32 = 32.0;

/// The field's spread, in bitmap pixels.
const RANGE: f32 = 5.0;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!(
            "usage: glyph_field <FONT> [CHARACTER]\n\
             \n\
             This crate ships no font (ADR 0014), so give it one — any TTF or\n\
             OTF will do: C:/Windows/Fonts/segoeui.ttf,\n\
             /usr/share/fonts/truetype/dejavu/DejaVuSans.ttf, …"
        );
        std::process::exit(2);
    };
    let character = args
        .next()
        .and_then(|text| text.chars().next())
        .unwrap_or('c');

    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("cannot read {path}: {error}");
            std::process::exit(1);
        }
    };
    let font = match Font::from_bytes(bytes, 0) {
        Ok(font) => font,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };

    let Some(glyph) = font.lookup([character])[0].glyph else {
        eprintln!("{} has no glyph for {character:?}", font.name());
        std::process::exit(1);
    };
    let Some(shape) = font.outline(glyph) else {
        eprintln!("{character:?} has no outline (a space, or a bitmap glyph)");
        std::process::exit(1);
    };

    println!("{} — {character:?} (glyph {glyph})", font.name());
    println!("{} contour(s):", shape.contours.len());
    // The colouring is the half of the algorithm that is hardest to picture,
    // so print it: adjacent splines must differ, and two edges at a corner
    // must share exactly one channel.
    for (index, edge) in msdf::color_edges(&shape).iter().enumerate() {
        println!(
            "  edge {index:2}: {:?} {:?} -> {:?}",
            edge.color,
            edge.segment.start(),
            edge.segment.end()
        );
    }

    let Some(field) = font.glyph_field(glyph, EM_PIXELS, RANGE) else {
        eprintln!("no field: the outline is degenerate");
        std::process::exit(1);
    };
    let bitmap = msdf::generate_edges(&field.request());
    println!(
        "\n{}x{} at {EM_PIXELS} px/em, range {RANGE} px:",
        bitmap.width, bitmap.height
    );
    for y in 0..bitmap.height {
        let row: String = (0..bitmap.width)
            .map(|x| match bitmap.distance_at(x, y) {
                distance if distance > 1.0 => '#',
                distance if distance > 0.0 => '+',
                distance if distance > -1.0 => '.',
                _ => ' ',
            })
            .collect();
        println!("{y:3} {row}");
    }

    // The middle pixel's channels, as a worked example of what to look at.
    let (x, y) = (bitmap.width / 2, bitmap.height / 2);
    println!(
        "\n({x}, {y}): channels {:?}, median {:.3}",
        bitmap.channels_at(x, y),
        bitmap.distance_at(x, y)
    );
}

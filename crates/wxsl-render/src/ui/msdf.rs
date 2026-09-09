//! Multi-channel signed distance fields, from glyph outlines.
//!
//! A single-channel distance field cannot represent a corner: whatever the
//! resolution, bilinear interpolation between two texels rounds it off. An
//! MSDF stores three fields whose *median* is the distance, and colours the
//! outline's edges so that the two edges meeting at a corner disagree in
//! exactly one channel — the median then keeps the corner sharp at any
//! magnification, which is what lets one atlas entry serve every text size
//! ([ADR 0014](../../../../docs/adr/0014-msdf-text-with-an-own-generator-and-app-supplied-fonts.md)).
//!
//! The pipeline is three passes over a [`Shape`]:
//!
//! 1. **Orientation** ([`Shape::normalize_orientation`]) — TrueType winds its
//!    outer contours clockwise and PostScript counter-clockwise, and the sign
//!    of a distance is which side of an edge you are on. Normalizing once
//!    means nothing downstream has to know which flavour of font this was.
//! 2. **Edge colouring** ([`color_edges`]) — find the corners, and give the
//!    splines between them alternating two-channel colours.
//! 3. **Generation** ([`generate`]) — per pixel and per channel, the signed
//!    *pseudo*-distance to the nearest edge carrying that channel.
//!
//! Nothing here touches the GPU or a font file: it is a pure function from an
//! outline to a bitmap, which is why it can be tested against a brute-force
//! distance field on a machine with no adapter.
//!
//! The technique is Viktor Chlumský's (*Shape Decomposition for Multi-channel
//! Distance Fields*, 2015); this is an independent implementation of the
//! method as described, not a translation of `msdfgen`.

use glam::Vec2;

/// One edge of a contour: a straight line, or a Bézier of degree 2 or 3.
///
/// Fonts contain quadratics (TrueType) or cubics (CFF/OpenType), so both are
/// carried through rather than flattened: flattening a curve into short lines
/// puts a colour boundary and a distance discontinuity at every joint, which
/// is visible as a shimmer on curved stems at small sizes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Segment {
    /// A straight line between two points.
    Line([Vec2; 2]),
    /// A quadratic Bézier: start, control, end.
    Quad([Vec2; 3]),
    /// A cubic Bézier: start, two controls, end.
    Cubic([Vec2; 4]),
}

impl Segment {
    /// The point the segment starts at.
    pub fn start(&self) -> Vec2 {
        match self {
            Segment::Line([a, _]) | Segment::Quad([a, ..]) | Segment::Cubic([a, ..]) => *a,
        }
    }

    /// The point the segment ends at.
    pub fn end(&self) -> Vec2 {
        match self {
            Segment::Line([_, b]) => *b,
            Segment::Quad([_, _, c]) => *c,
            Segment::Cubic([_, _, _, d]) => *d,
        }
    }

    /// The point at parameter `t`, extrapolating outside `0..=1`.
    pub fn point(&self, t: f32) -> Vec2 {
        match self {
            Segment::Line([a, b]) => a.lerp(*b, t),
            Segment::Quad([a, b, c]) => {
                let u = 1.0 - t;
                *a * (u * u) + *b * (2.0 * u * t) + *c * (t * t)
            }
            Segment::Cubic([a, b, c, d]) => {
                let u = 1.0 - t;
                *a * (u * u * u)
                    + *b * (3.0 * u * u * t)
                    + *c * (3.0 * u * t * t)
                    + *d * (t * t * t)
            }
        }
    }

    /// The derivative at parameter `t`, i.e. the tangent, unnormalized.
    ///
    /// Falls back to the chord when a Bézier's control point sits on an
    /// endpoint, which makes the true derivative vanish there. A font with a
    /// degenerate control point is common enough (it is how you spell "this
    /// curve is really a line") that a zero tangent would otherwise poison
    /// every sign computed from it.
    pub fn direction(&self, t: f32) -> Vec2 {
        let derivative = match self {
            Segment::Line([a, b]) => *b - *a,
            Segment::Quad([a, b, c]) => (*b - *a).lerp(*c - *b, t) * 2.0,
            Segment::Cubic([a, b, c, d]) => {
                let first = (*b - *a).lerp(*c - *b, t);
                let second = (*c - *b).lerp(*d - *c, t);
                first.lerp(second, t) * 3.0
            }
        };
        if derivative.length_squared() > f32::EPSILON {
            derivative
        } else {
            self.end() - self.start()
        }
    }

    /// The tangent at `t`, unit length, or `Vec2::X` for a point-sized
    /// segment (whose direction is genuinely undefined).
    fn tangent(&self, t: f32) -> Vec2 {
        let direction = self.direction(t);
        direction.try_normalize().unwrap_or(Vec2::X)
    }

    /// Whether this segment covers no distance at all, and so can be dropped:
    /// it contributes nothing to the field and has no meaningful direction.
    pub fn is_degenerate(&self) -> bool {
        let extent = match self {
            Segment::Line([a, b]) => (*b - *a).length(),
            Segment::Quad([a, b, c]) => (*b - *a).length() + (*c - *b).length(),
            Segment::Cubic([a, b, c, d]) => {
                (*b - *a).length() + (*c - *b).length() + (*d - *c).length()
            }
        };
        extent <= 1e-6
    }

    /// The signed distance from `p` to this segment, and the parameter it was
    /// found at.
    ///
    /// Positive means `p` is to the left of the segment's direction, which
    /// after [`Shape::normalize_orientation`] means *inside* the shape. The
    /// returned parameter can fall outside `0..=1`: it is then the projection
    /// onto the endpoint's tangent, which is what
    /// [`Segment::pseudo_distance`] needs to decide whether to extend the
    /// edge past its endpoint.
    pub fn signed_distance(&self, p: Vec2) -> (SignedDistance, f32) {
        if let Segment::Line([a, b]) = self {
            let along = *b - *a;
            let to_p = p - *a;
            let length_squared = along.length_squared();
            if length_squared <= f32::EPSILON {
                let distance = to_p.length();
                return (SignedDistance::new(distance, 0.0), 0.0);
            }
            let t = to_p.dot(along) / length_squared;
            let to_nearer_end = if t > 0.5 { p - *b } else { to_p };
            let endpoint_distance = to_nearer_end.length();
            if t > 0.0 && t < 1.0 {
                // Inside the segment: the perpendicular distance is exact and
                // already signed, and no endpoint can be nearer.
                let orthogonal = along.normalize().perp_dot(to_p);
                if orthogonal.abs() <= endpoint_distance {
                    return (
                        SignedDistance {
                            distance: orthogonal,
                            alignment: 0.0,
                        },
                        t,
                    );
                }
            }
            let sign = if along.perp_dot(to_p) >= 0.0 {
                1.0
            } else {
                -1.0
            };
            let alignment = along
                .normalize()
                .dot(to_nearer_end.normalize_or_zero())
                .abs();
            return (
                SignedDistance {
                    distance: sign * endpoint_distance,
                    alignment,
                },
                t,
            );
        }

        // Curves: a coarse scan for the basin, then Newton-Raphson on
        // `d/dt |B(t) - p|^2 / 2 = (B(t) - p) . B'(t) = 0`. Exact root
        // finding would mean solving a cubic (quadratics) or a quintic
        // (cubics); at the resolutions a glyph atlas is generated at, a few
        // refinement steps are indistinguishable and much shorter.
        let mut best_t = 0.0;
        let mut best_squared = f32::INFINITY;
        const SCAN: usize = 24;
        for step in 0..=SCAN {
            let t = step as f32 / SCAN as f32;
            let squared = (self.point(t) - p).length_squared();
            if squared < best_squared {
                best_squared = squared;
                best_t = t;
            }
        }
        let mut t = best_t;
        for _ in 0..8 {
            let offset = self.point(t) - p;
            let derivative = self.direction(t);
            let numerator = offset.dot(derivative);
            // The exact second derivative would need another match on the
            // segment kind; a finite difference of the first is enough to
            // keep Newton converging, and cannot make it diverge because the
            // step is clamped to the parameter range below.
            let epsilon = 1e-3;
            let curvature =
                (self.direction(t + epsilon) - self.direction(t - epsilon)) / (2.0 * epsilon);
            let denominator = derivative.length_squared() + offset.dot(curvature);
            if denominator.abs() <= f32::EPSILON {
                break;
            }
            let next = (t - numerator / denominator).clamp(0.0, 1.0);
            if (next - t).abs() <= 1e-6 {
                t = next;
                break;
            }
            t = next;
        }

        let clamped = t.clamp(0.0, 1.0);
        let point = self.point(clamped);
        let tangent = self.tangent(clamped);
        let to_p = p - point;
        let distance = to_p.length();
        let sign = if tangent.perp_dot(to_p) >= 0.0 {
            1.0
        } else {
            -1.0
        };
        let at_endpoint = clamped <= 0.0 || clamped >= 1.0;
        let alignment = if at_endpoint {
            tangent.dot(to_p.normalize_or_zero()).abs()
        } else {
            0.0
        };
        // Report the endpoint tangent's projection parameter, so that a point
        // "before" the start reads as t < 0 rather than t == 0.
        let reported = if clamped <= 0.0 {
            let projection = to_p.dot(tangent);
            if projection < 0.0 {
                projection.min(-f32::EPSILON)
            } else {
                0.0
            }
        } else if clamped >= 1.0 {
            let projection = to_p.dot(tangent);
            if projection > 0.0 {
                1.0 + projection
            } else {
                1.0
            }
        } else {
            clamped
        };
        (
            SignedDistance {
                distance: sign * distance,
                alignment,
            },
            reported,
        )
    }

    /// Extend the edge past its endpoints, for points that project outside it.
    ///
    /// The distance to a *finite* edge has a crease where the nearest point
    /// jumps from the edge's interior to its endpoint, and a crease in one
    /// channel is a visible seam in the median. Substituting the distance to
    /// the endpoint's infinite tangent line removes it, which is the whole
    /// reason distance fields for text are built from pseudo-distances.
    pub fn pseudo_distance(&self, p: Vec2, found: SignedDistance, t: f32) -> SignedDistance {
        if t < 0.0 {
            let tangent = self.tangent(0.0);
            let to_p = p - self.start();
            if to_p.dot(tangent) < 0.0 {
                let pseudo = tangent.perp_dot(to_p);
                if pseudo.abs() <= found.distance.abs() {
                    return SignedDistance {
                        distance: pseudo,
                        alignment: 0.0,
                    };
                }
            }
        } else if t > 1.0 {
            let tangent = self.tangent(1.0);
            let to_p = p - self.end();
            if to_p.dot(tangent) > 0.0 {
                let pseudo = tangent.perp_dot(to_p);
                if pseudo.abs() <= found.distance.abs() {
                    return SignedDistance {
                        distance: pseudo,
                        alignment: 0.0,
                    };
                }
            }
        }
        found
    }

    /// Twice the signed area between the segment and the origin, i.e. its
    /// contribution to its contour's winding. Positive for counter-clockwise.
    fn double_signed_area(&self) -> f32 {
        // The shoelace formula over the control polygon, weighted so that it
        // is exact for Bézier curves (the area of a Bézier is a fixed
        // combination of its control points' cross products).
        match self {
            Segment::Line([a, b]) => a.perp_dot(*b),
            Segment::Quad([a, b, c]) => {
                (a.perp_dot(*b) * 2.0 + b.perp_dot(*c) * 2.0 + a.perp_dot(*c)) / 3.0
            }
            Segment::Cubic([a, b, c, d]) => {
                (a.perp_dot(*b) * 6.0
                    + a.perp_dot(*c) * 3.0
                    + a.perp_dot(*d)
                    + b.perp_dot(*c) * 3.0
                    + b.perp_dot(*d) * 3.0
                    + c.perp_dot(*d) * 6.0)
                    / 10.0
            }
        }
    }

    /// The same segment, traversed backwards.
    fn reversed(&self) -> Segment {
        match self {
            Segment::Line([a, b]) => Segment::Line([*b, *a]),
            Segment::Quad([a, b, c]) => Segment::Quad([*c, *b, *a]),
            Segment::Cubic([a, b, c, d]) => Segment::Cubic([*d, *c, *b, *a]),
        }
    }

    /// The axis-aligned bounds of the control polygon.
    ///
    /// A Bézier is inside its control polygon's hull, so this is a
    /// conservative bound — which is what a glyph's atlas footprint wants.
    fn control_bounds(&self) -> (Vec2, Vec2) {
        let points: &[Vec2] = match self {
            Segment::Line(points) => points,
            Segment::Quad(points) => points,
            Segment::Cubic(points) => points,
        };
        let mut min = points[0];
        let mut max = points[0];
        for point in &points[1..] {
            min = min.min(*point);
            max = max.max(*point);
        }
        (min, max)
    }
}

/// A distance, and how head-on the edge was met.
///
/// The second field breaks ties, and the direction of that tie-break is
/// load-bearing rather than cosmetic. Two edges meeting at a vertex are
/// *exactly* the same distance from any point nearest that vertex, and they
/// do not always agree on which side of the outline the point is on: at a
/// reflex vertex — the terminal of a `c`, the notch of an `e` — the edge you
/// are beyond the *end* of reports the wrong side, because its half-plane
/// test is being asked about a region it does not own.
///
/// So the edge met more *perpendicularly* wins: `alignment` is `0` for a
/// sample that projects onto an edge's interior (perfectly perpendicular, and
/// always right about the side), and approaches `1` for one that projects far
/// past an endpoint along the edge's own direction (nearly end-on, and the
/// unreliable one). Smaller wins.
///
/// Getting this backwards produces exactly one symptom, and it took a dump of
/// the field to see it: the aperture of a `c` fills in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SignedDistance {
    /// Distance, positive inside the shape.
    pub distance: f32,
    /// `|cos|` of the angle between the edge and the direction to the
    /// sample: `0` when the sample projects onto the edge's interior, up to
    /// `1` when it is straight off an end.
    pub alignment: f32,
}

impl SignedDistance {
    /// Further away than any real edge can be.
    pub const FAR: SignedDistance = SignedDistance {
        distance: f32::NEG_INFINITY,
        // The worst possible tie-break, so that any real edge beats this
        // even in the (impossible) case of an equal distance.
        alignment: f32::INFINITY,
    };

    fn new(distance: f32, alignment: f32) -> Self {
        SignedDistance {
            distance,
            alignment,
        }
    }

    /// Whether `self` describes a nearer edge than `other`.
    pub fn is_nearer_than(&self, other: &SignedDistance) -> bool {
        let (mine, theirs) = (self.distance.abs(), other.distance.abs());
        mine < theirs || (mine == theirs && self.alignment < other.alignment)
    }
}

/// A closed loop of segments: one outer boundary, or one hole.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Contour {
    /// The segments, in traversal order, each starting where the last ended.
    pub segments: Vec<Segment>,
}

impl Contour {
    /// Twice the signed area, positive when wound counter-clockwise.
    pub fn double_signed_area(&self) -> f32 {
        self.segments
            .iter()
            .map(Segment::double_signed_area)
            .sum::<f32>()
    }

    /// Reverse the traversal direction, flipping which side is inside.
    pub fn reverse(&mut self) {
        self.segments.reverse();
        for segment in &mut self.segments {
            *segment = segment.reversed();
        }
    }

    /// Whether two consecutive segments meet at a corner rather than
    /// smoothly, given the sine of the angle below which a joint counts as
    /// smooth.
    fn is_corner(&self, index: usize, sin_threshold: f32) -> bool {
        let count = self.segments.len();
        let incoming = self.segments[(index + count - 1) % count].tangent(1.0);
        let outgoing = self.segments[index].tangent(0.0);
        // Two tests, because either alone misses a case: the dot product
        // catches a reversal (a spike, where the cross product is ~0), and
        // the cross product catches an ordinary bend.
        incoming.dot(outgoing) <= 0.0 || incoming.perp_dot(outgoing).abs() > sin_threshold
    }
}

/// A glyph outline: every contour of it, in one coordinate space.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shape {
    /// Outer boundaries and holes, in no particular order.
    pub contours: Vec<Contour>,
}

impl Shape {
    /// An empty shape, whose field is "outside" everywhere.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether there is anything to generate a field from.
    pub fn is_empty(&self) -> bool {
        self.contours.iter().all(|c| c.segments.is_empty())
    }

    /// The bounds of every contour's control polygon, or `None` if empty.
    pub fn bounds(&self) -> Option<(Vec2, Vec2)> {
        let mut bounds: Option<(Vec2, Vec2)> = None;
        for segment in self.contours.iter().flat_map(|c| c.segments.iter()) {
            let (min, max) = segment.control_bounds();
            bounds = Some(match bounds {
                None => (min, max),
                Some((low, high)) => (low.min(min), high.max(max)),
            });
        }
        bounds
    }

    /// Drop degenerate segments and empty contours.
    pub fn clean(&mut self) {
        for contour in &mut self.contours {
            contour.segments.retain(|segment| !segment.is_degenerate());
        }
        self.contours.retain(|contour| !contour.segments.is_empty());
    }

    /// Make outer contours run counter-clockwise, so that a positive distance
    /// means inside.
    ///
    /// Decided by the *total* signed area: holes are wound against their
    /// outer contour and are smaller than it, so the sum takes the outer
    /// contours' sign. This is the one place the difference between a
    /// TrueType and a PostScript outline is handled.
    pub fn normalize_orientation(&mut self) {
        let area: f32 = self
            .contours
            .iter()
            .map(Contour::double_signed_area)
            .sum::<f32>();
        if area < 0.0 {
            for contour in &mut self.contours {
                contour.reverse();
            }
        }
    }
}

/// Which of the three channels an edge writes to.
///
/// A corner is preserved when the two edges meeting there share exactly one
/// channel: the shared one records the corner's own distance, and the median
/// of the three reconstructs the crease. So the useful colours are the
/// two-channel ones, plus white for a contour with no corners at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EdgeColor(u8);

impl EdgeColor {
    /// Red and green.
    pub const YELLOW: EdgeColor = EdgeColor(0b011);
    /// Red and blue.
    pub const MAGENTA: EdgeColor = EdgeColor(0b101);
    /// Green and blue.
    pub const CYAN: EdgeColor = EdgeColor(0b110);
    /// All three channels: an edge whose neighbours are all smooth.
    pub const WHITE: EdgeColor = EdgeColor(0b111);

    /// Whether this colour writes channel `channel` (0 = red).
    pub fn has_channel(self, channel: usize) -> bool {
        self.0 & (1 << channel) != 0
    }

    /// The channel mask, as the compute shader's edge buffer carries it:
    /// bit 0 red, bit 1 green, bit 2 blue.
    pub fn channels(self) -> u32 {
        u32::from(self.0)
    }
}

/// The next two-channel colour, avoiding `current` and `banned`.
///
/// Deterministic rather than seeded: with three colours and at most two to
/// avoid there is always a choice, and a reproducible one makes a generated
/// atlas byte-for-byte comparable between runs.
fn next_color(current: EdgeColor, banned: EdgeColor) -> EdgeColor {
    const CYCLE: [EdgeColor; 3] = [EdgeColor::CYAN, EdgeColor::MAGENTA, EdgeColor::YELLOW];
    CYCLE
        .iter()
        .copied()
        .find(|candidate| *candidate != current && *candidate != banned)
        .unwrap_or(EdgeColor::WHITE)
}

/// An edge and the channels it writes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColoredEdge {
    /// The geometry.
    pub segment: Segment,
    /// The channels this edge contributes to.
    pub color: EdgeColor,
}

/// The angle, in degrees, below which a joint between two edges is treated as
/// smooth rather than as a corner.
///
/// Three degrees is the value the technique's reference implementation uses
/// and it holds up: font outlines routinely join curves at a fraction of a
/// degree, and calling those corners would colour a smooth stem in stripes.
pub const CORNER_ANGLE_DEGREES: f32 = 3.0;

/// Colour every edge of `shape`, returning them as one flat list.
///
/// Contours are coloured independently: a corner is a property of a joint,
/// and two different contours never share one.
pub fn color_edges(shape: &Shape) -> Vec<ColoredEdge> {
    let sin_threshold = CORNER_ANGLE_DEGREES.to_radians().sin();
    let mut edges = Vec::new();
    for contour in &shape.contours {
        let count = contour.segments.len();
        if count == 0 {
            continue;
        }
        let corners: Vec<usize> = (0..count)
            .filter(|index| contour.is_corner(*index, sin_threshold))
            .collect();

        match corners.len() {
            // A smooth loop — an `o`, a bowl, a dot. Every channel records
            // the same distance, so the median is the plain distance field.
            0 => edges.extend(contour.segments.iter().map(|segment| ColoredEdge {
                segment: *segment,
                color: EdgeColor::WHITE,
            })),
            // One corner: a teardrop. Two colours would put a second colour
            // boundary at a smooth joint on the far side of the loop, so the
            // loop is split in three with white in the middle, which shares a
            // channel with both of its neighbours and hides the seam.
            1 if count >= 3 => {
                let start = corners[0];
                let colors = [
                    EdgeColor::MAGENTA,
                    EdgeColor::WHITE,
                    next_color(EdgeColor::MAGENTA, EdgeColor::WHITE),
                ];
                for offset in 0..count {
                    let third = (offset * 3) / count;
                    edges.push(ColoredEdge {
                        segment: contour.segments[(start + offset) % count],
                        color: colors[third.min(2)],
                    });
                }
            }
            // Too few edges to split three ways, so nothing better than white
            // is available without subdividing the outline.
            1 => edges.extend(contour.segments.iter().map(|segment| ColoredEdge {
                segment: *segment,
                color: EdgeColor::WHITE,
            })),
            // The usual case: switch colour at every corner. The last spline
            // must also avoid the first one's colour, because the contour
            // wraps around and they meet at a corner too.
            _ => {
                let start = corners[0];
                let initial = next_color(EdgeColor::WHITE, EdgeColor::WHITE);
                let mut color = initial;
                let mut spline = 0;
                for offset in 0..count {
                    let index = (start + offset) % count;
                    if spline + 1 < corners.len() && corners[spline + 1] == index {
                        spline += 1;
                        let banned = if spline == corners.len() - 1 {
                            initial
                        } else {
                            EdgeColor::WHITE
                        };
                        color = next_color(color, banned);
                    }
                    edges.push(ColoredEdge {
                        segment: contour.segments[index],
                        color,
                    });
                }
            }
        }
    }
    edges
}

/// Where a shape sits in the bitmap being generated: a bitmap point is
/// `(shape_point + translate) * scale`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MsdfTransform {
    /// Bitmap pixels per shape unit.
    pub scale: f32,
    /// Shape-space offset applied before scaling, i.e. where the shape's
    /// origin lands.
    pub translate: Vec2,
}

/// A generated field: three channels, one byte each, row 0 at the top.
#[derive(Clone, Debug, PartialEq)]
pub struct MsdfBitmap {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The distance the `0..=1` byte range spans, in bitmap pixels. The
    /// shader needs it to know how many pixels of antialiasing the field is
    /// worth at the size being drawn.
    pub range: f32,
    /// `width * height * 3` bytes of RGB.
    pub pixels: Vec<u8>,
}

impl MsdfBitmap {
    /// The three channel values at a pixel, as signed distances in bitmap
    /// pixels — the inverse of what [`generate`] encoded.
    ///
    /// For tests and for debugging a field that came out wrong; the shader
    /// does the same arithmetic in reverse.
    pub fn channels_at(&self, x: u32, y: u32) -> [f32; 3] {
        let index = ((y * self.width + x) * 3) as usize;
        let mut out = [0.0; 3];
        for (channel, value) in out.iter_mut().enumerate() {
            *value = (self.pixels[index + channel] as f32 / 255.0 - 0.5) * self.range;
        }
        out
    }

    /// The reconstructed signed distance at a pixel, in bitmap pixels:
    /// the median of the three channels, positive inside.
    ///
    /// This is exactly what the fragment shader computes.
    pub fn distance_at(&self, x: u32, y: u32) -> f32 {
        let [r, g, b] = self.channels_at(x, y);
        r.max(g).min(r.min(g).max(b))
    }
}

/// The median of three channel distances: what the shader reconstructs.
pub fn median(channels: [f32; 3]) -> f32 {
    let [r, g, b] = channels;
    r.max(g).min(r.min(g).max(b))
}

/// Repair a pixel whose median disagrees with the true distance about which
/// side of the outline it is on.
///
/// The one artifact multi-channel fields are prone to: where two edges of
/// *different* colours pass within the field's range of each other — the
/// aperture of a `c`, the gap in an `e`, a hairline serif — each channel
/// reports its own nearest same-colour edge, and their median can land on the
/// wrong side of the outline entirely. On screen that is a `c` with its
/// opening bridged over.
///
/// A sign disagreement is the whole test, and it is enough: a genuine corner
/// is built from pseudo-distances that *keep* the true distance's sign (they
/// extend an edge's tangent, they do not cross it), so correcting only sign
/// errors leaves every corner sharp while removing every bridge. Where the
/// sign is wrong there is no corner worth preserving, and the true distance
/// in all three channels is exactly a single-channel field for that pixel.
pub fn correct_error(channels: [f32; 3], truth: f32) -> [f32; 3] {
    let reconstructed = median(channels);
    // Only a genuine crossing counts: at the outline itself both are ~0, and
    // "correcting" that would flatten the very edge being drawn.
    if reconstructed.signum() != truth.signum() && reconstructed.abs() > 1e-4 {
        return [truth; 3];
    }
    channels
}

/// Generate a `width` x `height` field for `shape`.
///
/// `range` is how far, in bitmap pixels, the field reaches on each side of
/// the outline; the byte range maps onto `-range/2 ..= range/2`. Bigger is
/// smoother antialiasing and more room for effects like outlines, at the cost
/// of atlas space, since a glyph's bitmap has to be padded by half of it.
///
/// `shape` should have been through [`Shape::normalize_orientation`] and
/// [`Shape::clean`]; [`color_edges`] does not care, but the sign of the
/// result does.
pub fn generate(
    shape: &Shape,
    width: u32,
    height: u32,
    transform: MsdfTransform,
    range: f32,
) -> MsdfBitmap {
    generate_edges(&MsdfRequest {
        edges: &color_edges(shape),
        width,
        height,
        transform,
        range,
    })
}

/// One field to generate: the input both backends take.
///
/// Colouring is already done, so that a batch of glyphs can be handed to the
/// GPU as one flat edge buffer ([`crate::ui::MsdfGenerator`]).
#[derive(Clone, Copy, Debug)]
pub struct MsdfRequest<'a> {
    /// The coloured edges of every contour, from [`color_edges`].
    pub edges: &'a [ColoredEdge],
    /// Bitmap width in pixels.
    pub width: u32,
    /// Bitmap height in pixels.
    pub height: u32,
    /// Where the outline sits in the bitmap.
    pub transform: MsdfTransform,
    /// The distance the byte range spans, in bitmap pixels.
    pub range: f32,
}

/// Generate a field from already-coloured edges, on the CPU.
///
/// The reference implementation: [`crate::ui::msdf_gpu`] is the same
/// algorithm as a compute pass, and a test compares the two.
pub fn generate_edges(request: &MsdfRequest<'_>) -> MsdfBitmap {
    let MsdfRequest {
        edges,
        width,
        height,
        transform,
        range,
    } = *request;
    let mut pixels = vec![0u8; (width as usize) * (height as usize) * 3];
    let range = range.max(1e-3);
    // Everything below works in shape units and converts once at the end, so
    // that a tiny `scale` cannot lose precision in the distances themselves.
    let shape_range = range / transform.scale.max(1e-6);

    for y in 0..height {
        for x in 0..width {
            // Row 0 is the top of the bitmap, but the shape's y axis points
            // up, so the row index is flipped here and nowhere else.
            let bitmap = Vec2::new(x as f32 + 0.5, (height - y - 1) as f32 + 0.5);
            let point = bitmap / transform.scale - transform.translate;

            let mut nearest = [(SignedDistance::FAR, 0.0f32, usize::MAX); 3];
            // The nearest edge of *any* colour, whose distance is the true
            // one. Kept for the error correction below.
            let mut truth = SignedDistance::FAR;
            for (index, edge) in edges.iter().enumerate() {
                let (found, t) = edge.segment.signed_distance(point);
                if found.is_nearer_than(&truth) {
                    truth = found;
                }
                for (channel, slot) in nearest.iter_mut().enumerate() {
                    if edge.color.has_channel(channel) && found.is_nearer_than(&slot.0) {
                        *slot = (found, t, index);
                    }
                }
            }

            let mut channels = [-shape_range; 3];
            for (channel, (found, t, index)) in nearest.iter().enumerate() {
                if let Some(edge) = edges.get(*index) {
                    channels[channel] = edge.segment.pseudo_distance(point, *found, *t).distance;
                }
                // Otherwise no edge writes this channel — an empty shape —
                // and the default says "as far outside as the field can see".
            }
            let corrected = correct_error(channels, truth.distance);

            let base = ((y * width + x) * 3) as usize;
            for (channel, distance) in corrected.iter().enumerate() {
                let encoded = distance / shape_range + 0.5;
                pixels[base + channel] = (encoded.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
    }

    MsdfBitmap {
        width,
        height,
        range,
        pixels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A counter-clockwise square from `(0,0)` to `(size,size)`.
    fn square(size: f32) -> Shape {
        let corners = [
            Vec2::new(0.0, 0.0),
            Vec2::new(size, 0.0),
            Vec2::new(size, size),
            Vec2::new(0.0, size),
        ];
        let segments = (0..4)
            .map(|i| Segment::Line([corners[i], corners[(i + 1) % 4]]))
            .collect();
        Shape {
            contours: vec![Contour { segments }],
        }
    }

    /// A circle of `radius` centred on `center`, as four quadratic arcs.
    fn circle(center: Vec2, radius: f32) -> Shape {
        // The control point of a quarter arc as a quadratic sits at the
        // corner of the bounding square; it is not a perfect circle, which
        // does not matter for a smooth-contour test.
        let axes = [Vec2::X, Vec2::Y, Vec2::NEG_X, Vec2::NEG_Y];
        let segments = (0..4)
            .map(|i| {
                let from = center + axes[i] * radius;
                let to = center + axes[(i + 1) % 4] * radius;
                let control = center + (axes[i] + axes[(i + 1) % 4]) * radius;
                Segment::Quad([from, control, to])
            })
            .collect();
        Shape {
            contours: vec![Contour { segments }],
        }
    }

    /// Brute-force distance from `p` to a shape's outline, by dense sampling.
    ///
    /// Unsigned, and deliberately dumb: it is the independent answer the
    /// generator is checked against.
    fn brute_force_distance(shape: &Shape, p: Vec2) -> f32 {
        let mut best = f32::INFINITY;
        for segment in shape.contours.iter().flat_map(|c| c.segments.iter()) {
            for step in 0..=2000 {
                let t = step as f32 / 2000.0;
                best = best.min((segment.point(t) - p).length());
            }
        }
        best
    }

    #[test]
    fn a_counter_clockwise_square_is_positive_inside() {
        let shape = square(10.0);
        let edges = color_edges(&shape);
        let nearest = |p: Vec2| {
            let mut best = SignedDistance::FAR;
            for edge in &edges {
                let (found, _) = edge.segment.signed_distance(p);
                if found.is_nearer_than(&best) {
                    best = found;
                }
            }
            best.distance
        };
        assert!(nearest(Vec2::new(5.0, 5.0)) > 0.0, "the centre is inside");
        assert!(
            nearest(Vec2::new(-1.0, 5.0)) < 0.0,
            "a point to the left is outside"
        );
        // Distance to the nearest of the four edges, not to a corner.
        assert!((nearest(Vec2::new(2.0, 5.0)) - 2.0).abs() < 1e-4);
    }

    #[test]
    fn orientation_is_normalized_whichever_way_the_font_wound_it() {
        let counter_clockwise = square(10.0);
        let mut clockwise = counter_clockwise.clone();
        clockwise.contours[0].reverse();
        assert_ne!(clockwise, counter_clockwise);
        assert!(clockwise.contours[0].double_signed_area() < 0.0);

        clockwise.normalize_orientation();
        assert!(clockwise.contours[0].double_signed_area() > 0.0);
        // Same loop, and now the same orientation, so the fields must match.
        let transform = MsdfTransform {
            scale: 2.0,
            translate: Vec2::splat(2.0),
        };
        let expected = generate(&counter_clockwise, 28, 28, transform, 4.0);
        let actual = generate(&clockwise, 28, 28, transform, 4.0);
        assert_eq!(actual.pixels, expected.pixels);
    }

    #[test]
    fn the_median_reconstructs_the_true_distance_away_from_corners() {
        // A circle has no corners at all, so every channel carries the same
        // distance and the median must be the plain distance field.
        let shape = circle(Vec2::splat(16.0), 10.0);
        let transform = MsdfTransform {
            scale: 1.0,
            translate: Vec2::ZERO,
        };
        let field = generate(&shape, 32, 32, transform, 8.0);
        let mut checked = 0;
        for y in 0..32u32 {
            for x in 0..32u32 {
                let point = Vec2::new(x as f32 + 0.5, (32 - y - 1) as f32 + 0.5);
                let truth = brute_force_distance(&shape, point);
                // Only where the field is not saturated: outside the range
                // the encoding has clamped and carries no information.
                if truth > 3.0 {
                    continue;
                }
                let reconstructed = field.distance_at(x, y).abs();
                assert!(
                    (reconstructed - truth).abs() < 0.15,
                    "at ({x}, {y}): field says {reconstructed}, brute force says {truth}"
                );
                checked += 1;
            }
        }
        assert!(checked > 100, "the test only checked {checked} pixels");
    }

    #[test]
    fn a_corner_keeps_its_shape_instead_of_rounding_off() {
        // The point of the whole exercise. Diagonally outside a right-angle
        // corner, the true distance is radial (`d * sqrt(2)` at an offset of
        // `d` on both axes) — and a field storing that rounds the corner off
        // when interpolated. The median of an MSDF instead reports the
        // *pseudo*-distance `d`, the distance to the corner's extended edges,
        // which reconstructs a sharp corner.
        let shape = square(20.0);
        let transform = MsdfTransform {
            scale: 1.0,
            // Room below and left of the origin for the outside samples.
            translate: Vec2::splat(6.0),
        };
        let field = generate(&shape, 32, 32, transform, 12.0);
        // Bitmap (2.5, 2.5) is shape (-3.5, -3.5): 3.5 outside the corner on
        // both axes, well inside the field's range.
        let x = 2;
        let y = 32 - 2 - 1;
        let reconstructed = field.distance_at(x, y);
        assert!(
            (reconstructed + 3.5).abs() < 0.6,
            "expected the pseudo-distance -3.5, got {reconstructed}"
        );
        let radial = -3.5 * 2f32.sqrt();
        assert!(
            (reconstructed - radial).abs() > 1.0,
            "the field reported the radial distance {radial}, i.e. a rounded corner"
        );
    }

    #[test]
    fn corners_get_colors_that_share_exactly_one_channel() {
        let shape = square(10.0);
        let edges = color_edges(&shape);
        assert_eq!(edges.len(), 4);
        for index in 0..4 {
            let here = edges[index].color;
            let next = edges[(index + 1) % 4].color;
            assert_ne!(here, next, "a corner needs two different colours");
            let shared = (here.0 & next.0).count_ones();
            assert_eq!(shared, 1, "{here:?} and {next:?} share {shared} channels");
        }
    }

    #[test]
    fn a_smooth_contour_is_all_white() {
        let edges = color_edges(&circle(Vec2::splat(10.0), 5.0));
        assert_eq!(edges.len(), 4);
        assert!(edges.iter().all(|edge| edge.color == EdgeColor::WHITE));
    }

    #[test]
    fn an_odd_number_of_corners_still_differs_across_the_wrap_around() {
        // A triangle is the case a naive two-colour alternation gets wrong:
        // the third corner joins the last edge to the first.
        let corners = [
            Vec2::new(0.0, 0.0),
            Vec2::new(10.0, 0.0),
            Vec2::new(5.0, 8.0),
        ];
        let segments = (0..3)
            .map(|i| Segment::Line([corners[i], corners[(i + 1) % 3]]))
            .collect();
        let shape = Shape {
            contours: vec![Contour { segments }],
        };
        let edges = color_edges(&shape);
        assert_eq!(edges.len(), 3);
        for index in 0..3 {
            assert_ne!(edges[index].color, edges[(index + 1) % 3].color);
        }
    }

    #[test]
    fn a_tie_at_a_vertex_goes_to_the_edge_met_perpendicularly() {
        // The bug this pins down: two edges meeting at a vertex are exactly
        // the same distance from a point nearest that vertex, and at a reflex
        // vertex they disagree about which side of the outline it is on. The
        // edge the point is beyond the *end* of is the wrong one to believe.
        let perpendicular = SignedDistance {
            distance: -4.0,
            alignment: 0.05,
        };
        let end_on = SignedDistance {
            distance: 4.0,
            alignment: 0.97,
        };
        assert!(perpendicular.is_nearer_than(&end_on));
        assert!(!end_on.is_nearer_than(&perpendicular));
        // Distance still comes first.
        let nearer = SignedDistance {
            distance: 1.0,
            alignment: 0.99,
        };
        assert!(nearer.is_nearer_than(&perpendicular));
        // And anything beats nothing.
        assert!(nearer.is_nearer_than(&SignedDistance::FAR));
    }

    #[test]
    fn error_correction_only_fires_on_a_sign_disagreement() {
        // A corner: the median is the pseudo-distance, smaller in magnitude
        // than the true radial distance but on the same side. Must be left
        // alone, because this *is* the corner.
        let corner = [-3.5, -3.5, -3.5];
        assert_eq!(correct_error(corner, -4.95), corner);
        // A thin gap: two channels agree on "inside" because each found its
        // own nearest same-colour edge, while the truth is just outside.
        // That is a bridge across the gap, and it has to go.
        let bridged = [2.0, 1.5, -1.0];
        assert_eq!(correct_error(bridged, -0.8), [-0.8; 3]);
        // And right on the outline, nothing is corrected: both are ~0 and
        // flattening it would blunt the edge being drawn.
        assert_eq!(correct_error([0.0, 0.0, 0.0], -0.2), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn a_thin_gap_between_two_edges_stays_open() {
        // Two horizontal bars a hair apart: the shape of a `c`'s aperture,
        // and the configuration a multi-channel field can bridge over. The
        // sign-based correction is unit-tested above; this is the property
        // that has to hold of the whole generator.
        let bar = |y: f32| Contour {
            segments: vec![
                Segment::Line([Vec2::new(0.0, y), Vec2::new(20.0, y)]),
                Segment::Line([Vec2::new(20.0, y), Vec2::new(20.0, y + 3.0)]),
                Segment::Line([Vec2::new(20.0, y + 3.0), Vec2::new(0.0, y + 3.0)]),
                Segment::Line([Vec2::new(0.0, y + 3.0), Vec2::new(0.0, y)]),
            ],
        };
        let mut shape = Shape {
            contours: vec![bar(0.0), bar(4.5)],
        };
        shape.normalize_orientation();

        let transform = MsdfTransform {
            scale: 1.0,
            translate: Vec2::new(2.0, 2.0),
        };
        // A range wider than the 1.5-unit gap, which is what makes the
        // channels interfere in the first place.
        let field = generate(&shape, 24, 16, transform, 6.0);
        // The row whose pixel centres fall in the middle of the gap. Row 0
        // is the top of the bitmap and the shape's y axis points up, so a
        // shape-space y maps to `height - (y + translate) - 0.5`.
        let gap_center = 3.75f32;
        let row = (16.0 - (gap_center + transform.translate.y) - 0.5).round() as u32;
        for x in 4..18 {
            let distance = field.distance_at(x, row);
            assert!(
                distance < 0.0,
                "the gap is bridged at ({x}, {row}): distance {distance}"
            );
        }
    }

    #[test]
    fn degenerate_outlines_produce_a_field_rather_than_a_panic() {
        let transform = MsdfTransform {
            scale: 1.0,
            translate: Vec2::ZERO,
        };
        // Nothing at all.
        let empty = generate(&Shape::new(), 4, 4, transform, 2.0);
        assert_eq!(empty.pixels.len(), 4 * 4 * 3);
        assert!(empty.pixels.iter().all(|byte| *byte == 0), "all outside");

        // A contour of zero-length segments, and a curve whose control points
        // are all the same point: both appear in real fonts.
        let mut shape = Shape {
            contours: vec![Contour {
                segments: vec![
                    Segment::Line([Vec2::ZERO, Vec2::ZERO]),
                    Segment::Quad([Vec2::ZERO; 3]),
                    Segment::Cubic([Vec2::splat(1.0); 4]),
                ],
            }],
        };
        let field = generate(&shape, 4, 4, transform, 2.0);
        assert_eq!(field.pixels.len(), 4 * 4 * 3);

        shape.clean();
        assert!(shape.is_empty(), "cleaning drops every degenerate segment");
    }

    #[test]
    fn a_hole_is_outside_the_shape() {
        // An `o`: a big square with a smaller one wound the other way inside
        // it. The hole's centre must read as outside.
        let mut shape = square(20.0);
        let mut hole = square(8.0);
        hole.contours[0].reverse();
        for segment in &mut hole.contours[0].segments {
            if let Segment::Line([a, b]) = segment {
                *a += Vec2::splat(6.0);
                *b += Vec2::splat(6.0);
            }
        }
        shape.contours.push(hole.contours.remove(0));
        shape.normalize_orientation();

        let transform = MsdfTransform {
            scale: 1.0,
            translate: Vec2::ZERO,
        };
        let field = generate(&shape, 20, 20, transform, 6.0);
        // Shape (10, 10) is the middle of the hole; bitmap y is flipped.
        assert!(
            field.distance_at(10, 20 - 10 - 1) < 0.0,
            "the middle of the hole is outside the glyph"
        );
        // And the wall between the outer edge and the hole is inside.
        assert!(field.distance_at(2, 20 - 10 - 1) > 0.0);
    }
}

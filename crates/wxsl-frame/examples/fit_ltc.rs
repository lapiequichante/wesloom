//! Fit the linearly transformed cosines (LTC) that approximate GGX, and
//! write them as `shaders/ltc_table.wxsl`, the table the environment LUT
//! bake copies into its second channel pair (ADR 0096).
//!
//! The method is Heitz, Dupuy, Hill and Neubelt's ("Real-Time
//! Polygonal-Light Shading with Linearly Transformed Cosines", 2016),
//! reimplemented here: for each (N·V, roughness) texel, a Nelder-Mead
//! search for the transform whose clamped cosine best matches the
//! cosine-weighted BRDF, under a cubed error integrated with both
//! distributions' samples. Each fit starts from its neighbour's, rougher
//! and more frontal first.
//!
//! ```sh
//! cargo run -p wxsl-frame --release --example fit_ltc -- crates/wxsl-frame/shaders/ltc_table.wxsl
//! ```

use std::f64::consts::PI;
use std::fmt::Write as _;

use glam::{DMat3, DVec3};

/// Texels per side, the LUT's: x is N·V, y perceptual roughness, both at
/// texel centres.
const SIZE: usize = 64;
/// Error samples per dimension, per distribution.
const SAMPLES: usize = 32;
/// The smallest GGX alpha fitted; a mirror's transform is singular.
const MIN_ALPHA: f64 = 1e-4;

/// GGX with height-correlated Smith masking, F = 1: the BRDF times the
/// cosine, and the pdf of sampling `l` from the distribution of normals.
fn ggx(v: DVec3, l: DVec3, alpha: f64) -> (f64, f64) {
    if v.z <= 0.0 {
        return (0.0, 0.0);
    }
    let h = (v + l).normalize();
    if h.z <= 0.0 {
        return (0.0, 0.0);
    }
    let a2 = alpha * alpha;
    let cos2 = h.z * h.z;
    let tan2 = (1.0 - cos2) / cos2;
    let d = 1.0 / (PI * a2 * cos2 * cos2 * (1.0 + tan2 / a2).powi(2));
    let pdf = d * h.z / (4.0 * v.dot(h).abs().max(1e-12));
    if l.z <= 0.0 {
        return (0.0, pdf);
    }
    let lambda = |w: DVec3| {
        let t2 = (1.0 - w.z * w.z) / (w.z * w.z);
        (-1.0 + (1.0 + a2 * t2).sqrt()) * 0.5
    };
    let g2 = 1.0 / (1.0 + lambda(v) + lambda(l));
    (d * g2 / (4.0 * v.z), pdf)
}

/// A direction from GGX's distribution of normals, reflected about it.
fn ggx_sample(v: DVec3, alpha: f64, u1: f64, u2: f64) -> DVec3 {
    let phi = 2.0 * PI * u1;
    let r = alpha * (u2 / (1.0 - u2)).sqrt();
    let n = DVec3::new(r * phi.cos(), r * phi.sin(), 1.0).normalize();
    -v + 2.0 * n * n.dot(v)
}

/// A clamped cosine, transformed: `m` takes the cosine's space to the
/// shading frame, built as `frame * [[m11, 0, m13], [0, m22, 0], [0, 0, 1]]`.
#[derive(Clone, Copy)]
struct Ltc {
    m11: f64,
    m22: f64,
    m13: f64,
    frame: DMat3,
    magnitude: f64,
    m: DMat3,
    inverse: DMat3,
    determinant: f64,
}

impl Ltc {
    fn new(frame: DMat3, magnitude: f64) -> Self {
        let mut ltc = Ltc {
            m11: 1.0,
            m22: 1.0,
            m13: 0.0,
            frame,
            magnitude,
            m: DMat3::IDENTITY,
            inverse: DMat3::IDENTITY,
            determinant: 1.0,
        };
        ltc.update();
        ltc
    }

    fn update(&mut self) {
        let shape = DMat3::from_cols_array(&[
            self.m11, 0.0, 0.0, //
            0.0, self.m22, 0.0, //
            self.m13, 0.0, 1.0,
        ]);
        self.m = self.frame * shape;
        self.inverse = self.m.inverse();
        self.determinant = self.m.determinant().abs();
    }

    fn eval(&self, l: DVec3) -> f64 {
        let original = (self.inverse * l).normalize();
        let back = self.m * original;
        let length = back.length();
        let jacobian = self.determinant / (length * length * length);
        self.magnitude * original.z.max(0.0) / PI / jacobian
    }

    fn sample(&self, u1: f64, u2: f64) -> DVec3 {
        let theta = u1.sqrt().acos();
        let phi = 2.0 * PI * u2;
        let l = DVec3::new(
            theta.sin() * phi.cos(),
            theta.sin() * phi.sin(),
            theta.cos(),
        );
        (self.m * l).normalize()
    }
}

/// The cubed difference between the LTC and the BRDF, integrated with the
/// balance heuristic over both distributions' samples.
fn error(ltc: &Ltc, v: DVec3, alpha: f64) -> f64 {
    let mut sum = 0.0;
    let mut term = |l: DVec3| {
        let (brdf, pdf_brdf) = ggx(v, l, alpha);
        let fitted = ltc.eval(l);
        let pdf_ltc = fitted / ltc.magnitude;
        let difference = (brdf - fitted).abs();
        let denominator = pdf_ltc + pdf_brdf;
        if denominator > 0.0 {
            sum += difference * difference * difference / denominator;
        }
    };
    for j in 0..SAMPLES {
        for i in 0..SAMPLES {
            let u1 = (i as f64 + 0.5) / SAMPLES as f64;
            let u2 = (j as f64 + 0.5) / SAMPLES as f64;
            term(ltc.sample(u1, u2));
            term(ggx_sample(v, alpha, u1, u2));
        }
    }
    sum / (SAMPLES * SAMPLES) as f64
}

/// The BRDF's integral (the LTC's magnitude) and its mean direction.
fn moments(v: DVec3, alpha: f64) -> (f64, DVec3) {
    let mut norm = 0.0;
    let mut direction = DVec3::ZERO;
    for j in 0..SAMPLES {
        for i in 0..SAMPLES {
            let u1 = (i as f64 + 0.5) / SAMPLES as f64;
            let u2 = (j as f64 + 0.5) / SAMPLES as f64;
            let l = ggx_sample(v, alpha, u1, u2);
            let (brdf, pdf) = ggx(v, l, alpha);
            if pdf > 0.0 {
                norm += brdf / pdf;
                direction += l * brdf / pdf;
            }
        }
    }
    let count = (SAMPLES * SAMPLES) as f64;
    direction.y = 0.0;
    (norm / count, direction.normalize_or(DVec3::Z))
}

/// Minimise `f` from `start` with Nelder-Mead.
fn nelder_mead(f: impl Fn([f64; 3]) -> f64, start: [f64; 3], step: f64) -> [f64; 3] {
    let mut simplex = [start; 4];
    for (axis, vertex) in simplex.iter_mut().skip(1).enumerate() {
        vertex[axis] += step;
    }
    let mut values = simplex.map(&f);
    for _ in 0..400 {
        let mut order = [0, 1, 2, 3];
        order.sort_by(|a, b| values[*a].total_cmp(&values[*b]));
        simplex = order.map(|i| simplex[i]);
        values = order.map(|i| values[i]);
        if (values[3] - values[0]).abs() <= 1e-10 * values[0].abs().max(1e-30) {
            break;
        }
        let mut centroid = [0.0; 3];
        for vertex in &simplex[..3] {
            for k in 0..3 {
                centroid[k] += vertex[k] / 3.0;
            }
        }
        let towards = |t: f64| -> [f64; 3] {
            std::array::from_fn(|k| centroid[k] + t * (simplex[3][k] - centroid[k]))
        };
        let reflected = towards(-1.0);
        let reflected_value = f(reflected);
        if reflected_value < values[0] {
            let expanded = towards(-2.0);
            let expanded_value = f(expanded);
            (simplex[3], values[3]) = if expanded_value < reflected_value {
                (expanded, expanded_value)
            } else {
                (reflected, reflected_value)
            };
        } else if reflected_value < values[2] {
            (simplex[3], values[3]) = (reflected, reflected_value);
        } else {
            let contracted = if reflected_value < values[3] {
                towards(-0.5)
            } else {
                towards(0.5)
            };
            let contracted_value = f(contracted);
            if contracted_value < values[3].min(reflected_value) {
                (simplex[3], values[3]) = (contracted, contracted_value);
            } else {
                for i in 1..4 {
                    simplex[i] = std::array::from_fn(|k| {
                        simplex[0][k] + 0.5 * (simplex[i][k] - simplex[0][k])
                    });
                    values[i] = f(simplex[i]);
                }
            }
        }
    }
    let best = (0..4)
        .min_by(|a, b| values[*a].total_cmp(&values[*b]))
        .unwrap();
    simplex[best]
}

/// Fit `ltc`'s shape to GGX at `v`, from its current one. An isotropic fit
/// (normal incidence) ties m22 to m11 and holds m13 at zero. The search
/// runs over the logarithms of m11 and m22 and over m13 / m11, so its
/// steps are relative: a smooth surface's shape is a thousandth of a
/// rough one's.
fn fit(ltc: &mut Ltc, v: DVec3, alpha: f64, isotropic: bool) {
    let shaped = |ltc: &Ltc, p: [f64; 3]| {
        let mut shaped = *ltc;
        shaped.m11 = p[0].exp();
        shaped.m22 = if isotropic { shaped.m11 } else { p[1].exp() };
        shaped.m13 = if isotropic { 0.0 } else { p[2] * shaped.m11 };
        shaped.update();
        shaped
    };
    let best = nelder_mead(
        |p| error(&shaped(ltc, p), v, alpha),
        [ltc.m11.ln(), ltc.m22.ln(), ltc.m13 / ltc.m11],
        0.05,
    );
    *ltc = shaped(ltc, best);
}

/// How far the fit is from GGX: the L1 difference over the BRDF's
/// integral, estimated with the same samples.
fn relative_error(ltc: &Ltc, v: DVec3, alpha: f64) -> f64 {
    let mut sum = 0.0;
    let mut term = |l: DVec3| {
        let (brdf, pdf_brdf) = ggx(v, l, alpha);
        let fitted = ltc.eval(l);
        let denominator = fitted / ltc.magnitude + pdf_brdf;
        if denominator > 0.0 {
            sum += (brdf - fitted).abs() / denominator;
        }
    };
    for j in 0..SAMPLES {
        for i in 0..SAMPLES {
            let u1 = (i as f64 + 0.5) / SAMPLES as f64;
            let u2 = (j as f64 + 0.5) / SAMPLES as f64;
            term(ltc.sample(u1, u2));
            term(ggx_sample(v, alpha, u1, u2));
        }
    }
    sum / (SAMPLES * SAMPLES) as f64 / ltc.magnitude
}

/// One roughness row: the normal-incidence fit `frontal` seeds the most
/// frontal texel, and each texel the next one down in N·V.
fn fit_row(row: usize, frontal: (f64, f64)) -> (Vec<[f32; 4]>, f64) {
    let roughness = (row as f64 + 0.5) / SIZE as f64;
    let alpha = (roughness * roughness).max(MIN_ALPHA);
    let mut shape = (frontal.0, frontal.1, 0.0);
    let mut out = vec![[0.0; 4]; SIZE];
    let mut worst: f64 = 0.0;
    for column in (0..SIZE).rev() {
        let n_dot_v = (column as f64 + 0.5) / SIZE as f64;
        let v = DVec3::new((1.0 - n_dot_v * n_dot_v).sqrt(), 0.0, n_dot_v);
        let (magnitude, z) = moments(v, alpha);
        let x = DVec3::new(z.z, 0.0, -z.x);
        let mut ltc = Ltc::new(DMat3::from_cols(x, DVec3::Y, z), magnitude);
        (ltc.m11, ltc.m22, ltc.m13) = shape;
        ltc.update();
        fit(&mut ltc, v, alpha, false);
        worst = worst.max(relative_error(&ltc, v, alpha));
        shape = (ltc.m11, ltc.m22, ltc.m13);
        // The shader rebuilds the inverse from four entries, scaled so the
        // y one is 1: the integral is scale-invariant, and the y row and
        // column are otherwise zero by symmetry.
        let inverse = ltc.inverse * (1.0 / ltc.inverse.y_axis.y);
        out[column] = [
            inverse.x_axis.x as f32,
            inverse.z_axis.x as f32,
            inverse.x_axis.z as f32,
            inverse.z_axis.z as f32,
        ];
    }
    (out, worst)
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "crates/wxsl-frame/shaders/ltc_table.wxsl".to_string());
    // Normal incidence, roughest first: each seeds the next smoother one.
    let mut frontal = vec![(1.0, 1.0); SIZE];
    let mut seed = 1.0;
    for row in (0..SIZE).rev() {
        let roughness = (row as f64 + 0.5) / SIZE as f64;
        let alpha = (roughness * roughness).max(MIN_ALPHA);
        let (magnitude, _) = moments(DVec3::Z, alpha);
        let mut ltc = Ltc::new(DMat3::IDENTITY, magnitude);
        (ltc.m11, ltc.m22) = (seed, seed);
        ltc.update();
        fit(&mut ltc, DVec3::Z, alpha, true);
        seed = ltc.m11;
        frontal[row] = (ltc.m11, ltc.m22);
    }
    let fitted: Vec<(Vec<[f32; 4]>, f64)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..SIZE)
            .map(|row| {
                let frontal = frontal[row];
                scope.spawn(move || fit_row(row, frontal))
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for (row, (_, worst)) in fitted.iter().enumerate().step_by(8) {
        println!("roughness row {row:2}: worst relative L1 error {worst:.3}");
    }
    let rows: Vec<&Vec<[f32; 4]>> = fitted.iter().map(|(row, _)| row).collect();

    let mut text = String::new();
    text.push_str(
        "//! ltc_table — the inverse LTC transforms that approximate GGX\n\
         //! (ADR 0096). Generated by `cargo run -p wxsl-frame --release\n\
         //! --example fit_ltc`; do not edit.\n\
         //!\n\
         //! One entry per environment-LUT texel, row-major, x = N·V and\n\
         //! y = perceptual roughness at texel centres: the inverse matrix's\n\
         //! (00, 02, 20, 22) entries, in the shading frame whose x axis is\n\
         //! the view projected onto the surface, scaled so its 11 entry is 1.\n\n",
    );
    let _ = writeln!(text, "const LTC_SIZE: u32 = {SIZE}u;");
    let _ = writeln!(text, "const LTC_TABLE = array<vec4f, {}>(", SIZE * SIZE);
    for row in rows {
        for entry in row {
            let _ = writeln!(
                text,
                "    vec4f({:.5e}, {:.5e}, {:.5e}, {:.5e}),",
                entry[0], entry[1], entry[2], entry[3]
            );
        }
    }
    text.push_str(");\n");
    std::fs::write(&path, text).expect("write the table");
    println!("wrote {path}");
}

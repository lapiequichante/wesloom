//! Order-3 real spherical harmonics: the basis, an environment's
//! projection, and the irradiance it gives.
//!
//! The coefficient order is the one `lighting.prt_diffuse_occlusion`
//! reads: band 0, then band 1 as (y, z, x), then band 2 as
//! (xy, yz, 3z² − 1, xz, x² − y²).

use glam::Vec3;

/// Nine coefficients per colour channel.
pub type Sh9 = [[f32; 3]; 9];

/// The basis at a unit direction.
pub fn basis(d: Vec3) -> [f32; 9] {
    [
        0.282_095,
        0.488_603 * d.y,
        0.488_603 * d.z,
        0.488_603 * d.x,
        1.092_548 * d.x * d.y,
        1.092_548 * d.y * d.z,
        0.315_392 * (3.0 * d.z * d.z - 1.0),
        1.092_548 * d.x * d.z,
        0.546_274 * (d.x * d.x - d.y * d.y),
    ]
}

/// The clamped cosine's weight per band (π, 2π/3, π/4), per coefficient.
const COSINE: [f32; 9] = {
    let (a0, a1, a2) = (
        std::f32::consts::PI,
        std::f32::consts::TAU / 3.0,
        std::f32::consts::FRAC_PI_4,
    );
    [a0, a1, a1, a1, a2, a2, a2, a2, a2]
};

/// The irradiance `sh` gives a surface facing `normal`, unshadowed: what
/// the diffuse node divides by.
pub fn irradiance(sh: &Sh9, normal: Vec3) -> [f32; 3] {
    let y = basis(normal.normalize_or_zero());
    let mut out = [0.0; 3];
    for (index, coefficient) in sh.iter().enumerate() {
        for channel in 0..3 {
            out[channel] += coefficient[channel] * y[index] * COSINE[index];
        }
    }
    out
}

/// A baked transfer applied to `sh`: the shadowed irradiance.
pub fn transfer(sh: &Sh9, transfer: &[f32; 9]) -> [f32; 3] {
    let mut out = [0.0; 3];
    for (coefficient, weight) in sh.iter().zip(transfer) {
        for channel in 0..3 {
            out[channel] += coefficient[channel] * weight;
        }
    }
    out
}

/// The direction an equirectangular texel centre looks along, inverting
/// the renderer's mapping (`wxsl.equirect_to_cube`): u is the longitude
/// `atan2(z, x)` and v runs from +y at the top to −y at the bottom.
pub fn equirect_direction(u: f32, v: f32) -> Vec3 {
    let longitude = (u - 0.5) * std::f32::consts::TAU;
    let latitude = (0.5 - v) * std::f32::consts::PI;
    Vec3::new(
        latitude.cos() * longitude.cos(),
        latitude.sin(),
        latitude.cos() * longitude.sin(),
    )
}

/// Project linear radiance in an equirectangular image, row-major from
/// the top, onto the basis.
pub fn project_equirect(width: u32, height: u32, pixels: &[[f32; 3]]) -> Sh9 {
    assert_eq!(
        pixels.len(),
        (width * height) as usize,
        "pixels for {width}×{height}"
    );
    let mut sh = [[0.0f64; 3]; 9];
    let texel = std::f64::consts::TAU / f64::from(width) * std::f64::consts::PI / f64::from(height);
    for row in 0..height {
        let v = (row as f32 + 0.5) / height as f32;
        // The solid angle of a texel shrinks with the cosine of its latitude.
        let area = texel * f64::from(((0.5 - v) * std::f32::consts::PI).cos());
        for column in 0..width {
            let direction = equirect_direction((column as f32 + 0.5) / width as f32, v);
            let radiance = pixels[(row * width + column) as usize];
            for (coefficient, y) in sh.iter_mut().zip(basis(direction)) {
                for channel in 0..3 {
                    coefficient[channel] += f64::from(radiance[channel] * y) * area;
                }
            }
        }
    }
    sh.map(|coefficient| coefficient.map(|value| value as f32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mapping_inverts_the_renderers() {
        for direction in [
            Vec3::X,
            Vec3::Y,
            Vec3::Z,
            Vec3::new(-0.3, 0.4, -0.8).normalize(),
        ] {
            // The forward mapping of `equirect_to_cube.wxsl`.
            let u = direction.z.atan2(direction.x) / std::f32::consts::TAU + 0.5;
            let v = 0.5 - direction.y.clamp(-1.0, 1.0).asin() / std::f32::consts::PI;
            let back = equirect_direction(u, v);
            assert!(
                back.distance(direction) < 1e-5,
                "{direction} came back as {back}"
            );
        }
    }

    #[test]
    fn a_uniform_sky_projects_onto_band_zero_and_irradiates_pi() {
        let (width, height) = (128, 64);
        let sh = project_equirect(
            width,
            height,
            &vec![[1.0, 0.5, 0.25]; (width * height) as usize],
        );
        let expected = 0.282_095 * 4.0 * std::f32::consts::PI;
        assert!((sh[0][0] - expected).abs() < 1e-3, "{}", sh[0][0]);
        for coefficient in &sh[1..] {
            assert!(coefficient[0].abs() < 1e-3, "{coefficient:?}");
        }
        let lit = irradiance(&sh, Vec3::new(0.2, 0.9, -0.1));
        assert!((lit[0] - std::f32::consts::PI).abs() < 1e-2, "{lit:?}");
        assert!(
            (lit[2] - std::f32::consts::FRAC_PI_4).abs() < 1e-2,
            "{lit:?}"
        );
    }

    #[test]
    fn a_sky_lit_from_above_lights_upward_faces() {
        let (width, height) = (128, 64);
        let pixels: Vec<[f32; 3]> = (0..width * height)
            .map(|index| {
                if index / width < height / 2 {
                    [1.0; 3]
                } else {
                    [0.0; 3]
                }
            })
            .collect();
        let sh = project_equirect(width, height, &pixels);
        let up = irradiance(&sh, Vec3::Y)[0];
        let down = irradiance(&sh, -Vec3::Y)[0];
        // A sky hemisphere irradiates an upward face with π.
        assert!((up - std::f32::consts::PI).abs() < 0.1, "{up}");
        assert!(down.abs() < 0.1, "{down}");
    }
}

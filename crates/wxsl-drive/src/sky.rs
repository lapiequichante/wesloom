//! The time of day: where the sun and the moon are, what colour reaches
//! the ground through the atmosphere, and the sky effect that turns both
//! into the equirectangular radiance the IBL is baked from.
//!
//! The sky is `lighting.sky_single_scattering` (ADR 0060), integrated once
//! for the sun and once for the moon, plus a faint night glow; it is
//! re-baked only when the hour changes.

use glam::Vec3;
use wxsl::core::Value;
use wxsl::render::effect::{Downlevel, Effect, EffectKind, EffectParameter, EffectShader};

/// The latitude the sun is seen from, and its declination (late spring).
const LATITUDE: f32 = 38.0;
const DECLINATION: f32 = 16.0;
/// Rayleigh optical depth at the zenith (scattering × scale height), and
/// Mie's with its absorption: the same medium the sky node integrates.
const RAYLEIGH_DEPTH: Vec3 = Vec3::new(0.0058 * 8.0, 0.0135 * 8.0, 0.0331 * 8.0);
const MIE_DEPTH: f32 = (0.0044 + 0.00044) * 1.2;
/// The sun's irradiance in the frame's units, outside the atmosphere.
const SUN: f32 = 3.2;
/// The full moon's, tinted by the eye's night shift rather than physics.
const MOON: Vec3 = Vec3::new(0.05, 0.065, 0.1);
/// What the sky effect is given, per unit of light irradiance.
const SKY_IRRADIANCE: f32 = 6.0;

/// The size of the baked sky, which the shader assumes.
pub const SKY_WIDTH: u32 = 1024;
pub const SKY_HEIGHT: u32 = 512;
/// The sky's pass label, whose parameters the hour sets.
pub const SKY_PASS: &str = "drive sky";

/// Everything the hour decides.
#[derive(Clone, Copy, Debug)]
pub struct DayLight {
    /// Towards the sun.
    pub sun: Vec3,
    /// Its colour and intensity at the ground; zero below the horizon.
    pub sun_color: Vec3,
    /// Towards the moon.
    pub moon: Vec3,
    pub moon_color: Vec3,
    /// 0 in daylight, 1 at night: lamps, headlights and windows.
    pub lights_on: f32,
    /// The camera's exposure: more at night, the way an eye adapts.
    pub exposure: f32,
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A body at hour angle `hour_angle` (radians, 0 at its highest) and
/// declination `declination` (degrees), as a direction: x east, y up,
/// z south.
fn direction(hour_angle: f32, declination: f32) -> Vec3 {
    let (phi, delta) = (LATITUDE.to_radians(), declination.to_radians());
    let up = phi.sin() * delta.sin() + phi.cos() * delta.cos() * hour_angle.cos();
    let east = -delta.cos() * hour_angle.sin();
    let north = phi.cos() * delta.sin() - phi.sin() * delta.cos() * hour_angle.cos();
    Vec3::new(east, up, -north).normalize()
}

/// What survives the atmosphere towards `direction`, per channel.
fn transmittance(direction: Vec3) -> Vec3 {
    // Kasten and Young's air mass, which stays finite at the horizon.
    let elevation = direction.y.clamp(-1.0, 1.0).asin().to_degrees().max(-2.0);
    let air_mass = 1.0
        / (elevation.to_radians().sin().max(0.0) + 0.50572 * (elevation + 6.07995).powf(-1.6364));
    let depth = (RAYLEIGH_DEPTH + Vec3::splat(MIE_DEPTH)) * air_mass;
    Vec3::new((-depth.x).exp(), (-depth.y).exp(), (-depth.z).exp())
}

impl DayLight {
    /// The light at `hours` (0..24, local solar time).
    pub fn at(hours: f32) -> Self {
        let hour_angle = (hours - 12.0) / 24.0 * std::f32::consts::TAU;
        let sun = direction(hour_angle, DECLINATION);
        // A full moon: opposite the sun in hour angle and declination.
        let moon = direction(hour_angle + std::f32::consts::PI, -DECLINATION);
        let above = |direction: Vec3| smoothstep(-0.03, 0.05, direction.y);
        let sun_color = transmittance(sun) * SUN * above(sun);
        let moon_color = MOON * transmittance(moon) * above(moon) * (1.0 - above(sun));
        let day = smoothstep(-0.12, 0.25, sun.y);
        DayLight {
            sun,
            sun_color,
            moon,
            moon_color,
            lights_on: 1.0 - smoothstep(-0.02, 0.12, sun.y),
            exposure: 1.2 + (1.0 - 1.2) * day,
        }
    }

    /// The light that casts the shadows: the sun while it is up, the moon
    /// otherwise.
    pub fn key(&self) -> (Vec3, Vec3) {
        if self.sun.y > -0.02 {
            (self.sun, self.sun_color)
        } else {
            (self.moon, self.moon_color)
        }
    }

    /// The sky pass's parameters, by name.
    pub fn sky_parameters(&self) -> [(&'static str, Value); 3] {
        let body = |direction: Vec3, irradiance: f32| {
            Value::Vec4([direction.x, direction.y, direction.z, irradiance])
        };
        [
            ("sun", body(self.sun, SUN * SKY_IRRADIANCE)),
            ("moon", body(self.moon, MOON.z * SKY_IRRADIANCE * 0.12)),
            ("night", Value::F32(self.lights_on)),
        ]
    }
}

/// The sky: Rayleigh/Mie single scattering for the sun and the moon,
/// into an equirectangular target of [`SKY_WIDTH`] × [`SKY_HEIGHT`].
pub const SKY: Effect = Effect {
    id: "drive.sky",
    label: "sky",
    description: "Rayleigh/Mie sky for the sun and the moon, with a night glow, equirectangular.",
    kind: EffectKind::Screen {
        vertex_entry: "drive_sky_vs",
        fragment_entry: "drive_sky_fs",
    },
    inputs: &[],
    outputs: &[],
    parameters: &[
        EffectParameter {
            name: "sun",
            default: Value::Vec4([0.0, 1.0, 0.0, 20.0]),
        },
        EffectParameter {
            name: "moon",
            default: Value::Vec4([0.0, -1.0, 0.0, 0.0]),
        },
        EffectParameter {
            name: "night",
            default: Value::F32(0.0),
        },
    ],
    shader: EffectShader::Source {
        path: "package::drive::sky",
        wxsl: include_str!("../shaders/sky.wxsl"),
    },
    downlevel: Downlevel::Portable,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noon_is_high_and_midnight_is_dark() {
        let noon = DayLight::at(12.0);
        assert!(noon.sun.y > 0.8);
        assert!(noon.lights_on < 1e-3);
        assert!(noon.sun_color.min_element() > 1.5);
        let midnight = DayLight::at(0.0);
        assert!(midnight.sun.y < -0.3);
        assert_eq!(midnight.sun_color, Vec3::ZERO);
        assert!(midnight.lights_on > 0.999);
        assert!(midnight.moon.y > 0.3);
        assert!(midnight.exposure > noon.exposure);
    }

    #[test]
    fn the_sun_rises_in_the_east_and_reddens_low() {
        let morning = DayLight::at(6.8);
        assert!(morning.sun.x > 0.5 && morning.sun.y > 0.0);
        let evening = DayLight::at(18.6);
        assert!(evening.sun.x < -0.5);
        let low = transmittance(Vec3::new(1.0, 0.03, 0.0).normalize());
        assert!(low.x > low.z * 4.0, "{low:?}");
    }

    #[test]
    fn the_key_light_switches_to_the_moon_at_night() {
        assert_eq!(DayLight::at(12.0).key().0, DayLight::at(12.0).sun);
        assert_eq!(DayLight::at(1.0).key().0, DayLight::at(1.0).moon);
    }
}

//! The lights: the sun or the moon (the one that casts shadows), a street
//! lamp over every few dozen metres of road, and the car's headlights and
//! tail lights. The lamps and the headlights are shaped by IES profiles
//! baked into the cookie array (ADR 0097).
//!
//! The headlight profile is `resources/ies/headlight.ies`. The lamps'
//! is written here, as an LM-63 file, and read back through the same
//! parser: a full-cutoff "bat wing" that throws its peak along the road
//! at about 58° from straight down and nothing above 80°.

use std::error::Error;
use std::fmt::Write as _;

use glam::{Mat4, Vec3};
use wxsl::core::scene::TagExpr;
use wxsl::render::environment::IesProfile;
use wxsl::render::{GpuContext, Light};

use crate::road::Lamp;
use crate::sky::DayLight;

/// The cookie array's layers.
const HEADLIGHT_LAYER: u32 = 0;
const LAMP_LAYER: u32 = 1;
const TAIL_LAYER: u32 = 2;
const COOKIE_SIZE: u32 = 512;

/// A lamp's peak intensity, its colour, and how far it reaches: past
/// the deck halfway to the next lamp, short of the ground under it
/// (`terrain::CLEARANCE`), which the deck would shade.
const LAMP_INTENSITY: f32 = 450.0;
const LAMP_COLOR: Vec3 = Vec3::new(1.0, 0.8, 0.58);
pub const LAMP_RANGE: f32 = 26.0;
/// A headlight's peak intensity and reach.
const HEADLIGHT_INTENSITY: f32 = 9000.0;
const HEADLIGHT_RANGE: f32 = 160.0;
const HEADLIGHT_COLOR: Vec3 = Vec3::new(0.95, 0.97, 1.0);
/// A tail light's peak intensity, its colour and its reach: barely a
/// glow on the road behind.
const TAIL_INTENSITY: f32 = 0.15;
const TAIL_COLOR: Vec3 = Vec3::new(1.0, 0.04, 0.02);
const TAIL_RANGE: f32 = 6.0;

/// The street lamp's profile as an LM-63 file: type C, symmetric about
/// its axis, candela over γ from straight down.
pub fn lamp_ies() -> String {
    let gammas: Vec<f32> = (0..=36).map(|step| step as f32 * 2.5).collect();
    let candela: Vec<f32> = gammas
        .iter()
        .map(|&gamma| {
            let wing = (-((gamma - 58.0) / 11.0).powi(2)).exp();
            let cutoff = ((80.0 - gamma) / 8.0).clamp(0.0, 1.0);
            (4500.0 * (0.3 + 0.7 * wing) * cutoff).round()
        })
        .collect();
    lm63("full-cutoff street lamp, symmetric", &gammas, &candela)
}

/// The tail light's profile: brightest along its axis, gone by 65°.
pub fn tail_ies() -> String {
    let gammas: Vec<f32> = (0..=36).map(|step| step as f32 * 2.5).collect();
    let candela: Vec<f32> = gammas
        .iter()
        .map(|&gamma| {
            let t = (gamma / 65.0).clamp(0.0, 1.0);
            (100.0 * (1.0 - t * t).powi(2)).round()
        })
        .collect();
    lm63("tail light, symmetric", &gammas, &candela)
}

/// A type C, rotationally symmetric LM-63 file of `candela` over
/// `gammas` from the axis.
fn lm63(luminaire: &str, gammas: &[f32], candela: &[f32]) -> String {
    let mut text = format!(
        "IESNA:LM-63-2002\n[TEST] synthetic, written by the wxsl-drive demo\n\
         [MANUFAC] wxsl\n[LUMINAIRE] {luminaire}\nTILT=NONE\n"
    );
    let _ = writeln!(text, "1 -1 1 {} 1 1 2 0.5 0.5 0.1", gammas.len());
    text.push_str("1.0 1.0 150\n");
    let line = |values: &[f32]| {
        values
            .iter()
            .map(|value| format!("{value}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let _ = writeln!(text, "{}", line(gammas));
    text.push_str("0\n");
    let _ = writeln!(text, "{}", line(candela));
    text
}

/// `value` as IEEE half-float bits, for values in 0..=1.
fn f16_bits(value: f32) -> u16 {
    if value < 6.1e-5 {
        return 0;
    }
    let bits = value.min(1.0).to_bits();
    let exponent = ((bits >> 23) & 0xff) + 15 - 127;
    ((exponent << 10) | ((bits >> 13) & 0x3ff)) as u16
}

/// Bake the profiles into a cookie array, one layer each.
pub fn cookies(gpu: &GpuContext) -> Result<wgpu::TextureView, Box<dyn Error>> {
    let headlight = IesProfile::parse(include_str!("../../../resources/ies/headlight.ies"))?;
    let lamp = IesProfile::parse(&lamp_ies())?;
    let tail = IesProfile::parse(&tail_ies())?;
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("drive light profiles"),
        size: wgpu::Extent3d {
            width: COOKIE_SIZE,
            height: COOKIE_SIZE,
            depth_or_array_layers: 3,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (layer, profile) in [
        (HEADLIGHT_LAYER, &headlight),
        (LAMP_LAYER, &lamp),
        (TAIL_LAYER, &tail),
    ] {
        let texels: Vec<u8> = profile
            .texels(COOKIE_SIZE, COOKIE_SIZE)
            .into_iter()
            .flat_map(|value| {
                let bits = f16_bits(value).to_le_bytes();
                [bits, bits, bits, f16_bits(1.0).to_le_bytes()].concat()
            })
            .collect();
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &texels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(COOKIE_SIZE * 8),
                rows_per_image: Some(COOKIE_SIZE),
            },
            wgpu::Extent3d {
                width: COOKIE_SIZE,
                height: COOKIE_SIZE,
                depth_or_array_layers: 1,
            },
        );
    }
    Ok(texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    }))
}

/// Where the car's lamps are, in its own space (forward is -z).
#[derive(Clone, Copy, Debug)]
pub struct CarLamps {
    /// The two headlights, left then right.
    pub head: [Vec3; 2],
    /// The two tail lights.
    pub tail: [Vec3; 2],
}

/// How near a head of the right-hand row must be to the car for its
/// shadow: full inside the first distance, fading out by the second.
/// Lamps stand [`crate::road::LAMP_SPACING`] apart, so at most two heads
/// of the row are ever inside the second.
const LAMP_SHADOW_NEAR: f32 = 14.0;
const LAMP_SHADOW_FAR: f32 = 26.0;
/// How many lamp heads cast shadows at once: six atlas tiles each.
const SHADOWED_LAMPS: usize = 2;
/// The tags on the car's draws and on the deck's: what the headlights'
/// shadows draw, so the deck keeps their beams off the piers below.
pub const CAR_TAG: &str = "car";
pub const DECK_TAG: &str = "deck";

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The frame's lights: the key light, the car's, and the lamps near
/// `eye`, the ones over the car casting shadows.
///
/// Shadow tiles go in list order, largest first (`abi::shadow_tile`):
/// by day all to the sun's cascades; at night first to the headlights,
/// then to the lamps over the car, the moon's faint shadow last.
pub fn lights(
    day: &DayLight,
    lamps: &[Lamp],
    car: Mat4,
    car_lamps: &CarLamps,
    eye: Vec3,
) -> Vec<Light> {
    let (key, key_color) = day.key();
    let mut key = Light::directional(key, key_color.normalize_or_zero(), key_color.length())
        .casting_shadow(30.0)
        .with_cascades(4)
        .with_source_radius(0.0047);
    key.shadow_distance = 140.0;
    key.shadow_normal_bias = 0.05;
    if day.lights_on <= 0.0 {
        return vec![key];
    }
    let mut lights = Vec::new();
    let forward = car.transform_vector3(Vec3::NEG_Z).normalize();
    for position in car_lamps.head {
        // Straight ahead and a touch down; the profile holds the beam.
        let aim = (forward - car.transform_vector3(Vec3::Y).normalize() * 0.012).normalize();
        let mut headlight = Light::spot(
            car.transform_point3(position),
            aim,
            35f32.to_radians(),
            55f32.to_radians(),
            HEADLIGHT_COLOR,
            HEADLIGHT_INTENSITY * day.lights_on,
        )
        .with_profile(HEADLIGHT_LAYER)
        .with_range(HEADLIGHT_RANGE)
        .casting_shadow(1.0)
        .with_source_radius(0.04)
        .with_normal_bias(0.04)
        // The car and the deck: the deck streams past the light, so the
        // shadow is redrawn every frame, of these two only.
        .with_shadow_casters(TagExpr::tag(CAR_TAG).or(TagExpr::tag(DECK_TAG)));
        headlight.shadow_distance = HEADLIGHT_RANGE;
        lights.push(headlight);
    }

    // Each head of the right-hand row (the car's carriageway) near the
    // car is two lights: a shadowed one carrying `shadow` of its intensity
    // and a local one the rest, so a lamp's shadow fades in and out
    // instead of switching.
    let at = car.transform_point3(Vec3::ZERO);
    let lamp = |head: Vec3, share: f32| {
        Light::point(
            head - Vec3::Y * 0.2,
            LAMP_COLOR,
            LAMP_INTENSITY * day.lights_on * share,
        )
        .with_profile(LAMP_LAYER)
        .with_range(LAMP_RANGE)
    };
    let mut near: Vec<(Vec3, f32)> = lamps
        .iter()
        .map(|lamp| lamp.heads[1])
        .map(|head| {
            (
                head,
                1.0 - smoothstep(LAMP_SHADOW_NEAR, LAMP_SHADOW_FAR, head.distance(at)),
            )
        })
        .filter(|(_, shadow)| *shadow > 0.0)
        .collect();
    near.sort_by(|a, b| b.1.total_cmp(&a.1));
    near.truncate(SHADOWED_LAMPS);
    for &(head, shadow) in &near {
        let mut light = lamp(head, shadow)
            .casting_shadow(1.0)
            .with_source_radius(0.15)
            .with_normal_bias(0.06);
        light.shadow_distance = LAMP_RANGE;
        lights.push(light);
    }
    lights.push(key);

    // The tail lights: a faint red wash on the road just behind, from
    // spots aimed back and down, so they never reach the middle of the
    // road.
    let up = car.transform_vector3(Vec3::Y).normalize();
    for position in car_lamps.tail {
        lights.push(
            Light::spot(
                car.transform_point3(position),
                (-forward - up * 0.45).normalize(),
                25f32.to_radians(),
                60f32.to_radians(),
                TAIL_COLOR,
                TAIL_INTENSITY * day.lights_on,
            )
            .with_profile(TAIL_LAYER)
            .with_range(TAIL_RANGE),
        );
    }
    // The lamps within reach of what the camera sees well; the light grid
    // bins them (ADR 0076).
    for lamp_at in lamps {
        if lamp_at.foot.distance(eye) > 700.0 {
            continue;
        }
        for head in lamp_at.heads {
            let shadowed = near
                .iter()
                .find(|(near, _)| *near == head)
                .map_or(0.0, |(_, shadow)| *shadow);
            if shadowed < 1.0 {
                lights.push(lamp(head, 1.0 - shadowed));
            }
        }
    }
    lights
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lamp_profile_parses_and_peaks_off_axis() {
        let profile = IesProfile::parse(&lamp_ies()).expect("our own file reads back");
        let peak = profile.max_candela();
        assert!((profile.candela(57.5, 0.0) - peak).abs() < peak * 0.02);
        assert!(profile.candela(0.0, 0.0) < peak * 0.5);
        // Full cutoff: nothing at or above the horizon.
        assert_eq!(profile.candela(90.0, 45.0), 0.0);
        assert_eq!(profile.candela(120.0, 0.0), 0.0);
    }

    #[test]
    fn night_brings_the_lamps_and_day_only_the_sun() {
        let lamps = crate::road::Road::new().lamps();
        let car = CarLamps {
            head: [Vec3::new(-0.6, 0.6, -2.2), Vec3::new(0.6, 0.6, -2.2)],
            tail: [Vec3::new(-0.6, 0.7, 2.2), Vec3::new(0.6, 0.7, 2.2)],
        };
        let eye = lamps[0].foot;
        let day = lights(&DayLight::at(12.0), &lamps, Mat4::IDENTITY, &car, eye);
        assert_eq!(day.len(), 1);
        assert!(day[0].casts_shadow);
        let night = lights(&DayLight::at(23.0), &lamps, Mat4::IDENTITY, &car, eye);
        assert!(night.len() > 10);
        assert!(night
            .iter()
            .filter(|light| light.range > 0.0)
            .all(|light| light.profile_layer.is_some()));
        // The headlights, the key and the lamp heads over the car cast
        // shadows; everything else goes to the light grid.
        let shadowed = night.iter().filter(|light| light.casts_shadow).count();
        assert!((3..=3 + SHADOWED_LAMPS).contains(&shadowed));
        assert_eq!(
            night.iter().filter(|light| !light.is_local()).count(),
            shadowed
        );
    }

    #[test]
    fn a_lamp_lights_the_deck_to_the_next_but_never_the_ground_under_it() {
        use crate::road::{LAMP_HEIGHT, LAMP_SPACING};
        let height = LAMP_HEIGHT - 0.2;
        assert!(height + crate::terrain::CLEARANCE >= LAMP_RANGE);
        // Halfway along, at the far edge of its carriageway.
        let midway = Vec3::new(LAMP_SPACING * 0.5, height, 6.1).length();
        assert!(midway < LAMP_RANGE * 0.85, "{midway} m");
    }

    #[test]
    fn a_lamp_shadow_fades_in_without_changing_its_light() {
        let lamps = crate::road::Road::new().lamps();
        let car = CarLamps {
            head: [Vec3::ZERO; 2],
            tail: [Vec3::ZERO; 2],
        };
        let head = lamps[1].heads[0];
        let night = DayLight::at(23.0);
        for offset in [0.0, 8.0, 12.0, 15.0, 30.0] {
            let at = Mat4::from_translation(head - Vec3::Y * 9.0 + Vec3::X * offset);
            let total: f32 = lights(&night, &lamps, at, &car, head)
                .iter()
                .filter(|light| light.position_or_direction.distance(head) < 0.5)
                .map(|light| light.intensity)
                .sum();
            assert!(
                (total - LAMP_INTENSITY * night.lights_on).abs() < 1e-2,
                "{offset}: {total}"
            );
        }
    }
}

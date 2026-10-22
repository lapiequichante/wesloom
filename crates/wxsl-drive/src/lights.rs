//! The lights: the sun or the moon (the one that casts shadows), a street
//! lamp over every few dozen metres of road, and the car's headlights and
//! tail lights. The lamps and the headlights are shaped by IES profiles
//! baked into the cookie array (ADR 0097).
//!
//! The headlight profile is `resources/ies/headlight.ies`, the lamps'
//! `resources/ies/street_lamp.ies`: a wide batwing, drawn so the lamps,
//! range window included, light the carriageway evenly, and nothing past
//! 70°. Either file can be swapped for a measured one. The tail
//! lights' is written here, as an LM-63 file read back through the same
//! parser.

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
const LAMP_INTENSITY: f32 = 6500.0;
const LAMP_COLOR: Vec3 = Vec3::new(1.0, 0.8, 0.58);
pub const LAMP_RANGE: f32 = 26.0;
/// A lamp head's tilt from straight down towards its street side, as
/// roadway luminaires are mounted. It also orients the profile: an axis
/// this far from vertical puts C = 0 along the road and C = 90° across it,
/// towards the street side (`light_profile` in `bindings.wxsl`), where a
/// vertical one would fix C = 0 to world -x.
const LAMP_TILT: f32 = 10.0 * std::f32::consts::PI / 180.0;
/// A lamp's cone from straight down: wide enough that everything up to a
/// car's roof within its range is inside, so it is a spot with one shadow
/// tile rather than a point light's six. Its profile reaches 80°, but
/// past this angle it only lights deck beyond its range.
const LAMP_CONE: f32 = 72.0 * std::f32::consts::PI / 180.0;
/// A headlight's peak intensity and reach.
const HEADLIGHT_INTENSITY: f32 = 4500.0;
const HEADLIGHT_RANGE: f32 = 160.0;
const HEADLIGHT_COLOR: Vec3 = Vec3::new(0.92, 0.97, 1.0);
/// A tail light's peak intensity, its colour and its reach: barely a
/// glow on the road behind.
const TAIL_INTENSITY: f32 = 0.35;
const TAIL_COLOR: Vec3 = Vec3::new(1.0, 0.04, 0.02);
const TAIL_RANGE: f32 = 6.0;

/// The street lamp's profile: type C, γ from straight down.
const LAMP_IES: &str = include_str!("../../../resources/ies/street_lamp.ies");

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
    let lamp = IesProfile::parse(LAMP_IES)?;
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
    /// How bright the tail lights are: 1 at night, more braking.
    pub tail_glow: f32,
}

/// How near a lamp head, of either carriageway, must be to the car for its
/// shadow: full while its light reaches the car at all, so nothing lights
/// the car's insides through its gaps, then fading out by the second
/// distance, where its light is only on the road. Lamps stand
/// [`crate::road::LAMP_SPACING`] apart, so at most three posts, six
/// heads, are ever inside it.
const LAMP_SHADOW_NEAR: f32 = LAMP_RANGE;
const LAMP_SHADOW_FAR: f32 = LAMP_RANGE + 12.0;
/// How many lamp heads cast shadows at once: one atlas tile each.
const SHADOWED_LAMPS: usize = 6;
/// The tags on the car's draws and on the deck's: what the headlights'
/// shadows draw, so the deck keeps their beams off the piers below.
pub const CAR_TAG: &str = "car";
pub const DECK_TAG: &str = "deck";
/// The tag on the car's decimated stand-in, which casts its shadows in its
/// place and is drawn by no other pass.
pub const CAR_SHADOW_TAG: &str = "car_shadow";

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A lamp's heads, each with its street side: away from the median.
fn heads(lamp: &Lamp) -> [(Vec3, Vec3); 2] {
    [(lamp.heads[0], -lamp.across), (lamp.heads[1], lamp.across)]
}

/// The frame's lights: the key light, the car's, and the lamps near
/// `eye`, the ones over the car casting shadows.
///
/// Shadow tiles go in list order, largest first (`abi::shadow_tile`):
/// by day all to the sun's cascades; at night first to the headlights and
/// the tail lights, whose static tiles keep their slots (ADR 0101), then
/// to the lamps near the car, nearest first. The sun casts its shadow
/// while it is up; the moon casts none, which saves its four cascades.
pub fn lights(
    day: &DayLight,
    lamps: &[Lamp],
    car: Mat4,
    car_lamps: &CarLamps,
    eye: Vec3,
) -> Vec<Light> {
    let (direction, key_color) = day.key();
    let mut key = Light::directional(direction, key_color.normalize_or_zero(), key_color.length());
    if day.sun_up() {
        key = key
            .casting_shadow(30.0)
            .with_cascades(4)
            // Far down the road, the split leaning logarithmic so the
            // first cascade still holds the car in about 20 m.
            .with_cascade_split(0.85)
            // The towers' tops, kilometres up-sun of the long shadows
            // they throw on the road under a low sun.
            .with_caster_reach(3000.0)
            .with_source_radius(0.0047);
        key.shadow_distance = 500.0;
        key.shadow_normal_bias = 0.05;
    }
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
        // The shadow starts at the housing's back: the car's body around
        // the bulb, behind it, does not shadow its own beam.
        .with_shadow_near(crate::car::BULB_SETBACK * 2.)
        .with_source_radius(0.04)
        .with_normal_bias(0.04)
        // The car and the deck, drawn once in the car's frame (ADR 0101):
        // the deck kept is the stretch under the car when it was drawn,
        // which still keeps the beams off the piers below.
        .with_shadow_casters(TagExpr::tag(CAR_TAG).or(TagExpr::tag(DECK_TAG)))
        .with_static_shadow(car);
        headlight.shadow_distance = HEADLIGHT_RANGE;
        lights.push(headlight);
    }

    // The tail lights: a faint red wash on the road just behind, from
    // spots aimed back and down, so they never reach the middle of the
    // road. The car shadows them, drawn once in its frame like the
    // headlights, so they light nothing inside it.
    let up = car.transform_vector3(Vec3::Y).normalize();
    for position in car_lamps.tail {
        let mut tail = Light::spot(
            car.transform_point3(position),
            (-forward - up * 0.45).normalize(),
            25f32.to_radians(),
            60f32.to_radians(),
            TAIL_COLOR,
            TAIL_INTENSITY * car_lamps.tail_glow,
        )
        .with_profile(TAIL_LAYER)
        .with_range(TAIL_RANGE)
        .casting_shadow(1.0)
        .with_normal_bias(0.02)
        .with_shadow_casters(TagExpr::tag(CAR_TAG))
        .with_static_shadow(car);
        tail.shadow_distance = TAIL_RANGE;
        lights.push(tail);
    }

    // Each lamp head near the car is two lights: a shadowed one carrying
    // `shadow` of its intensity and a local one the rest, so a lamp's
    // shadow fades in and out instead of switching.
    let at = car.transform_point3(Vec3::ZERO);
    let lamp = |(head, street): (Vec3, Vec3), share: f32| {
        Light::spot(
            head - Vec3::Y * 0.2,
            (Vec3::NEG_Y + street * LAMP_TILT.tan()).normalize(),
            LAMP_CONE - 2f32.to_radians(),
            LAMP_CONE,
            LAMP_COLOR,
            LAMP_INTENSITY * day.lights_on * share,
        )
        .with_profile(LAMP_LAYER)
        .with_range(LAMP_RANGE)
    };
    let mut near: Vec<((Vec3, Vec3), f32)> = lamps
        .iter()
        .flat_map(heads)
        .map(|head| {
            (
                head,
                1.0 - smoothstep(LAMP_SHADOW_NEAR, LAMP_SHADOW_FAR, head.0.distance(at)),
            )
        })
        .filter(|(_, shadow)| *shadow > 0.0)
        .collect();
    // Nearest first, so the lamps over the car get the larger tiles.
    near.sort_by(|a, b| a.0 .0.distance(at).total_cmp(&b.0 .0.distance(at)));
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

    // The lamps within reach of what the camera sees well; the light grid
    // bins them (ADR 0076).
    for lamp_at in lamps {
        if lamp_at.foot.distance(eye) > 700.0 {
            continue;
        }
        for head in heads(lamp_at) {
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
    fn the_lamp_profile_throws_along_the_road_and_spares_the_ground_under_it() {
        let profile = IesProfile::parse(LAMP_IES).expect("street_lamp.ies parses");
        let peak = profile.max_candela();
        // A batwing along the road both ways, dim straight down.
        assert!(profile.candela(60.0, 0.0) > peak * 0.8);
        assert!(profile.candela(60.0, 180.0) > peak * 0.8);
        assert!(profile.candela(0.0, 0.0) < peak * 0.15);
        // Across, it stops at the parapet and hands over to the other
        // head at the median.
        assert_eq!(profile.candela(45.0, 90.0), 0.0);
        assert!(profile.candela(40.0, 270.0) < profile.candela(20.0, 270.0) * 0.5);
        // Full cutoff: nothing past 70°.
        assert_eq!(profile.candela(72.5, 0.0), 0.0);
        assert_eq!(profile.candela(90.0, 45.0), 0.0);
        assert_eq!(profile.candela(120.0, 0.0), 0.0);
    }

    #[test]
    fn night_brings_the_lamps_and_day_only_the_sun() {
        let lamps = crate::road::Road::new().lamps();
        let car = CarLamps {
            head: [Vec3::new(-0.6, 0.6, -2.2), Vec3::new(0.6, 0.6, -2.2)],
            tail: [Vec3::new(-0.6, 0.7, 2.2), Vec3::new(0.6, 0.7, 2.2)],
            tail_glow: 1.0,
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
        // The headlights, the tail lights and the lamp heads near the car
        // cast shadows, the moon none; everything else goes to the light
        // grid.
        let shadowed = night.iter().filter(|light| light.casts_shadow).count();
        assert!((4..=4 + SHADOWED_LAMPS).contains(&shadowed));
        assert_eq!(
            night.iter().filter(|light| !light.is_local()).count(),
            shadowed + 1
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
    fn a_lamp_cone_holds_everything_its_range_reaches_up_to_a_roof() {
        // A point inside the range but outside the cone sits higher above
        // the deck than a car's roof.
        let roof = 1.5;
        let below_head = crate::road::LAMP_HEIGHT - 0.2;
        assert!(LAMP_RANGE * LAMP_CONE.cos() < below_head - roof);
    }

    #[test]
    fn every_light_that_reaches_the_car_casts_its_shadow() {
        let lamps = crate::road::Road::new().lamps();
        let car_lamps = CarLamps {
            head: [Vec3::new(-0.6, 0.6, -2.2), Vec3::new(0.6, 0.6, -2.2)],
            tail: [Vec3::new(-0.6, 0.7, 2.2), Vec3::new(0.6, 0.7, 2.2)],
            tail_glow: 1.0,
        };
        let night = DayLight::at(23.0);
        let road = crate::road::Road::new();
        for s in [0.0, 9.0, 18.0, 27.0, 400.0] {
            let frame = road.frame(s);
            let car = Mat4::from_translation(frame.at(crate::road::LANE, 0.0));
            let at = car.transform_point3(Vec3::ZERO);
            let all = lights(&night, &lamps, car, &car_lamps, at);
            let unshadowed = all
                .iter()
                .filter(|light| !light.casts_shadow && light.intensity > 0.0)
                .filter(|light| light.position_or_direction.distance(at) < light.range)
                .count();
            assert_eq!(unshadowed, 0, "{s} m: a light reaches the car unshadowed");
            // And the atlas has room for every one of those shadows.
            let environment = wxsl::render::Environment {
                lights: all,
                ..wxsl::render::Environment::default()
            };
            let tiles = environment.shadow_tiles();
            for (index, light) in environment.uniform_lights().iter().enumerate() {
                assert!(
                    !light.casts_shadow || tiles.iter().any(|tile| tile.light == index as u32),
                    "{s} m: light {index} got no shadow tile"
                );
            }
        }
    }

    #[test]
    fn a_lamp_shadow_fades_in_without_changing_its_light() {
        let lamps = crate::road::Road::new().lamps();
        let car = CarLamps {
            head: [Vec3::ZERO; 2],
            tail: [Vec3::ZERO; 2],
            tail_glow: 1.0,
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

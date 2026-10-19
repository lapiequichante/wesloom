//! Cameras and lights per demo, and the IBL grid's scene.

use super::*;

pub(crate) fn gallery_area_light() -> Light {
    Light::rectangle(
        Vec3::new(0.0, 4.2, 0.0),
        Vec3::new(1.6, 0.0, 0.0),
        Vec3::new(0.0, 0.0, 1.0),
        Vec3::new(1.0, 0.9, 0.75),
        6.0,
    )
    .casting_shadow(8.0)
}

pub(crate) fn demo_environment(demo: &Demo, aspect: f32, time: f32) -> Environment {
    // A sky demo turns the lamps off: what is left in the image is the
    // ambient term and nothing else, so the roughness and grazing-angle
    // response it gets out of the baked table is the whole picture.
    let lights = match demo.sky {
        Some(_) => Vec::new(),
        None if lights::lights(demo.name).is_some() => {
            lights::lights(demo.name).unwrap_or_default()
        }
        None if demo.name == "shadow" => vec![
            // The sun: directional, casting into the atlas's key tile —
            // the 1024² one — over a 12 m box around the origin. Low
            // enough in the sky that a floating cube's shadow falls in
            // front of it, where the camera can see it.
            Light::directional(Vec3::new(0.8, 0.6, 0.5), Vec3::new(1.0, 0.93, 0.8), 2.6)
                .casting_shadow(6.0),
            // A cool fill so the shadow side reads as shadowed, not black.
            Light::point(Vec3::new(-3.0, 1.6, -1.6), Vec3::new(0.4, 0.5, 0.8), 8.0),
        ],
        None if demo.name == "cascade" => vec![
            // The same kind of sun, but split: four cascades fitted to the
            // camera over a 48 m reach. The field of pillars is what makes
            // the split legible — the near pillars' shadows are the key
            // tiles' job, the far ones the outer cascades', and the
            // sharpness falls off with them.
            Light::directional(Vec3::new(0.7, 0.75, 0.45), Vec3::new(1.0, 0.93, 0.8), 2.4)
                .casting_shadow(24.0)
                .with_cascades(4),
            Light::point(Vec3::new(-4.0, 2.0, -4.0), Vec3::new(0.35, 0.45, 0.7), 8.0),
        ],
        None if demo.name == "point-shadow" => vec![
            // One lamp overhead claiming six tiles — a face per direction —
            // so the cubes it hangs over shadow the floor outward in every
            // direction at once. High enough that the shadow spreads well
            // past the casters' own footprints, where the camera can see it.
            Light::point(Vec3::new(0.0, 4.2, 0.0), Vec3::new(1.0, 0.9, 0.75), 40.0)
                .casting_shadow(8.0),
            Light::point(Vec3::new(-4.5, 1.4, -3.5), Vec3::new(0.3, 0.4, 0.65), 7.0),
        ],
        None if demo.name == "area-shadow" => vec![
            gallery_area_light(),
            Light::point(Vec3::new(-4.5, 1.4, -3.5), Vec3::new(0.3, 0.4, 0.65), 3.0),
        ],
        None if matches!(demo.name, "spot-shadow" | "spot-cookie") => {
            let mut spot = Light::spot(
                Vec3::new(-2.0, 4.2, 1.0),
                Vec3::new(2.0, -5.2, -1.0),
                25.0_f32.to_radians(),
                40.0_f32.to_radians(),
                Vec3::new(1.0, 0.9, 0.75),
                85.0,
            )
            .casting_shadow(8.0);
            if demo.name == "spot-cookie" {
                spot = spot.with_cookie(lights::WINDOW_LAYER);
            }
            vec![spot]
        }
        None if demo.name == "sixteen" => {
            // The budget, spent: sixteen lamps in a ring, hue walking with
            // the angle. No shadows — the point of the demo is that all
            // sixteen rows of the light array reach the shader.
            (0..16)
                .map(|index| {
                    let angle = index as f32 / 16.0 * core::f32::consts::TAU;
                    Light::point(
                        Vec3::new(angle.cos() * 3.2, 1.3, angle.sin() * 3.2),
                        Vec3::new(
                            0.5 + 0.5 * (angle).cos(),
                            0.5 + 0.5 * (angle + 2.1).cos(),
                            0.5 + 0.5 * (angle + 4.2).cos(),
                        ) * 1.6,
                        9.0,
                    )
                })
                .collect()
        }
        None if demo.name == "clustered" => {
            // A 32 × 32 lattice of lamps a hand's width over the floor,
            // each drifting on its own small orbit. Every one has a range,
            // so none takes a uniform slot: the light grid carries them
            // all, and the floor is a field of overlapping pools.
            (0..1024)
                .map(|index| {
                    let (column, row) = ((index % 32) as f32, (index / 32) as f32);
                    let phase = index as f32 * 2.399;
                    let drift =
                        Vec3::new((time + phase).cos(), 0.0, (time * 0.8 + phase).sin()) * 0.12;
                    let hue = index as f32 * 2.39996;
                    Light::point(
                        Vec3::new(column * 0.36 - 5.6, -1.42, row * 0.36 - 5.6) + drift,
                        Vec3::new(
                            0.55 + 0.45 * hue.sin(),
                            0.55 + 0.45 * (hue + 2.1).sin(),
                            0.55 + 0.45 * (hue + 4.2).sin(),
                        ),
                        0.5,
                    )
                    .with_range(0.4)
                })
                .collect()
        }
        None => vec![
            // Key: warm and close. The bloom demos turn this up far enough
            // that the specular highlight crosses the effect's threshold.
            Light::point(
                Vec3::new(2.6, 3.0, 2.2),
                Vec3::new(1.0, 0.86, 0.72),
                demo.key_intensity,
            ),
            Light::point(Vec3::new(-3.0, 1.2, -1.6), Vec3::new(0.5, 0.65, 1.0), 18.0),
            Light::directional(Vec3::new(-0.4, 0.7, -1.0), Vec3::new(0.7, 0.75, 0.9), 1.1),
        ],
    };
    let (ambient_sky, ambient_ground) = demo
        .sky
        .or_else(|| {
            if matches!(demo.name, "area-texture" | "neon") {
                return Some((Vec3::splat(0.01), Vec3::splat(0.005)));
            }
            if demo.name == "headlights" {
                return Some((Vec3::new(0.006, 0.008, 0.014), Vec3::splat(0.002)));
            }
            (matches!(demo.name, "area-shadow" | "spot-shadow" | "spot-cookie"))
                .then_some((Vec3::splat(0.04), Vec3::splat(0.02)))
        })
        .unwrap_or((Vec3::new(0.14, 0.19, 0.28), Vec3::new(0.05, 0.04, 0.035)));
    Environment {
        camera: if let Some(camera) = lights::camera(demo.name, aspect) {
            camera
        } else if matches!(demo.name, "ibl-grid" | "ibl-grid-sky") {
            ibl_grid_camera(aspect)
        } else if demo.name == "glass-probes" {
            Camera {
                eye: Vec3::new(0.0, 3.4, 9.6),
                target: Vec3::new(0.0, 0.0, -0.4),
                aspect,
                ..Camera::default()
            }
        } else if demo.name == "glass" {
            Camera {
                eye: Vec3::new(0.0, 0.7, 6.8),
                target: Vec3::ZERO,
                aspect,
                ..Camera::default()
            }
        } else if demo.name == "cascade" {
            // High and back: the pillar field, and the sharpness falloff
            // from the near cascades to the far ones, in one view.
            Camera {
                eye: Vec3::new(1.0, 6.0, 10.5),
                target: Vec3::new(0.0, -0.8, 0.0),
                aspect,
                ..Camera::default()
            }
        } else if demo.name == "area-shadow" {
            Camera {
                eye: Vec3::new(5.5, 2.6, 10.0),
                target: Vec3::new(0.0, 1.0, 0.0),
                aspect,
                ..Camera::default()
            }
        } else if matches!(
            demo.name,
            "point-shadow" | "spot-shadow" | "spot-cookie" | "sixteen" | "clustered"
        ) {
            // Level with the lamp ring: the cube's shadow spreading outward
            // and the sixteen pools of colour both read from a low angle.
            Camera {
                eye: Vec3::new(0.0, 3.1, 8.2),
                target: Vec3::new(0.0, -0.4, 0.0),
                aspect,
                ..Camera::default()
            }
        } else {
            Camera {
                eye: Vec3::new(2.4, 1.9, 3.2),
                target: Vec3::ZERO,
                aspect,
                ..Camera::default()
            }
        },
        lights,
        ambient_sky,
        ambient_ground,
        exposure: 1.0,
        time,
        previous_time: time,
        // Every gallery camera is fixed, so the velocity stage sees
        // object motion only — and `None` says exactly that.
        previous_camera: None,
        origin: Default::default(),
        cluster_lights: true,
    }
}

/// Front row is dielectric; increasing depth increases metallic. Every row
/// contains the exact roughness endpoints and three evenly spaced values.
pub(crate) fn ibl_grid_samples() -> [(Vec3, f32, f32); 25] {
    std::array::from_fn(|index| {
        let column = index % 5;
        let row = index / 5;
        (
            Vec3::new((column as f32 - 2.0) * 2.2, 0.85, (2.0 - row as f32) * 2.2),
            column as f32 / 4.0,
            row as f32 / 4.0,
        )
    })
}

pub(crate) fn ibl_grid_camera(aspect: f32) -> Camera {
    Camera {
        eye: Vec3::new(0.0, 12.0, 14.0),
        target: Vec3::new(0.0, 0.5, 0.0),
        aspect,
        ..Camera::default()
    }
}

/// One shader, independent uniform blocks: no 25 compile-time material variants.
pub(crate) fn ibl_grid_material_graph() -> Graph {
    let registry = wxsl::stdlib::registry();
    let mut graph = Graph::new("IBL roughness/metallic grid");
    let out = graph.add_node("output.surface");
    graph.set_param(out, "base_color", Value::Vec3([0.65, 0.24, 0.08]));
    for name in ["roughness", "metallic"] {
        let parameter = graph.add(
            Node::new("param.value")
                .with_setting("name", name)
                .with_param("value", Value::F32(0.0)),
        );
        graph
            .set_generic(&registry, parameter, "T", ValueType::F32)
            .expect("scalar parameter");
        graph
            .wire(&registry, (parameter, "out"), (out, name))
            .expect("surface scalar");
    }
    graph
}

pub(crate) fn ibl_grid_floor_graph() -> Graph {
    let mut graph = Graph::new("matte grey IBL floor");
    let out = graph.add_node("output.surface");
    graph.set_param(out, "base_color", Value::Vec3([0.3; 3]));
    graph.set_param(out, "roughness", Value::F32(1.0));
    graph.set_param(out, "metallic", Value::F32(0.0));
    graph
}

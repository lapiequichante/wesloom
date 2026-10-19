//! One frame of one demo.

use super::*;

impl Stage {
    /// Render one frame of `demo` at `time`, into `view`, where `step`
    /// is how long the frame before it took — the shutter the velocity
    /// stage's previous-instance row answers for. A screenshot states
    /// the demo's own step; the live loop states what it measured.
    pub(crate) fn render(
        &mut self,
        demo: &Demo,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
        time: f32,
        step: f32,
    ) -> Result<(), Box<dyn Error>> {
        if demo.name == "glass-probes" {
            let mut environment = demo_environment(demo, width as f32 / height.max(1) as f32, time);
            if let Some((camera, previous)) = self.camera_override {
                environment.camera = camera;
                environment.previous_camera = previous;
            }
            let mut probes = match self.probes.take() {
                Some(probes) if probes.matches_source(self) => probes,
                _ => glass_probes::GlassProbes::new(
                    self,
                    width,
                    height,
                    self.renderer.target().format,
                )?,
            };
            probes.render(self, view, &environment, width, height)?;
            self.probes = Some(probes);
            return Ok(());
        }
        let uses_ibl = matches!(
            demo.name,
            "ibl" | "ibl-mirror" | "ibl-grid" | "ibl-grid-sky" | "glass"
        );
        let use_sky = self.ibl_sky || demo.name == "ibl-grid-sky";
        let source_changed = uses_ibl
            && use_sky
                != self
                    .renderer
                    .render_graph()
                    .resource_by_label("gallery sky")
                    .is_some();
        let optics_changed = (demo.name == "glass")
            != self
                .renderer
                .render_graph()
                .resource_by_label("gallery glass color")
                .is_some();
        let quality_changed = demo.name == "glass" &&
            self.renderer.render_graph().passes().iter().any(|p| matches!(&p.kind, wxsl::render::pass::PassKind::Screen { effect } if effect == "wxsl.refraction"))
                != (self.refraction_quality == RefractionQuality::Precise);
        if !uses_ibl
            || self.renderer.render_graph().environment_maps().is_none()
            || source_changed
            || optics_changed
            || quality_changed
        {
            self.renderer.remove_import("gallery HDR");
            apply(demo, &mut self.renderer)?;
            if uses_ibl {
                use wxsl::render::pass::{
                    Attachment, Extent, PassDesc, Policy, Read, ResourceDesc,
                };
                use wxsl::render::types::{Color, TextureFormat};
                let mut graph = self.renderer.render_graph().clone();
                if demo.name == "glass" {
                    // Optical objects never enter the opaque depth/colour chain.
                    for index in 0..graph.passes().len() {
                        if let wxsl::render::pass::PassKind::Geometry { source, .. } =
                            &mut graph.pass_mut(index).unwrap().kind
                        {
                            *source = wxsl::render::pass::DrawSource::Scene(
                                wxsl::core::scene::TagExpr::parse("!refractive")?,
                            );
                        }
                    }
                }
                let imported = graph.resource(ResourceDesc::imported(
                    "gallery HDR",
                    if use_sky {
                        TextureFormat::Rgba8Unorm
                    } else {
                        TextureFormat::Rgba32Float
                    },
                ));
                let source = if use_sky {
                    let sky = graph.resource(
                        ResourceDesc::color("gallery sky", TextureFormat::Rgba16Float)
                            .with_extent(Extent::Fixed {
                                width: 1024,
                                height: 512,
                            })
                            .persistent(0),
                    );
                    graph.pass(
                        PassDesc::screen("gallery sky", "wxsl.sky")
                            .with_reads([Read::current(imported)])
                            .with_color(Attachment::clear(sky, Color::BLACK))
                            .with_policy(Policy::Once),
                    );
                    sky
                } else {
                    imported
                };
                let maps = wxsl::render::effect::ibl::append_filtered_bake(
                    &mut graph,
                    source,
                    "gallery IBL",
                    128,
                    16,
                    8,
                );
                graph.declare_environment_maps(maps.diffuse, maps.specular);
                graph.set_environment_scale(if use_sky { 1.0 } else { self.ibl_source.scale });
                let tonemap = graph
                    .passes()
                    .iter()
                    .position(|pass| {
                        matches!(&pass.kind,
                    wxsl::render::pass::PassKind::Screen { effect } if effect == "wxsl.tonemap")
                    })
                    .ok_or("IBL chain needs tonemap")?;
                let color = graph.passes()[tonemap].reads[0].resource;
                let depth = graph
                    .passes()
                    .iter()
                    .filter_map(|pass| pass.depth.as_ref().map(|a| a.resource))
                    .next_back()
                    .ok_or("IBL chain needs opaque depth")?;
                let background = wxsl::render::effect::ibl::append_background(
                    &mut graph,
                    color,
                    depth,
                    maps.radiance,
                    "gallery environment background",
                );
                let background = if demo.name == "glass" {
                    wxsl::render::effect::refraction::append_with_quality(
                        &mut graph,
                        background,
                        depth,
                        maps.radiance,
                        "gallery glass",
                        3, // One sphere crossing + two torus crossings, each front/back.
                        self.refraction_quality,
                    )
                } else {
                    background
                };
                graph.pass_mut(tonemap).unwrap().reads[0] = Read::current(background);
                self.renderer.set_graph(graph)?;
                self.renderer.import_resource(
                    "gallery HDR",
                    if use_sky {
                        self.sky_extent.clone()
                    } else {
                        self.ibl_source.view.clone()
                    },
                );
            }
        }
        self.ensure_material(demo.features, demo.model, demo.macros)?;
        self.tints = instance_tints(demo.instances);
        let mut environment = demo_environment(demo, width as f32 / height.max(1) as f32, time);
        if let Some((camera, previous)) = self.camera_override {
            environment.camera = camera;
            environment.previous_camera = previous;
        }
        if demo.name == "glass" {
            let mut draws = DrawList::new();
            draws.push(
                DrawItem::new(&self.sphere, &self.glass[0].material)
                    .with_bindings(&self.glass[0].bindings)
                    .with_transform(Mat4::from_translation(Vec3::new(-1.15, 0.0, 0.0))),
            );
            draws.push(
                DrawItem::new(&self.glass_torus, &self.glass[1].material)
                    .with_bindings(&self.glass[1].bindings)
                    .with_transform(
                        Mat4::from_translation(Vec3::new(1.25, 0.0, -0.3))
                            * Mat4::from_rotation_x(0.3),
                    ),
            );
            self.renderer.render(
                &self.gpu.device,
                &self.gpu.queue,
                &RenderRequest {
                    view,
                    environment: &environment,
                    draws: &draws,
                },
            )?;
            return Ok(());
        }
        if matches!(demo.name, "ibl-grid" | "ibl-grid-sky") {
            let mut draws = DrawList::new();
            draws.push(
                DrawItem::new(&self.grid_floor_mesh, &self.grid_floor.material)
                    .with_bindings(&self.grid_floor.bindings),
            );
            for ((position, _, _), bindings) in ibl_grid_samples().iter().zip(&self.grid_bindings) {
                draws.push(
                    DrawItem::new(&self.sphere, &self.grid_material)
                        .with_transform(Mat4::from_translation(*position))
                        .with_bindings(bindings),
                );
            }
            self.renderer.render(
                &self.gpu.device,
                &self.gpu.queue,
                &RenderRequest {
                    view,
                    environment: &environment,
                    draws: &draws,
                },
            )?;
            return Ok(());
        }
        if demo.name == "shadow" {
            // The atlas proof: a sun with a shadow map, a cube to cast it,
            // and a ground plane far enough below to catch it as more than
            // a contact line.
            let tint =
                InstanceAttributes::new().with("instance_tint", Value::Vec3([1.0, 0.85, 0.6]));
            let draws: DrawList = [
                DrawItem::new(&self.grid_floor_mesh, &self.shadow_floor.material)
                    .with_bindings(&self.shadow_floor.bindings)
                    .with_transform(Mat4::from_translation(Vec3::new(0.0, -1.6, 0.0))),
                DrawItem::new(&self.mesh, &self.material)
                    .with_transform(
                        Mat4::from_translation(Vec3::new(0.0, 0.4, 0.0)) * cube_transform(time),
                    )
                    .with_bindings(&self.bindings)
                    .with_attributes(&tint),
            ]
            .into_iter()
            .collect();
            self.renderer.render(
                &self.gpu.device,
                &self.gpu.queue,
                &RenderRequest {
                    view,
                    environment: &environment,
                    draws: &draws,
                },
            )?;
            return Ok(());
        }
        if demo.name == "cascade" {
            // The cascaded atlas: a field of pillars over one sun split
            // into four camera-fitted boxes. The nearest pillars sit where
            // the key cascades are sharp; the far ones ride the outer
            // cascades, and their shadows show the falloff.
            let floor = Mat4::from_translation(Vec3::new(0.0, -1.6, 0.0));
            let mut draws = DrawList::new();
            draws.push(
                DrawItem::new(&self.grid_floor_mesh, &self.shadow_floor.material)
                    .with_bindings(&self.shadow_floor.bindings)
                    .with_transform(floor),
            );
            let pillars = [
                (Vec3::new(-0.9, -1.1, 0.6), [1.0, 0.85, 0.6]),
                (Vec3::new(2.4, -1.1, -1.4), [0.85, 0.9, 1.0]),
                (Vec3::new(-3.4, -1.1, 2.6), [1.0, 0.75, 0.7]),
                (Vec3::new(4.8, -1.1, 2.0), [0.8, 1.0, 0.85]),
                (Vec3::new(-5.6, -1.1, -2.8), [0.95, 0.9, 0.7]),
                (Vec3::new(6.4, -1.1, -3.4), [0.7, 0.85, 1.0]),
                (Vec3::new(-2.0, -1.1, 6.8), [1.0, 0.8, 0.9]),
                (Vec3::new(8.0, -1.1, 5.6), [0.85, 1.0, 0.75]),
            ];
            let tints: Vec<InstanceAttributes> = pillars
                .iter()
                .map(|(_, tint)| {
                    InstanceAttributes::new().with("instance_tint", Value::Vec3(*tint))
                })
                .collect();
            for ((position, _), tint) in pillars.iter().zip(&tints) {
                draws.push(
                    DrawItem::new(&self.mesh, &self.material)
                        .with_transform(Mat4::from_translation(*position))
                        .with_bindings(&self.bindings)
                        .with_attributes(tint),
                );
            }
            self.renderer.render(
                &self.gpu.device,
                &self.gpu.queue,
                &RenderRequest {
                    view,
                    environment: &environment,
                    draws: &draws,
                },
            )?;
            return Ok(());
        }
        if matches!(
            demo.name,
            "point-shadow" | "area-shadow" | "spot-shadow" | "spot-cookie"
        ) {
            // Six faces, one lamp: two cubes under the light, and the
            // floor catching both shadows in every direction the lamp
            // reaches.
            let floor = Mat4::from_translation(Vec3::new(0.0, -1.6, 0.0));
            let warm =
                InstanceAttributes::new().with("instance_tint", Value::Vec3([1.0, 0.85, 0.6]));
            let cool =
                InstanceAttributes::new().with("instance_tint", Value::Vec3([0.8, 0.9, 1.0]));
            let mut draws: DrawList = [
                DrawItem::new(&self.grid_floor_mesh, &self.shadow_floor.material)
                    .with_bindings(&self.shadow_floor.bindings)
                    .with_transform(floor),
                DrawItem::new(&self.mesh, &self.material)
                    .with_transform(
                        floor
                            * Mat4::from_translation(Vec3::new(0.0, 3.1, 0.0))
                            * cube_transform(time),
                    )
                    .with_bindings(&self.bindings)
                    .with_attributes(&warm),
                DrawItem::new(&self.mesh, &self.material)
                    .with_transform(
                        floor
                            * Mat4::from_translation(Vec3::new(1.9, 2.2, 1.2))
                            * Mat4::from_rotation_y(time * 0.6),
                    )
                    .with_bindings(&self.bindings)
                    .with_attributes(&cool),
            ]
            .into_iter()
            .collect();
            if demo.name == "area-shadow" {
                let light = gallery_area_light();
                // The shared plate is 3.4 units wide; match the emitter bounds.
                let transform = Mat4::from_cols(
                    (light.area_u * (2.0 / 3.4)).extend(0.0),
                    light.area_u.cross(light.area_v).normalize().extend(0.0),
                    (-light.area_v * (2.0 / 3.4)).extend(0.0),
                    light.position_or_direction.extend(1.0),
                );
                draws.push(
                    DrawItem::new(&self.plate, &self.area_emitter.material)
                        .with_bindings(&self.area_emitter.bindings)
                        .with_transform(transform),
                );
            }
            self.renderer.render(
                &self.gpu.device,
                &self.gpu.queue,
                &RenderRequest {
                    view,
                    environment: &environment,
                    draws: &draws,
                },
            )?;
            return Ok(());
        }
        if matches!(demo.name, "area-texture" | "neon") {
            let draws = self.lights.draws(
                lights::Shared {
                    floor: &self.grid_floor_mesh,
                    cube: &self.mesh,
                    sphere: &self.sphere,
                    plate: &self.plate,
                },
                demo.name,
                time,
            );
            self.renderer.render(
                &self.gpu.device,
                &self.gpu.queue,
                &RenderRequest {
                    view,
                    environment: &environment,
                    draws: &draws,
                },
            )?;
            return Ok(());
        }
        if matches!(demo.name, "sixteen" | "clustered") {
            // The whole light budget: sixteen coloured lamps in a ring
            // around one cube. Every lamp's row reaches the shader, so the
            // floor is a wheel of coloured pools and nothing is dropped.
            let floor = Mat4::from_translation(Vec3::new(0.0, -1.6, 0.0));
            let plain =
                InstanceAttributes::new().with("instance_tint", Value::Vec3([1.0, 1.0, 1.0]));
            let draws: DrawList = [
                DrawItem::new(&self.grid_floor_mesh, &self.shadow_floor.material)
                    .with_bindings(&self.shadow_floor.bindings)
                    .with_transform(floor),
                DrawItem::new(&self.mesh, &self.material)
                    .with_transform(
                        floor
                            * Mat4::from_translation(Vec3::new(0.0, 0.9, 0.0))
                            * cube_transform(time),
                    )
                    .with_bindings(&self.bindings)
                    .with_attributes(&plain),
            ]
            .into_iter()
            .collect();
            self.renderer.render(
                &self.gpu.device,
                &self.gpu.queue,
                &RenderRequest {
                    view,
                    environment: &environment,
                    draws: &draws,
                },
            )?;
            return Ok(());
        }
        if demo.name == "peel" {
            let tint =
                InstanceAttributes::new().with("instance_tint", Value::Vec3([1.0, 1.0, 1.0]));
            // The torus, the sphere, and the sorted tier's plate. The
            // sphere sits in the torus's tube so a ray through the pair
            // meets both surfaces, and the plate hangs behind and below
            // them — the working case for the tiers (plan5 D5): a sorted
            // transparent the peel composite does not cover. It is
            // submitted *first*, so only the sorted pass's own
            // back-to-front sort and the peel's depth windows decide what
            // lands in front.
            let spin = Mat4::from_rotation_y(time * 0.35);
            let surfaces = [
                (
                    &self.plate,
                    &self.plate_material,
                    &self.plate_bindings,
                    spin * Mat4::from_translation(Vec3::new(-0.9, -1.15, -1.9))
                        * Mat4::from_rotation_x(1.2),
                ),
                (
                    &self.torus,
                    &self.peel_material,
                    &self.peel_bindings,
                    spin * Mat4::from_rotation_x(0.7),
                ),
                (
                    &self.sphere,
                    &self.peel_material,
                    &self.sphere_bindings,
                    spin * Mat4::from_translation(Vec3::new(0.72, 0.08, 0.12)),
                ),
            ];
            let draws: DrawList = surfaces
                .into_iter()
                .map(|(mesh, material, bindings, place)| {
                    DrawItem::new(mesh, material)
                        .with_transform(place)
                        .with_bindings(bindings)
                        .with_attributes(&tint)
                })
                .collect();
            self.renderer.render(
                &self.gpu.device,
                &self.gpu.queue,
                &RenderRequest {
                    view,
                    environment: &environment,
                    draws: &draws,
                },
            )?;
            return Ok(());
        }
        if demo.name == "render-order" {
            // Three draws, one blended pass: the underlay at order -1,
            // the sheet at order 0, the hologram at order 1 *behind the
            // sheet in depth* — drawn after it anyway, because the group
            // order is the draws' data, not the submission's (plan5 D4).
            // The pass sorts back to front, so the composite is the
            // groups in ascending order whatever order they arrive in.
            // The hologram is a *sphere* on purpose: the tier's assumption
            // is no self-overlap, and a convex draw honours it (a torus
            // folds against itself — that is what `pass.peel` is for).
            let [underlay, sheet, hologram] = &self.order;
            // The tint uniform is the graph's colour here; the per-instance
            // attribute it declares is the peel block's white identity.
            let tint =
                InstanceAttributes::new().with("instance_tint", Value::Vec3([1.0, 1.0, 1.0]));
            let stand = Mat4::from_rotation_x(FRAC_PI_2);
            let items = [
                DrawItem::new(&self.plate, &underlay.material)
                    .with_transform(Mat4::from_translation(Vec3::new(0.0, 0.0, -1.8)) * stand)
                    .with_bindings(&underlay.bindings)
                    .with_attributes(&tint),
                DrawItem::new(&self.plate, &sheet.material)
                    .with_transform(Mat4::from_translation(Vec3::new(0.0, 0.0, 0.5)) * stand)
                    .with_bindings(&sheet.bindings)
                    .with_attributes(&tint),
                DrawItem::new(&self.sphere, &hologram.material)
                    .with_transform(
                        Mat4::from_translation(Vec3::new(0.0, 0.0, -0.6))
                            * Mat4::from_rotation_y(time * 0.6),
                    )
                    .with_bindings(&hologram.bindings)
                    .with_attributes(&tint),
            ];
            let draws: DrawList = items.into_iter().collect();
            self.renderer.render(
                &self.gpu.device,
                &self.gpu.queue,
                &RenderRequest {
                    view,
                    environment: &environment,
                    draws: &draws,
                },
            )?;
            return Ok(());
        }
        if demo.name == "sdf-aa" {
            // One card, blended: the AA'd checker and the SDF disc whose
            // edge is one screen-space pixel wide (plan5 D6).
            let draws: DrawList = [DrawItem::new(&self.plate, &self.sdf_aa.material)
                .with_transform(
                    Mat4::from_translation(Vec3::new(0.0, 0.0, 0.4))
                        * Mat4::from_rotation_x(FRAC_PI_2),
                )
                .with_bindings(&self.sdf_aa.bindings)]
            .into_iter()
            .collect();
            self.renderer.render(
                &self.gpu.device,
                &self.gpu.queue,
                &RenderRequest {
                    view,
                    environment: &environment,
                    draws: &draws,
                },
            )?;
            return Ok(());
        }
        if matches!(demo.name, "iridescence" | "sheen" | "ibl-mirror") {
            let layer = if demo.name == "ibl-mirror" {
                &self.mirror
            } else if demo.name == "sheen" {
                &self.sheen
            } else {
                &self.iridescent
            };
            let draws: DrawList = [DrawItem::new(&self.sphere, &layer.material)
                .with_transform(cube_transform(time * 0.5))
                .with_bindings(&layer.bindings)]
            .into_iter()
            .collect();
            self.renderer.render(
                &self.gpu.device,
                &self.gpu.queue,
                &RenderRequest {
                    view,
                    environment: &environment,
                    draws: &draws,
                },
            )?;
            return Ok(());
        }
        // A demo with a material of its own draws it; every other demo
        // draws the shared cube. The bake material declares no per-instance
        // attributes, so its tints are empty and one copy draws.
        let (material, bindings, tints, instances) = if demo.material.is_some() {
            let bake = self.bake.as_ref().expect("the bake stage was built");
            // The table's view rides with its demo: every other demo took
            // it back, and a view nothing declares is an error by name.
            self.renderer
                .import_resource("roughness_bake", bake.view.clone());
            (&bake.material, &bake.bindings, &[][..], 1)
        } else {
            (
                &self.material,
                &self.bindings,
                &self.tints[..],
                demo.instances,
            )
        };
        let draws = cube_draws(
            &self.mesh,
            material,
            bindings,
            tints,
            instances,
            time,
            MotionPlan {
                motion: demo.motion,
                step,
            },
        );
        self.renderer.render(
            &self.gpu.device,
            &self.gpu.queue,
            &RenderRequest {
                view,
                environment: &environment,
                draws: &draws,
            },
        )?;
        Ok(())
    }
}

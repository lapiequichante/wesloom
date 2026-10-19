//! The GPU side shared by every demo: renderer, meshes, materials.

use super::*;

mod render;

/// Everything a frame needs, shared by every demo: one renderer, one
/// mesh, one material, one set of bindings.
pub(crate) struct Stage {
    pub(crate) ibl_source: wxsl::render::environment::EnvironmentImage,
    pub(crate) ibl_sky: bool,
    pub(crate) refraction_quality: RefractionQuality,
    pub(crate) probes: Option<glass_probes::GlassProbes>,
    /// The screen graph normalizes UV by its input image's size. The sky
    /// bake's extent input must match its output, independent of HDR dimensions.
    pub(crate) sky_extent: wgpu::TextureView,
    pub(crate) gpu: GpuContext,
    pub(crate) renderer: Renderer,
    /// The scene's graph, recompiled when a demo's feature set differs
    /// from the one the current material was resolved against.
    pub(crate) scene_graph: Graph,
    pub(crate) material: wxsl::render::Material,
    pub(crate) mesh: Mesh,
    pub(crate) bindings: wxsl::render::MaterialBindings,
    pub(crate) tints: Vec<InstanceAttributes>,
    /// The demo texture and sampler, kept so a rebuilt material's
    /// bindings can be refilled.
    pub(crate) texture: wgpu::TextureView,
    pub(crate) sampler: wgpu::Sampler,
    /// The feature set the current material was resolved against.
    pub(crate) material_features: &'static [&'static str],
    pub(crate) material_model: Option<&'static str>,
    pub(crate) material_macros: &'static [(&'static str, wxsl::core::macros::MacroValue)],
    /// The peel demo's surfaces: the cube graph with the alpha uniform,
    /// on a torus and a sphere that occupy the same space.
    pub(crate) peel_material: wxsl::render::Material,
    pub(crate) peel_bindings: wxsl::render::MaterialBindings,
    /// Same material, with the `tint` uniform set blue.
    pub(crate) sphere_bindings: wxsl::render::MaterialBindings,
    pub(crate) sphere: Mesh,
    /// The sorted tier (plan5 D5): the same peel surface with no layer
    /// budget, so no peel pass reads it; the tiered document composites
    /// it over the pair through the sorted-tier pass.
    pub(crate) plate_material: wxsl::render::Material,
    pub(crate) plate_bindings: wxsl::render::MaterialBindings,
    pub(crate) plate: Mesh,
    pub(crate) torus: Mesh,
    /// The render-order demo's three cards: the order `-1` underlay, the
    /// order-0 sheet, and the order-1 hologram behind it in depth yet
    /// drawn after it — the group order is data (plan5 D4).
    pub(crate) order: [DemoMaterial; 3],
    /// The sdf-aa demo's blended pattern surface (plan5 D6): an AA'd
    /// checker and an SDF disc whose edge is `screen_width`-wide
    /// coverage.
    pub(crate) sdf_aa: DemoMaterial,
    /// The iridescence demo's thin-film sphere (plan4's ticket 6).
    pub(crate) iridescent: DemoMaterial,
    /// Graph-authored sheen tint and roughness (ADR 0059).
    pub(crate) sheen: DemoMaterial,
    /// White conductor, no procedural texture or direct lamps.
    pub(crate) mirror: DemoMaterial,
    pub(crate) glass: [DemoMaterial; 2],
    pub(crate) glass_torus: Mesh,
    pub(crate) grid_material: wxsl::render::Material,
    pub(crate) grid_bindings: Vec<wxsl::render::MaterialBindings>,
    pub(crate) grid_floor: DemoMaterial,
    pub(crate) shadow_floor: DemoMaterial,
    pub(crate) area_emitter: DemoMaterial,
    /// The `area-texture`, `neon` and `headlights` demos' scenery.
    pub(crate) lights: lights::LightStage,
    pub(crate) grid_floor_mesh: Mesh,
    pub(crate) camera_override: Option<(Camera, Option<Camera>)>,
    /// The bake demo's own material and the table its bake pass writes
    /// through (ADR 0045): the graph is parsed once, the effect and the
    /// material are compiled from it, the texture is the *material's*
    /// (the host creates and owns it; the pass list only writes through
    /// it), and the view rides the renderer as an import under the
    /// declaration's name.
    pub(crate) bake: Option<BakeStage>,
}

/// The bake demo's half of the stage. The view is kept because the import
/// it feeds is *per demo*: the demos before and after take the view back
/// (`apply` removes it), and each bake frame hands it over again.
pub(crate) struct BakeStage {
    pub(crate) material: wxsl::render::Material,
    pub(crate) bindings: wxsl::render::MaterialBindings,
    pub(crate) view: wgpu::TextureView,
}

/// One demo's own material and its bindings — the bake stage's shape,
/// for the demos that need a material of their own but no bake table.
pub(crate) struct DemoMaterial {
    pub(crate) material: wxsl::render::Material,
    pub(crate) bindings: wxsl::render::MaterialBindings,
}

impl DemoMaterial {
    pub(crate) fn new(
        gpu: &GpuContext,
        renderer: &mut Renderer,
        texture: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
        graph: &Graph,
        config: &wxsl::render::material::MaterialConfig,
    ) -> Result<Self, Box<dyn Error>> {
        let material =
            wxsl::render::Material::with_config(graph, &wxsl::stdlib::registry(), config)?;
        let bindings = demo_bindings(
            &gpu.device,
            &gpu.queue,
            renderer,
            &material,
            texture,
            sampler,
        )?;
        Ok(DemoMaterial { material, bindings })
    }
}

impl Stage {
    pub(crate) fn new(
        gpu: GpuContext,
        target: TargetConfig,
        ibl_source: &str,
    ) -> Result<Self, Box<dyn Error>> {
        let registry = wxsl::stdlib::registry();
        let scene_graph: Graph =
            serde_json::from_str(include_str!("../../../assets/pbr_cube.wxsl.json"))?;
        scene_graph.validate(&registry)?;
        let material = wxsl::render::Material::from_graph(&scene_graph, &registry)?;
        let mut renderer = Renderer::new(&gpu.device, wxsl::stdlib_library(), target)?;
        for effect in [
            wxsl::render::effect::ibl::EQUIRECT_TO_CUBE,
            wxsl::render::effect::ibl::DIFFUSE,
            wxsl::render::effect::ibl::SPECULAR,
            wxsl::render::effect::ibl::RESAMPLE,
        ] {
            renderer.add_effect(effect);
        }
        let ibl_sky = ibl_source == "sky";
        let sky_extent = gpu
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("gallery sky extent"),
                size: wgpu::Extent3d {
                    width: 1024,
                    height: 512,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());
        let ibl_source = if ibl_sky {
            wxsl::render::environment::upload_environment_image(
                &gpu.device,
                &gpu.queue,
                1,
                1,
                &[[0.0; 3]],
            )?
        } else {
            #[cfg(not(target_arch = "wasm32"))]
            let reader = image::ImageReader::open(ibl_source)?.with_guessed_format()?;
            // A page has no file system: the default HDR travels in the module.
            #[cfg(target_arch = "wasm32")]
            let reader =
                image::ImageReader::new(std::io::Cursor::new(WEB_HDR)).with_guessed_format()?;
            if reader.format() != Some(image::ImageFormat::Hdr) {
                return Err("--ibl-source expects a Radiance .hdr image or sky".into());
            }
            let image = reader.decode()?.to_rgb32f();
            let pixels: Vec<[f32; 3]> = image.pixels().map(|pixel| pixel.0).collect();
            println!(
                "IBL HDR: {ibl_source} ({}x{}, max radiance {:.3})",
                image.width(),
                image.height(),
                pixels.iter().flatten().copied().fold(0.0_f32, f32::max)
            );
            wxsl::render::environment::upload_environment_image(
                &gpu.device,
                &gpu.queue,
                image.width(),
                image.height(),
                &pixels,
            )?
        };
        // The proof effects (ADRs 0035–0037) ship as descriptors; an
        // application registers them, which is the whole of "add a
        // compute pass" now.
        renderer.add_effect(BRDF_LUT);
        renderer.add_effect(LUT_VIEW);
        renderer.add_effect(RAMP_FILL);
        renderer.add_effect(RAMP_VIEW);
        // And the effects that are *graphs* (ADRs 0040, 0060). `tonemap`
        // registers under the id every stock document already names, so
        // every demo above presents through a generated module from here
        // on — which is the claim, and the fact that none of the images
        // move is the evidence.
        let nodes = wxsl::stdlib::registry();
        renderer.add_effect(wxsl::effects::tonemap(&nodes)?);
        renderer.add_effect(wxsl::effects::fxaa(&nodes)?);
        renderer.add_effect(wxsl::effects::sky(&nodes)?);
        // The bake effect is generated from the material subgraph (ADR
        // 0045) — registration is where generation happens, so a cone that
        // reads something a bake cannot evaluate is a failure here rather
        // than at the first frame.
        renderer.add_effect(bake_effect());
        let mesh = Mesh::cube(&gpu.device, 1.6);
        let (texture, sampler) = demo_texture(&gpu.device, &gpu.queue);
        let bindings = demo_bindings(
            &gpu.device,
            &gpu.queue,
            &mut renderer,
            &material,
            &texture,
            &sampler,
        )?;
        let bake = BakeStage::new(&gpu, &mut renderer)?;
        let peel_graph = peel_material_graph(&scene_graph)?;
        // The peeled pair asks for the demo's whole layer budget (plan5
        // D5): transparency is tiered now, and a transparent with no
        // budget is nobody the peel reads.
        let peel_material = wxsl::render::Material::with_config(
            &peel_graph,
            &registry,
            &wxsl::render::material::MaterialConfig::default()
                .with_tags(Tags::from_iter([TAG_TRANSPARENT]))
                .with_shadows(false, false)
                .with_max_layers(4),
        )?;
        let plate_material = wxsl::render::Material::with_config(
            &peel_graph,
            &registry,
            &wxsl::render::material::MaterialConfig::default()
                .with_tags(Tags::from_iter([TAG_TRANSPARENT]))
                .with_shadows(false, false),
        )?;
        let peel_bindings = demo_bindings(
            &gpu.device,
            &gpu.queue,
            &mut renderer,
            &peel_material,
            &texture,
            &sampler,
        )?;
        let mut sphere_bindings = demo_bindings(
            &gpu.device,
            &gpu.queue,
            &mut renderer,
            &peel_material,
            &texture,
            &sampler,
        )?;
        sphere_bindings.set("tint", Value::Vec3([0.05, 0.22, 1.0]))?;
        sphere_bindings.upload(&gpu.device, &gpu.queue)?;
        let mut plate_bindings = demo_bindings(
            &gpu.device,
            &gpu.queue,
            &mut renderer,
            &plate_material,
            &texture,
            &sampler,
        )?;
        // Green, so the tier reads as itself in the picture.
        plate_bindings.set("tint", Value::Vec3([0.1, 0.9, 0.2]))?;
        plate_bindings.upload(&gpu.device, &gpu.queue)?;
        let sphere = Mesh::sphere(&gpu.device, 0.85);
        let torus = Mesh::torus(&gpu.device, 1.15, 0.38);
        let plate = Mesh::plane(&gpu.device, 3.4);
        // The render-order demo's cards, from the peel graph so the tint
        // and alpha uniforms drive them: underlay, sheet, hologram.
        let mut cards: Vec<DemoMaterial> = Vec::new();
        for (order, tint, alpha) in [
            (-1, [0.2, 0.2, 0.2], 1.0),
            (0, [0.1, 0.9, 0.2], 0.5),
            (1, [1.0, 0.15, 0.1], 0.9),
        ] {
            let config = wxsl::render::material::MaterialConfig::default()
                .with_tags(Tags::from_iter([TAG_TRANSPARENT]))
                .with_shadows(false, false)
                .with_render_order(order);
            let mut card = DemoMaterial::new(
                &gpu,
                &mut renderer,
                &texture,
                &sampler,
                &peel_graph,
                &config,
            )?;
            card.bindings.set("tint", Value::Vec3(tint))?;
            card.bindings.set("alpha", Value::F32(alpha))?;
            card.bindings.upload(&gpu.device, &gpu.queue)?;
            cards.push(card);
        }
        let order: [DemoMaterial; 3] = cards
            .try_into()
            .unwrap_or_else(|_| unreachable!("exactly three cards were pushed"));
        let sdf_graph = sdf_aa_material_graph()?;
        let sdf_aa = DemoMaterial::new(
            &gpu,
            &mut renderer,
            &texture,
            &sampler,
            &sdf_graph,
            &wxsl::render::material::MaterialConfig::default()
                .with_tags(Tags::from_iter([TAG_TRANSPARENT]))
                .with_shadows(false, false),
        )?;
        let iridescent_graph = iridescent_material_graph()?;
        let film_set = wxsl::core::lighting::LightingSet::single(
            *wxsl::core::lighting::DEFAULT_MODELS
                .iter()
                .find(|model| model.name == "wxsl.iridescent")
                .expect("shipped film model"),
        );
        let film_material = wxsl::render::Material::with_lighting(
            &iridescent_graph,
            &registry,
            &wxsl::render::material::MaterialConfig::default().with_model("iridescent"),
            &film_set,
        )?;
        let film_bindings = demo_bindings(
            &gpu.device,
            &gpu.queue,
            &mut renderer,
            &film_material,
            &texture,
            &sampler,
        )?;
        let iridescent = DemoMaterial {
            material: film_material,
            bindings: film_bindings,
        };
        let mut sheen_scene: wxsl::core::scene::Scene =
            serde_json::from_str(include_str!("../../../assets/sheen.scene.json"))?;
        let sheen_entry = sheen_scene.materials.remove(0);
        let sheen_set = wxsl::core::lighting::LightingSet::single(
            *wxsl::core::lighting::DEFAULT_MODELS
                .iter()
                .find(|model| model.name == "wxsl.sheen")
                .expect("shipped sheen model"),
        );
        let sheen_material = wxsl::render::Material::with_lighting(
            &sheen_entry.graph,
            &registry,
            &sheen_entry.config,
            &sheen_set,
        )?;
        let sheen_bindings = demo_bindings(
            &gpu.device,
            &gpu.queue,
            &mut renderer,
            &sheen_material,
            &texture,
            &sampler,
        )?;
        let sheen = DemoMaterial {
            material: sheen_material,
            bindings: sheen_bindings,
        };
        let mut mirror_graph = Graph::new("mirror sphere");
        let output = mirror_graph.add_node("output.surface");
        mirror_graph.set_param(output, "base_color", Value::Vec3([1.0; 3]));
        mirror_graph.set_param(output, "metallic", Value::F32(1.0));
        mirror_graph.set_param(output, "roughness", Value::F32(0.0));
        let mirror = DemoMaterial::new(
            &gpu,
            &mut renderer,
            &texture,
            &sampler,
            &mirror_graph,
            &wxsl::render::material::MaterialConfig::default().with_shadows(false, false),
        )?;
        let glass = [
            ([1.0; 3], "glass_sphere"),
            ([0.18, 0.45, 0.9], "glass_torus"),
        ]
        .map(|(tint, tag)| {
            let mut graph = Graph::new("optical glass");
            let output = graph.add_node("output.surface");
            graph.set_param(output, "base_color", Value::Vec3(tint));
            graph.set_param(output, "roughness", Value::F32(0.0));
            graph.set_param(output, "ior", Value::F32(1.5));
            graph.set_param(output, "attenuation_distance", Value::F32(1.0));
            DemoMaterial::new(
                &gpu,
                &mut renderer,
                &texture,
                &sampler,
                &graph,
                &wxsl::render::material::MaterialConfig::default()
                    .with_shadows(false, false)
                    .with_tags(wxsl::core::scene::Tags::from_iter(["refractive", tag])),
            )
        });
        let [glass_sphere, glass_torus] = glass;
        let glass = [glass_sphere?, glass_torus?];
        // Smooth normals do not smooth rasterized depth: a strong lens
        // magnifies the coarse tube's planar triangles at its exit surface.
        let (vertices, indices) = wxsl::render::mesh::torus_geometry(1.15, 0.38, 128, 96);
        let glass_torus = Mesh::new(&gpu.device, "optical torus", &vertices, &indices);
        let grid_config =
            wxsl::render::material::MaterialConfig::default().with_shadows(false, false);
        let grid_material = wxsl::render::Material::with_config(
            &ibl_grid_material_graph(),
            &registry,
            &grid_config,
        )?;
        let mut grid_bindings = Vec::new();
        for (_, roughness, metallic) in ibl_grid_samples() {
            let mut bindings = renderer.material_bindings(&gpu.device, &grid_material);
            bindings.set("roughness", Value::F32(roughness))?;
            bindings.set("metallic", Value::F32(metallic))?;
            bindings.upload(&gpu.device, &gpu.queue)?;
            grid_bindings.push(bindings);
        }
        let grid_floor = DemoMaterial::new(
            &gpu,
            &mut renderer,
            &texture,
            &sampler,
            &ibl_grid_floor_graph(),
            &grid_config,
        )?;
        let grid_floor_mesh = Mesh::plane(&gpu.device, 14.0);
        let shadow_floor = DemoMaterial::new(
            &gpu,
            &mut renderer,
            &texture,
            &sampler,
            &ibl_grid_floor_graph(),
            &wxsl::render::material::MaterialConfig::default(),
        )?;
        let mut emitter_graph = Graph::new("area emitter");
        let output = emitter_graph.add_node(wxsl::core::abi::SURFACE_OUTPUT_ID);
        emitter_graph.set_param(output, "base_color", Value::Vec3([0.0; 3]));
        emitter_graph.set_param(output, "emissive", Value::Vec3([8.0, 7.2, 6.0]));
        let area_emitter = DemoMaterial::new(
            &gpu,
            &mut renderer,
            &texture,
            &sampler,
            &emitter_graph,
            &wxsl::render::material::MaterialConfig::default().with_shadows(false, false),
        )?;
        let light_textures = lights::light_textures(&gpu)?;
        renderer.set_light_cookies(&gpu.device, Some(&light_textures.array));
        let lights =
            lights::LightStage::new(&gpu, &mut renderer, &light_textures, &texture, &sampler)?;
        Ok(Stage {
            ibl_source,
            ibl_sky,
            refraction_quality: RefractionQuality::Layered,
            probes: None,
            sky_extent,
            gpu,
            renderer,
            scene_graph,
            material,
            mesh,
            bindings,
            tints: instance_tints(1),
            texture,
            sampler,
            material_features: &[],
            material_model: None,
            material_macros: &[],
            peel_material,
            peel_bindings,
            plate_material,
            plate_bindings,
            plate,
            sphere_bindings,
            sphere,
            torus,
            order,
            sdf_aa,
            iridescent,
            sheen,
            mirror,
            glass,
            glass_torus,
            grid_material,
            grid_bindings,
            grid_floor,
            shadow_floor,
            area_emitter,
            lights,
            grid_floor_mesh,
            camera_override: None,
            bake: Some(bake),
        })
    }

    /// Put the scene's material on `features`' plan, if the demo's
    /// differs from the current one (plan2 P12). A material is resolved
    /// against the plan of the pipeline it will draw under — that is the
    /// handshake the frame compile checks.
    pub(crate) fn ensure_material(
        &mut self,
        features: &'static [&'static str],
        model: Option<&'static str>,
        macros: &'static [(&'static str, wxsl::core::macros::MacroValue)],
    ) -> Result<(), Box<dyn Error>> {
        if self.material_features == features
            && self.material_model == model
            && self.material_macros == macros
        {
            return Ok(());
        }
        let mut pins = wxsl::core::macros::MacroSet::new();
        for (name, value) in macros {
            pins.set(*name, *value);
        }
        let registry = wxsl::stdlib::registry();
        let material = wxsl::render::Material::with_lighting(
            &self.scene_graph,
            &registry,
            &wxsl::render::material::MaterialConfig {
                features: wxsl::core::lighting::feature_requests(features)?,
                model: model.map(str::to_string),
                macros: pins,
                ..wxsl::render::material::MaterialConfig::default()
            },
            self.renderer.lighting(),
        )?;
        self.bindings = demo_bindings(
            &self.gpu.device,
            &self.gpu.queue,
            &mut self.renderer,
            &material,
            &self.texture,
            &self.sampler,
        )?;
        self.material = material;
        self.material_features = features;
        self.material_model = model;
        self.material_macros = macros;
        Ok(())
    }
}

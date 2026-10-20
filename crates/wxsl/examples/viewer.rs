//! A glTF viewer: a `.glb` or `.gltf` file imported as a scene document
//! (ADR 0080), lit by an HDR environment, orbited with the mouse.
//!
//! Three demos name the Khronos sample models in `resources/glb/` (see its
//! README for their licenses); any other path opens that file.
//!
//! ```text
//! cargo run -p wxsl --features gltf --example viewer
//! cargo run -p wxsl --features gltf --example viewer -- car-concept
//! cargo run -p wxsl --features gltf --example viewer -- glam-velvet-sofa
//! cargo run -p wxsl --features gltf --example viewer -- path/to/model.glb
//! cargo run -p wxsl --features gltf --example viewer -- glam-velvet-sofa --screenshot out.png
//! cargo run -p wxsl --features gltf --release --example viewer -- --grid 30 --bench 200
//! ```
//!
//! `--zoom Z` frames the screenshot closer (below 1) or further, and
//! `--yaw A` turns it around the model (radians, 0 looks from +Z).
//!
//! `--grid N` repeats the model N×N times, and `--bench FRAMES` renders
//! that many frames headless, framed and from inside the grid, and prints
//! the frame times with the draws and draw calls (plan6 G3, ADR 0086).
//!
//! In the window: drag to orbit, scroll to zoom, `1`/`2`/`3` switch demo,
//! `Space` toggles the turntable, `P` toggles PRT, `H` cycles the
//! environments in `resources/hdri/` (sunrise, overcast, sunny),
//! `Esc` quits. `--hdr <file.hdr>` starts on another environment, which
//! `H` cycles with the others.
//!
//! The frame ends in depth of field and FXAA. Dragging with the right
//! button moves the sharp distance (horizontally, from the model's centre)
//! and the amount of blur (vertically, up for more); `D` turns the depth
//! of field off and on.
//!
//! `S` (or `--streak`) smears the model as if it shot along the world's Z
//! axis within the exposure, the background still: a viewer-local screen
//! effect over the model's depth (`viewer_streak.wxsl`), no velocity
//! buffer.
//!
//! **Precomputed radiance transfer** (ADR 0092). When a model has bakes
//! beside it (`wxslCar.gray.prt` and `wxslCar.color.prt` beside
//! `wxslCar.glb`, made by
//! `cargo run -p wxsl-prt --release -- resources/glb/WXSLCar/wxslCar.glb`),
//! its materials read them: the environment's diffuse light is shadowed by
//! the model itself and bounced off it, in grayscale or in colour, and its
//! reflections are occluded. `P` cycles off, grayscale and colour, and
//! `--prt off|gray|color` picks the start (colour by default), for
//! screenshots to compare. The environment is projected onto spherical
//! harmonics when the HDR loads, so `--hdr` relights the bakes.
//!
//! **Skinned models** (ADR 0099) play their clips: each skin's clips are
//! baked into a bone texture when the file opens, and the GPU skins the
//! vertices from it, so a frame only moves the row the materials read.
//! `A` cycles the clips and the imported pose; `--clip N` picks the first
//! one and `--time T` where a screenshot is taken in it
//! (`viewer -- me --clip 2 --time 1.2 --screenshot out.png`). A PRT bake
//! holds one pose, the first frame of the clip it was baked for (the first
//! by default), so it is right at the start of that clip and drifts as
//! the skin moves away from it.
//!
//! The lighting is image-based only, unless the file carries
//! `KHR_lights_punctual` lights: the HDR is convolved into diffuse and GGX
//! cubes once, and drawn behind the model. Transmissive materials (the
//! car's glass) go through the refraction tier over the same environment.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use glam::{Mat4, Vec3};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};
use wxsl::asset::{BoneTexture, ImportedScene, BONE_FRAME_PARAM, BONE_TEXTURE_SLOT};
use wxsl::core::scene::MeshSource;
use wxsl::core::Value;
use wxsl::render::environment::EnvironmentImage;
use wxsl::render::gpu::{GpuContext, OffscreenTarget};
use wxsl::render::mesh::{AttributeValues, MeshData};
use wxsl::render::scene::SceneImage;
use wxsl::render::{Camera, Environment, RenderRequest, Renderer, TargetConfig};
use wxsl::scene::SceneResources;
use wxsl_prt::{Sh9, Transfer};

/// The environments `H` cycles, under `resources/hdri/`; the first is the
/// default.
const HDRS: &[&str] = &[
    "lakeside_sunrise_1k.hdr",
    "kloofendal_overcast_1k.hdr",
    "kloofendal_48d_partly_cloudy_1k.hdr",
];

/// The demos: a name, and the file under `resources/glb/`.
const DEMOS: &[(&str, &str)] = &[
    ("me", "me_car/me_car.glb"),
    ("wxsl-car", "WXSLCar/wxslCar.glb"),
    ("wxsl-garage", "WXSLGarage/wxslGarage.glb"),
    ("glam-velvet-sofa", "GlamVelvetSofa/GlamVelvetSofa.glb"),
    ("damaged-helmet", "DamagedHelmet/DamagedHelmet.glb"),
    ("car-concept", "CarConcept/CarConcept.glb"),
];

const USAGE: &str = "viewer [demo|file.glb] [--hdr file.hdr] [--screenshot out.png] [--size WxH]
              [--grid N] [--bench FRAMES] [--prt off|gray|color] [--zoom Z] [--yaw A]
              [--focus F] [--blur B] [--no-dof] [--streak] [--clip N] [--time T]
demos: wxsl-car (default), wxsl-garage, me, glam-velvet-sofa, damaged-helmet, car-concept
window: drag orbits, scroll zooms, 1/2/3 switch demo, Space toggles the turntable,
        right-drag sets the focus (x) and the blur (y), D toggles depth of field,
        S toggles a motion blur along Z, A cycles a skinned model's clips,
        P cycles PRT off / gray / color (when the model has .prt bakes),
        H cycles the environment (sunrise, overcast, sunny), Esc quits";

fn resources_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources")
}

/// A demo name or a path, as a file.
fn model_path(name: &str) -> PathBuf {
    match DEMOS.iter().find(|(demo, _)| *demo == name) {
        Some((_, file)) => resources_dir().join("glb").join(file),
        None => PathBuf::from(name),
    }
}

struct Options {
    model: String,
    hdr: PathBuf,
    screenshot: Option<PathBuf>,
    size: (u32, u32),
    grid: u32,
    bench: Option<u32>,
    prt: Prt,
    zoom: f32,
    yaw: f32,
    lens: Lens,
    /// Whether the model streaks along Z (`S`).
    streak: bool,
    /// The clip a skinned model starts playing.
    clip: usize,
    /// Where in it a screenshot or a bench is taken, in seconds.
    time: f32,
}

fn parse_options() -> Result<Options, Box<dyn Error>> {
    let mut options = Options {
        model: DEMOS[0].0.to_string(),
        hdr: resources_dir().join("hdri").join(HDRS[0]),
        screenshot: None,
        size: (1280, 720),
        grid: 1,
        bench: None,
        prt: Prt::Color,
        zoom: 1.0,
        yaw: Orbit::default().yaw,
        lens: Lens::default(),
        streak: false,
        clip: 0,
        time: 0.0,
    };
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let mut value = || arguments.next().ok_or(format!("{argument} needs a value"));
        match argument.as_str() {
            "--hdr" => options.hdr = PathBuf::from(value()?),
            "--screenshot" => options.screenshot = Some(PathBuf::from(value()?)),
            "--grid" => options.grid = value()?.parse::<u32>()?.max(1),
            "--bench" => options.bench = Some(value()?.parse()?),
            "--prt" => options.prt = Prt::parse(&value()?)?,
            "--no-prt" => options.prt = Prt::Off,
            "--zoom" => options.zoom = value()?.parse()?,
            "--yaw" => options.yaw = value()?.parse()?,
            "--focus" => options.lens.focus = value()?.parse()?,
            "--blur" => options.lens.blur = value()?.parse()?,
            "--no-dof" => options.lens.enabled = false,
            "--streak" => options.streak = true,
            "--clip" => options.clip = value()?.parse()?,
            "--time" => options.time = value()?.parse()?,
            "--size" => {
                let text = value()?;
                let (width, height) = text.split_once('x').ok_or("--size is WxH")?;
                options.size = (width.parse()?, height.parse()?);
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag `{other}`\n{USAGE}").into())
            }
            model => options.model = model.to_string(),
        }
    }
    Ok(options)
}

/// The environment: the uploaded HDR, and its projection onto spherical
/// harmonics for the PRT bake to be lit by.
struct Hdr {
    image: EnvironmentImage,
    sh: Sh9,
}

/// Read a Radiance `.hdr` and upload it as the IBL source.
fn load_hdr(gpu: &GpuContext, path: &Path) -> Result<Hdr, Box<dyn Error>> {
    let image = image::ImageReader::open(path)?
        .with_guessed_format()?
        .decode()?
        .to_rgb32f();
    let pixels: Vec<[f32; 3]> = image.pixels().map(|pixel| pixel.0).collect();
    Ok(Hdr {
        image: wxsl::render::environment::upload_environment_image(
            &gpu.device,
            &gpu.queue,
            image.width(),
            image.height(),
            &pixels,
        )?,
        sh: wxsl_prt::sh::project_equirect(image.width(), image.height(), &pixels),
    })
}

/// The stock forward pipeline, with the HDR convolved into the
/// environment maps and drawn behind the scene. With `refractive`, the
/// `refractive` materials leave the opaque passes for the refraction tier
/// (ADR 0067), traced over the same environment.
fn add_ibl(
    renderer: &mut Renderer,
    hdr: &EnvironmentImage,
    refractive: bool,
) -> Result<(), Box<dyn Error>> {
    use wxsl::render::effect::{ibl, refraction};
    use wxsl::render::pass::{Attachment, DrawSource, PassDesc, PassKind, Read, ResourceDesc};
    use wxsl::render::types::{Color, TextureFormat};

    renderer.set_pipeline(wxsl::render::StockPipeline::Forward);
    renderer.remove_import("viewer HDR");
    let mut graph = renderer.render_graph().clone();
    if refractive {
        for index in 0..graph.passes().len() {
            if let Some(PassKind::Geometry { source, .. }) =
                graph.pass_mut(index).map(|pass| &mut pass.kind)
            {
                *source = DrawSource::Scene(wxsl::core::scene::TagExpr::parse("!refractive")?);
            }
        }
    }
    let source = graph.resource(ResourceDesc::imported(
        "viewer HDR",
        TextureFormat::Rgba32Float,
    ));
    let maps = ibl::append_filtered_bake(&mut graph, source, "viewer IBL", 128, 16, 8);
    graph.declare_environment_maps(maps.diffuse, maps.specular);
    graph.set_environment_scale(hdr.scale);
    let tonemap = graph
        .passes()
        .iter()
        .position(
            |pass| matches!(&pass.kind, PassKind::Screen { effect } if effect == "wxsl.tonemap"),
        )
        .ok_or("the pipeline has no tonemap")?;
    let color = graph.passes()[tonemap].reads[0].resource;
    let depth = graph
        .passes()
        .iter()
        .filter_map(|pass| pass.depth.as_ref().map(|attachment| attachment.resource))
        .next_back()
        .ok_or("the pipeline has no depth")?;
    let mut background =
        ibl::append_background(&mut graph, color, depth, maps.radiance, "viewer background");
    if refractive {
        background = refraction::append(
            &mut graph,
            background,
            depth,
            maps.radiance,
            "viewer glass",
            2,
        );
    }
    let hdr_target = |name: &str| ResourceDesc::color(name, TextureFormat::Rgba16Float);
    // The streak, on the linear image; `streak_motion` sets it every frame
    // and a zero motion passes the image through.
    let streaked = graph.resource(hdr_target("viewer streaked"));
    graph.pass(
        PassDesc::screen(STREAK_PASS, STREAK.id)
            .with_color(Attachment::clear(streaked, Color::TRANSPARENT))
            .with_reads([Read::current(background), Read::current(depth)]),
    );
    let background = streaked;
    // Depth of field over the linear image, before the tonemap; the lens
    // is set every frame by `Lens::apply`.
    let coc = graph.resource(hdr_target("viewer dof coc"));
    let far = graph.resource(hdr_target("viewer dof far"));
    let focused = graph.resource(hdr_target("viewer dof"));
    let clear = |resource| Attachment::clear(resource, Color::TRANSPARENT);
    graph.pass(
        PassDesc::screen(DOF_CIRCLES, "wxsl.dof_coc")
            .with_color(clear(coc))
            .with_reads([Read::current(depth)]),
    );
    graph.pass(
        PassDesc::screen("viewer dof far", "wxsl.dof_far")
            .with_color(clear(far))
            .with_reads([Read::current(background), Read::current(coc)])
            .with_parameter("max_coc", Value::F32(MAX_COC)),
    );
    graph.pass(
        PassDesc::screen("viewer dof near", "wxsl.dof_near")
            .with_color(clear(focused))
            .with_reads([
                Read::current(background),
                Read::current(coc),
                Read::current(far),
            ])
            .with_parameter("max_coc", Value::F32(MAX_COC)),
    );
    // FXAA after the tonemap, on display values: the tonemap writes an
    // intermediate, and FXAA takes over its attachment.
    let encoded = graph.resource(hdr_target("viewer encoded"));
    let pass = graph.pass_mut(tonemap).ok_or("tonemap pass")?;
    pass.reads[0] = Read::current(focused);
    let presented = std::mem::replace(&mut pass.color[0], clear(encoded));
    graph.pass(
        PassDesc::screen("viewer fxaa", "wxsl.fxaa")
            .with_color(presented)
            .with_reads([Read::current(encoded)]),
    );
    renderer.set_graph(graph)?;
    renderer.import_resource("viewer HDR", hdr.view.clone());
    Ok(())
}

/// The streak's pass, which `streak_motion` sets.
const STREAK_PASS: &str = "viewer streak";

/// The `S` key's effect: the model smeared along one screen vector.
const STREAK: wxsl::render::effect::Effect = {
    use wxsl::render::effect::{
        Downlevel, Effect, EffectInput, EffectInputKind, EffectKind, EffectParameter, EffectShader,
    };
    Effect {
        id: "viewer.streak",
        label: "streak",
        description: "Smear what has depth along one screen vector, over a still background.",
        kind: EffectKind::Screen {
            vertex_entry: "viewer_streak_vs",
            fragment_entry: "viewer_streak_fs",
        },
        inputs: &[
            EffectInput {
                name: "color",
                kind: EffectInputKind::Image,
                description: "The frame, as linear radiance.",
                history: 0,
            },
            EffectInput {
                name: "depth",
                kind: EffectInputKind::Image,
                description: "Opaque depth; 1 is the background.",
                history: 0,
            },
        ],
        outputs: &[],
        parameters: &[EffectParameter {
            name: "motion",
            default: Value::Vec2([0.0, 0.0]),
        }],
        shader: EffectShader::Source {
            path: "package::viewer::streak",
            wxsl: include_str!("viewer_streak.wxsl"),
        },
        downlevel: Downlevel::Portable,
    }
};

/// How far the model travels along Z during the exposure, in model radii.
const STREAK_LENGTH: f32 = 0.05;

/// Set the streak for `camera`: the screen vector of a move along +Z from
/// the model's centre, or zero when `on` is false.
fn streak_motion(
    renderer: &mut Renderer,
    camera: &Camera,
    radius: f32,
    on: bool,
) -> Result<(), Box<dyn Error>> {
    let motion = if on {
        let view_proj = camera.view_proj();
        let screen = |point: Vec3| {
            let clip = view_proj * point.extend(1.0);
            glam::Vec2::new(clip.x, -clip.y) / clip.w * 0.5
        };
        let target = camera.target;
        screen(target + Vec3::Z * radius * STREAK_LENGTH) - screen(target)
    } else {
        glam::Vec2::ZERO
    };
    renderer.set_pass_param(STREAK_PASS, "motion", Value::Vec2(motion.to_array()))?;
    Ok(())
}

/// The depth-of-field pass the lens' numbers go to.
const DOF_CIRCLES: &str = "viewer dof circles";
/// The widest blur disc, in pixels.
const MAX_COC: f32 = 24.0;

/// The depth of field's lens, relative to the orbit so that it frames any
/// model alike.
#[derive(Clone, Copy)]
struct Lens {
    /// The sharp distance over the camera's distance to the model's centre.
    focus: f32,
    /// How much blur: the aperture, relative to the scene's size.
    blur: f32,
    enabled: bool,
}

impl Default for Lens {
    fn default() -> Self {
        Lens {
            focus: 1.0,
            blur: 1.0,
            enabled: true,
        }
    }
}

impl Lens {
    /// Set the circles-of-confusion pass for `camera`, which looks at the
    /// model's centre.
    fn apply(self, renderer: &mut Renderer, camera: &Camera) -> Result<(), Box<dyn Error>> {
        let distance = (camera.target - camera.eye).length();
        // A 100 mm lens; the f-number shrinks with the scene so that the
        // same blur reads the same on a helmet and on a garage. Disabled,
        // a pinhole: every disc is under a pixel.
        let f_number = if self.enabled {
            120.0 / (distance * self.blur).max(1e-6)
        } else {
            1e6
        };
        for (name, value) in [
            ("near", camera.near),
            ("far", camera.far),
            ("focus", distance * self.focus),
            ("f_number", f_number),
            ("focal", 0.1f32.min(distance * self.focus * 0.5)),
        ] {
            renderer.set_pass_param(DOF_CIRCLES, name, Value::F32(value))?;
        }
        Ok(())
    }
}

/// What the viewer shows of a model's PRT bakes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prt {
    Off,
    Gray,
    Color,
}

impl Prt {
    const CYCLE: [Prt; 3] = [Prt::Off, Prt::Gray, Prt::Color];

    fn transfer(self) -> Option<Transfer> {
        match self {
            Prt::Off => None,
            Prt::Gray => Some(Transfer::Gray),
            Prt::Color => Some(Transfer::Color),
        }
    }

    fn parse(text: &str) -> Result<Self, String> {
        match text {
            "off" => Ok(Prt::Off),
            "gray" => Ok(Prt::Gray),
            "color" => Ok(Prt::Color),
            other => Err(format!("--prt is off, gray or color, not `{other}`")),
        }
    }
}

/// The rate clips are baked into the bone textures at.
const BONE_FPS: f32 = 60.0;

/// One skin's baked clips, uploaded (ADR 0099).
struct Bones {
    baked: BoneTexture,
    view: wgpu::TextureView,
}

impl Bones {
    /// Bake skin `skin` of `imported` through every clip and upload it as
    /// half floats: the material group's textures are filterable.
    fn upload(gpu: &GpuContext, imported: &ImportedScene, skin: usize) -> Self {
        let baked = BoneTexture::bake(imported, skin, BONE_FPS);
        let size = wgpu::Extent3d {
            width: baked.width,
            height: baked.height,
            depth_or_array_layers: 1,
        };
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("viewer bones"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let bytes: Vec<u8> = baked
            .texels
            .iter()
            .flatten()
            .flat_map(|value| f16_bits(*value).to_le_bytes())
            .collect();
        gpu.queue.write_texture(
            texture.as_image_copy(),
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(baked.width * 8),
                rows_per_image: None,
            },
            size,
        );
        Bones {
            baked,
            view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
        }
    }
}

/// `value` as IEEE half-float bits, rounded to nearest.
fn f16_bits(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mantissa = bits & 0x7f_ffff;
    if exponent >= 31 {
        // Too large, or not a number: infinity keeps the sign at least.
        return sign | 0x7c00;
    }
    if exponent <= 0 {
        if exponent < -10 {
            return sign;
        }
        let mantissa = (mantissa | 0x80_0000) >> (1 - exponent);
        return sign | ((mantissa + 0x1000) >> 13) as u16;
    }
    // A carry out of the mantissa rounds into the exponent, as it should.
    sign | (((exponent as u32) << 10) + ((mantissa + 0x1000) >> 13)) as u16
}

/// One opened file, ready to draw.
struct Model {
    imported: ImportedScene,
    /// One per skin, in [`ImportedScene::skins`] order.
    bones: Vec<Bones>,
    /// The clip playing, or `None` for the pose the file was saved in.
    clip: Option<usize>,
    /// One set per PRT bake found beside the file, each reading its own
    /// streams; one plain set when there is none.
    variants: Vec<(Option<Transfer>, SceneResources)>,
    /// The variant drawn.
    active: usize,
    /// What it shows.
    prt: Prt,
    /// Whether the IBL pipeline has the refraction tier.
    refractive: bool,
    center: Vec3,
    radius: f32,
}

impl Model {
    fn open(
        gpu: &GpuContext,
        renderer: &mut Renderer,
        hdr: &Hdr,
        path: &Path,
        grid: u32,
        prt: Prt,
        clip: usize,
    ) -> Result<Self, Box<dyn Error>> {
        let started = Instant::now();
        let mut imported = wxsl::asset::import_gltf(path)?;
        repeat(&mut imported, grid);
        println!(
            "{}: {} nodes, {} meshes, {} materials, {} textures ({:.0?})",
            path.display(),
            imported.scene.nodes.len(),
            imported.meshes.len(),
            imported.scene.materials.len(),
            imported.scene.textures.len(),
            started.elapsed()
        );
        for warning in &imported.warnings {
            println!("  warning: {warning}");
        }
        let refractive = imported
            .scene
            .materials
            .iter()
            .any(|material| material.config.tags.contains(wxsl::asset::TAG_REFRACTIVE));
        add_ibl(renderer, &hdr.image, refractive)?;
        renderer.set_lighting(imported.lighting()?)?;
        let meshes: Vec<MeshData> = imported
            .meshes
            .iter()
            .map(wxsl::render::gltf::mesh_data)
            .collect();
        let bones: Vec<Bones> = (0..imported.skins.len())
            .map(|skin| Bones::upload(gpu, &imported, skin))
            .collect();
        for (index, animation) in imported.animations.iter().enumerate() {
            println!(
                "  clip {index}: `{}`, {:.2} s",
                animation.name, animation.duration
            );
        }
        let mut variants = Vec::new();
        for bake in prt_bakes(&imported, path, grid)? {
            let (mut scene, mut streams) = (imported.clone(), meshes.clone());
            attach_prt(&mut scene, &mut streams, &bake, &hdr.sh)?;
            let resources = load_resources(gpu, renderer, &scene, &streams, &bones)?;
            variants.push((Some(bake.transfer), resources));
        }
        if variants.is_empty() {
            variants.push((
                None,
                load_resources(gpu, renderer, &imported, &meshes, &bones)?,
            ));
        }
        let clip = (!imported.animations.is_empty())
            .then_some(clip.min(imported.animations.len().saturating_sub(1)));
        let (center, radius) = bounds(&imported, clip);
        let mut model = Model {
            imported,
            bones,
            clip,
            variants,
            active: 0,
            prt: Prt::Off,
            refractive,
            center,
            radius,
        };
        model.set_prt(gpu, prt)?;
        println!("  loaded in {:.0?}", started.elapsed());
        Ok(model)
    }

    fn resources(&self) -> &SceneResources {
        &self.variants[self.active].1
    }

    /// Show the playing clip at `time` seconds: every skinned material of
    /// the drawn variant reads its bone texture's row for it.
    fn animate(&mut self, gpu: &GpuContext, time: f32) -> Result<(), Box<dyn Error>> {
        let (_, resources) = &mut self.variants[self.active];
        for (material, skin) in self.imported.material_skins.iter().enumerate() {
            let (Some(skin), Some(bindings)) = (skin, resources.bindings_mut(material)) else {
                continue;
            };
            let frame = match self.clip {
                Some(clip) => self.bones[*skin].baked.frame(clip, time),
                None => 0.0,
            };
            bindings.set(BONE_FRAME_PARAM, Value::F32(frame))?;
        }
        resources.upload(&gpu.device, &gpu.queue)?;
        Ok(())
    }

    /// The next clip, then the pose the file was saved in.
    fn cycle_clip(&mut self) {
        let count = self.imported.animations.len();
        self.clip = match self.clip {
            Some(clip) if clip + 1 < count => Some(clip + 1),
            Some(_) => None,
            None => (count > 0).then_some(0),
        };
        match self.clip {
            Some(clip) => println!("clip `{}`", self.imported.animations[clip].name),
            None => println!("no clip"),
        }
    }

    /// What this model can show: off, then each bake it has.
    fn modes(&self) -> Vec<Prt> {
        Prt::CYCLE
            .into_iter()
            .filter(|mode| match mode.transfer() {
                None => self.variants.iter().any(|(transfer, _)| transfer.is_some()),
                Some(wanted) => self
                    .variants
                    .iter()
                    .any(|(transfer, _)| *transfer == Some(wanted)),
            })
            .collect()
    }

    /// Light the model with `hdr`: the pipeline bakes its maps again, and
    /// every PRT variant takes its spherical harmonics.
    fn set_hdr(
        &mut self,
        gpu: &GpuContext,
        renderer: &mut Renderer,
        hdr: &Hdr,
    ) -> Result<(), Box<dyn Error>> {
        add_ibl(renderer, &hdr.image, self.refractive)?;
        for (transfer, resources) in &mut self.variants {
            if transfer.is_none() {
                continue;
            }
            for material in 0..resources.materials().len() {
                if let Some(bindings) = resources.bindings_mut(material) {
                    for (index, coefficient) in hdr.sh.iter().enumerate() {
                        let name = format!("{}{index}", wxsl_prt::SH_PARAM_PREFIX);
                        bindings.set(&name, Value::Vec3(*coefficient))?;
                    }
                }
            }
            resources.upload(&gpu.device, &gpu.queue)?;
        }
        Ok(())
    }

    /// Show `prt`, or the nearest mode this model has. Off draws a baked
    /// variant at strength 0, which is no PRT without a recompile.
    fn set_prt(&mut self, gpu: &GpuContext, prt: Prt) -> Result<(), Box<dyn Error>> {
        let modes = self.modes();
        let prt = match prt {
            _ if modes.contains(&prt) => prt,
            // Colour asked for and only grayscale baked, or the reverse.
            Prt::Gray | Prt::Color => modes.last().copied().unwrap_or(Prt::Off),
            Prt::Off => Prt::Off,
        };
        self.active = match prt.transfer() {
            Some(wanted) => self
                .variants
                .iter()
                .position(|(transfer, _)| *transfer == Some(wanted))
                .unwrap_or(0),
            None => self.active,
        };
        self.prt = prt;
        let (transfer, resources) = &mut self.variants[self.active];
        if transfer.is_some() {
            let strength = Value::F32(if prt == Prt::Off { 0.0 } else { 1.0 });
            for material in 0..resources.materials().len() {
                if let Some(bindings) = resources.bindings_mut(material) {
                    bindings.set(wxsl_prt::STRENGTH_PARAM, strength)?;
                }
            }
        }
        resources.upload(&gpu.device, &gpu.queue)?;
        Ok(())
    }
}

/// Compile and upload `imported` with `meshes`, against the renderer's
/// plan.
fn load_resources(
    gpu: &GpuContext,
    renderer: &mut Renderer,
    imported: &ImportedScene,
    meshes: &[MeshData],
    bones: &[Bones],
) -> Result<SceneResources, Box<dyn Error>> {
    let mut resources = SceneResources::load_resolved(
        &gpu.device,
        &imported.scene,
        &wxsl::stdlib::registry(),
        imported.directory.as_deref(),
        renderer.lighting(),
        &[],
        meshes,
    )?;
    resources.set_images(
        imported
            .images
            .iter()
            .map(|bytes| SceneImage::Encoded(bytes.clone()))
            .collect(),
    );
    resources.create_bindings(&gpu.device, &gpu.queue, renderer)?;
    for (material, skin) in imported.material_skins.iter().enumerate() {
        if let (Some(skin), Some(bindings)) = (skin, resources.bindings_mut(material)) {
            bindings.set_texture(BONE_TEXTURE_SLOT, &bones[*skin].view)?;
        }
    }
    Ok(resources)
}

/// The PRT bakes beside `path`: `<stem>.gray.prt` and `<stem>.color.prt`,
/// whichever exist.
fn prt_bakes(
    imported: &ImportedScene,
    path: &Path,
    grid: u32,
) -> Result<Vec<wxsl_prt::Bake>, Box<dyn Error>> {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let mut bakes = Vec::new();
    for transfer in Transfer::ALL {
        let file = path.with_file_name(format!("{stem}.{}.prt", transfer.name()));
        if !file.exists() {
            continue;
        }
        // The bake is for the model where the file puts it, not for copies.
        if grid > 1 {
            println!("  {}: not used with --grid", file.display());
            continue;
        }
        let bake = wxsl_prt::Bake::read(std::io::BufReader::new(std::fs::File::open(&file)?))
            .and_then(|bake| bake.check(imported).map(|()| bake))
            .map_err(|error| format!("{}: {error}", file.display()))?;
        println!("  PRT: {}", file.display());
        bakes.push(bake);
    }
    Ok(bakes)
}

/// Put `bake`'s streams on `meshes` and rewire every material of
/// `imported` to read them, lit by `environment`.
fn attach_prt(
    imported: &mut ImportedScene,
    meshes: &mut [MeshData],
    bake: &wxsl_prt::Bake,
    environment: &Sh9,
) -> Result<(), Box<dyn Error>> {
    for (index, mesh) in meshes.iter_mut().enumerate() {
        let streams = bake.streams(index).ok_or("bake and model disagree")?;
        for (name, values) in bake.transfer.streams().iter().zip(streams) {
            mesh.attributes
                .insert(name.to_string(), AttributeValues::Vec4(values));
        }
    }
    let registry = wxsl::stdlib::registry();
    for material in &mut imported.scene.materials {
        wxsl_prt::apply(&mut material.graph, &registry, environment, bake.transfer)?;
    }
    Ok(())
}

/// Repeat every instance of `imported` on a `grid`×`grid` floor, spaced by
/// the model's size.
fn repeat(imported: &mut ImportedScene, grid: u32) {
    if grid <= 1 {
        return;
    }
    let (_, radius) = bounds(imported, None);
    let scene = &mut imported.scene;
    let originals = std::mem::take(&mut scene.instances);
    for row in 0..grid {
        for column in 0..grid {
            let offset =
                Mat4::from_translation(Vec3::new(column as f32, 0.0, row as f32) * radius * 2.5);
            for original in &originals {
                let mut copy = original.clone();
                let world = Mat4::from_cols_array(&scene.instance_world(original));
                copy.transform = (offset * world).to_cols_array();
                copy.node = None;
                scene.instances.push(copy);
            }
        }
    }
}

/// The centre and radius of every instance's vertices, in world space; a
/// skinned one's where its joints put it rather than its node, at the
/// start of `clip` or in the pose the file was saved in.
fn bounds(imported: &ImportedScene, clip: Option<usize>) -> (Vec3, f32) {
    let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    let mut posed = imported.scene.clone();
    if let Some(animation) = clip.and_then(|clip| imported.animations.get(clip)) {
        animation.pose(0.0, &imported.scene.nodes, &mut posed.nodes);
    }
    let scene = &imported.scene;
    for instance in &scene.instances {
        let MeshSource::Embedded { index } = scene.meshes[instance.mesh].source else {
            continue;
        };
        let geometry = &imported.meshes[index];
        let world = Mat4::from_cols_array(&scene.instance_world(instance));
        let joints: Vec<Mat4> = match imported.material_skins[instance.material] {
            Some(skin) => {
                let skin = &imported.skins[skin];
                skin.joint_matrices(&posed, &scene.node_world(skin.node))
                    .iter()
                    .map(Mat4::from_cols_array)
                    .collect()
            }
            None => Vec::new(),
        };
        for (at, vertex) in geometry.vertices.iter().enumerate() {
            let mut point = Vec3::from_array(vertex.position);
            if let (false, Some(indices), Some(weights)) =
                (joints.is_empty(), &geometry.joints, &geometry.weights)
            {
                point = indices[at]
                    .iter()
                    .zip(weights[at])
                    .filter_map(|(&joint, weight)| {
                        Some(joints.get(joint as usize)?.transform_point3(point) * weight)
                    })
                    .sum();
            }
            let point = world.transform_point3(point);
            min = min.min(point);
            max = max.max(point);
        }
    }
    if min.x > max.x {
        return (Vec3::ZERO, 1.0);
    }
    ((min + max) * 0.5, ((max - min).length() * 0.5).max(1e-3))
}

/// Where the camera is, around the model.
#[derive(Clone, Copy)]
struct Orbit {
    yaw: f32,
    pitch: f32,
    zoom: f32,
}

impl Default for Orbit {
    fn default() -> Self {
        Orbit {
            yaw: 0.6,
            pitch: 0.15,
            zoom: 1.0,
        }
    }
}

/// The closest the camera comes to the model's centre, in metres.
const MIN_DISTANCE: f32 = 0.1;

/// The camera's distance at zoom 1: the whole model in a 45° field.
fn framed_distance(model: &Model) -> f32 {
    model.radius / (45f32.to_radians() * 0.5).sin() * 1.05
}

fn environment(model: &Model, orbit: Orbit, aspect: f32) -> Environment {
    let fov_y = 45f32.to_radians();
    let distance = framed_distance(model) * orbit.zoom;
    let direction = Vec3::new(
        orbit.yaw.sin() * orbit.pitch.cos(),
        orbit.pitch.sin(),
        orbit.yaw.cos() * orbit.pitch.cos(),
    );
    let lights = wxsl::scene::environment(&model.imported.scene, aspect)
        .map(|environment| environment.lights)
        .unwrap_or_default();
    Environment {
        camera: Camera {
            eye: model.center + direction * distance,
            target: model.center,
            fov_y,
            aspect,
            near: (distance - model.radius * 1.5).max(distance * 0.01),
            far: distance + model.radius * 4.0,
            ..Camera::default()
        },
        lights,
        ..Environment::default()
    }
}

fn renderer_for(gpu: &GpuContext, target: TargetConfig) -> Result<Renderer, Box<dyn Error>> {
    let mut renderer = Renderer::new(&gpu.device, wxsl::stdlib_library(), target)?;
    for effect in [
        wxsl::render::effect::ibl::EQUIRECT_TO_CUBE,
        wxsl::render::effect::ibl::DIFFUSE,
        wxsl::render::effect::ibl::SPECULAR,
        wxsl::render::effect::ibl::RESAMPLE,
    ] {
        renderer.add_effect(effect);
    }
    renderer.add_effect(wxsl::effects::fxaa(&wxsl::stdlib::registry())?);
    renderer.add_effect(STREAK);
    Ok(renderer)
}

fn screenshot(options: &Options, file: &Path) -> Result<(), Box<dyn Error>> {
    let gpu = pollster::block_on(GpuContext::headless())?;
    let (width, height) = options.size;
    let target = OffscreenTarget::new(&gpu.device, width, height);
    let hdr = load_hdr(&gpu, &options.hdr)?;
    let mut renderer = renderer_for(&gpu, TargetConfig::new(width, height, target.format()))?;
    let mut model = Model::open(
        &gpu,
        &mut renderer,
        &hdr,
        &model_path(&options.model),
        options.grid,
        options.prt,
        options.clip,
    )?;
    model.animate(&gpu, options.time)?;
    let orbit = Orbit {
        zoom: options.zoom,
        yaw: options.yaw,
        ..Orbit::default()
    };
    let environment = environment(&model, orbit, width as f32 / height as f32);
    options.lens.apply(&mut renderer, &environment.camera)?;
    streak_motion(
        &mut renderer,
        &environment.camera,
        model.radius,
        options.streak,
    )?;
    let draws = model.resources().draw_list();
    // The environment maps are baked by the first frame; render twice so
    // the saved one is the steady state.
    for _ in 0..2 {
        renderer.render(
            &gpu.device,
            &gpu.queue,
            &RenderRequest {
                view: target.view(),
                environment: &environment,
                draws: &draws,
            },
        )?;
    }
    gpu.wait();
    let pixels = target.read_rgba8(&gpu.device, &gpu.queue);
    image::save_buffer(
        file,
        &pixels,
        width,
        height,
        image::ExtendedColorType::Rgba8,
    )?;
    println!("saved {}", file.display());
    Ok(())
}

struct State {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    gpu: GpuContext,
    renderer: Renderer,
    format: wgpu::TextureFormat,
    hdr: Hdr,
    model: Model,
}

struct App {
    options: Options,
    state: Option<State>,
    error: Option<Box<dyn Error>>,
    orbit: Orbit,
    turntable: bool,
    dragging: bool,
    /// The right button is down: the mouse moves the lens.
    focusing: bool,
    cursor: Option<(f64, f64)>,
    last_frame: Instant,
    /// Where the playing clip is, in seconds.
    clip_time: f32,
}

impl App {
    fn create_state(&mut self, event_loop: &ActiveEventLoop) -> Result<State, Box<dyn Error>> {
        let (width, height) = self.options.size;
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title(format!("wxsl viewer — {}", self.options.model))
                    .with_inner_size(winit::dpi::LogicalSize::new(width, height)),
            )?,
        );
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance.create_surface(Arc::clone(&window))?;
        let gpu = pollster::block_on(GpuContext::new(instance, Some(&surface)))?;
        // Linear: the tonemap encodes sRGB itself.
        let capabilities = surface.get_capabilities(&gpu.adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|format| !format.is_srgb())
            .unwrap_or(capabilities.formats[0]);
        let size = window.inner_size();
        let hdr = load_hdr(&gpu, &self.options.hdr)?;
        let mut renderer = renderer_for(
            &gpu,
            TargetConfig::new(size.width.max(1), size.height.max(1), format),
        )?;
        let model = Model::open(
            &gpu,
            &mut renderer,
            &hdr,
            &model_path(&self.options.model),
            self.options.grid,
            self.options.prt,
            self.options.clip,
        )?;
        let mut state = State {
            window,
            surface,
            gpu,
            renderer,
            format,
            hdr,
            model,
        };
        configure_surface(&mut state, size.width, size.height);
        println!("adapter: {}", state.gpu.adapter.get_info().name);
        Ok(state)
    }

    fn switch(&mut self, demo: usize) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let (name, _) = DEMOS[demo];
        match Model::open(
            &state.gpu,
            &mut state.renderer,
            &state.hdr,
            &model_path(name),
            self.options.grid,
            self.options.prt,
            self.options.clip,
        ) {
            Ok(model) => {
                state.model = model;
                self.options.model = name.to_string();
                self.orbit = Orbit::default();
                self.clip_time = 0.0;
                state.window.set_title(&format!("wxsl viewer — {name}"));
            }
            Err(error) => eprintln!("cannot open {name}: {error}"),
        }
    }

    /// The next environment: the ones in `HDRS`, and a `--hdr` file
    /// outside them.
    fn cycle_hdr(&mut self) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let directory = resources_dir().join("hdri");
        let mut cycle: Vec<PathBuf> = HDRS.iter().map(|name| directory.join(name)).collect();
        if !cycle.contains(&self.options.hdr) {
            cycle.insert(0, self.options.hdr.clone());
        }
        let at = cycle.iter().position(|path| *path == self.options.hdr);
        let next = &cycle[at.map_or(0, |at| (at + 1) % cycle.len())];
        let switched = load_hdr(&state.gpu, next).and_then(|hdr| {
            state.model.set_hdr(&state.gpu, &mut state.renderer, &hdr)?;
            state.hdr = hdr;
            Ok(())
        });
        match switched {
            Ok(()) => {
                self.options.hdr = next.clone();
                println!("environment {}", next.display());
            }
            Err(error) => eprintln!("cannot load {}: {error}", next.display()),
        }
    }

    /// The next of off, grayscale and colour this model has.
    fn cycle_prt(&mut self) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let modes = state.model.modes();
        if modes.is_empty() {
            println!("no PRT bake for {}", self.options.model);
            return;
        }
        let at = modes.iter().position(|mode| *mode == state.model.prt);
        let next = modes[at.map_or(0, |at| (at + 1) % modes.len())];
        match state.model.set_prt(&state.gpu, next) {
            Ok(()) => {
                self.options.prt = state.model.prt;
                println!("PRT {:?}", state.model.prt);
            }
            Err(error) => eprintln!("cannot switch PRT: {error}"),
        }
    }

    fn render(&mut self) {
        let elapsed = self.last_frame.elapsed().as_secs_f32();
        self.last_frame = Instant::now();
        if self.turntable && !self.dragging {
            self.orbit.yaw += elapsed * 0.3;
        }
        self.clip_time += elapsed;
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if let Err(error) = state.model.animate(&state.gpu, self.clip_time) {
            eprintln!("cannot animate: {error}");
        }
        let frame = match state.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => {
                let size = state.window.inner_size();
                configure_surface(state, size.width, size.height);
                return;
            }
            _ => return,
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let size = state.window.inner_size();
        let environment = environment(
            &state.model,
            self.orbit,
            size.width as f32 / size.height.max(1) as f32,
        );
        if let Err(error) = self
            .options
            .lens
            .apply(&mut state.renderer, &environment.camera)
        {
            eprintln!("cannot set the lens: {error}");
        }
        if let Err(error) = streak_motion(
            &mut state.renderer,
            &environment.camera,
            state.model.radius,
            self.options.streak,
        ) {
            eprintln!("cannot set the streak: {error}");
        }
        if let Err(error) = state.renderer.render(
            &state.gpu.device,
            &state.gpu.queue,
            &RenderRequest {
                view: &view,
                environment: &environment,
                draws: &state.model.resources().draw_list(),
            },
        ) {
            eprintln!("cannot render: {error}");
        }
        state.gpu.queue.present(frame);
    }
}

fn configure_surface(state: &mut State, width: u32, height: u32) {
    let (width, height) = (width.max(1), height.max(1));
    state.surface.configure(
        &state.gpu.device,
        &wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: state.format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width,
            height,
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
        },
    );
    state.renderer.resize(&state.gpu.device, width, height);
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        match self.create_state(event_loop) {
            Ok(state) => {
                self.state = Some(state);
                println!("{USAGE}");
            }
            Err(error) => {
                self.error = Some(error);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(state) = self.state.as_mut() {
                    configure_surface(state, size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => self.render(),
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => self.dragging = state == ElementState::Pressed,
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Right,
                ..
            } => {
                self.focusing = state == ElementState::Pressed;
                if !self.focusing {
                    let lens = self.options.lens;
                    println!("focus {:.2}, blur {:.2}", lens.focus, lens.blur);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some((x, y)) = self.cursor {
                    let (dx, dy) = ((position.x - x) as f32, (position.y - y) as f32);
                    if self.dragging {
                        self.orbit.yaw -= dx * 0.008;
                        self.orbit.pitch = (self.orbit.pitch + dy * 0.008).clamp(-1.45, 1.45);
                    }
                    if self.focusing {
                        let lens = &mut self.options.lens;
                        // Right moves the sharp plane away; up blurs more.
                        lens.focus = (lens.focus * (dx * 0.004).exp()).clamp(0.1, 10.0);
                        lens.blur = (lens.blur * (-dy * 0.006).exp()).clamp(0.02, 20.0);
                    }
                }
                self.cursor = Some((position.x, position.y));
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(position) => position.y as f32 / 40.0,
                };
                // Down to `MIN_DISTANCE` from the centre, whatever the size.
                let closest = self.state.as_ref().map_or(0.2, |state| {
                    (MIN_DISTANCE / framed_distance(&state.model)).min(0.2)
                });
                self.orbit.zoom = (self.orbit.zoom * 0.9f32.powf(lines)).clamp(closest, 5.0);
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        state: ElementState::Pressed,
                        repeat: false,
                        ..
                    },
                ..
            } => match code {
                KeyCode::Escape | KeyCode::KeyQ => event_loop.exit(),
                KeyCode::Space => self.turntable = !self.turntable,
                KeyCode::KeyP => self.cycle_prt(),
                KeyCode::KeyH => self.cycle_hdr(),
                KeyCode::KeyD => {
                    let lens = &mut self.options.lens;
                    lens.enabled = !lens.enabled;
                    println!("depth of field {}", if lens.enabled { "on" } else { "off" });
                }
                KeyCode::KeyA => {
                    if let Some(state) = self.state.as_mut() {
                        state.model.cycle_clip();
                        self.options.clip = state.model.clip.unwrap_or(0);
                        self.clip_time = 0.0;
                    }
                }
                KeyCode::KeyS => {
                    self.options.streak = !self.options.streak;
                    println!("streak {}", if self.options.streak { "on" } else { "off" });
                }
                KeyCode::Digit1 => self.switch(0),
                KeyCode::Digit2 => self.switch(1),
                KeyCode::Digit3 => self.switch(2),
                _ => {}
            },
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _: &ActiveEventLoop) {
        if let Some(state) = self.state.as_ref() {
            state.window.request_redraw();
        }
    }
}

/// Render `frames` frames headless, framed and then from inside the grid,
/// and print what they cost.
fn bench(options: &Options, frames: u32) -> Result<(), Box<dyn Error>> {
    let gpu = pollster::block_on(GpuContext::headless())?;
    let (width, height) = options.size;
    let target = OffscreenTarget::new(&gpu.device, width, height);
    let hdr = load_hdr(&gpu, &options.hdr)?;
    let mut renderer = renderer_for(&gpu, TargetConfig::new(width, height, target.format()))?;
    let mut model = Model::open(
        &gpu,
        &mut renderer,
        &hdr,
        &model_path(&options.model),
        options.grid,
        options.prt,
        options.clip,
    )?;
    model.animate(&gpu, options.time)?;
    let aspect = width as f32 / height as f32;
    let views = [
        ("framed", Orbit::default()),
        (
            "inside",
            Orbit {
                zoom: 0.3,
                ..Orbit::default()
            },
        ),
    ];
    for (name, orbit) in views {
        let environment = environment(&model, orbit, aspect);
        options.lens.apply(&mut renderer, &environment.camera)?;
        streak_motion(
            &mut renderer,
            &environment.camera,
            model.radius,
            options.streak,
        )?;
        let (mut cpu, mut wall, mut gpu_ms) = (0.0, 0.0, Vec::new());
        // Two frames first: the environment bake and the shader compiles.
        for frame in 0..frames + 2 {
            let started = Instant::now();
            let draws = model.resources().draw_list();
            renderer.render(
                &gpu.device,
                &gpu.queue,
                &RenderRequest {
                    view: target.view(),
                    environment: &environment,
                    draws: &draws,
                },
            )?;
            let recorded = started.elapsed().as_secs_f64();
            gpu.wait();
            if frame >= 2 {
                cpu += recorded;
                wall += started.elapsed().as_secs_f64();
                gpu_ms.extend(renderer.frame_time());
            }
        }
        let (mut draws, mut calls) = (0, 0);
        for pass in renderer.render_graph().passes() {
            draws += renderer.pass_draw_count(&pass.label).unwrap_or(0);
            calls += renderer.pass_call_count(&pass.label).unwrap_or(0);
        }
        let per = |total: f64| total * 1000.0 / f64::from(frames.max(1));
        let gpu_text = if gpu_ms.is_empty() {
            "no timestamps".to_string()
        } else {
            format!("{:.2} ms", gpu_ms.iter().sum::<f32>() / gpu_ms.len() as f32)
        };
        println!(
            "{name}: {} instances, {draws} draws in {calls} calls · cpu {:.2} ms · \
             cpu+wait {:.2} ms · gpu {gpu_text}",
            model.resources().instance_count(),
            per(cpu),
            per(wall),
        );
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let options = parse_options()?;
    if let Some(frames) = options.bench {
        return bench(&options, frames);
    }
    if let Some(file) = options.screenshot.clone() {
        return screenshot(&options, &file);
    }
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        options,
        state: None,
        error: None,
        orbit: Orbit::default(),
        turntable: true,
        dragging: false,
        focusing: false,
        cursor: None,
        last_frame: Instant::now(),
        clip_time: 0.0,
    };
    event_loop.run_app(&mut app)?;
    match app.error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

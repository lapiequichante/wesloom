//! The live material preview, and the compiled source behind it.
//!
//! The editor's answer to "what does this graph *do*": the graph is compiled
//! to WXSL, the WXSL to WGSL, and the result rendered onto a mesh in an
//! offscreen target that the interface then draws as a textured quad. Which
//! is why the editor needs the renderer rather than a picture handed to it
//! from outside (ADR 0013) — the preview is the same [`Renderer`], the same
//! variant cache and the same two pipelines a shipped application would use,
//! so what is on screen here is what the application will get.
//!
//! It also keeps the two intermediate forms as text: [`Preview::wxsl`] is
//! what codegen produced and [`Preview::wgsl`] is what the compiler made of
//! it. Those are the two code panels, and they are the whole reason a shader
//! graph is debuggable at all.

use glam::{Mat4, Vec3};
use wxsl_core::graph::Graph;
use wxsl_core::macros::MacroSet;
use wxsl_core::node::{NodeRegistry, ValueType};
use wxsl_render::gpu::OffscreenTarget;
use wxsl_render::ui::draw::TextureId;
use wxsl_render::ui::UiRenderer;
use wxsl_render::{
    AttributeValues, Camera, DrawItem, Environment, InstanceAttributes, Light, Material,
    MaterialBindings, Mesh, MeshKind, RenderError, RenderRequest, Renderer, ShaderLibrary,
    StockPipeline, SwapProgress, TargetConfig,
};

use crate::highlight::{self, Run};

/// How the preview is doing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PreviewStatus {
    /// Nothing has been compiled yet.
    #[default]
    Empty,
    /// The graph compiled and is rendering.
    Ok,
    /// Something refused it. The strings are the errors, in the order they
    /// were reported — [`Graph::validate`] returns all of them, and showing
    /// one at a time turns fixing a graph into a guessing game.
    Failed(Vec<String>),
}

impl PreviewStatus {
    /// Whether the graph currently compiles.
    pub fn is_ok(&self) -> bool {
        matches!(self, PreviewStatus::Ok)
    }

    /// The errors, if it does not.
    pub fn errors(&self) -> &[String] {
        match self {
            PreviewStatus::Failed(errors) => errors,
            _ => &[],
        }
    }
}

/// The offscreen material preview.
pub struct Preview {
    renderer: Renderer,
    target: OffscreenTarget,
    texture: TextureId,
    mesh: Mesh,
    mesh_kind: MeshKind,
    material: Option<Material>,
    /// The material's own bind group, rebuilt whenever the graph changes
    /// what it declares.
    bindings: Option<MaterialBindings>,
    /// A value for every per-instance attribute the material declares.
    ///
    /// Stand-ins, like [`Preview::placeholder`]: the editor is not the
    /// application, and a material that cannot be drawn cannot be
    /// previewed.
    attributes: InstanceAttributes,
    /// A zero-filled stand-in for the block the graph expects the
    /// *application* to supply. The editor is not that application, so
    /// there is nothing truer it could bind — and binding nothing would
    /// mean the graph could not be previewed at all.
    user: Option<wgpu::BindGroup>,
    /// Bound wherever a graph declares a texture it has no way to supply
    /// yet. A checker rather than white, so "this is a placeholder" is
    /// visible rather than a guess.
    placeholder: wgpu::TextureView,
    placeholder_sampler: wgpu::Sampler,
    wxsl: String,
    wgsl: String,
    // Computed once, alongside `wxsl`/`wgsl`, rather than every frame the
    // code panel draws: highlighting is a full lex of a file that can run
    // to thousands of lines, and it only actually changes when the source
    // does.
    wxsl_highlight: Vec<Run>,
    wgsl_highlight: Vec<Run>,
    status: PreviewStatus,
    /// Whether a pipeline swap is in flight, so that the code panel can be
    /// refreshed the frame it lands rather than a frame late.
    swapping: bool,
    /// Whether the mesh turns on its own.
    pub spinning: bool,
    angle: f32,
}

impl Preview {
    /// The size the preview renders at, in physical pixels.
    ///
    /// Fixed rather than following the panel: the panel's width changes as
    /// the window does, and a preview that reallocates its target and
    /// reconfigures two pipelines on every drag of a splitter is a stutter
    /// for no visible gain. It is drawn scaled to fit.
    pub const SIZE: u32 = 512;

    /// Set the preview up, registering its target with the UI renderer so
    /// the interface can draw it as an image.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        ui: &mut UiRenderer,
        library: ShaderLibrary,
    ) -> Result<Self, RenderError> {
        let target = OffscreenTarget::new(device, Self::SIZE, Self::SIZE);
        let renderer = Renderer::new(
            device,
            library,
            TargetConfig::new(Self::SIZE, Self::SIZE, target.format()),
        )?;
        let texture = ui.register_texture(device, target.view());
        Ok(Preview {
            renderer,
            target,
            texture,
            mesh: Mesh::from_kind(device, MeshKind::default()),
            mesh_kind: MeshKind::default(),
            material: None,
            bindings: None,
            attributes: InstanceAttributes::new(),
            user: None,
            placeholder: placeholder_texture(device, queue),
            placeholder_sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("wxsl preview placeholder"),
                address_mode_u: wgpu::AddressMode::Repeat,
                address_mode_v: wgpu::AddressMode::Repeat,
                mag_filter: wgpu::FilterMode::Nearest,
                ..Default::default()
            }),
            wxsl: String::new(),
            wgsl: String::new(),
            wxsl_highlight: Vec::new(),
            wgsl_highlight: Vec::new(),
            status: PreviewStatus::Empty,
            swapping: false,
            spinning: true,
            angle: 0.6,
        })
    }

    /// The texture id the interface draws the preview with.
    pub fn texture(&self) -> TextureId {
        self.texture
    }

    /// The WXSL codegen produced for the current graph.
    pub fn wxsl(&self) -> &str {
        &self.wxsl
    }

    /// [`Preview::wxsl`], coloured by [`crate::highlight`].
    pub fn wxsl_highlight(&self) -> &[Run] {
        &self.wxsl_highlight
    }

    /// The WGSL the active render path compiled that WXSL to.
    pub fn wgsl(&self) -> &str {
        &self.wgsl
    }

    /// [`Preview::wgsl`], coloured by [`crate::highlight`].
    pub fn wgsl_highlight(&self) -> &[Run] {
        &self.wgsl_highlight
    }

    /// Whether the graph compiles, and what stopped it if not.
    pub fn status(&self) -> &PreviewStatus {
        &self.status
    }

    /// The pipeline the preview is drawn with.
    pub fn pipeline(&self) -> StockPipeline {
        // The preview never installs a pass list of its own, so there is
        // always a stock one.
        self.renderer.pipeline().unwrap_or_default()
    }

    /// How far a requested pipeline swap has got, or `None` when none is
    /// in flight — what the status bar's `compiling 3/7` reads.
    pub fn swap_progress(&self) -> Option<SwapProgress> {
        self.renderer.swap_progress()
    }

    /// The stage whose module the code panel is showing.
    pub fn display_stage(&self) -> wxsl_core::abi::MaterialStage {
        self.renderer.display_stage()
    }

    /// Which mesh the preview is drawn on.
    pub fn mesh_kind(&self) -> MeshKind {
        self.mesh_kind
    }

    /// How many shader variants have been compiled, and the cache's hit and
    /// miss counts — the numbers that show a macro or path switch being
    /// absorbed rather than recompiled.
    pub fn variant_stats(&self) -> (usize, wxsl_render::variants::CacheStats) {
        (self.renderer.variant_count(), self.renderer.cache_stats())
    }

    /// Switch pipeline when its shaders are ready.
    ///
    /// The graph is unchanged: a material is authored once and compiled
    /// per *stage* (ADR 0005, ADR 0022). The switch is requested rather
    /// than applied, so the preview keeps drawing the current pipeline
    /// while the missing stages compile — the editor is exactly the place
    /// where a frozen frame on a button press is most obvious.
    pub fn set_pipeline(&mut self, device: &wgpu::Device, pipeline: StockPipeline) {
        let requested = match self.material.as_ref() {
            Some(material) => self
                .renderer
                .request_pipeline(pipeline, &[material])
                .is_ok(),
            // No material to compile anything for: nothing to wait on.
            None => false,
        };
        if !requested {
            self.renderer.set_pipeline(pipeline);
        }
        self.swapping = self.renderer.swap_progress().is_some();
        if !self.swapping {
            self.refresh_wgsl(device);
        }
    }

    /// Switch the preview mesh.
    pub fn set_mesh(&mut self, device: &wgpu::Device, kind: MeshKind) {
        if kind != self.mesh_kind {
            self.mesh = Mesh::from_kind(device, kind);
            self.mesh_kind = kind;
            self.restream(device);
        }
    }

    /// Give the preview mesh a stand-in stream for every per-vertex
    /// attribute the current material declares.
    ///
    /// A gradient along each axis rather than a constant, so an author
    /// wiring one up sees *something varying* and can tell the attribute
    /// arrived from the geometry rather than from a default. The same
    /// reasoning as the magenta checker: a visible placeholder beats a
    /// black surface, and it stays a placeholder until a mesh with real
    /// streams can be loaded (ADR 0024).
    fn restream(&mut self, device: &wgpu::Device) {
        let Some(material) = self.material.as_ref() else {
            return;
        };
        let data = wxsl_render::mesh::MeshData::from_kind(self.mesh_kind);
        let extent = data
            .vertices
            .iter()
            .fold(0.0_f32, |widest, vertex| {
                widest.max(vertex.position.iter().fold(0.0_f32, |a, c| a.max(c.abs())))
            })
            .max(f32::EPSILON);
        for attribute in material.vertex_attributes() {
            let along = |index: usize| {
                let position = data.vertices[index].position;
                core::array::from_fn::<f32, 4, _>(|axis| {
                    (position[axis.min(2)] / extent + 1.0) * 0.5
                })
            };
            let values = match attribute.ty {
                ValueType::F32 => AttributeValues::F32(
                    (0..data.vertices.len())
                        .map(|index| along(index)[0])
                        .collect(),
                ),
                ValueType::Vec2 => AttributeValues::Vec2(
                    (0..data.vertices.len())
                        .map(|index| [along(index)[0], along(index)[1]])
                        .collect(),
                ),
                ValueType::Vec4 => AttributeValues::Vec4(
                    (0..data.vertices.len())
                        .map(|index| {
                            let v = along(index);
                            [v[0], v[1], v[2], 1.0]
                        })
                        .collect(),
                ),
                _ => AttributeValues::Vec3(
                    (0..data.vertices.len())
                        .map(|index| {
                            let v = along(index);
                            [v[0], v[1], v[2]]
                        })
                        .collect(),
                ),
            };
            let _ = self
                .mesh
                .set_attribute(device, attribute.name.as_str(), &values);
        }
    }

    /// Recompile from `graph`.
    ///
    /// Validation first, so a graph with a missing input is reported as that
    /// rather than as whatever codegen makes of it. Every error is kept, not
    /// just the first.
    pub fn rebuild(
        &mut self,
        device: &wgpu::Device,
        graph: &Graph,
        registry: &NodeRegistry,
        macros: &MacroSet,
    ) {
        let mut errors = Vec::new();
        if let Err(problems) = graph.validate(registry) {
            errors.extend(problems.0.iter().map(|error| error.to_string()));
        }
        if !errors.is_empty() {
            self.status = PreviewStatus::Failed(errors);
            // The last material that worked keeps rendering, so the preview
            // does not go black while a graph is mid-edit.
            return;
        }

        match Material::from_graph_with_macros(graph, registry, macros) {
            Ok(material) => {
                self.wxsl = material.wxsl(self.renderer.display_stage()).to_string();
                self.wxsl_highlight = highlight::highlight(&self.wxsl);
                self.rebind(device, &material);
                self.material = Some(material);
                // After the material is in place: the streams to invent
                // are the ones it declares.
                self.restream(device);
                self.status = PreviewStatus::Ok;
                self.refresh_wgsl(device);
            }
            Err(error) => {
                self.status = PreviewStatus::Failed(vec![error.to_string()]);
            }
        }
    }

    /// Rebuild the material's own bind group, and the stand-in for the
    /// application's.
    ///
    /// Every declared texture and sampler gets the placeholder, because
    /// the editor has no way to author one yet and a material that cannot
    /// be bound cannot be previewed. The parameters start at whatever the
    /// graph declared, which is what the author just typed.
    fn rebind(&mut self, device: &wgpu::Device, material: &Material) {
        let mut bindings = self.renderer.material_bindings(device, material);
        for resource in &material.interface().resources {
            let name = resource.name.as_str();
            let bound = match resource.ty {
                ValueType::Sampler => bindings.set_sampler(name, &self.placeholder_sampler),
                _ => bindings.set_texture(name, &self.placeholder),
            };
            debug_assert!(bound.is_ok(), "the interface named this resource");
        }
        self.bindings = Some(bindings);

        // Per-instance attributes get one, not zero. The editor draws a
        // single object, so there is no *per*-instance variation to
        // show, and the honest-looking choice — zero — is black, which
        // multiplied into a base colour is a preview that went dark for
        // a reason the author cannot see. One is the identity for the
        // multiply these are nearly always used for, so the preview
        // shows the material and not the placeholder.
        self.attributes = InstanceAttributes::new();
        for field in material.instance_attributes() {
            if let Some(value) = field.ty.splat(1.0).or_else(|| field.ty.zero()) {
                self.attributes.set(field.name.as_str(), value);
            }
        }

        self.user = material.interface().user.as_ref().map(|block| {
            // Zeroed: the editor is not the application, so the honest
            // stand-in is "nothing has been set".
            let size = u64::from(block.layout.size()).max(16);
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("wxsl preview application block"),
                size,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let layout = self
                .renderer
                .user_layout(device, material)
                .expect("the material declares a block")
                .clone();
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("wxsl preview application block"),
                layout: &layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: wxsl_core::abi::BINDING_USER_BLOCK,
                    resource: buffer.as_entire_binding(),
                }],
            })
        });
    }

    /// Re-read the WGSL for the active path, compiling it if needed.
    ///
    /// Also where a shader-compiler error surfaces: the WXSL can be perfectly
    /// well-formed and still be rejected downstream, and that diagnostic —
    /// with the offending line and a caret — is the most useful thing the
    /// editor can show.
    fn refresh_wgsl(&mut self, device: &wgpu::Device) {
        let Some(material) = self.material.as_ref() else {
            return;
        };
        match self.renderer.material_wgsl(device, material) {
            Ok(wgsl) => {
                self.wgsl_highlight = highlight::highlight(&wgsl);
                self.wgsl = wgsl;
                if self.status == PreviewStatus::Empty {
                    self.status = PreviewStatus::Ok;
                }
            }
            Err(error) => {
                self.wgsl.clear();
                self.wgsl_highlight.clear();
                self.status = PreviewStatus::Failed(vec![error.to_string()]);
            }
        }
    }

    /// Render one frame of preview, if there is a material to render.
    ///
    /// `dt` advances the spin; `time` drives an animated material's `time`
    /// input, so a graph using it animates in the editor the way it will in
    /// an application.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        dt: f32,
        time: f32,
    ) -> Result<(), RenderError> {
        // A swap that landed since the last frame changed which stage the
        // code panel should be showing.
        if self.swapping && self.renderer.swap_progress().is_none() {
            self.swapping = false;
            self.refresh_wgsl(device);
        }
        let Some(material) = self.material.as_ref() else {
            return Ok(());
        };
        if self.spinning {
            self.angle += dt * 0.6;
        }
        if let Some(bindings) = self.bindings.as_mut() {
            bindings.upload(device, queue)?;
        }
        let environment = preview_environment(time);
        let model = Mat4::from_rotation_y(self.angle) * Mat4::from_rotation_x(self.angle * 0.35);
        let mut item = DrawItem::new(&self.mesh, material)
            .with_transform(model)
            .with_attributes(&self.attributes);
        if let Some(bindings) = self.bindings.as_ref() {
            item = item.with_bindings(bindings);
        }
        if let Some(user) = self.user.as_ref() {
            item = item.with_user(user);
        }
        let draws = wxsl_render::single_draw(item);
        self.renderer.render(
            device,
            queue,
            &RenderRequest {
                view: self.target.view(),
                environment: &environment,
                draws: &draws,
            },
        )
    }

    /// Turn the preview by a drag, in pixels.
    pub fn drag(&mut self, delta: f32) {
        self.angle += delta * 0.01;
    }
}

/// An 8x8 magenta-and-grey checker, for a texture the graph declares and
/// the editor cannot yet supply.
///
/// The universal "nothing is bound here" signal, and deliberately loud:
/// white would look like a material choice, and black like a bug in the
/// lighting.
fn placeholder_texture(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::TextureView {
    const SIDE: u32 = 8;
    let mut texels = Vec::with_capacity((SIDE * SIDE * 4) as usize);
    for y in 0..SIDE {
        for x in 0..SIDE {
            let dark = (x / 2 + y / 2) % 2 == 0;
            texels.extend_from_slice(if dark {
                &[220u8, 30, 200, 255]
            } else {
                &[40u8, 40, 44, 255]
            });
        }
    }
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("wxsl preview placeholder"),
        size: wgpu::Extent3d {
            width: SIDE,
            height: SIDE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // Not `Srgb`: what a material samples is linear, because the ABI
        // encodes sRGB itself at the end of shading.
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        texture.as_image_copy(),
        &texels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(SIDE * 4),
            rows_per_image: Some(SIDE),
        },
        wgpu::Extent3d {
            width: SIDE,
            height: SIDE,
            depth_or_array_layers: 1,
        },
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// The camera and lights the preview uses.
///
/// A three-point-ish setup: a key light high and to the right, a cooler fill
/// from the left, and a hemisphere ambient so an unlit side is not black.
/// Fixed, because the preview's job is to show the *material*, and a scene
/// the user can also change is one more thing to blame when it looks wrong.
pub fn preview_environment(time: f32) -> Environment {
    Environment {
        camera: Camera {
            eye: Vec3::new(2.6, 1.9, 3.4),
            target: Vec3::ZERO,
            // The target is square, so the aspect is one; the panel scales
            // the finished image to fit rather than stretching the camera.
            aspect: 1.0,
            ..Camera::default()
        },
        lights: vec![
            // The key light: high, to the right, slightly warm.
            Light::directional(Vec3::new(0.6, 0.9, 0.5), Vec3::new(1.0, 0.96, 0.9), 3.4),
            // A cooler fill from the left, close enough to fall off.
            Light::point(Vec3::new(-3.0, 1.2, 2.0), Vec3::new(0.55, 0.7, 1.0), 12.0),
        ],
        ambient_sky: Vec3::new(0.16, 0.19, 0.26),
        ambient_ground: Vec3::new(0.05, 0.05, 0.06),
        exposure: 1.0,
        time,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_reports_every_error_rather_than_the_first() {
        let status = PreviewStatus::Failed(vec!["one".into(), "two".into()]);
        assert!(!status.is_ok());
        assert_eq!(status.errors().len(), 2);
        assert!(PreviewStatus::Ok.is_ok());
        assert!(PreviewStatus::Ok.errors().is_empty());
        assert!(!PreviewStatus::Empty.is_ok());
    }

    #[test]
    fn the_preview_environment_lights_the_material_from_two_sides() {
        let scene = preview_environment(1.5);
        assert_eq!(scene.time, 1.5);
        assert_eq!(scene.lights.len(), 2);
        // An unlit side still gets the sky, so nothing on screen is pure
        // black — which is what makes a dark material readable at all.
        assert!(scene.ambient_sky.length() > 0.0);
    }
}

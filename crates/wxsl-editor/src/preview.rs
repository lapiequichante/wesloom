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
use wxsl_core::node::NodeRegistry;
use wxsl_render::gpu::OffscreenTarget;
use wxsl_render::ui::draw::TextureId;
use wxsl_render::ui::UiRenderer;
use wxsl_render::{
    Camera, Light, Material, Mesh, MeshKind, RenderError, RenderPath, RenderRequest, Renderer,
    Scene, ShaderLibrary, TargetConfig,
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
    wxsl: String,
    wgsl: String,
    // Computed once, alongside `wxsl`/`wgsl`, rather than every frame the
    // code panel draws: highlighting is a full lex of a file that can run
    // to thousands of lines, and it only actually changes when the source
    // does.
    wxsl_highlight: Vec<Run>,
    wgsl_highlight: Vec<Run>,
    status: PreviewStatus,
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
            wxsl: String::new(),
            wgsl: String::new(),
            wxsl_highlight: Vec::new(),
            wgsl_highlight: Vec::new(),
            status: PreviewStatus::Empty,
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

    /// The active render path.
    pub fn path(&self) -> RenderPath {
        self.renderer.path()
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

    /// Switch render path. The graph is unchanged: a material is written once
    /// and compiled per path (ADR 0005).
    pub fn set_path(&mut self, device: &wgpu::Device, path: RenderPath) {
        self.renderer.set_path(path);
        self.refresh_wgsl(device);
    }

    /// Switch the preview mesh.
    pub fn set_mesh(&mut self, device: &wgpu::Device, kind: MeshKind) {
        if kind != self.mesh_kind {
            self.mesh = Mesh::from_kind(device, kind);
            self.mesh_kind = kind;
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
                self.wxsl = material.wxsl().to_string();
                self.wxsl_highlight = highlight::highlight(&self.wxsl);
                self.material = Some(material);
                self.status = PreviewStatus::Ok;
                self.refresh_wgsl(device);
            }
            Err(error) => {
                self.status = PreviewStatus::Failed(vec![error.to_string()]);
            }
        }
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
        let Some(material) = self.material.as_ref() else {
            return Ok(());
        };
        if self.spinning {
            self.angle += dt * 0.6;
        }
        let scene = preview_scene(time);
        self.renderer.render(
            device,
            queue,
            &RenderRequest {
                view: self.target.view(),
                scene: &scene,
                model: Mat4::from_rotation_y(self.angle) * Mat4::from_rotation_x(self.angle * 0.35),
                mesh: &self.mesh,
                material,
            },
        )
    }

    /// Turn the preview by a drag, in pixels.
    pub fn drag(&mut self, delta: f32) {
        self.angle += delta * 0.01;
    }
}

/// The camera and lights the preview uses.
///
/// A three-point-ish setup: a key light high and to the right, a cooler fill
/// from the left, and a hemisphere ambient so an unlit side is not black.
/// Fixed, because the preview's job is to show the *material*, and a scene
/// the user can also change is one more thing to blame when it looks wrong.
pub fn preview_scene(time: f32) -> Scene {
    Scene {
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
    fn the_preview_scene_lights_the_material_from_two_sides() {
        let scene = preview_scene(1.5);
        assert_eq!(scene.time, 1.5);
        assert_eq!(scene.lights.len(), 2);
        // An unlit side still gets the sky, so nothing on screen is pure
        // black — which is what makes a dark material readable at all.
        assert!(scene.ambient_sky.length() > 0.0);
    }
}

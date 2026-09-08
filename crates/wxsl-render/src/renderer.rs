//! [`Renderer`]: the application-facing front end that hides the path switch.
//!
//! It owns the shader library, the variant cache, the scene uniforms and both
//! pipelines, and exposes one [`Renderer::render`] that works the same
//! whichever path is active. Switching path with [`Renderer::set_path`] does
//! not invalidate anything: the variant cache keeps both paths' shaders, so
//! flipping back and forth compiles each variant once
//! ([ADR 0005](../../../docs/adr/0005-render-pipeline-abstraction-and-shader-switching.md)).

use glam::Mat4;

use crate::error::RenderError;
use crate::library::ShaderLibrary;
use crate::material::Material;
use crate::mesh::Mesh;
use crate::path::RenderPath;
use crate::pipeline::{
    DeferredPipeline, Draw, ForwardPipeline, FrameInput, Pipeline, TargetConfig,
};
use crate::scene::{Scene, SceneBindings};
use crate::variants::{CacheStats, ShaderVariants};

/// What to render, for [`Renderer::render`].
///
/// One mesh and one material: this renderer is scene-graph-free on purpose —
/// batching, culling and sorting belong to the application, and the pipelines
/// underneath already take a slice of draws when that day comes.
pub struct RenderRequest<'a> {
    /// Where the final colour goes.
    pub view: &'a wgpu::TextureView,
    /// Camera, lights and ambient environment.
    pub scene: &'a Scene,
    /// The object's model matrix.
    pub model: Mat4,
    /// The geometry.
    pub mesh: &'a Mesh,
    /// The material, compiled from a graph.
    pub material: &'a Material,
}

/// Owns the pipelines and the shader cache, and renders a frame.
pub struct Renderer {
    library: ShaderLibrary,
    variants: ShaderVariants,
    bindings: SceneBindings,
    forward: ForwardPipeline,
    deferred: DeferredPipeline,
    path: RenderPath,
    target: TargetConfig,
}

impl Renderer {
    /// Create a renderer for `target`, resolving shader imports against
    /// `library`.
    ///
    /// The library must contain the shader ABI modules; `check_abi` runs here
    /// so a missing library is reported now rather than as a confusing
    /// import error on the first material compiled.
    pub fn new(
        device: &wgpu::Device,
        library: ShaderLibrary,
        target: TargetConfig,
    ) -> Result<Self, RenderError> {
        library.check_abi()?;
        let bindings = SceneBindings::new(device);
        let mut renderer = Renderer {
            forward: ForwardPipeline::new(device, &bindings),
            deferred: DeferredPipeline::new(device, &bindings),
            bindings,
            library,
            variants: ShaderVariants::new(),
            path: RenderPath::default(),
            target,
        };
        renderer.forward.configure(device, target);
        renderer.deferred.configure(device, target);
        Ok(renderer)
    }

    /// The active render path.
    pub fn path(&self) -> RenderPath {
        self.path
    }

    /// Switch render path. Takes effect on the next [`Renderer::render`].
    pub fn set_path(&mut self, path: RenderPath) {
        self.path = path;
    }

    /// The current target size and format.
    pub fn target(&self) -> TargetConfig {
        self.target
    }

    /// Resize the render targets.
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.target = TargetConfig {
            width: width.max(1),
            height: height.max(1),
            ..self.target
        };
        self.forward.configure(device, self.target);
        self.deferred.configure(device, self.target);
    }

    /// The shader library imports are resolved against.
    pub fn library(&self) -> &ShaderLibrary {
        &self.library
    }

    /// Variant cache hit/miss counts — how often a path or macro switch
    /// actually cost a compile.
    pub fn cache_stats(&self) -> CacheStats {
        self.variants.stats()
    }

    /// Number of shader variants compiled so far.
    pub fn variant_count(&self) -> usize {
        self.variants.len()
    }

    /// Compile `material` for the active path without rendering.
    ///
    /// Worth calling ahead of a path switch if a mid-frame compile hitch
    /// matters; [`Renderer::render`] does it lazily otherwise.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        material: &Material,
    ) -> Result<(), RenderError> {
        self.variants
            .material(device, &self.library, &material.shader, self.path)?;
        if self.path.is_deferred() {
            self.variants
                .lighting_pass(device, &self.library, material.macros())?;
        }
        Ok(())
    }

    /// The WGSL the active path compiles `material` to.
    ///
    /// This is the answer to "what did my graph actually become", which is
    /// most of shader-graph debugging.
    pub fn material_wgsl(
        &mut self,
        device: &wgpu::Device,
        material: &Material,
    ) -> Result<String, RenderError> {
        let variant = self
            .variants
            .material(device, &self.library, &material.shader, self.path)?;
        Ok(variant.wgsl.clone())
    }

    /// Render one frame.
    ///
    /// Compiles whatever variant the active path needs, uploads the scene
    /// uniforms, records the path's passes and submits them. Which path ran
    /// is invisible from here — that is the point.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        request: &RenderRequest<'_>,
    ) -> Result<(), RenderError> {
        let variant =
            self.variants
                .material(device, &self.library, &request.material.shader, self.path)?;
        let lighting = if self.path.is_deferred() {
            Some(
                self.variants
                    .lighting_pass(device, &self.library, request.material.macros())?,
            )
        } else {
            None
        };

        self.bindings.update(queue, request.scene, request.model);

        let draws = [Draw {
            mesh: request.mesh,
            material: &variant,
        }];
        let input = FrameInput {
            target: request.view,
            bindings: &self.bindings,
            draws: &draws,
            lighting: lighting.as_deref(),
        };

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("wxsl frame"),
        });
        let pipeline: &mut dyn Pipeline = match self.path {
            RenderPath::Forward => &mut self.forward,
            RenderPath::Deferred => &mut self.deferred,
        };
        pipeline.configure(device, self.target);
        pipeline.record(device, &mut encoder, &input)?;
        queue.submit([encoder.finish()]);
        Ok(())
    }
}

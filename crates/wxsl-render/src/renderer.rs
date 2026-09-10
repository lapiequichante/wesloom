//! [`Renderer`]: the application-facing front end that runs a pass list.
//!
//! It owns the shader library, the variant cache, the frame bindings, the
//! `wgpu` pipeline cache and the resource pool, and exposes one
//! [`Renderer::render`] that works whichever pipeline is active. Switching
//! with [`Renderer::set_path`] invalidates nothing: the variant cache keeps
//! both paths' shaders, so flipping back and forth compiles each variant
//! once ([ADR 0005](../../../docs/adr/0005-render-pipeline-abstraction-and-shader-switching.md)).
//!
//! What changed in M1 is underneath: the two hand-written pipeline structs
//! are gone, and a frame is a [`RenderGraph`] the engine schedules and
//! records ([ADR 0021](../../../docs/adr/0021-a-declarative-render-graph-and-a-scene-document.md)).
//! [`Renderer::set_graph`] is the escape hatch — an application with its own
//! pass list hands it over and everything below behaves the same.

use std::collections::HashMap;
use std::sync::Arc;

use wxsl_core::abi;

use crate::draw::{DrawItem, DrawList};
use crate::environment::{Environment, FrameBindings};
use crate::error::RenderError;
use crate::graph::{PassEncoder, RecordedPass, RenderGraph, ResourcePool, Schedule};
use crate::library::ShaderLibrary;
use crate::material::Material;
use crate::pass::{DrawSource, PassKind, ScreenShader};
use crate::path::RenderPath;
use crate::pipeline::{self, PipelineCache, TargetConfig};
use crate::variants::{CacheStats, ShaderVariant, ShaderVariants};

/// What to render, for [`Renderer::render`].
///
/// A draw *list*, not one mesh: the renderer is still scene-graph-free —
/// batching, culling and sorting belong to the application — but "one
/// object per frame" was never a design decision, only an unfinished one.
pub struct RenderRequest<'a> {
    /// Where the final colour goes.
    pub view: &'a wgpu::TextureView,
    /// Camera, lights and ambient environment.
    pub environment: &'a Environment,
    /// What to draw. A draw's index here is the row of the instance buffer
    /// its vertices read their transform from.
    pub draws: &'a DrawList<'a>,
}

/// Owns the pass list, the pipelines and the shader cache, and renders a
/// frame.
pub struct Renderer {
    library: ShaderLibrary,
    variants: ShaderVariants,
    bindings: FrameBindings,
    pipelines: PipelineCache,
    pool: ResourcePool,
    graph: RenderGraph,
    schedule: Schedule,
    /// Whether `graph` is one of the stock pass lists, and so should be
    /// rebuilt when the path or the target changes. A graph an application
    /// supplied is left alone.
    stock: bool,
    path: RenderPath,
    target: TargetConfig,
}

impl Renderer {
    /// Create a renderer for `target`, resolving shader imports against
    /// `library`.
    ///
    /// The library must contain the shader ABI modules; `check_abi` runs
    /// here so a missing library is reported now rather than as a confusing
    /// import error on the first material compiled.
    pub fn new(
        device: &wgpu::Device,
        library: ShaderLibrary,
        target: TargetConfig,
    ) -> Result<Self, RenderError> {
        library.check_abi()?;
        let path = RenderPath::default();
        let graph = pipeline::graph_for(path, target);
        let schedule = graph.schedule()?;
        let mut pool = ResourcePool::new();
        pool.configure(device, &schedule, target);
        Ok(Renderer {
            bindings: FrameBindings::new(device),
            library,
            variants: ShaderVariants::new(),
            pipelines: PipelineCache::new(),
            pool,
            graph,
            schedule,
            stock: true,
            path,
            target,
        })
    }

    /// The active render path.
    pub fn path(&self) -> RenderPath {
        self.path
    }

    /// Switch render path, rebuilding the stock pass list for it.
    ///
    /// Does nothing if an application supplied its own graph: what a pass
    /// draws with is then that graph's business.
    pub fn set_path(&mut self, path: RenderPath) {
        if self.path == path {
            return;
        }
        self.path = path;
        if self.stock {
            self.rebuild();
        }
    }

    /// The pass list being run.
    pub fn render_graph(&self) -> &RenderGraph {
        &self.graph
    }

    /// Run `graph` instead of the stock pass list for the current path.
    ///
    /// Validated immediately: a pass list that cannot be ordered is an
    /// error here rather than a `wgpu` complaint mid-frame.
    pub fn set_graph(&mut self, graph: RenderGraph) -> Result<(), RenderError> {
        self.schedule = graph.schedule()?;
        self.graph = graph;
        self.stock = false;
        Ok(())
    }

    /// Go back to the stock pass list for the current path.
    pub fn use_stock_graph(&mut self) {
        self.stock = true;
        self.rebuild();
    }

    fn rebuild(&mut self) {
        self.graph = pipeline::graph_for(self.path, self.target);
        self.schedule = self
            .graph
            .schedule()
            .expect("the stock pass lists schedule at every size; a test checks it");
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
        if self.stock {
            self.rebuild();
        }
        self.pool.configure(device, &self.schedule, self.target);
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

    /// Number of `wgpu` pipelines built so far — one per (variant, pass
    /// state, target formats).
    pub fn pipeline_count(&self) -> usize {
        self.pipelines.len()
    }

    /// Compile `material` for every stage the active pass list needs,
    /// without rendering.
    ///
    /// Worth calling ahead of a path switch if a mid-frame compile hitch
    /// matters; [`Renderer::render`] does it lazily otherwise.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        material: &Material,
    ) -> Result<(), RenderError> {
        for pass in self.graph.passes() {
            match &pass.kind {
                PassKind::Geometry { path, .. } => {
                    self.variants
                        .material(device, &self.library, &material.shader, *path)?;
                }
                PassKind::Screen {
                    shader: ScreenShader::DeferredLighting,
                } => {
                    self.variants
                        .lighting_pass(device, &self.library, material.macros())?;
                }
                PassKind::Compute { .. } => {}
            }
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
    /// Compiles whatever variants the pass list needs, uploads the frame
    /// bindings and every instance transform in one go, then lets the graph
    /// order and record the passes. Which pipeline ran is invisible from
    /// here — that is the point.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        request: &RenderRequest<'_>,
    ) -> Result<(), RenderError> {
        let plan = self.compile_frame(device, request.draws)?;

        self.bindings.update(
            device,
            queue,
            request.environment,
            &request.draws.transforms(),
        );
        self.pool.configure(device, &self.schedule, self.target);

        // Destructured so the recording closure can hold the pipeline cache
        // mutably while the graph and the pool are borrowed alongside it.
        let Renderer {
            bindings,
            pipelines,
            pool,
            graph,
            schedule,
            ..
        } = self;

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("wxsl frame"),
        });
        graph.record(
            device,
            &mut encoder,
            schedule,
            pool,
            &[(RenderGraph::TARGET, request.view)],
            |pass, encoder| {
                record_pass(
                    device,
                    bindings,
                    pipelines,
                    &plan,
                    request.draws,
                    pass,
                    encoder,
                )
            },
        )?;
        queue.submit([encoder.finish()]);
        Ok(())
    }

    /// Compile every shader variant this frame's pass list will ask for.
    ///
    /// Up front rather than inside the recording closure, because the
    /// closure already holds the pipeline cache and the variant cache is
    /// the one thing that may have to *compile* — which is where a frame
    /// can fail, and failing before a pass is opened is the difference
    /// between an error and a half-recorded encoder.
    fn compile_frame(
        &mut self,
        device: &wgpu::Device,
        draws: &DrawList<'_>,
    ) -> Result<FramePlan, RenderError> {
        let mut plan = FramePlan::default();
        for (index, pass) in self.graph.passes().iter().enumerate() {
            match &pass.kind {
                PassKind::Geometry { source, path } => {
                    let selected: Vec<u32> = match source {
                        DrawSource::Scene(selector) => {
                            draws.select(selector).map(|(index, _)| index).collect()
                        }
                        DrawSource::Indirect { draw, .. } => vec![*draw as u32],
                    };
                    let mut variants = Vec::with_capacity(selected.len());
                    for instance in selected {
                        let item = &draws.items()[instance as usize];
                        let variant = self.variants.material(
                            device,
                            &self.library,
                            &item.material.shader,
                            *path,
                        )?;
                        variants.push((instance, variant));
                    }
                    plan.geometry.insert(index, variants);
                }
                PassKind::Screen {
                    shader: ScreenShader::DeferredLighting,
                } => {
                    // The lighting pass has no material graph, but it does
                    // have the macro set the materials were built with —
                    // tonemapping and the debug-normal view are `@if`s
                    // inside the shading function it calls.
                    let macros = draws
                        .items()
                        .first()
                        .map(|item| item.material.macros().clone())
                        .unwrap_or_default();
                    let variant = self
                        .variants
                        .lighting_pass(device, &self.library, &macros)?;
                    plan.screen.insert(index, variant);
                }
                PassKind::Compute { .. } => {}
            }
        }
        Ok(plan)
    }
}

/// The shader variants one frame's passes need, resolved before recording.
#[derive(Default)]
struct FramePlan {
    /// Per geometry pass: the draws it issues, with the variant each is
    /// drawn with, in instance order.
    geometry: HashMap<usize, Vec<(u32, Arc<ShaderVariant>)>>,
    /// Per screen pass: its shader.
    screen: HashMap<usize, Arc<ShaderVariant>>,
}

/// Issue one pass's work into the encoder the graph opened.
fn record_pass(
    device: &wgpu::Device,
    bindings: &FrameBindings,
    pipelines: &mut PipelineCache,
    plan: &FramePlan,
    draws: &DrawList<'_>,
    pass: &RecordedPass<'_>,
    encoder: PassEncoder<'_, '_>,
) -> Result<(), RenderError> {
    let index = pass.index;
    match (&pass.desc.kind, encoder) {
        (PassKind::Geometry { source, .. }, PassEncoder::Render(render)) => {
            let Some(entries) = plan.geometry.get(&index) else {
                return Ok(());
            };
            render.set_bind_group(abi::GROUP_FRAME, bindings.bind_group(), &[]);
            if let Some(group) = pass.pass_bind_group.as_ref() {
                render.set_bind_group(abi::GROUP_PASS, group, &[]);
            }
            for (instance, variant) in entries {
                let item = &draws.items()[*instance as usize];
                let pipeline = pipelines.geometry(
                    device,
                    bindings.layout(),
                    pass.pass_layout.as_ref(),
                    variant,
                    pass.desc.state,
                    &pass.color_formats,
                    &pass.pass_bindings,
                );
                render.set_pipeline(pipeline);
                match source {
                    DrawSource::Scene(_) => {
                        item.mesh.draw_instances(render, *instance..*instance + 1);
                    }
                    DrawSource::Indirect {
                        buffer,
                        offset,
                        count,
                        ..
                    } => {
                        // One call per record rather than
                        // `multi_draw_indexed_indirect`, which is a native
                        // extension: the point here is that the draw path
                        // is the same, not that it is already optimal.
                        item.mesh.bind(render);
                        for record in 0..u64::from(*count) {
                            render.draw_indexed_indirect(buffer, offset + record * 20);
                        }
                    }
                }
            }
        }
        (PassKind::Screen { .. }, PassEncoder::Render(render)) => {
            let variant = plan
                .screen
                .get(&index)
                .ok_or(RenderError::NoLightingShader)?;
            let pipeline = pipelines.screen(
                device,
                bindings.layout(),
                pass.pass_layout.as_ref(),
                variant,
                pass.desc.state,
                &pass.color_formats,
                &pass.pass_bindings,
            );
            render.set_pipeline(pipeline);
            render.set_bind_group(abi::GROUP_FRAME, bindings.bind_group(), &[]);
            if let Some(group) = pass.pass_bind_group.as_ref() {
                render.set_bind_group(abi::GROUP_PASS, group, &[]);
            }
            // A fullscreen triangle, from the vertex index.
            render.draw(0..3, 0..1);
        }
        (PassKind::Compute { .. }, PassEncoder::Compute(_)) => {
            // Nothing in the stock pass lists dispatches yet; M7 is the
            // first user. The graph already opens the pass, which is the
            // half that would otherwise have to be invented then.
        }
        _ => unreachable!("the graph opens the encoder its pass kind calls for"),
    }
    Ok(())
}

/// A draw list holding one object, for the common single-mesh case.
///
/// The editor's material preview and the `pbr_cube` demo both draw exactly
/// one thing, and making them each build a list by hand would be ceremony
/// for its own sake.
pub fn single_draw<'a>(item: DrawItem<'a>) -> DrawList<'a> {
    core::iter::once(item).collect()
}

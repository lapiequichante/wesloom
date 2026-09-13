//! [`Renderer`]: the application-facing front end that runs a pass list.
//!
//! It owns the shader library, the variant cache, the frame bindings, the
//! `wgpu` pipeline cache and the resource pool, and exposes one
//! [`Renderer::render`] that works whichever pipeline is active.
//!
//! A pipeline is a [`RenderGraph`] the engine schedules and records
//! ([ADR 0021](../../../docs/adr/0021-a-declarative-render-graph-and-a-scene-document.md)),
//! and which shader variant each of its passes draws with is that pass's
//! [`MaterialStage`]
//! ([ADR 0022](../../../docs/adr/0022-material-stages-replace-the-render-path-enum.md)).
//!
//! # Two ways to swap
//!
//! * [`Renderer::set_pipeline`] switches immediately. Anything not already
//!   compiled is compiled during the next frame, which is a hitch.
//! * [`Renderer::request_pipeline`] switches when it is ready: the missing
//!   variants compile on a worker thread while the current pipeline keeps
//!   presenting, and the swap lands in one frame.
//!   [`Renderer::swap_progress`] is what a `compiling 3/7` indicator reads.
//!
//! # What a draw carries
//!
//! Group 0 is the frame's and the renderer owns it. Groups 1 and 2 are the
//! material's — the parameters and textures its graph declares, and the
//! block it expects the application to supply — and both arrive *on the
//! draw*, because the resources behind them belong to the application.
//! [`Renderer::material_bindings`] makes the first and
//! [`Renderer::user_layout`] describes the second
//! ([ADR 0023](../../../docs/adr/0023-a-material-declares-its-resources.md)).
//!
//! Either way the variant cache keeps what it compiled, keyed on the
//! *stage* rather than the pipeline — so the second swap between two
//! pipelines is free, and the third costs nothing at all.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use wxsl_core::abi::{self, MaterialStage};
use wxsl_core::lighting::LightingSet;

use crate::bindings::{BindingLayouts, MaterialBindings};
use crate::draw::{DrawItem, DrawList};
use crate::effect::{Effect, EffectKind, EffectRegistry};
use crate::environment::{Environment, FrameBindings, ShadowMaps};
use crate::error::RenderError;
use crate::graph::{PassEncoder, RecordedPass, RenderGraph, ResourcePool, Schedule};
use crate::library::ShaderLibrary;
use crate::material::Material;
use crate::pass::{DrawSource, PassKind, PassView, Policy};
use crate::pipeline::{MaterialGroups, PipelineCache, PipelineConfig, StockPipeline, TargetConfig};
use crate::swap::{PipelineSwap, Request, SwapProgress};
use crate::variants::{CacheStats, EffectRequest, MaterialRequest, ShaderVariant, ShaderVariants};

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
    layouts: BindingLayouts,
    pipelines: PipelineCache,
    pool: ResourcePool,
    graph: RenderGraph,
    schedule: Schedule,
    /// Which stock pipeline is running, or `None` for one an application
    /// supplied. A stock one is rebuilt when the target changes; someone
    /// else's is left alone.
    stock: Option<StockPipeline>,
    swap: Option<PipelineSwap>,
    /// The screen effects this renderer can run, and a pipeline document
    /// may name. Shipped with the lighting pass and bloom; extended with
    /// [`Renderer::add_effect`](plan2 P4).
    effects: EffectRegistry,
    /// Every knob the stock pass list varies by, in one place: size,
    /// format, clear colour, lighting set. Mutated by `set_lighting`,
    /// `set_pipeline` and `resize`, read by `rebuild`
    /// ([plan2 P2](../../../plan2.md)).
    config: PipelineConfig,
    /// The execution policies' run bookkeeping (plan2 P10), per pass by
    /// declaration index: how many times each pass has actually run since
    /// the current pass list was set.
    runs: Vec<u32>,
    /// The pool generation and target size at which each pass last ran.
    /// A pass of policy `once` is due when its generation is not the
    /// pool's — a reallocation destroyed every slot's contents, so the
    /// bake must happen again — and `on resize` adds its size to the
    /// question.
    last_run: Vec<(u64, (u32, u32))>,
    /// Marks for `on demand` passes, keyed by pass label — the name a
    /// document author gave the pass, which is the name an application
    /// knows.
    demanded: HashSet<String>,
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
        let stock = StockPipeline::default();
        let config = PipelineConfig::new(target);
        let graph = stock.graph(&config);
        let schedule = graph.schedule()?;
        let mut pool = ResourcePool::new();
        pool.configure(device, &schedule, target);
        let pass_count = graph.passes().len();
        Ok(Renderer {
            bindings: FrameBindings::new(device),
            layouts: BindingLayouts::new(),
            library,
            variants: ShaderVariants::new(),
            pipelines: PipelineCache::new(),
            pool,
            graph,
            schedule,
            stock: Some(stock),
            swap: None,
            effects: EffectRegistry::shipped(),
            runs: vec![0; pass_count],
            last_run: vec![(u64::MAX, (0, 0)); pass_count],
            demanded: HashSet::new(),
            config,
        })
    }

    /// Enable a different set of lighting models for the deferred path,
    /// now.
    ///
    /// The set decides the G-buffer's shape, so the deferred pass list is
    /// rebuilt — and every material must have been compiled against the
    /// same set ([`crate::material::Material::with_lighting`]); a draw
    /// whose material disagrees is a named error when the frame is
    /// compiled, before a pass is opened. Every model's module must be in
    /// the library, which is checked here rather than left to the first
    /// shader compilation.
    pub fn set_lighting(&mut self, set: LightingSet) -> Result<(), RenderError> {
        for model in set.models() {
            if !self.library.contains(model.module) {
                return Err(RenderError::MissingModule {
                    module: model.module.to_string(),
                });
            }
        }
        // WebGPU guarantees 32 bytes per sample across a pass's colour
        // attachments, and the G-buffer is one pass. The base targets spend
        // 24 of it once alignment is paid; a set whose requests overrun the
        // rest would only fail at pipeline creation, in a message naming
        // bytes rather than models. Checked here, where the set is being
        // named anyway.
        let bytes = crate::pipeline::gbuffer_bytes_per_sample(&set);
        if bytes > crate::pipeline::MAX_GBUFFER_BYTES_PER_SAMPLE {
            return Err(RenderError::Lighting {
                material: String::new(),
                error: format!(
                    "the G-buffer layout for {} needs {bytes} bytes per sample; the \
                     most a pass may carry is {} — drop a model that requests a target",
                    set.signature(),
                    crate::pipeline::MAX_GBUFFER_BYTES_PER_SAMPLE
                ),
            });
        }
        self.config.lighting = set;
        self.rebuild();
        Ok(())
    }

    /// The lighting models currently enabled.
    pub fn lighting(&self) -> &LightingSet {
        &self.config.lighting
    }

    /// Enable material features for the deferred path, by name.
    ///
    /// Each feature asks for a G-buffer channel beside the lighting set's
    /// own (plan2 P12), so the deferred pass list is rebuilt — and every
    /// material must have been resolved against the same plan (a material
    /// carrying a different set of channels is a named error when the
    /// frame is compiled). A material whose graph pins a feature's macro
    /// while its pipeline does not carry the channel is the same error
    /// from the other side.
    pub fn set_features(&mut self, names: &[&str]) -> Result<(), RenderError> {
        let features = wxsl_core::lighting::feature_requests(names).map_err(|error| {
            RenderError::Lighting {
                material: String::new(),
                error: error.to_string(),
            }
        })?;
        let plan = self
            .config
            .lighting
            .plan(&features)
            .map_err(|error| RenderError::Lighting {
                material: String::new(),
                error: error.to_string(),
            })?;
        let bytes = crate::pipeline::gbuffer_layout_bytes_per_sample(plan.layout());
        if bytes > crate::pipeline::MAX_GBUFFER_BYTES_PER_SAMPLE {
            return Err(RenderError::Lighting {
                material: String::new(),
                error: format!(
                    "the G-buffer layout with {names:?} needs {bytes} bytes per sample; the \
                     most a pass may carry is {} — drop a feature or a model that requests a \
                     target",
                    crate::pipeline::MAX_GBUFFER_BYTES_PER_SAMPLE
                ),
            });
        }
        self.config.features = features;
        self.rebuild();
        Ok(())
    }

    /// The material features currently enabled, by name.
    pub fn features(&self) -> Vec<&'static str> {
        self.config
            .features
            .iter()
            .map(|request| request.source.name())
            .collect()
    }

    /// The screen effects this renderer can run.
    pub fn effects(&self) -> &EffectRegistry {
        &self.effects
    }

    /// Teach the renderer one more screen effect.
    ///
    /// An effect registered here can be named by a pipeline document's
    /// `pass.screen` nodes and run by the next frame — the pass-level
    /// version of handing the renderer a shader library
    /// ([ADR 0009](../../../docs/adr/0009-the-application-supplies-the-shader-library.md),
    /// extended from shaders to passes; plan2 P4). Re-registering an id
    /// replaces the effect under it.
    pub fn add_effect(&mut self, effect: crate::effect::Effect) {
        self.effects.add(effect);
    }

    /// Ask for the pass labelled `label` to run on the next frame — the
    /// `on demand` policy's half of the bargain (plan2 P10). The name is
    /// the pass's label, which is what a pipeline document (or a
    /// hand-built [`PassDesc`]) calls it; several passes sharing a label
    /// are all marked.
    ///
    /// Marks are consumed when the pass runs, so a mark left while the
    /// pass list is mid-swap still applies to the pass in the *new* list.
    pub fn mark_pass(&mut self, label: &str) {
        self.demanded.insert(label.to_string());
    }

    /// How many times the pass labelled `label` has actually run since
    /// the current pass list was set — the observable answer to "did the
    /// once-only bake run, and did it run once?" A pass of policy
    /// `per frame` in an ordinary frame counts up with the frame count.
    ///
    /// The count resets only when the pass list itself is replaced; a
    /// reallocation re-runs a `once` pass and the count climbs past its
    /// old value, which is how the re-run is observable.
    pub fn pass_run_count(&self, label: &str) -> Option<u32> {
        let index = self.graph.passes().iter().position(|p| p.label == label)?;
        self.runs.get(index).copied()
    }

    /// Fresh run bookkeeping for a new pass list: nothing has run, every
    /// `once` and `on resize` pass is due.
    fn reset_runs(&mut self) {
        self.runs = vec![0; self.graph.passes().len()];
        self.last_run = vec![(u64::MAX, (0, 0)); self.graph.passes().len()];
    }

    /// The stock pipeline in use, or `None` when an application supplied
    /// its own pass list.
    pub fn pipeline(&self) -> Option<StockPipeline> {
        self.stock
    }

    /// Switch pipeline now.
    ///
    /// Whatever the new pass list needs and the cache does not have is
    /// compiled during the next [`Renderer::render`], which is a hitch on
    /// the first swap. [`Renderer::request_pipeline`] is the version that
    /// does not stutter.
    pub fn set_pipeline(&mut self, pipeline: StockPipeline) {
        if self.stock == Some(pipeline) {
            return;
        }
        self.stock = Some(pipeline);
        self.swap = None;
        self.rebuild();
    }

    /// The pass list being run.
    pub fn render_graph(&self) -> &RenderGraph {
        &self.graph
    }

    /// Run `graph` instead of a stock pass list, now.
    ///
    /// Validated immediately: a pass list that cannot be ordered is an
    /// error here rather than a `wgpu` complaint mid-frame.
    pub fn set_graph(&mut self, graph: RenderGraph) -> Result<(), RenderError> {
        self.schedule = graph.schedule()?;
        self.graph = graph;
        self.stock = None;
        self.swap = None;
        self.reset_runs();
        Ok(())
    }

    fn rebuild(&mut self) {
        let Some(stock) = self.stock else { return };
        self.graph = stock.graph(&self.config);
        self.schedule = self
            .graph
            .schedule()
            .expect("the stock pass lists schedule at every size; a test checks it");
        self.reset_runs();
    }

    /// Switch to `pipeline` once its shaders are ready, without stuttering.
    ///
    /// Compiles whatever `materials` will need and the cache does not have
    /// on a worker thread; the current pipeline keeps presenting until
    /// every one has arrived, at which point the swap lands in a single
    /// frame. Poll it with [`Renderer::render`] (which does it for you) or
    /// [`Renderer::poll_swap`], and watch it with
    /// [`Renderer::swap_progress`].
    ///
    /// Requesting a swap replaces any swap already in flight, which is
    /// what a user clicking twice means.
    pub fn request_pipeline(
        &mut self,
        pipeline: StockPipeline,
        materials: &[&Material],
    ) -> Result<(), RenderError> {
        let graph = pipeline.graph(&self.config);
        self.request_graph_inner(graph, Some(pipeline), materials)
    }

    /// The same, for a pass list an application built itself.
    pub fn request_graph(
        &mut self,
        graph: RenderGraph,
        materials: &[&Material],
    ) -> Result<(), RenderError> {
        self.request_graph_inner(graph, None, materials)
    }

    fn request_graph_inner(
        &mut self,
        graph: RenderGraph,
        pipeline: Option<StockPipeline>,
        materials: &[&Material],
    ) -> Result<(), RenderError> {
        let schedule = graph.schedule()?;
        let requests = self.missing_variants(&graph, materials);
        // Nothing to wait for: adopting it now is not a stutter, it is the
        // whole swap. This is the second swap between two pipelines, the
        // one the stage-keyed cache makes free.
        if requests.is_empty() {
            self.graph = graph;
            self.schedule = schedule;
            self.stock = pipeline;
            self.swap = None;
            self.reset_runs();
            return Ok(());
        }
        self.swap = Some(PipelineSwap::start(
            graph,
            schedule,
            pipeline,
            &self.library,
            requests,
        ));
        Ok(())
    }

    /// Everything `graph` will ask for that is not compiled yet.
    fn missing_variants(&self, graph: &RenderGraph, materials: &[&Material]) -> Vec<Request> {
        let mut requests: Vec<Request> = Vec::new();
        let mut seen = Vec::new();
        let push = |request: Request, seen: &mut Vec<_>, out: &mut Vec<Request>| {
            let key = request.key();
            // Two passes wanting the same stage of the same material is
            // ordinary — a shadow pass and a depth prepass will be exactly
            // that — and compiling it twice would be a waste and a
            // progress count that never reaches its total.
            if self.variants.contains(&key) || seen.contains(&key) {
                return;
            }
            seen.push(key);
            out.push(request);
        };
        for pass in graph.passes() {
            match &pass.kind {
                PassKind::Geometry { stage, .. } => {
                    for material in materials {
                        push(
                            Request::Material(MaterialRequest::new(material, *stage)),
                            &mut seen,
                            &mut requests,
                        );
                    }
                }
                PassKind::Screen { effect } | PassKind::Compute { effect } => {
                    // An effect this renderer does not know cannot be
                    // requested — and cannot be compiled later either, so
                    // `compile_frame` is where the named error fires. A
                    // *known* one is requested once per macro set, which
                    // is what a swap waits on.
                    if let Some(known) = self.effects.get(effect) {
                        for material in materials {
                            push(
                                Request::Effect(EffectRequest::new(
                                    known,
                                    material.macros(),
                                    &self.config.lighting,
                                    &self.config.features,
                                )),
                                &mut seen,
                                &mut requests,
                            );
                        }
                    }
                }
            }
        }
        requests
    }

    /// How far a requested swap has got, or `None` when none is in flight.
    pub fn swap_progress(&self) -> Option<SwapProgress> {
        self.swap.as_ref().map(PipelineSwap::progress)
    }

    /// Collect whatever the background compile has finished, and land the
    /// swap if it is all there.
    ///
    /// [`Renderer::render`] calls this, so an application that renders
    /// every frame need not. Call it directly to advance a swap while
    /// nothing is being drawn.
    pub fn poll_swap(&mut self, device: &wgpu::Device) -> Result<(), RenderError> {
        let Some(swap) = self.swap.as_mut() else {
            return Ok(());
        };
        for compiled in swap.drain() {
            match compiled.wgsl {
                Ok(wgsl) => {
                    self.variants
                        .insert(device, compiled.key, compiled.label, wgsl);
                }
                Err(error) => {
                    // The new pipeline cannot be built. Keep presenting
                    // the old one and hand the error up, rather than
                    // swapping to something that will fail every frame.
                    self.swap = None;
                    return Err(error);
                }
            }
        }
        if swap.is_stalled() {
            self.swap = None;
            return Err(RenderError::SwapAbandoned);
        }
        if swap.is_complete() {
            let swap = self.swap.take().expect("checked just above");
            self.graph = swap.graph;
            self.schedule = swap.schedule;
            self.stock = swap.pipeline;
            self.reset_runs();
        }
        Ok(())
    }

    /// Somewhere to put `material`'s uniform parameters, textures and
    /// samplers.
    ///
    /// Starts at the values the graph's `param` nodes declared; a texture
    /// or sampler the graph declares has to be bound before the material
    /// can draw, and [`MaterialBindings::upload`] names any that were
    /// not. Hand the result to a draw with
    /// [`DrawItem::with_bindings`].
    ///
    /// One per *object*, not one per material, whenever two objects
    /// sharing a material want different values — that is the whole
    /// reason these are not owned by the [`Material`].
    pub fn material_bindings(
        &mut self,
        device: &wgpu::Device,
        material: &Material,
    ) -> MaterialBindings {
        let interface = material.interface();
        let layouts = self.layouts.layouts(device, interface);
        // A material declaring nothing still gets bindings, with an empty
        // layout: it keeps the caller from having to ask whether it needs
        // any, and an empty bind group is never bound.
        let layout = match layouts.material.as_ref() {
            Some(layout) => layout.clone(),
            None => device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("wxsl material (empty)"),
                entries: &[],
            }),
        };
        MaterialBindings::new(device, interface, &layout)
    }

    /// The layout of the block `material` expects the application to
    /// supply, or `None` if it expects none.
    ///
    /// The application creates its own buffer and bind group against
    /// this, and hands the group back on the draw
    /// ([`DrawItem::with_user`]). What is in it is never this crate's
    /// business; `wgpu` is what checks the two agree
    /// ([ADR 0023](../../../docs/adr/0023-a-material-declares-its-resources.md)).
    pub fn user_layout(
        &mut self,
        device: &wgpu::Device,
        material: &Material,
    ) -> Option<&wgpu::BindGroupLayout> {
        self.layouts
            .layouts(device, material.interface())
            .user
            .as_ref()
    }

    /// The current target size and format.
    pub fn target(&self) -> TargetConfig {
        self.config.target
    }

    /// Resize the render targets.
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.config.target = TargetConfig {
            width: width.max(1),
            height: height.max(1),
            ..self.config.target
        };
        self.rebuild();
        self.pool
            .configure(device, &self.schedule, self.config.target);
    }

    /// The shader library imports are resolved against.
    pub fn library(&self) -> &ShaderLibrary {
        &self.library
    }

    /// Variant cache hit/miss counts — how often a pipeline or macro switch
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

    /// The frame group's buffers, for a caller that wants to know what a
    /// frame cost: how many instance row shapes it carried, and how many
    /// rows fit before the next reallocation.
    pub fn frame_bindings(&self) -> &FrameBindings {
        &self.bindings
    }

    /// Every stage the active pass list draws with, in pass order.
    pub fn stages(&self) -> Vec<MaterialStage> {
        let mut stages = Vec::new();
        for pass in self.graph.passes() {
            if let PassKind::Geometry { stage, .. } = &pass.kind {
                if !stages.contains(stage) {
                    stages.push(*stage);
                }
            }
        }
        stages
    }

    /// The stage whose module best answers "what did my graph become on
    /// this pipeline" — the one that writes colour, if any.
    ///
    /// A forward pass list has two geometry passes and only the second is
    /// interesting to read; a code panel showing the depth-only module
    /// would be technically accurate and useless.
    pub fn display_stage(&self) -> MaterialStage {
        let stages = self.stages();
        stages
            .iter()
            .copied()
            .find(|stage| stage.color_targets() > 0)
            .or_else(|| stages.first().copied())
            .unwrap_or_default()
    }

    /// Compile `material` for every stage the active pass list needs,
    /// without rendering.
    ///
    /// Worth calling ahead of a pipeline switch if a mid-frame compile
    /// hitch matters; [`Renderer::render`] does it lazily otherwise, and
    /// [`Renderer::request_pipeline`] does it in the background.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        material: &Material,
    ) -> Result<(), RenderError> {
        for pass in self.graph.passes() {
            match &pass.kind {
                PassKind::Geometry { stage, .. } => {
                    self.variants
                        .material(device, &self.library, material, *stage)?;
                }
                PassKind::Screen { effect } | PassKind::Compute { effect } => {
                    let known =
                        self.effects
                            .get(effect)
                            .ok_or_else(|| RenderError::UnknownEffect {
                                effect: effect.clone(),
                            })?;
                    self.variants.effect(
                        device,
                        &self.library,
                        known,
                        material.macros(),
                        &self.config.lighting,
                        &self.config.features,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// The WGSL `material` compiles to on this pipeline's display stage.
    ///
    /// This is the answer to "what did my graph actually become", which is
    /// most of shader-graph debugging.
    pub fn material_wgsl(
        &mut self,
        device: &wgpu::Device,
        material: &Material,
    ) -> Result<String, RenderError> {
        self.material_wgsl_for(device, material, self.display_stage())
    }

    /// The WGSL `material` compiles to for one particular stage.
    pub fn material_wgsl_for(
        &mut self,
        device: &wgpu::Device,
        material: &Material,
        stage: MaterialStage,
    ) -> Result<String, RenderError> {
        let variant = self
            .variants
            .material(device, &self.library, material, stage)?;
        Ok(variant.wgsl.clone())
    }

    /// Render one frame.
    ///
    /// Advances any pipeline swap in flight, compiles whatever variants the
    /// pass list needs, uploads the frame bindings and every instance
    /// transform in one go, then lets the graph order and record the
    /// passes. Which pipeline ran is invisible from here — that is the
    /// point.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        request: &RenderRequest<'_>,
    ) -> Result<(), RenderError> {
        self.poll_swap(device)?;
        let plan = self.compile_frame(device, request.environment, request.draws)?;

        // One row per draw, in every shape the frame's materials asked
        // for — and the place a draw that forgot a declared per-instance
        // attribute is told so, before a pass is opened.
        let rows = request.draws.instance_rows()?;
        self.bindings.update(
            device,
            queue,
            request.environment,
            &request.draws.transforms(),
            &rows,
        );
        self.pool
            .configure(device, &self.schedule, self.config.target);
        // After the pool, because this is the one resource read from
        // outside the pass list: the frame group binds it, so the renderer
        // is what carries the view across (`abi::BINDING_SHADOW_MAPS`).
        if let Some(maps) = self.graph.shadow_maps() {
            if let Some(slot) = self.schedule.slot(maps, self.pool.frame(), 0) {
                if let Some(view) = self.pool.slot_view(slot) {
                    self.bindings
                        .set_shadow_maps(device, (self.pool.generation(), slot), view);
                }
            }
        }

        // Which passes run this frame: the execution policies, decided
        // (plan2 P10). `ran` is "ran since the pool last allocated" — a
        // reallocation destroyed every slot's contents, so a `once` pass
        // bakes again; `size_changed` is what `on resize` adds.
        let generation = self.pool.generation();
        let size = (self.config.target.width, self.config.target.height);
        let mut run = vec![false; self.graph.passes().len()];
        for (index, pass) in self.graph.passes().iter().enumerate() {
            let demanded = if pass.policy == Policy::OnDemand {
                self.demanded.remove(&pass.label)
            } else {
                false
            };
            let (ran, size_changed) = {
                let last = self.last_run[index];
                (last.0 == generation, last.1 != size)
            };
            if pass.policy.due(ran, size_changed, demanded) {
                run[index] = true;
                self.runs[index] += 1;
                self.last_run[index] = (generation, size);
            }
        }

        // Destructured so the recording closure can hold the pipeline
        // cache mutably while the graph, the pool and the effect registry
        // are borrowed alongside it.
        let Renderer {
            bindings,
            layouts,
            pipelines,
            pool,
            graph,
            schedule,
            effects,
            ..
        } = self;

        // A pass drawing *into* the shadow maps must not also have them
        // bound, so which of the two frame groups it gets is decided from
        // what it writes.
        let shadow_maps = graph.shadow_maps();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("wxsl frame"),
        });
        graph.record(
            device,
            &mut encoder,
            schedule,
            pool,
            &[(RenderGraph::TARGET, request.view)],
            &|index| run[index],
            |pass, encoder| {
                let shadows = match shadow_maps {
                    Some(maps) if pass.desc.written().any(|id| id == maps) => ShadowMaps::Detached,
                    _ => ShadowMaps::Bound,
                };
                record_pass(
                    device,
                    bindings,
                    layouts,
                    pipelines,
                    effects,
                    &plan,
                    request.draws,
                    pass,
                    shadows,
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
        environment: &Environment,
        draws: &DrawList<'_>,
    ) -> Result<FramePlan, RenderError> {
        let mut plan = FramePlan::default();
        for (index, pass) in self.graph.passes().iter().enumerate() {
            match &pass.kind {
                PassKind::Geometry { source, stage } => {
                    // A shadow pass built for a light that is not casting
                    // this frame draws nothing at all. Its slice is still
                    // cleared to the far plane, which reads as "lit" —
                    // the same answer as an empty shadow map, for the cost
                    // of a clear rather than of a pass list rebuilt every
                    // time the application moves a light.
                    if let PassView::Light { index: light } = pass.view {
                        if !environment.light_casts_shadow(light) {
                            plan.geometry.insert(index, Vec::new());
                            continue;
                        }
                    }
                    let selected: Vec<u32> = match source {
                        DrawSource::Scene(selector) => {
                            draws.select(selector).map(|(index, _)| index).collect()
                        }
                        DrawSource::Indirect { draw, .. } => vec![*draw as u32],
                    };
                    let mut variants = Vec::with_capacity(selected.len());
                    for instance in selected {
                        let item = &draws.items()[instance as usize];
                        // The other half of the shadow story, and the one
                        // that is a *selection*: a material that casts no
                        // shadow is simply not drawn into one. Filtered
                        // here rather than while recording, so it does not
                        // compile a variant it will never draw.
                        if *stage == MaterialStage::SHADOW && !item.material.cast_shadow() {
                            continue;
                        }
                        // Before anything is recorded: a mesh that cannot
                        // supply what the material declares is an error
                        // naming all three, not a `wgpu` complaint about
                        // vertex buffer 4 and not a frame that draws
                        // whatever was left bound at that slot.
                        // The material's G-buffer module was generated for
                        // the layout of the set it was compiled against;
                        // drawing it under a different set would be a
                        // `wgpu` complaint about a fragment target count
                        // with no mention of either set. Caught here, by
                        // name, before anything is recorded.
                        if item.material.lighting().set() != &self.config.lighting {
                            return Err(RenderError::Lighting {
                                material: item.material.name.clone(),
                                error: format!(
                                    "compiled against lighting set {}, but the renderer                                      enables {}",
                                    item.material.lighting().set().signature(),
                                    self.config.lighting.signature()
                                ),
                            });
                        }
                        // The same for feature channels: the material's
                        // G-buffer struct has a field per channel of the
                        // plan it was resolved against (plan2 P12).
                        if item.material.lighting().features() != self.config.features.as_slice() {
                            return Err(RenderError::Lighting {
                                material: item.material.name.clone(),
                                error: format!(
                                    "compiled with feature channels {:?}, but the renderer \
                                     enables {:?}",
                                    item.material
                                        .lighting()
                                        .features()
                                        .iter()
                                        .map(|request| request.source.name())
                                        .collect::<Vec<_>>(),
                                    self.config
                                        .features
                                        .iter()
                                        .map(|request| request.source.name())
                                        .collect::<Vec<_>>()
                                ),
                            });
                        }
                        // And a material that pins a feature's macro while
                        // its pipeline does not carry the channel is the
                        // request going unanswered — named rather than
                        // silently shaded without the feature.
                        for feature in wxsl_core::lighting::FEATURES {
                            let demands = matches!(
                                item.material.macros().get(feature.macro_name),
                                Some(wxsl_core::macros::MacroValue::Flag(true))
                            );
                            if demands
                                && !self
                                    .config
                                    .features
                                    .iter()
                                    .any(|request| request.source.name() == feature.name)
                            {
                                return Err(RenderError::Lighting {
                                    material: item.material.name.clone(),
                                    error: format!(
                                        "pins `{macro}`, which asks for the `{name}` channel, \
                                         but this pipeline does not enable the `{name}` \
                                         feature — enable it with `Renderer::set_features`",
                                        macro = feature.macro_name,
                                        name = feature.name,
                                    ),
                                });
                            }
                        }
                        item.mesh.check_attributes(
                            &item.material.name,
                            item.material.vertex_attributes(),
                        )?;
                        let variant =
                            self.variants
                                .material(device, &self.library, item.material, *stage)?;
                        variants.push((instance, variant));
                    }
                    plan.geometry.insert(index, variants);
                }
                PassKind::Screen { effect } | PassKind::Compute { effect } => {
                    // An effect has no material graph, but it does have
                    // the macro set the materials were built with — the
                    // lighting pass's shading function has `@if`s of its
                    // own (tonemapping, the debug-normal view), and every
                    // effect compiles under the same set.
                    let known =
                        self.effects
                            .get(effect)
                            .ok_or_else(|| RenderError::UnknownEffect {
                                effect: effect.clone(),
                            })?;
                    let macros = draws
                        .items()
                        .first()
                        .map(|item| item.material.macros().clone())
                        .unwrap_or_default();
                    let variant = self.variants.effect(
                        device,
                        &self.library,
                        known,
                        &macros,
                        &self.config.lighting,
                        &self.config.features,
                    )?;
                    if known.is_compute() {
                        plan.compute.insert(index, (known, variant));
                    } else {
                        plan.screen.insert(index, variant);
                    }
                }
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
    /// Per compute pass: its effect and shader — the effect carries the
    /// entry point and workgroup count the dispatch needs.
    compute: HashMap<usize, (Effect, Arc<ShaderVariant>)>,
}

/// Issue one pass's work into the encoder the graph opened.
#[allow(clippy::too_many_arguments)]
fn record_pass(
    device: &wgpu::Device,
    bindings: &FrameBindings,
    layouts: &mut BindingLayouts,
    pipelines: &mut PipelineCache,
    effects: &EffectRegistry,
    plan: &FramePlan,
    draws: &DrawList<'_>,
    pass: &RecordedPass<'_>,
    shadows: ShadowMaps,
    encoder: PassEncoder<'_, '_>,
) -> Result<(), RenderError> {
    let index = pass.index;
    match (&pass.desc.kind, encoder) {
        (PassKind::Geometry { source, stage }, PassEncoder::Render(render)) => {
            let Some(entries) = plan.geometry.get(&index) else {
                return Ok(());
            };
            // Which point of view this pass renders from, as the dynamic
            // offset of the frame group's camera binding. Zero for
            // everything but a shadow pass, so the mechanism is invisible
            // to a pipeline that has none.
            let view = &[bindings.view_offset(pass.desc.view)];
            render.set_bind_group(abi::GROUP_FRAME, bindings.bind_group(shadows), view);
            if let Some(group) = pass.pass_bind_group.as_ref() {
                render.set_bind_group(abi::GROUP_PASS, group, &[]);
            }
            // Group 0 is bound once above and rebound only when the
            // instance row *shape* changes, which for a frame whose
            // materials declare the same per-instance attributes — the
            // usual frame, and every frame with none — is never.
            let mut bound_shape: Option<&str> = None;
            for (instance, variant) in entries {
                let item = &draws.items()[*instance as usize];
                let shape = item.material.instance_signature();
                if bound_shape != Some(shape) {
                    render.set_bind_group(
                        abi::GROUP_FRAME,
                        bindings.instance_group(shape, shadows),
                        view,
                    );
                    bound_shape = Some(shape);
                }
                let groups = layouts.layouts(device, item.material.interface());
                let pipeline = pipelines.geometry(
                    device,
                    bindings.layout(),
                    &MaterialGroups {
                        vertex: item.material.vertex_attributes(),
                        material: groups.material.as_ref(),
                        user: groups.user.as_ref(),
                        signature: item.material.signature(),
                    },
                    pass.pass_layout.as_ref(),
                    variant,
                    item.material.shader(*stage).fragment_entry.as_deref(),
                    pass.desc.state,
                    &pass.color_formats,
                    &pass.pass_bindings,
                );
                render.set_pipeline(pipeline);
                // Groups 1 and 2 are the material's — one it fills and one
                // it only describes — and both are per draw, because the
                // resources behind them are the application's.
                if groups.material.is_some() {
                    let group = item
                        .bindings
                        .and_then(MaterialBindings::bind_group)
                        .ok_or_else(|| RenderError::MissingDrawBindings {
                            material: item.material.name.clone(),
                            group: "material",
                        })?;
                    render.set_bind_group(abi::GROUP_MATERIAL, group, &[]);
                }
                if groups.user.is_some() {
                    let group = item.user.ok_or_else(|| RenderError::MissingDrawBindings {
                        material: item.material.name.clone(),
                        group: "user",
                    })?;
                    render.set_bind_group(abi::GROUP_USER, group, &[]);
                }
                item.mesh
                    .bind_attributes(render, item.material.vertex_attributes());
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
        (PassKind::Screen { effect }, PassEncoder::Render(render)) => {
            let variant = plan
                .screen
                .get(&index)
                .ok_or_else(|| RenderError::UnknownEffect {
                    effect: effect.clone(),
                })?;
            // The entry points are the effect's, not the ABI's: the
            // pipeline is built from whichever module the effect mounted.
            let known = effects
                .get(effect)
                .ok_or_else(|| RenderError::UnknownEffect {
                    effect: effect.clone(),
                })?;
            let EffectKind::Screen {
                vertex_entry,
                fragment_entry,
            } = known.kind
            else {
                return Err(RenderError::UnknownEffect {
                    effect: effect.clone(),
                });
            };
            let pipeline = pipelines.screen(
                device,
                bindings.layout(),
                pass.pass_layout.as_ref(),
                variant,
                vertex_entry,
                fragment_entry,
                pass.desc.state,
                &pass.color_formats,
                &pass.pass_bindings,
            );
            render.set_pipeline(pipeline);
            render.set_bind_group(
                abi::GROUP_FRAME,
                bindings.bind_group(shadows),
                &[bindings.view_offset(pass.desc.view)],
            );
            if let Some(group) = pass.pass_bind_group.as_ref() {
                render.set_bind_group(abi::GROUP_PASS, group, &[]);
            }
            // A fullscreen triangle, from the vertex index.
            render.draw(0..3, 0..1);
        }
        (PassKind::Compute { effect }, PassEncoder::Compute(compute)) => {
            let (known, variant) =
                plan.compute
                    .get(&index)
                    .ok_or_else(|| RenderError::UnknownEffect {
                        effect: effect.clone(),
                    })?;
            let EffectKind::Compute { entry, workgroups } = known.kind else {
                return Err(RenderError::UnknownEffect {
                    effect: effect.clone(),
                });
            };
            // No frame group: a compute effect declares only `@group(3)`,
            // and the pipeline layout was built to say so.
            let pipeline = pipelines.compute(
                device,
                pass.pass_layout.as_ref(),
                variant,
                entry,
                &pass.pass_bindings,
            );
            compute.set_pipeline(pipeline);
            if let Some(group) = pass.pass_bind_group.as_ref() {
                compute.set_bind_group(abi::GROUP_PASS, group, &[]);
            }
            compute.dispatch_workgroups(workgroups[0], workgroups[1], workgroups[2]);
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

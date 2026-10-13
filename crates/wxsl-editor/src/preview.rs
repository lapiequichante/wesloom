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

use std::collections::BTreeMap;

use glam::{Mat4, Vec3};
use wxsl_core::graph::Graph;
use wxsl_core::lighting;
use wxsl_core::macros::{MacroSet, MacroValue};
use wxsl_core::node::{NodeRegistry, Value, ValueType};
use wxsl_render::gpu::OffscreenTarget;
use wxsl_render::pass::ResourceId;
use wxsl_render::ui::draw::TextureId;
use wxsl_render::ui::UiRenderer;
use wxsl_render::{
    AttributeValues, Camera, DrawItem, Environment, InstanceAttributes, Light, Material,
    MaterialBindings, Mesh, MeshKind, RenderError, RenderRequest, Renderer, ShaderLibrary,
    StockPipeline, SwapProgress, TargetConfig,
};

use crate::highlight::{self, Run};
use crate::stepping;

/// The label the stock presets give their tonemap pass, whose `curve` a
/// debug view switches off.
const TONEMAP_PASS: &str = "tonemap";

/// A pass list being stepped through (plan3 N9).
struct Stepping {
    /// The pass list as it ran before stepping: what is cut, and — for a
    /// document — what is put back.
    full: wxsl_render::RenderGraph,
    /// Its passes in schedule order, by declaration index.
    order: Vec<usize>,
    /// How many passes of `order` run.
    count: usize,
    /// The resource shown, or `None` for what the last pass run wrote.
    show: Option<ResourceId>,
    /// The stock pipeline to go back to, or `None` for `full` itself.
    restore: Option<StockPipeline>,
    /// The scene time the frame is frozen at, so each step shows the same
    /// frame.
    time: f32,
}

/// What the frame tab shows of a pass list being stepped through.
#[derive(Clone, Debug, PartialEq)]
pub struct StepView {
    /// Every pass label, in schedule order.
    pub passes: Vec<String>,
    /// How many of them run.
    pub count: usize,
    /// The resources the running passes write, by id and label, and
    /// whether an inspect effect can draw each.
    pub resources: Vec<(ResourceId, String, bool)>,
    /// The resource on screen, by label: `None` for the frame's target.
    pub shown: Option<String>,
    /// Whether the user chose it, rather than it following the last pass.
    pub pinned: bool,
}

/// The B side of the A/B split: a second renderer on a stock pipeline,
/// drawing the same material and mesh into its own target. Two renderers
/// rather than one switching pass lists, because a switch per frame would
/// reallocate the resource pool every frame.
struct Compare {
    renderer: Renderer,
    pipeline: StockPipeline,
    bindings: Option<MaterialBindings>,
    user: Option<wgpu::BindGroup>,
}

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
    /// The material canvas's stock pipeline choice, remembered across
    /// visits to the pipeline canvas — the renderer holds it while it
    /// holds a stock pass list, and *not* while a document is installed.
    stock: StockPipeline,
    /// The G-buffer debug view the lighting pass returns instead of
    /// shading, by [`lighting::DebugView`] id; 0 shades (plan3 N9).
    debug_view: i32,
    /// The B side of an A/B split (plan3 N9), when one is up.
    compare: Option<Compare>,
    /// The pass list being stepped through, when stepping.
    stepping: Option<Stepping>,
    /// The scene time of the last frame, which stepping freezes.
    time: f32,
    /// The B side's target and texture, registered once with the UI so
    /// that turning the split on needs no access to the UI renderer.
    compare_target: OffscreenTarget,
    compare_texture: TextureId,
    /// What a B-side renderer is built from, and what a hot reload edits.
    library: ShaderLibrary,
    /// Edited modules a reload refused, kept so the next edit is checked
    /// together with them rather than against a library missing them.
    pending: BTreeMap<String, String>,
    /// Why the last reload was refused; empty once one lands.
    reload_errors: Vec<String>,
    /// Where the split's edge sits, as a fraction of the preview's width:
    /// A is drawn left of it, B right.
    pub split: f32,
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
            library.clone(),
            TargetConfig::new(Self::SIZE, Self::SIZE, target.format()),
        )?;
        let texture = ui.register_texture(device, target.view());
        let compare_target = OffscreenTarget::new(device, Self::SIZE, Self::SIZE);
        let compare_texture = ui.register_texture(device, compare_target.view());
        Ok(Preview {
            compare: None,
            stepping: None,
            time: 0.0,
            compare_target,
            compare_texture,
            library,
            pending: BTreeMap::new(),
            reload_errors: Vec::new(),
            split: 0.5,
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
            stock: StockPipeline::default(),
            debug_view: 0,
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

    /// Why the last shader reload was refused, if it was.
    pub fn reload_errors(&self) -> &[String] {
        &self.reload_errors
    }

    /// Hot reload (plan3 N9): replace library modules by edited sources.
    ///
    /// Every shader the running pass list asks of the material is compiled
    /// against the edited library first, with no device. If any fails the
    /// errors are kept for the problems panel and the last working library
    /// keeps rendering; otherwise both renderers take it, which drops their
    /// compiled variants. Returns whether it landed.
    pub fn reload(
        &mut self,
        device: &wgpu::Device,
        changes: impl IntoIterator<Item = (String, String)>,
    ) -> bool {
        self.pending.extend(changes);
        let mut library = self.library.clone();
        for (module, source) in &self.pending {
            library.insert(module.clone(), source.clone());
        }
        let materials: Vec<&Material> = self.material.iter().collect();
        let mut errors = self.renderer.check_library(&library, &materials);
        if let Some(compare) = self.compare.as_ref() {
            for error in compare.renderer.check_library(&library, &materials) {
                if !errors.contains(&error) {
                    errors.push(error);
                }
            }
        }
        if errors.is_empty() {
            let renderers = core::iter::once(&mut self.renderer)
                .chain(self.compare.as_mut().map(|compare| &mut compare.renderer));
            for renderer in renderers {
                if let Err(error) = renderer.set_library(library.clone()) {
                    errors.push(error.to_string());
                }
            }
        }
        if !errors.is_empty() {
            self.reload_errors = errors;
            return false;
        }
        self.library = library;
        self.pending.clear();
        self.reload_errors.clear();
        self.refresh_wgsl(device);
        true
    }

    /// The pipeline the material preview is drawn with.
    ///
    /// The material canvas's own choice, remembered across visits to the
    /// pipeline canvas: while a pipeline document is installed the
    /// renderer has no stock pipeline, and this is what coming back
    /// restores.
    pub fn pipeline(&self) -> StockPipeline {
        self.stock
    }

    /// Whether the renderer runs an installed document rather than a stock
    /// pass list.
    pub fn runs_document(&self) -> bool {
        match self.stepping.as_ref() {
            // A cut is installed as a graph; what it was cut from decides.
            Some(stepping) => stepping.restore.is_none(),
            None => self.renderer.pipeline().is_none(),
        }
    }

    /// The effects this renderer can run — what the pipeline canvas's
    /// palette offers beside the document vocabulary, and what its
    /// documents compile against.
    pub fn effects(&self) -> &wxsl_render::effect::EffectRegistry {
        self.renderer.effects()
    }

    /// The knobs the renderer's pass lists are built under: the config a
    /// pipeline document compiles against, and the channel plan the
    /// pipeline inspector draws.
    pub fn pipeline_config(&self) -> &wxsl_render::PipelineConfig {
        self.renderer.pipeline_config()
    }

    /// The pass list currently running — what the pipeline canvas's
    /// passes panel lists.
    pub fn render_graph(&self) -> &wxsl_render::RenderGraph {
        self.renderer.render_graph()
    }

    /// Run a pipeline document's compiled pass list, now.
    ///
    /// The pipeline canvas's compile loop ends here. Nothing about the
    /// *material* changes; the same draw list renders through whatever
    /// pass list is installed, which is the whole point of a pipeline
    /// being data.
    pub fn install_document(&mut self, graph: wxsl_render::RenderGraph) -> Result<(), RenderError> {
        self.stepping = None;
        self.renderer.set_graph(graph)
    }

    /// Tune one of the running pipeline's effect parameters — the pass
    /// label is the document node's, the parameter's name the effect
    /// descriptor's (ADR 0042). A buffer write the next frame presents,
    /// which is why the canvas's sliders cost no recompile.
    pub fn set_pass_param(
        &mut self,
        pass: &str,
        name: &str,
        value: wxsl_core::node::Value,
    ) -> Result<(), RenderError> {
        self.renderer.set_pass_param(pass, name, value)
    }

    /// The current value of one pass parameter, for a slider to start
    /// from.
    pub fn pass_param(&self, pass: &str, name: &str) -> Option<wxsl_core::node::Value> {
        self.renderer.pass_param(pass, name)
    }

    /// The material features the preview's pipeline carries, by name —
    /// what the features panel's checkboxes start from (plan3 N9).
    pub fn features(&self) -> Vec<&'static str> {
        self.renderer.features()
    }

    /// Enable material features by name, rebuilding the pass list against
    /// the widened plan. The caller marks the material dirty afterwards, so
    /// it is regenerated against the same plan — the handshake both sides
    /// check.
    pub fn set_features(&mut self, names: &[&str]) -> Result<(), RenderError> {
        // A new plan is a new pass list: stepping the old one ends.
        self.stop_stepping();
        self.renderer.set_features(names)?;
        // The B side resolves the same material, so it carries the same
        // plan.
        if let Some(compare) = self.compare.as_mut() {
            compare.renderer.set_features(names)?;
        }
        Ok(())
    }

    /// Ask the pass labelled `label` to run on the next frame — the policy
    /// inspector's "run now" ([`Renderer::mark_pass`](wxsl_render::Renderer::mark_pass)).
    pub fn mark_pass(&mut self, label: &str) {
        self.renderer.mark_pass(label);
    }

    /// How many times the pass labelled `label` has run since the pass list
    /// was set — the policy inspector's "runs" column.
    pub fn pass_run_count(&self, label: &str) -> Option<u32> {
        self.renderer.pass_run_count(label)
    }

    /// How many draws the pass labelled `label` issued last frame — the
    /// policy inspector's "draws" column.
    pub fn pass_draw_count(&self, label: &str) -> Option<u32> {
        self.renderer.pass_draw_count(label)
    }

    /// Fresh run bookkeeping: every `once` and `on resize` pass due again
    /// — the policy inspector's reset.
    pub fn reset_runs(&mut self) {
        self.renderer.reset_runs();
    }

    /// The preview's last frame, read back as RGBA8 rows — for a test
    /// that checks what a debug view shows.
    pub fn read_rgba8(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Vec<u8> {
        self.target.read_rgba8(device, queue)
    }

    /// The debug view the preview shows, 0 when it shades.
    pub fn debug_view(&self) -> i32 {
        self.debug_view
    }

    /// Every debug view, and whether the running plan carries what it
    /// reads — what the toolbar's view menu lists.
    pub fn debug_views(&self) -> Vec<(lighting::DebugView, bool)> {
        let plan = self
            .renderer
            .lighting()
            .plan(&self.renderer.pipeline_config().features)
            .ok();
        lighting::debug_views()
            .into_iter()
            .map(|view| {
                let available = plan.as_ref().is_some_and(|plan| view.available(plan));
                (view, available)
            })
            .collect()
    }

    /// Show a G-buffer debug view, or shade again with 0.
    ///
    /// The view is a material macro the lighting pass inherits, so the
    /// caller marks the material dirty afterwards; the tonemap's curve
    /// follows at the next frame, so the view shows the stored value.
    pub fn set_debug_view(&mut self, id: i32) {
        self.debug_view = id;
    }

    /// The material the preview last compiled, for the inspector's
    /// resolved-config rows.
    pub fn material(&self) -> Option<&Material> {
        self.material.as_ref()
    }

    /// The material's uniform parameters, in layout order, at the values
    /// the buffer currently holds.
    pub fn material_params(&self) -> Vec<(String, wxsl_core::node::Value)> {
        let Some(bindings) = self.bindings.as_ref() else {
            return Vec::new();
        };
        bindings
            .interface()
            .params
            .fields()
            .iter()
            .filter_map(|field| {
                let name = field.name.as_str();
                Some((name.to_string(), bindings.get(name)?))
            })
            .collect()
    }

    /// Write one parameter into the buffer: an upload at the next frame,
    /// no recompile — the slider path ADR 0023 exists for.
    pub fn set_material_param(
        &mut self,
        name: &str,
        value: wxsl_core::node::Value,
    ) -> Result<(), RenderError> {
        if let Some(bindings) = self.compare.as_mut().and_then(|c| c.bindings.as_mut()) {
            bindings.set(name, value)?;
        }
        match self.bindings.as_mut() {
            Some(bindings) => bindings.set(name, value),
            None => Ok(()),
        }
    }

    /// Pause on the running pass list and step through it (plan3 N9):
    /// the frame freezes, and every pass runs until a step cuts it.
    pub fn start_stepping(&mut self) -> Result<(), RenderError> {
        if self.stepping.is_some() {
            return Ok(());
        }
        for effect in [stepping::INSPECT, stepping::INSPECT_DEPTH] {
            if self.renderer.effects().get(effect.id).is_none() {
                self.renderer.add_effect(effect);
            }
        }
        let full = self.renderer.render_graph().clone();
        let order = full.schedule()?.order().to_vec();
        self.stepping = Some(Stepping {
            count: order.len(),
            order,
            full,
            show: None,
            restore: self.renderer.pipeline(),
            time: self.time,
        });
        self.apply_step()
    }

    /// Run the first `count` passes (at least one), showing `show` — or
    /// what the last of them wrote, with `None`.
    pub fn step_to(&mut self, count: usize, show: Option<ResourceId>) -> Result<(), RenderError> {
        let Some(stepping) = self.stepping.as_mut() else {
            return Ok(());
        };
        stepping.count = count.clamp(1, stepping.order.len().max(1));
        stepping.show = show;
        self.apply_step()
    }

    /// Back to the full pass list, running live.
    pub fn stop_stepping(&mut self) {
        let Some(stepping) = self.stepping.take() else {
            return;
        };
        match stepping.restore {
            Some(stock) => self.renderer.set_pipeline(stock),
            None => {
                // It ran before stepping, so it installs again.
                let _ = self.renderer.set_graph(stepping.full);
            }
        }
    }

    /// The pass list being stepped through, for the frame tab.
    pub fn stepping(&self) -> Option<StepView> {
        let stepping = self.stepping.as_ref()?;
        let passes = stepping
            .order
            .iter()
            .map(|&index| stepping.full.passes()[index].label.clone())
            .collect();
        let resources = self.step_resources(stepping);
        let shown = self
            .shown_resource(stepping)
            .map(|id| stepping.full.resources()[id.index()].label.clone());
        Some(StepView {
            passes,
            count: stepping.count,
            resources,
            shown,
            pinned: stepping.show.is_some(),
        })
    }

    /// The textures the running passes write, in the order they are first
    /// written, the frame's target left out.
    fn step_resources(&self, stepping: &Stepping) -> Vec<(ResourceId, String, bool)> {
        let mut seen = Vec::new();
        for &index in &stepping.order[..stepping.count] {
            for id in stepping.full.passes()[index].written() {
                if id != wxsl_render::RenderGraph::TARGET && !seen.contains(&id) {
                    seen.push(id);
                }
            }
        }
        seen.into_iter()
            .map(|id| {
                let desc = &stepping.full.resources()[id.index()];
                (id, desc.label.clone(), stepping::effect_for(desc).is_some())
            })
            .collect()
    }

    /// The resource on screen: the chosen one, or the first drawable one
    /// the last running pass wrote — `None` when that pass wrote the
    /// frame's target, which is then simply the frame.
    fn shown_resource(&self, stepping: &Stepping) -> Option<ResourceId> {
        if stepping.show.is_some() {
            return stepping.show;
        }
        let last = &stepping.full.passes()[*stepping.order.get(stepping.count.checked_sub(1)?)?];
        if last
            .written()
            .any(|id| id == wxsl_render::RenderGraph::TARGET)
        {
            return None;
        }
        last.written().find(|&id| {
            id != wxsl_render::RenderGraph::TARGET
                && stepping::effect_for(&stepping.full.resources()[id.index()]).is_some()
        })
    }

    /// Install the current cut.
    fn apply_step(&mut self) -> Result<(), RenderError> {
        let Some(stepping) = self.stepping.as_ref() else {
            return Ok(());
        };
        let show = self.shown_resource(stepping).and_then(|id| {
            let effect = stepping::effect_for(&stepping.full.resources()[id.index()])?;
            Some((id, effect))
        });
        let cut = stepping.full.stepped(stepping.count, show)?;
        self.renderer.set_graph(cut)?;
        if show.is_some() {
            self.renderer.set_pass_param(
                wxsl_render::RenderGraph::STEP_INSPECT_PASS,
                "view_size",
                Value::Vec2([Self::SIZE as f32; 2]),
            )?;
        }
        Ok(())
    }

    /// The B side's pipeline, when the A/B split is up.
    pub fn compare(&self) -> Option<StockPipeline> {
        self.compare.as_ref().map(|compare| compare.pipeline)
    }

    /// The texture the B side draws into.
    pub fn compare_texture(&self) -> TextureId {
        self.compare_texture
    }

    /// The B side's last frame, read back as RGBA8 rows.
    pub fn compare_rgba8(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Vec<u8> {
        self.compare_target.read_rgba8(device, queue)
    }

    /// Put up the A/B split with `pipeline` on the B side, or take it down
    /// with `None`. The B renderer is built on first use and carries the
    /// A side's lighting set and feature channels, so the one material
    /// resolves against both.
    pub fn set_compare(
        &mut self,
        device: &wgpu::Device,
        pipeline: Option<StockPipeline>,
    ) -> Result<(), RenderError> {
        let Some(pipeline) = pipeline else {
            self.compare = None;
            return Ok(());
        };
        if let Some(compare) = self.compare.as_mut() {
            compare.renderer.set_pipeline(pipeline);
            compare.pipeline = pipeline;
            return Ok(());
        }
        let mut renderer = Renderer::new(
            device,
            self.library.clone(),
            TargetConfig::new(Self::SIZE, Self::SIZE, self.compare_target.format()),
        )?;
        renderer.set_lighting(self.renderer.lighting().clone())?;
        renderer.set_features(&self.renderer.features())?;
        renderer.set_pipeline(pipeline);
        let (bindings, user) = match self.material.as_ref() {
            Some(material) => {
                let (mut bindings, user) = bind_material(
                    &mut renderer,
                    device,
                    material,
                    &self.placeholder,
                    &self.placeholder_sampler,
                );
                // The A side's tuned values, not the graph's defaults.
                for (name, value) in self.material_params() {
                    bindings.set(&name, value)?;
                }
                (Some(bindings), user)
            }
            None => (None, None),
        };
        self.compare = Some(Compare {
            renderer,
            pipeline,
            bindings,
            user,
        });
        Ok(())
    }

    /// Back to the stock pipeline the material canvas chose.
    ///
    /// What leaving the pipeline canvas means: the two canvases share one
    /// renderer, so each puts back what it expects before the other's
    /// next frame.
    pub fn restore_stock(&mut self) {
        self.stepping = None;
        self.renderer.set_pipeline(self.stock);
        self.swapping = false;
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
        self.stop_stepping();
        // The material canvas's choice, whether or not the swap has to
        // wait: it is what leaving the pipeline canvas restores.
        self.stock = pipeline;
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

        // Resolved against the renderer's own plan — its lighting set and
        // feature channels — because a material's G-buffer module is
        // generated for that layout and the frame checks the handshake by
        // name (plan3 N9: the features panel can change the plan live).
        let mut config = wxsl_render::material::MaterialConfig::with_macros(macros.clone());
        config.features = self.renderer.pipeline_config().features.clone();
        if self.debug_view != 0 {
            config
                .macros
                .set(lighting::DEBUG_VIEW_MACRO, MacroValue::Int(self.debug_view));
        }
        match Material::with_lighting(graph, registry, &config, self.renderer.lighting()) {
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
        let (bindings, user) = bind_material(
            &mut self.renderer,
            device,
            material,
            &self.placeholder,
            &self.placeholder_sampler,
        );
        self.bindings = Some(bindings);
        self.user = user;
        if let Some(compare) = self.compare.as_mut() {
            let (bindings, user) = bind_material(
                &mut compare.renderer,
                device,
                material,
                &self.placeholder,
                &self.placeholder_sampler,
            );
            compare.bindings = Some(bindings);
            compare.user = user;
        }

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
        // Kept in step every frame rather than at the toggle: a pipeline
        // swap or a document install can bring a fresh tonemap pass.
        let curve = Value::F32(if self.debug_view == 0 { 1.0 } else { 0.0 });
        let renderers = core::iter::once(&mut self.renderer)
            .chain(self.compare.as_mut().map(|compare| &mut compare.renderer));
        for renderer in renderers {
            if renderer
                .pass_param(TONEMAP_PASS, "curve")
                .is_some_and(|current| current != curve)
            {
                renderer.set_pass_param(TONEMAP_PASS, "curve", curve)?;
            }
        }
        // Stepping shows one frame, pass by pass: nothing moves.
        let (dt, time) = match self.stepping.as_ref() {
            Some(stepping) => (0.0, stepping.time),
            None => (dt, time),
        };
        self.time = time;
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
        )?;
        let Some(compare) = self.compare.as_mut() else {
            return Ok(());
        };
        if let Some(bindings) = compare.bindings.as_mut() {
            bindings.upload(device, queue)?;
        }
        let mut item = DrawItem::new(&self.mesh, material)
            .with_transform(model)
            .with_attributes(&self.attributes);
        if let Some(bindings) = compare.bindings.as_ref() {
            item = item.with_bindings(bindings);
        }
        if let Some(user) = compare.user.as_ref() {
            item = item.with_user(user);
        }
        compare.renderer.render(
            device,
            queue,
            &RenderRequest {
                view: self.compare_target.view(),
                environment: &environment,
                draws: &wxsl_render::single_draw(item),
            },
        )
    }

    /// Turn the preview by a drag, in pixels.
    pub fn drag(&mut self, delta: f32) {
        self.angle += delta * 0.01;
    }
}

/// The material's own bind group on `renderer`, every declared texture and
/// sampler bound to the placeholder, and the zeroed stand-in for the
/// application's block. Per renderer, because each caches its own layouts:
/// the A/B split's B side binds the same material again.
fn bind_material(
    renderer: &mut Renderer,
    device: &wgpu::Device,
    material: &Material,
    placeholder: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> (MaterialBindings, Option<wgpu::BindGroup>) {
    let mut bindings = renderer.material_bindings(device, material);
    for resource in &material.interface().resources {
        let name = resource.name.as_str();
        let bound = match resource.ty {
            ValueType::Sampler => bindings.set_sampler(name, sampler),
            _ => bindings.set_texture(name, placeholder),
        };
        debug_assert!(bound.is_ok(), "the interface named this resource");
    }
    let user = material.interface().user.as_ref().map(|block| {
        // Zeroed: the editor is not the application, so the honest
        // stand-in is "nothing has been set".
        let size = u64::from(block.layout.size()).max(16);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wxsl preview application block"),
            size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = renderer
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
    (bindings, user)
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
        // The preview has no velocity stage; there is nothing to be the
        // previous frame *of*.
        previous_time: time,
        previous_camera: None,
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

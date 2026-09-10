//! [`RenderError`]: everything that can go wrong between a graph and a frame.

use core::fmt;

use wxsl_core::error::CodegenError;

/// An error from compiling a material or running a pipeline.
#[derive(Debug)]
#[non_exhaustive]
pub enum RenderError {
    /// The graph could not be turned into WXSL.
    Codegen(CodegenError),
    /// The WXSL compiler rejected the module.
    ///
    /// The diagnostic is kept as pre-rendered text rather than as a
    /// `wxsl_lang::Diagnostics`: it is already pretty-printed with source
    /// spans and a caret, and
    /// keeping the compiler's error type out of this crate's public API means
    /// a change to the compiler's own error type does not become a
    /// breaking change here.
    ShaderCompile {
        /// Root module that was being compiled.
        module: String,
        /// The compiler's diagnostic.
        diagnostic: String,
    },
    /// A module the shader imports is not in the [`crate::library::ShaderLibrary`].
    ///
    /// Almost always a missing `library.add_stdlib(…)`-shaped call: the
    /// generated module imports the shader ABI, which `wxsl-stdlib`
    /// provides and this crate deliberately does not depend on.
    MissingModule {
        /// The module that could not be resolved.
        module: String,
    },
    /// A module path is not valid WXSL syntax.
    InvalidModulePath {
        /// The offending path.
        module: String,
    },
    /// A pipeline was asked to record a frame before `configure` gave it a
    /// target size and format.
    NotConfigured,
    /// The deferred path was asked to record a frame with no lighting-pass
    /// shader. [`crate::renderer::Renderer`] compiles one for you.
    NoLightingShader,
    /// A pass list could not be ordered, validated or allocated.
    Graph(crate::graph::GraphError),
    /// A background pipeline swap will never finish: the thread compiling
    /// it is gone. Only reachable if that thread panicked, and reported
    /// rather than left as an indicator counting to a total it can no
    /// longer reach.
    SwapAbandoned,
    /// A parameter could not be written: no such name, or the wrong type
    /// for the one there is.
    MaterialParameter {
        /// The parameter that was addressed.
        name: String,
        /// What `wxsl-core`'s layout objected to.
        reason: String,
    },
    /// A texture or sampler was bound under a name the material's graph
    /// does not declare. Almost always a typo against the `name` setting
    /// on a `texture.*` node.
    UndeclaredMaterialResource {
        /// The name that was used.
        name: String,
    },
    /// A texture was bound where the graph declares a sampler, or the
    /// other way round.
    MaterialResourceKind {
        /// The resource's name.
        name: String,
        /// What the graph declares it as.
        declared: String,
    },
    /// A declared texture or sampler was never bound, so there is no bind
    /// group to build. Named, because "the surface is black" is a much
    /// worse way to find this out.
    UnboundMaterialResource {
        /// The resource's name.
        name: String,
        /// What kind of resource it is.
        kind: String,
    },
    /// A draw's material declares a group the draw did not carry: either
    /// [`crate::draw::DrawItem::bindings`] for its own parameters and
    /// textures, or [`crate::draw::DrawItem::user`] for the block it
    /// expects the application to supply.
    MissingDrawBindings {
        /// The material's name.
        material: String,
        /// Which group is missing, by its `abi::BIND_GROUPS` name.
        group: &'static str,
    },
    /// A per-vertex stream is not as long as the mesh it was given to.
    VertexStreamLength {
        /// The mesh's label.
        mesh: String,
        /// The stream's name.
        attribute: String,
        /// How many values were supplied.
        supplied: usize,
        /// How many vertices the mesh has.
        vertices: usize,
    },
    /// A material declares a per-vertex attribute the mesh it is drawn on
    /// does not carry.
    ///
    /// Reported when the frame is compiled, before any pass is opened, so
    /// it names the material, the attribute and the mesh rather than
    /// arriving as a `wgpu` complaint about vertex buffer 4 — or, worse,
    /// as a frame that merely looks wrong.
    MissingVertexAttribute {
        /// The material that declares it.
        material: String,
        /// The attribute's name.
        attribute: String,
        /// The mesh's label.
        mesh: String,
        /// What the mesh does carry.
        available: Vec<String>,
    },
    /// A mesh carries the declared attribute under a different type.
    VertexAttributeType {
        /// The material that declares it.
        material: String,
        /// The attribute's name.
        attribute: String,
        /// The mesh's label.
        mesh: String,
        /// The type the mesh supplies.
        supplied: wxsl_core::node::ValueType,
        /// The type the graph declares.
        declared: wxsl_core::node::ValueType,
    },
    /// A material declares a per-instance attribute the draw does not
    /// supply. See `DrawItem::with_attributes`.
    MissingInstanceAttribute {
        /// The material that declares it.
        material: String,
        /// The attribute's name.
        attribute: String,
        /// Which draw, by its index in the list.
        draw: usize,
    },
    /// A draw supplies a per-instance attribute at the wrong type.
    InstanceAttributeType {
        /// The material that declares it.
        material: String,
        /// The attribute's name.
        attribute: String,
        /// What `wxsl-core`'s layout objected to.
        reason: String,
    },
    /// A pass wants a resource the graph does not own and nobody supplied a
    /// view for. Every imported resource — the frame's target above all —
    /// has to be handed to [`crate::graph::RenderGraph::record`].
    MissingImport {
        /// The resource's label.
        resource: String,
    },
    /// A geometry file could not be read.
    ///
    /// Only reachable with the `gltf` feature; the variant exists either
    /// way so that matching on this enum does not change with the feature
    /// set.
    Import {
        /// The file that was being read.
        path: String,
        /// What the importer objected to.
        reason: String,
    },
    /// The UI atlas has no room left for an entry of that size.
    ///
    /// Not silently dropped: a missing glyph is a hole in the interface, and
    /// the caller is the only one who can decide between repacking, growing
    /// the atlas and living with it. See [`crate::ui::Atlas::clear`].
    AtlasFull {
        /// Width of the entry that did not fit.
        width: u32,
        /// Height of the entry that did not fit.
        height: u32,
    },
    /// A font's bytes could not be parsed.
    InvalidFont {
        /// What the parser objected to.
        reason: String,
    },
    /// No `wgpu` adapter matched the requested options.
    NoAdapter,
    /// `wgpu` refused to create a device.
    NoDevice(wgpu::RequestDeviceError),
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderError::Codegen(error) => write!(f, "cannot generate WXSL: {error}"),
            RenderError::ShaderCompile { module, diagnostic } => {
                write!(f, "cannot compile `{module}`:\n{diagnostic}")
            }
            RenderError::MissingModule { module } => write!(
                f,
                "the shader library has no module `{module}`; the ABI modules come from \
                 wxsl-stdlib, which this crate does not depend on — add them to the \
                 library (see `ShaderLibrary::insert`)"
            ),
            RenderError::InvalidModulePath { module } => {
                write!(f, "`{module}` is not a valid WXSL module path")
            }
            RenderError::NotConfigured => {
                f.write_str("pipeline has no target yet: call `configure` first")
            }
            RenderError::NoLightingShader => {
                f.write_str("the deferred path needs a compiled lighting-pass shader")
            }
            RenderError::Graph(error) => write!(f, "cannot run the pass list: {error}"),
            RenderError::SwapAbandoned => {
                f.write_str("the pipeline swap was abandoned: its compiler thread is gone")
            }
            RenderError::MaterialParameter { name, reason } => {
                write!(f, "cannot set parameter `{name}`: {reason}")
            }
            RenderError::VertexStreamLength {
                mesh,
                attribute,
                supplied,
                vertices,
            } => write!(
                f,
                "stream `{attribute}` has {supplied} values and mesh `{mesh}` has {vertices} vertices"
            ),
            RenderError::MissingVertexAttribute {
                material,
                attribute,
                mesh,
                available,
            } => write!(
                f,
                "material `{material}` requires the per-vertex attribute `{attribute}`, \
                 which mesh `{mesh}` does not carry (it carries: {})",
                if available.is_empty() {
                    "nothing beyond the base vertex".to_string()
                } else {
                    available.join(", ")
                }
            ),
            RenderError::VertexAttributeType {
                material,
                attribute,
                mesh,
                supplied,
                declared,
            } => write!(
                f,
                "material `{material}` declares `{attribute}` as {declared}, and mesh \
                 `{mesh}` supplies it as {supplied}"
            ),
            RenderError::MissingInstanceAttribute {
                material,
                attribute,
                draw,
            } => write!(
                f,
                "material `{material}` requires the per-instance attribute `{attribute}`, \
                 which draw {draw} does not supply"
            ),
            RenderError::InstanceAttributeType {
                material,
                attribute,
                reason,
            } => write!(
                f,
                "material `{material}`'s per-instance attribute `{attribute}`: {reason}"
            ),
            RenderError::UndeclaredMaterialResource { name } => write!(
                f,
                "this material declares no texture or sampler called `{name}`"
            ),
            RenderError::MaterialResourceKind { name, declared } => write!(
                f,
                "`{name}` is declared as {declared}, and was bound as something else"
            ),
            RenderError::UnboundMaterialResource { name, kind } => write!(
                f,
                "the {kind} `{name}` was never bound, so this material cannot draw"
            ),
            RenderError::MissingDrawBindings { material, group } => write!(
                f,
                "`{material}` declares a `{group}` bind group, but the draw carried none"
            ),
            RenderError::MissingImport { resource } => write!(
                f,
                "no view was supplied for the imported resource `{resource}`"
            ),
            RenderError::Import { path, reason } => {
                write!(f, "cannot import `{path}`: {reason}")
            }
            RenderError::AtlasFull { width, height } => write!(
                f,
                "the UI atlas has no room for a {width}x{height} entry; clear it, or                  build it larger (see `Atlas::new`)"
            ),
            RenderError::InvalidFont { reason } => {
                write!(f, "cannot read the font: {reason}")
            }
            RenderError::NoAdapter => f.write_str("no suitable wgpu adapter"),
            RenderError::NoDevice(error) => write!(f, "wgpu refused a device: {error}"),
        }
    }
}

impl std::error::Error for RenderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RenderError::Codegen(error) => Some(error),
            RenderError::Graph(error) => Some(error),
            RenderError::NoDevice(error) => Some(error),
            _ => None,
        }
    }
}

impl From<CodegenError> for RenderError {
    fn from(value: CodegenError) -> Self {
        RenderError::Codegen(value)
    }
}

impl From<crate::graph::GraphError> for RenderError {
    fn from(value: crate::graph::GraphError) -> Self {
        RenderError::Graph(value)
    }
}

//! [`RenderError`]: everything that can go wrong between a graph and a frame.

use core::fmt;

use wesloom_core::error::CodegenError;

/// An error from compiling a material or running a pipeline.
#[derive(Debug)]
#[non_exhaustive]
pub enum RenderError {
    /// The graph could not be turned into WESL.
    Codegen(CodegenError),
    /// The `wesl` compiler rejected the module.
    ///
    /// The diagnostic is kept as pre-rendered text rather than as a
    /// `wesl::Error`: it is already pretty-printed with source spans, and
    /// keeping the compiler's error type out of this crate's public API means
    /// a `wesl` release (it is still 0.x, see ADR 0003) does not become a
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
    /// generated module imports the shader ABI, which `wesloom-stdlib`
    /// provides and this crate deliberately does not depend on.
    MissingModule {
        /// The module that could not be resolved.
        module: String,
    },
    /// A module path is not valid WESL syntax.
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
    /// No `wgpu` adapter matched the requested options.
    NoAdapter,
    /// `wgpu` refused to create a device.
    NoDevice(wgpu::RequestDeviceError),
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderError::Codegen(error) => write!(f, "cannot generate WESL: {error}"),
            RenderError::ShaderCompile { module, diagnostic } => {
                write!(f, "cannot compile `{module}`:\n{diagnostic}")
            }
            RenderError::MissingModule { module } => write!(
                f,
                "the shader library has no module `{module}`; the ABI modules come from \
                 wesloom-stdlib, which this crate does not depend on — add them to the \
                 library (see `ShaderLibrary::insert`)"
            ),
            RenderError::InvalidModulePath { module } => {
                write!(f, "`{module}` is not a valid WESL module path")
            }
            RenderError::NotConfigured => {
                f.write_str("pipeline has no target yet: call `configure` first")
            }
            RenderError::NoLightingShader => {
                f.write_str("the deferred path needs a lighting-pass shader in `FrameInput`")
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

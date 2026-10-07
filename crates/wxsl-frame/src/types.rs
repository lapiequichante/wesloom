//! Backend-neutral graphics vocabulary for frame plans (ADR 0048).

/// AstcBlock in a frame's graphics state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AstcBlock {
    /// B4x4.
    B4x4,
    /// B5x4.
    B5x4,
    /// B5x5.
    B5x5,
    /// B6x5.
    B6x5,
    /// B6x6.
    B6x6,
    /// B8x5.
    B8x5,
    /// B8x6.
    B8x6,
    /// B8x8.
    B8x8,
    /// B10x5.
    B10x5,
    /// B10x6.
    B10x6,
    /// B10x8.
    B10x8,
    /// B10x10.
    B10x10,
    /// B12x10.
    B12x10,
    /// B12x12.
    B12x12,
}

/// AstcChannel in a frame's graphics state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AstcChannel {
    /// Unorm.
    Unorm,
    /// UnormSrgb.
    UnormSrgb,
    /// Hdr.
    Hdr,
}

/// Face in a frame's graphics state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    /// Front.
    Front,
    /// Back.
    Back,
}

/// CompareFunction in a frame's graphics state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CompareFunction {
    /// Never.
    Never,
    /// Less.
    Less,
    /// Equal.
    Equal,
    /// LessEqual.
    LessEqual,
    /// Greater.
    Greater,
    /// NotEqual.
    NotEqual,
    /// GreaterEqual.
    GreaterEqual,
    /// Always.
    Always,
}

/// BlendFactor in a frame's graphics state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlendFactor {
    /// Zero.
    Zero,
    /// One.
    One,
    /// Src.
    Src,
    /// OneMinusSrc.
    OneMinusSrc,
    /// SrcAlpha.
    SrcAlpha,
    /// OneMinusSrcAlpha.
    OneMinusSrcAlpha,
    /// Dst.
    Dst,
    /// OneMinusDst.
    OneMinusDst,
    /// DstAlpha.
    DstAlpha,
    /// OneMinusDstAlpha.
    OneMinusDstAlpha,
    /// SrcAlphaSaturated.
    SrcAlphaSaturated,
    /// Constant.
    Constant,
    /// OneMinusConstant.
    OneMinusConstant,
    /// Src1.
    Src1,
    /// OneMinusSrc1.
    OneMinusSrc1,
    /// Src1Alpha.
    Src1Alpha,
    /// OneMinusSrc1Alpha.
    OneMinusSrc1Alpha,
}

/// BlendOperation in a frame's graphics state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlendOperation {
    /// Add.
    Add,
    /// Subtract.
    Subtract,
    /// ReverseSubtract.
    ReverseSubtract,
    /// Min.
    Min,
    /// Max.
    Max,
}

/// Texture formats named by frame resources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextureFormat {
    /// R8Unorm.
    R8Unorm,
    /// R8Snorm.
    R8Snorm,
    /// R8Uint.
    R8Uint,
    /// R8Sint.
    R8Sint,
    /// R16Uint.
    R16Uint,
    /// R16Sint.
    R16Sint,
    /// R16Unorm.
    R16Unorm,
    /// R16Snorm.
    R16Snorm,
    /// R16Float.
    R16Float,
    /// Rg8Unorm.
    Rg8Unorm,
    /// Rg8Snorm.
    Rg8Snorm,
    /// Rg8Uint.
    Rg8Uint,
    /// Rg8Sint.
    Rg8Sint,
    /// R32Uint.
    R32Uint,
    /// R32Sint.
    R32Sint,
    /// R32Float.
    R32Float,
    /// Rg16Uint.
    Rg16Uint,
    /// Rg16Sint.
    Rg16Sint,
    /// Rg16Unorm.
    Rg16Unorm,
    /// Rg16Snorm.
    Rg16Snorm,
    /// Rg16Float.
    Rg16Float,
    /// Rgba8Unorm.
    Rgba8Unorm,
    /// Rgba8UnormSrgb.
    Rgba8UnormSrgb,
    /// Rgba8Snorm.
    Rgba8Snorm,
    /// Rgba8Uint.
    Rgba8Uint,
    /// Rgba8Sint.
    Rgba8Sint,
    /// Bgra8Unorm.
    Bgra8Unorm,
    /// Bgra8UnormSrgb.
    Bgra8UnormSrgb,
    /// Rgb9e5Ufloat.
    Rgb9e5Ufloat,
    /// Rgb10a2Uint.
    Rgb10a2Uint,
    /// Rgb10a2Unorm.
    Rgb10a2Unorm,
    /// Rg11b10Ufloat.
    Rg11b10Ufloat,
    /// R64Uint.
    R64Uint,
    /// Rg32Uint.
    Rg32Uint,
    /// Rg32Sint.
    Rg32Sint,
    /// Rg32Float.
    Rg32Float,
    /// Rgba16Uint.
    Rgba16Uint,
    /// Rgba16Sint.
    Rgba16Sint,
    /// Rgba16Unorm.
    Rgba16Unorm,
    /// Rgba16Snorm.
    Rgba16Snorm,
    /// Rgba16Float.
    Rgba16Float,
    /// Rgba32Uint.
    Rgba32Uint,
    /// Rgba32Sint.
    Rgba32Sint,
    /// Rgba32Float.
    Rgba32Float,
    /// Stencil8.
    Stencil8,
    /// Depth16Unorm.
    Depth16Unorm,
    /// Depth24Plus.
    Depth24Plus,
    /// Depth24PlusStencil8.
    Depth24PlusStencil8,
    /// Depth32Float.
    Depth32Float,
    /// Depth32FloatStencil8.
    Depth32FloatStencil8,
    /// NV12.
    NV12,
    /// P010.
    P010,
    /// Bc1RgbaUnorm.
    Bc1RgbaUnorm,
    /// Bc1RgbaUnormSrgb.
    Bc1RgbaUnormSrgb,
    /// Bc2RgbaUnorm.
    Bc2RgbaUnorm,
    /// Bc2RgbaUnormSrgb.
    Bc2RgbaUnormSrgb,
    /// Bc3RgbaUnorm.
    Bc3RgbaUnorm,
    /// Bc3RgbaUnormSrgb.
    Bc3RgbaUnormSrgb,
    /// Bc4RUnorm.
    Bc4RUnorm,
    /// Bc4RSnorm.
    Bc4RSnorm,
    /// Bc5RgUnorm.
    Bc5RgUnorm,
    /// Bc5RgSnorm.
    Bc5RgSnorm,
    /// Bc6hRgbUfloat.
    Bc6hRgbUfloat,
    /// Bc6hRgbFloat.
    Bc6hRgbFloat,
    /// Bc7RgbaUnorm.
    Bc7RgbaUnorm,
    /// Bc7RgbaUnormSrgb.
    Bc7RgbaUnormSrgb,
    /// Etc2Rgb8Unorm.
    Etc2Rgb8Unorm,
    /// Etc2Rgb8UnormSrgb.
    Etc2Rgb8UnormSrgb,
    /// Etc2Rgb8A1Unorm.
    Etc2Rgb8A1Unorm,
    /// Etc2Rgb8A1UnormSrgb.
    Etc2Rgb8A1UnormSrgb,
    /// Etc2Rgba8Unorm.
    Etc2Rgba8Unorm,
    /// Etc2Rgba8UnormSrgb.
    Etc2Rgba8UnormSrgb,
    /// EacR11Unorm.
    EacR11Unorm,
    /// EacR11Snorm.
    EacR11Snorm,
    /// EacRg11Unorm.
    EacRg11Unorm,
    /// EacRg11Snorm.
    EacRg11Snorm,
    /// ASTC compressed format.
    Astc {
        /// Block dimensions.
        block: AstcBlock,
        /// Channel encoding.
        channel: AstcChannel,
    },
}

/// A linear RGBA clear color.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    /// Red.
    pub r: f64,
    /// Green.
    pub g: f64,
    /// Blue.
    pub b: f64,
    /// Alpha.
    pub a: f64,
}

impl Color {
    /// Transparent black.
    pub const TRANSPARENT: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };
    /// Opaque black.
    pub const BLACK: Self = Self {
        a: 1.0,
        ..Self::TRANSPARENT
    };
}

/// One blend equation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlendComponent {
    /// Source multiplier.
    pub src_factor: BlendFactor,
    /// Destination multiplier.
    pub dst_factor: BlendFactor,
    /// Operation after multiplying.
    pub operation: BlendOperation,
}

impl BlendComponent {
    /// Replace the destination with the source.
    pub const REPLACE: Self = Self {
        src_factor: BlendFactor::One,
        dst_factor: BlendFactor::Zero,
        operation: BlendOperation::Add,
    };
    /// Premultiplied source over destination.
    pub const OVER: Self = Self {
        dst_factor: BlendFactor::OneMinusSrcAlpha,
        ..Self::REPLACE
    };
}

/// Color and alpha blend equations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlendState {
    /// Color equation.
    pub color: BlendComponent,
    /// Alpha equation.
    pub alpha: BlendComponent,
}

impl BlendState {
    /// Replace both channels.
    pub const REPLACE: Self = Self {
        color: BlendComponent::REPLACE,
        alpha: BlendComponent::REPLACE,
    };
    /// Straight-alpha blending.
    pub const ALPHA_BLENDING: Self = Self {
        color: BlendComponent {
            src_factor: BlendFactor::SrcAlpha,
            dst_factor: BlendFactor::OneMinusSrcAlpha,
            operation: BlendOperation::Add,
        },
        alpha: BlendComponent::OVER,
    };
    /// Premultiplied-alpha blending.
    pub const PREMULTIPLIED_ALPHA_BLENDING: Self = Self {
        color: BlendComponent::OVER,
        alpha: BlendComponent::OVER,
    };
}

bitflags::bitflags! {
    /// Uses a texture resource permits.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct TextureUsages: u32 {
        /// COPY_SRC usage.
        const COPY_SRC = 1 << 0;
        /// COPY_DST usage.
        const COPY_DST = 1 << 1;
        /// TEXTURE_BINDING usage.
        const TEXTURE_BINDING = 1 << 2;
        /// STORAGE_BINDING usage.
        const STORAGE_BINDING = 1 << 3;
        /// RENDER_ATTACHMENT usage.
        const RENDER_ATTACHMENT = 1 << 4;
        /// TRANSIENT_ATTACHMENT usage.
        const TRANSIENT_ATTACHMENT = 1 << 5;
        /// STORAGE_ATOMIC usage.
        const STORAGE_ATOMIC = 1 << 6;
    }
}

bitflags::bitflags! {
    /// Uses a buffer resource permits.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct BufferUsages: u32 {
        /// MAP_READ usage.
        const MAP_READ = 1 << 0;
        /// MAP_WRITE usage.
        const MAP_WRITE = 1 << 1;
        /// COPY_SRC usage.
        const COPY_SRC = 1 << 2;
        /// COPY_DST usage.
        const COPY_DST = 1 << 3;
        /// INDEX usage.
        const INDEX = 1 << 4;
        /// VERTEX usage.
        const VERTEX = 1 << 5;
        /// UNIFORM usage.
        const UNIFORM = 1 << 6;
        /// STORAGE usage.
        const STORAGE = 1 << 7;
        /// INDIRECT usage.
        const INDIRECT = 1 << 8;
        /// QUERY_RESOLVE usage.
        const QUERY_RESOLVE = 1 << 9;
        /// BLAS_INPUT usage.
        const BLAS_INPUT = 1 << 10;
        /// TLAS_INPUT usage.
        const TLAS_INPUT = 1 << 11;
    }
}

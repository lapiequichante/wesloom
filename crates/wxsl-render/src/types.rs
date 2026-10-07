//! Explicit conversion between neutral plans and wgpu (ADR 0048).

use crate::pass::{Dimension, PassState};
pub use wxsl_frame::types::*;

/// A neutral value's lossless mapping to its wgpu counterpart.
pub trait WgpuType: Sized {
    /// Native wgpu type.
    type Native;
    /// Map to the device vocabulary.
    fn to_wgpu(self) -> Self::Native;
    /// Map a native value to shared data.
    fn from_wgpu(value: Self::Native) -> Self;
}

impl WgpuType for AstcBlock {
    type Native = wgpu::AstcBlock;
    fn to_wgpu(self) -> Self::Native {
        match self {
            Self::B4x4 => wgpu::AstcBlock::B4x4,
            Self::B5x4 => wgpu::AstcBlock::B5x4,
            Self::B5x5 => wgpu::AstcBlock::B5x5,
            Self::B6x5 => wgpu::AstcBlock::B6x5,
            Self::B6x6 => wgpu::AstcBlock::B6x6,
            Self::B8x5 => wgpu::AstcBlock::B8x5,
            Self::B8x6 => wgpu::AstcBlock::B8x6,
            Self::B8x8 => wgpu::AstcBlock::B8x8,
            Self::B10x5 => wgpu::AstcBlock::B10x5,
            Self::B10x6 => wgpu::AstcBlock::B10x6,
            Self::B10x8 => wgpu::AstcBlock::B10x8,
            Self::B10x10 => wgpu::AstcBlock::B10x10,
            Self::B12x10 => wgpu::AstcBlock::B12x10,
            Self::B12x12 => wgpu::AstcBlock::B12x12,
        }
    }
    fn from_wgpu(value: Self::Native) -> Self {
        match value {
            wgpu::AstcBlock::B4x4 => Self::B4x4,
            wgpu::AstcBlock::B5x4 => Self::B5x4,
            wgpu::AstcBlock::B5x5 => Self::B5x5,
            wgpu::AstcBlock::B6x5 => Self::B6x5,
            wgpu::AstcBlock::B6x6 => Self::B6x6,
            wgpu::AstcBlock::B8x5 => Self::B8x5,
            wgpu::AstcBlock::B8x6 => Self::B8x6,
            wgpu::AstcBlock::B8x8 => Self::B8x8,
            wgpu::AstcBlock::B10x5 => Self::B10x5,
            wgpu::AstcBlock::B10x6 => Self::B10x6,
            wgpu::AstcBlock::B10x8 => Self::B10x8,
            wgpu::AstcBlock::B10x10 => Self::B10x10,
            wgpu::AstcBlock::B12x10 => Self::B12x10,
            wgpu::AstcBlock::B12x12 => Self::B12x12,
        }
    }
}

impl WgpuType for AstcChannel {
    type Native = wgpu::AstcChannel;
    fn to_wgpu(self) -> Self::Native {
        match self {
            Self::Unorm => wgpu::AstcChannel::Unorm,
            Self::UnormSrgb => wgpu::AstcChannel::UnormSrgb,
            Self::Hdr => wgpu::AstcChannel::Hdr,
        }
    }
    fn from_wgpu(value: Self::Native) -> Self {
        match value {
            wgpu::AstcChannel::Unorm => Self::Unorm,
            wgpu::AstcChannel::UnormSrgb => Self::UnormSrgb,
            wgpu::AstcChannel::Hdr => Self::Hdr,
        }
    }
}

impl WgpuType for Face {
    type Native = wgpu::Face;
    fn to_wgpu(self) -> Self::Native {
        match self {
            Self::Front => wgpu::Face::Front,
            Self::Back => wgpu::Face::Back,
        }
    }
    fn from_wgpu(value: Self::Native) -> Self {
        match value {
            wgpu::Face::Front => Self::Front,
            wgpu::Face::Back => Self::Back,
        }
    }
}

impl WgpuType for CompareFunction {
    type Native = wgpu::CompareFunction;
    fn to_wgpu(self) -> Self::Native {
        match self {
            Self::Never => wgpu::CompareFunction::Never,
            Self::Less => wgpu::CompareFunction::Less,
            Self::Equal => wgpu::CompareFunction::Equal,
            Self::LessEqual => wgpu::CompareFunction::LessEqual,
            Self::Greater => wgpu::CompareFunction::Greater,
            Self::NotEqual => wgpu::CompareFunction::NotEqual,
            Self::GreaterEqual => wgpu::CompareFunction::GreaterEqual,
            Self::Always => wgpu::CompareFunction::Always,
        }
    }
    fn from_wgpu(value: Self::Native) -> Self {
        match value {
            wgpu::CompareFunction::Never => Self::Never,
            wgpu::CompareFunction::Less => Self::Less,
            wgpu::CompareFunction::Equal => Self::Equal,
            wgpu::CompareFunction::LessEqual => Self::LessEqual,
            wgpu::CompareFunction::Greater => Self::Greater,
            wgpu::CompareFunction::NotEqual => Self::NotEqual,
            wgpu::CompareFunction::GreaterEqual => Self::GreaterEqual,
            wgpu::CompareFunction::Always => Self::Always,
        }
    }
}

impl WgpuType for BlendFactor {
    type Native = wgpu::BlendFactor;
    fn to_wgpu(self) -> Self::Native {
        match self {
            Self::Zero => wgpu::BlendFactor::Zero,
            Self::One => wgpu::BlendFactor::One,
            Self::Src => wgpu::BlendFactor::Src,
            Self::OneMinusSrc => wgpu::BlendFactor::OneMinusSrc,
            Self::SrcAlpha => wgpu::BlendFactor::SrcAlpha,
            Self::OneMinusSrcAlpha => wgpu::BlendFactor::OneMinusSrcAlpha,
            Self::Dst => wgpu::BlendFactor::Dst,
            Self::OneMinusDst => wgpu::BlendFactor::OneMinusDst,
            Self::DstAlpha => wgpu::BlendFactor::DstAlpha,
            Self::OneMinusDstAlpha => wgpu::BlendFactor::OneMinusDstAlpha,
            Self::SrcAlphaSaturated => wgpu::BlendFactor::SrcAlphaSaturated,
            Self::Constant => wgpu::BlendFactor::Constant,
            Self::OneMinusConstant => wgpu::BlendFactor::OneMinusConstant,
            Self::Src1 => wgpu::BlendFactor::Src1,
            Self::OneMinusSrc1 => wgpu::BlendFactor::OneMinusSrc1,
            Self::Src1Alpha => wgpu::BlendFactor::Src1Alpha,
            Self::OneMinusSrc1Alpha => wgpu::BlendFactor::OneMinusSrc1Alpha,
        }
    }
    fn from_wgpu(value: Self::Native) -> Self {
        match value {
            wgpu::BlendFactor::Zero => Self::Zero,
            wgpu::BlendFactor::One => Self::One,
            wgpu::BlendFactor::Src => Self::Src,
            wgpu::BlendFactor::OneMinusSrc => Self::OneMinusSrc,
            wgpu::BlendFactor::SrcAlpha => Self::SrcAlpha,
            wgpu::BlendFactor::OneMinusSrcAlpha => Self::OneMinusSrcAlpha,
            wgpu::BlendFactor::Dst => Self::Dst,
            wgpu::BlendFactor::OneMinusDst => Self::OneMinusDst,
            wgpu::BlendFactor::DstAlpha => Self::DstAlpha,
            wgpu::BlendFactor::OneMinusDstAlpha => Self::OneMinusDstAlpha,
            wgpu::BlendFactor::SrcAlphaSaturated => Self::SrcAlphaSaturated,
            wgpu::BlendFactor::Constant => Self::Constant,
            wgpu::BlendFactor::OneMinusConstant => Self::OneMinusConstant,
            wgpu::BlendFactor::Src1 => Self::Src1,
            wgpu::BlendFactor::OneMinusSrc1 => Self::OneMinusSrc1,
            wgpu::BlendFactor::Src1Alpha => Self::Src1Alpha,
            wgpu::BlendFactor::OneMinusSrc1Alpha => Self::OneMinusSrc1Alpha,
        }
    }
}

impl WgpuType for BlendOperation {
    type Native = wgpu::BlendOperation;
    fn to_wgpu(self) -> Self::Native {
        match self {
            Self::Add => wgpu::BlendOperation::Add,
            Self::Subtract => wgpu::BlendOperation::Subtract,
            Self::ReverseSubtract => wgpu::BlendOperation::ReverseSubtract,
            Self::Min => wgpu::BlendOperation::Min,
            Self::Max => wgpu::BlendOperation::Max,
        }
    }
    fn from_wgpu(value: Self::Native) -> Self {
        match value {
            wgpu::BlendOperation::Add => Self::Add,
            wgpu::BlendOperation::Subtract => Self::Subtract,
            wgpu::BlendOperation::ReverseSubtract => Self::ReverseSubtract,
            wgpu::BlendOperation::Min => Self::Min,
            wgpu::BlendOperation::Max => Self::Max,
        }
    }
}

impl WgpuType for TextureFormat {
    type Native = wgpu::TextureFormat;
    fn to_wgpu(self) -> Self::Native {
        match self {
            Self::R8Unorm => wgpu::TextureFormat::R8Unorm,
            Self::R8Snorm => wgpu::TextureFormat::R8Snorm,
            Self::R8Uint => wgpu::TextureFormat::R8Uint,
            Self::R8Sint => wgpu::TextureFormat::R8Sint,
            Self::R16Uint => wgpu::TextureFormat::R16Uint,
            Self::R16Sint => wgpu::TextureFormat::R16Sint,
            Self::R16Unorm => wgpu::TextureFormat::R16Unorm,
            Self::R16Snorm => wgpu::TextureFormat::R16Snorm,
            Self::R16Float => wgpu::TextureFormat::R16Float,
            Self::Rg8Unorm => wgpu::TextureFormat::Rg8Unorm,
            Self::Rg8Snorm => wgpu::TextureFormat::Rg8Snorm,
            Self::Rg8Uint => wgpu::TextureFormat::Rg8Uint,
            Self::Rg8Sint => wgpu::TextureFormat::Rg8Sint,
            Self::R32Uint => wgpu::TextureFormat::R32Uint,
            Self::R32Sint => wgpu::TextureFormat::R32Sint,
            Self::R32Float => wgpu::TextureFormat::R32Float,
            Self::Rg16Uint => wgpu::TextureFormat::Rg16Uint,
            Self::Rg16Sint => wgpu::TextureFormat::Rg16Sint,
            Self::Rg16Unorm => wgpu::TextureFormat::Rg16Unorm,
            Self::Rg16Snorm => wgpu::TextureFormat::Rg16Snorm,
            Self::Rg16Float => wgpu::TextureFormat::Rg16Float,
            Self::Rgba8Unorm => wgpu::TextureFormat::Rgba8Unorm,
            Self::Rgba8UnormSrgb => wgpu::TextureFormat::Rgba8UnormSrgb,
            Self::Rgba8Snorm => wgpu::TextureFormat::Rgba8Snorm,
            Self::Rgba8Uint => wgpu::TextureFormat::Rgba8Uint,
            Self::Rgba8Sint => wgpu::TextureFormat::Rgba8Sint,
            Self::Bgra8Unorm => wgpu::TextureFormat::Bgra8Unorm,
            Self::Bgra8UnormSrgb => wgpu::TextureFormat::Bgra8UnormSrgb,
            Self::Rgb9e5Ufloat => wgpu::TextureFormat::Rgb9e5Ufloat,
            Self::Rgb10a2Uint => wgpu::TextureFormat::Rgb10a2Uint,
            Self::Rgb10a2Unorm => wgpu::TextureFormat::Rgb10a2Unorm,
            Self::Rg11b10Ufloat => wgpu::TextureFormat::Rg11b10Ufloat,
            Self::R64Uint => wgpu::TextureFormat::R64Uint,
            Self::Rg32Uint => wgpu::TextureFormat::Rg32Uint,
            Self::Rg32Sint => wgpu::TextureFormat::Rg32Sint,
            Self::Rg32Float => wgpu::TextureFormat::Rg32Float,
            Self::Rgba16Uint => wgpu::TextureFormat::Rgba16Uint,
            Self::Rgba16Sint => wgpu::TextureFormat::Rgba16Sint,
            Self::Rgba16Unorm => wgpu::TextureFormat::Rgba16Unorm,
            Self::Rgba16Snorm => wgpu::TextureFormat::Rgba16Snorm,
            Self::Rgba16Float => wgpu::TextureFormat::Rgba16Float,
            Self::Rgba32Uint => wgpu::TextureFormat::Rgba32Uint,
            Self::Rgba32Sint => wgpu::TextureFormat::Rgba32Sint,
            Self::Rgba32Float => wgpu::TextureFormat::Rgba32Float,
            Self::Stencil8 => wgpu::TextureFormat::Stencil8,
            Self::Depth16Unorm => wgpu::TextureFormat::Depth16Unorm,
            Self::Depth24Plus => wgpu::TextureFormat::Depth24Plus,
            Self::Depth24PlusStencil8 => wgpu::TextureFormat::Depth24PlusStencil8,
            Self::Depth32Float => wgpu::TextureFormat::Depth32Float,
            Self::Depth32FloatStencil8 => wgpu::TextureFormat::Depth32FloatStencil8,
            Self::NV12 => wgpu::TextureFormat::NV12,
            Self::P010 => wgpu::TextureFormat::P010,
            Self::Bc1RgbaUnorm => wgpu::TextureFormat::Bc1RgbaUnorm,
            Self::Bc1RgbaUnormSrgb => wgpu::TextureFormat::Bc1RgbaUnormSrgb,
            Self::Bc2RgbaUnorm => wgpu::TextureFormat::Bc2RgbaUnorm,
            Self::Bc2RgbaUnormSrgb => wgpu::TextureFormat::Bc2RgbaUnormSrgb,
            Self::Bc3RgbaUnorm => wgpu::TextureFormat::Bc3RgbaUnorm,
            Self::Bc3RgbaUnormSrgb => wgpu::TextureFormat::Bc3RgbaUnormSrgb,
            Self::Bc4RUnorm => wgpu::TextureFormat::Bc4RUnorm,
            Self::Bc4RSnorm => wgpu::TextureFormat::Bc4RSnorm,
            Self::Bc5RgUnorm => wgpu::TextureFormat::Bc5RgUnorm,
            Self::Bc5RgSnorm => wgpu::TextureFormat::Bc5RgSnorm,
            Self::Bc6hRgbUfloat => wgpu::TextureFormat::Bc6hRgbUfloat,
            Self::Bc6hRgbFloat => wgpu::TextureFormat::Bc6hRgbFloat,
            Self::Bc7RgbaUnorm => wgpu::TextureFormat::Bc7RgbaUnorm,
            Self::Bc7RgbaUnormSrgb => wgpu::TextureFormat::Bc7RgbaUnormSrgb,
            Self::Etc2Rgb8Unorm => wgpu::TextureFormat::Etc2Rgb8Unorm,
            Self::Etc2Rgb8UnormSrgb => wgpu::TextureFormat::Etc2Rgb8UnormSrgb,
            Self::Etc2Rgb8A1Unorm => wgpu::TextureFormat::Etc2Rgb8A1Unorm,
            Self::Etc2Rgb8A1UnormSrgb => wgpu::TextureFormat::Etc2Rgb8A1UnormSrgb,
            Self::Etc2Rgba8Unorm => wgpu::TextureFormat::Etc2Rgba8Unorm,
            Self::Etc2Rgba8UnormSrgb => wgpu::TextureFormat::Etc2Rgba8UnormSrgb,
            Self::EacR11Unorm => wgpu::TextureFormat::EacR11Unorm,
            Self::EacR11Snorm => wgpu::TextureFormat::EacR11Snorm,
            Self::EacRg11Unorm => wgpu::TextureFormat::EacRg11Unorm,
            Self::EacRg11Snorm => wgpu::TextureFormat::EacRg11Snorm,
            Self::Astc { block, channel } => wgpu::TextureFormat::Astc {
                block: block.to_wgpu(),
                channel: channel.to_wgpu(),
            },
        }
    }
    fn from_wgpu(value: Self::Native) -> Self {
        match value {
            wgpu::TextureFormat::R8Unorm => Self::R8Unorm,
            wgpu::TextureFormat::R8Snorm => Self::R8Snorm,
            wgpu::TextureFormat::R8Uint => Self::R8Uint,
            wgpu::TextureFormat::R8Sint => Self::R8Sint,
            wgpu::TextureFormat::R16Uint => Self::R16Uint,
            wgpu::TextureFormat::R16Sint => Self::R16Sint,
            wgpu::TextureFormat::R16Unorm => Self::R16Unorm,
            wgpu::TextureFormat::R16Snorm => Self::R16Snorm,
            wgpu::TextureFormat::R16Float => Self::R16Float,
            wgpu::TextureFormat::Rg8Unorm => Self::Rg8Unorm,
            wgpu::TextureFormat::Rg8Snorm => Self::Rg8Snorm,
            wgpu::TextureFormat::Rg8Uint => Self::Rg8Uint,
            wgpu::TextureFormat::Rg8Sint => Self::Rg8Sint,
            wgpu::TextureFormat::R32Uint => Self::R32Uint,
            wgpu::TextureFormat::R32Sint => Self::R32Sint,
            wgpu::TextureFormat::R32Float => Self::R32Float,
            wgpu::TextureFormat::Rg16Uint => Self::Rg16Uint,
            wgpu::TextureFormat::Rg16Sint => Self::Rg16Sint,
            wgpu::TextureFormat::Rg16Unorm => Self::Rg16Unorm,
            wgpu::TextureFormat::Rg16Snorm => Self::Rg16Snorm,
            wgpu::TextureFormat::Rg16Float => Self::Rg16Float,
            wgpu::TextureFormat::Rgba8Unorm => Self::Rgba8Unorm,
            wgpu::TextureFormat::Rgba8UnormSrgb => Self::Rgba8UnormSrgb,
            wgpu::TextureFormat::Rgba8Snorm => Self::Rgba8Snorm,
            wgpu::TextureFormat::Rgba8Uint => Self::Rgba8Uint,
            wgpu::TextureFormat::Rgba8Sint => Self::Rgba8Sint,
            wgpu::TextureFormat::Bgra8Unorm => Self::Bgra8Unorm,
            wgpu::TextureFormat::Bgra8UnormSrgb => Self::Bgra8UnormSrgb,
            wgpu::TextureFormat::Rgb9e5Ufloat => Self::Rgb9e5Ufloat,
            wgpu::TextureFormat::Rgb10a2Uint => Self::Rgb10a2Uint,
            wgpu::TextureFormat::Rgb10a2Unorm => Self::Rgb10a2Unorm,
            wgpu::TextureFormat::Rg11b10Ufloat => Self::Rg11b10Ufloat,
            wgpu::TextureFormat::R64Uint => Self::R64Uint,
            wgpu::TextureFormat::Rg32Uint => Self::Rg32Uint,
            wgpu::TextureFormat::Rg32Sint => Self::Rg32Sint,
            wgpu::TextureFormat::Rg32Float => Self::Rg32Float,
            wgpu::TextureFormat::Rgba16Uint => Self::Rgba16Uint,
            wgpu::TextureFormat::Rgba16Sint => Self::Rgba16Sint,
            wgpu::TextureFormat::Rgba16Unorm => Self::Rgba16Unorm,
            wgpu::TextureFormat::Rgba16Snorm => Self::Rgba16Snorm,
            wgpu::TextureFormat::Rgba16Float => Self::Rgba16Float,
            wgpu::TextureFormat::Rgba32Uint => Self::Rgba32Uint,
            wgpu::TextureFormat::Rgba32Sint => Self::Rgba32Sint,
            wgpu::TextureFormat::Rgba32Float => Self::Rgba32Float,
            wgpu::TextureFormat::Stencil8 => Self::Stencil8,
            wgpu::TextureFormat::Depth16Unorm => Self::Depth16Unorm,
            wgpu::TextureFormat::Depth24Plus => Self::Depth24Plus,
            wgpu::TextureFormat::Depth24PlusStencil8 => Self::Depth24PlusStencil8,
            wgpu::TextureFormat::Depth32Float => Self::Depth32Float,
            wgpu::TextureFormat::Depth32FloatStencil8 => Self::Depth32FloatStencil8,
            wgpu::TextureFormat::NV12 => Self::NV12,
            wgpu::TextureFormat::P010 => Self::P010,
            wgpu::TextureFormat::Bc1RgbaUnorm => Self::Bc1RgbaUnorm,
            wgpu::TextureFormat::Bc1RgbaUnormSrgb => Self::Bc1RgbaUnormSrgb,
            wgpu::TextureFormat::Bc2RgbaUnorm => Self::Bc2RgbaUnorm,
            wgpu::TextureFormat::Bc2RgbaUnormSrgb => Self::Bc2RgbaUnormSrgb,
            wgpu::TextureFormat::Bc3RgbaUnorm => Self::Bc3RgbaUnorm,
            wgpu::TextureFormat::Bc3RgbaUnormSrgb => Self::Bc3RgbaUnormSrgb,
            wgpu::TextureFormat::Bc4RUnorm => Self::Bc4RUnorm,
            wgpu::TextureFormat::Bc4RSnorm => Self::Bc4RSnorm,
            wgpu::TextureFormat::Bc5RgUnorm => Self::Bc5RgUnorm,
            wgpu::TextureFormat::Bc5RgSnorm => Self::Bc5RgSnorm,
            wgpu::TextureFormat::Bc6hRgbUfloat => Self::Bc6hRgbUfloat,
            wgpu::TextureFormat::Bc6hRgbFloat => Self::Bc6hRgbFloat,
            wgpu::TextureFormat::Bc7RgbaUnorm => Self::Bc7RgbaUnorm,
            wgpu::TextureFormat::Bc7RgbaUnormSrgb => Self::Bc7RgbaUnormSrgb,
            wgpu::TextureFormat::Etc2Rgb8Unorm => Self::Etc2Rgb8Unorm,
            wgpu::TextureFormat::Etc2Rgb8UnormSrgb => Self::Etc2Rgb8UnormSrgb,
            wgpu::TextureFormat::Etc2Rgb8A1Unorm => Self::Etc2Rgb8A1Unorm,
            wgpu::TextureFormat::Etc2Rgb8A1UnormSrgb => Self::Etc2Rgb8A1UnormSrgb,
            wgpu::TextureFormat::Etc2Rgba8Unorm => Self::Etc2Rgba8Unorm,
            wgpu::TextureFormat::Etc2Rgba8UnormSrgb => Self::Etc2Rgba8UnormSrgb,
            wgpu::TextureFormat::EacR11Unorm => Self::EacR11Unorm,
            wgpu::TextureFormat::EacR11Snorm => Self::EacR11Snorm,
            wgpu::TextureFormat::EacRg11Unorm => Self::EacRg11Unorm,
            wgpu::TextureFormat::EacRg11Snorm => Self::EacRg11Snorm,
            wgpu::TextureFormat::Astc { block, channel } => Self::Astc {
                block: AstcBlock::from_wgpu(block),
                channel: AstcChannel::from_wgpu(channel),
            },
        }
    }
}

impl WgpuType for TextureUsages {
    type Native = wgpu::TextureUsages;
    fn to_wgpu(self) -> Self::Native {
        let mut result = Self::Native::empty();
        if self.contains(Self::COPY_SRC) {
            result |= Self::Native::COPY_SRC;
        }
        if self.contains(Self::COPY_DST) {
            result |= Self::Native::COPY_DST;
        }
        if self.contains(Self::TEXTURE_BINDING) {
            result |= Self::Native::TEXTURE_BINDING;
        }
        if self.contains(Self::STORAGE_BINDING) {
            result |= Self::Native::STORAGE_BINDING;
        }
        if self.contains(Self::RENDER_ATTACHMENT) {
            result |= Self::Native::RENDER_ATTACHMENT;
        }
        if self.contains(Self::TRANSIENT_ATTACHMENT) {
            result |= Self::Native::TRANSIENT_ATTACHMENT;
        }
        if self.contains(Self::STORAGE_ATOMIC) {
            result |= Self::Native::STORAGE_ATOMIC;
        }
        result
    }
    fn from_wgpu(value: Self::Native) -> Self {
        let mut result = Self::empty();
        if value.contains(Self::Native::COPY_SRC) {
            result |= Self::COPY_SRC;
        }
        if value.contains(Self::Native::COPY_DST) {
            result |= Self::COPY_DST;
        }
        if value.contains(Self::Native::TEXTURE_BINDING) {
            result |= Self::TEXTURE_BINDING;
        }
        if value.contains(Self::Native::STORAGE_BINDING) {
            result |= Self::STORAGE_BINDING;
        }
        if value.contains(Self::Native::RENDER_ATTACHMENT) {
            result |= Self::RENDER_ATTACHMENT;
        }
        if value.contains(Self::Native::TRANSIENT_ATTACHMENT) {
            result |= Self::TRANSIENT_ATTACHMENT;
        }
        if value.contains(Self::Native::STORAGE_ATOMIC) {
            result |= Self::STORAGE_ATOMIC;
        }
        result
    }
}

impl WgpuType for BufferUsages {
    type Native = wgpu::BufferUsages;
    fn to_wgpu(self) -> Self::Native {
        let mut result = Self::Native::empty();
        if self.contains(Self::MAP_READ) {
            result |= Self::Native::MAP_READ;
        }
        if self.contains(Self::MAP_WRITE) {
            result |= Self::Native::MAP_WRITE;
        }
        if self.contains(Self::COPY_SRC) {
            result |= Self::Native::COPY_SRC;
        }
        if self.contains(Self::COPY_DST) {
            result |= Self::Native::COPY_DST;
        }
        if self.contains(Self::INDEX) {
            result |= Self::Native::INDEX;
        }
        if self.contains(Self::VERTEX) {
            result |= Self::Native::VERTEX;
        }
        if self.contains(Self::UNIFORM) {
            result |= Self::Native::UNIFORM;
        }
        if self.contains(Self::STORAGE) {
            result |= Self::Native::STORAGE;
        }
        if self.contains(Self::INDIRECT) {
            result |= Self::Native::INDIRECT;
        }
        if self.contains(Self::QUERY_RESOLVE) {
            result |= Self::Native::QUERY_RESOLVE;
        }
        if self.contains(Self::BLAS_INPUT) {
            result |= Self::Native::BLAS_INPUT;
        }
        if self.contains(Self::TLAS_INPUT) {
            result |= Self::Native::TLAS_INPUT;
        }
        result
    }
    fn from_wgpu(value: Self::Native) -> Self {
        let mut result = Self::empty();
        if value.contains(Self::Native::MAP_READ) {
            result |= Self::MAP_READ;
        }
        if value.contains(Self::Native::MAP_WRITE) {
            result |= Self::MAP_WRITE;
        }
        if value.contains(Self::Native::COPY_SRC) {
            result |= Self::COPY_SRC;
        }
        if value.contains(Self::Native::COPY_DST) {
            result |= Self::COPY_DST;
        }
        if value.contains(Self::Native::INDEX) {
            result |= Self::INDEX;
        }
        if value.contains(Self::Native::VERTEX) {
            result |= Self::VERTEX;
        }
        if value.contains(Self::Native::UNIFORM) {
            result |= Self::UNIFORM;
        }
        if value.contains(Self::Native::STORAGE) {
            result |= Self::STORAGE;
        }
        if value.contains(Self::Native::INDIRECT) {
            result |= Self::INDIRECT;
        }
        if value.contains(Self::Native::QUERY_RESOLVE) {
            result |= Self::QUERY_RESOLVE;
        }
        if value.contains(Self::Native::BLAS_INPUT) {
            result |= Self::BLAS_INPUT;
        }
        if value.contains(Self::Native::TLAS_INPUT) {
            result |= Self::TLAS_INPUT;
        }
        result
    }
}

impl WgpuType for Color {
    type Native = wgpu::Color;
    fn to_wgpu(self) -> Self::Native {
        Self::Native {
            r: self.r,
            g: self.g,
            b: self.b,
            a: self.a,
        }
    }
    fn from_wgpu(value: Self::Native) -> Self {
        Self {
            r: value.r,
            g: value.g,
            b: value.b,
            a: value.a,
        }
    }
}

impl WgpuType for BlendComponent {
    type Native = wgpu::BlendComponent;
    fn to_wgpu(self) -> Self::Native {
        Self::Native {
            src_factor: self.src_factor.to_wgpu(),
            dst_factor: self.dst_factor.to_wgpu(),
            operation: self.operation.to_wgpu(),
        }
    }
    fn from_wgpu(value: Self::Native) -> Self {
        Self {
            src_factor: BlendFactor::from_wgpu(value.src_factor),
            dst_factor: BlendFactor::from_wgpu(value.dst_factor),
            operation: BlendOperation::from_wgpu(value.operation),
        }
    }
}

impl WgpuType for BlendState {
    type Native = wgpu::BlendState;
    fn to_wgpu(self) -> Self::Native {
        Self::Native {
            color: self.color.to_wgpu(),
            alpha: self.alpha.to_wgpu(),
        }
    }
    fn from_wgpu(value: Self::Native) -> Self {
        Self {
            color: BlendComponent::from_wgpu(value.color),
            alpha: BlendComponent::from_wgpu(value.alpha),
        }
    }
}

/// Native storage shape of a texture resource.
pub fn texture_dimension(dimension: Dimension) -> wgpu::TextureDimension {
    match dimension {
        Dimension::D2 | Dimension::D2Array | Dimension::Cube => wgpu::TextureDimension::D2,
        Dimension::D3 => wgpu::TextureDimension::D3,
    }
}

/// Native whole-resource view shape.
pub fn view_dimension(dimension: Dimension) -> wgpu::TextureViewDimension {
    match dimension {
        Dimension::D2 => wgpu::TextureViewDimension::D2,
        Dimension::D2Array => wgpu::TextureViewDimension::D2Array,
        Dimension::Cube => wgpu::TextureViewDimension::Cube,
        Dimension::D3 => wgpu::TextureViewDimension::D3,
    }
}

/// Depth state of a neutral pass, if it uses a depth attachment.
pub fn depth_stencil(state: PassState) -> Option<wgpu::DepthStencilState> {
    state.depth_format.map(|format| wgpu::DepthStencilState {
        format: format.to_wgpu(),
        depth_write_enabled: Some(state.depth_write),
        depth_compare: Some(state.depth_compare.to_wgpu()),
        stencil: Default::default(),
        bias: Default::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_neutral_enum_round_trips_through_wgpu() {
        for value in [
            AstcBlock::B4x4,
            AstcBlock::B5x4,
            AstcBlock::B5x5,
            AstcBlock::B6x5,
            AstcBlock::B6x6,
            AstcBlock::B8x5,
            AstcBlock::B8x6,
            AstcBlock::B8x8,
            AstcBlock::B10x5,
            AstcBlock::B10x6,
            AstcBlock::B10x8,
            AstcBlock::B10x10,
            AstcBlock::B12x10,
            AstcBlock::B12x12,
        ] {
            assert_eq!(AstcBlock::from_wgpu(value.to_wgpu()), value);
        }
        for value in [AstcChannel::Unorm, AstcChannel::UnormSrgb, AstcChannel::Hdr] {
            assert_eq!(AstcChannel::from_wgpu(value.to_wgpu()), value);
        }
        for value in [Face::Front, Face::Back] {
            assert_eq!(Face::from_wgpu(value.to_wgpu()), value);
        }
        for value in [
            CompareFunction::Never,
            CompareFunction::Less,
            CompareFunction::Equal,
            CompareFunction::LessEqual,
            CompareFunction::Greater,
            CompareFunction::NotEqual,
            CompareFunction::GreaterEqual,
            CompareFunction::Always,
        ] {
            assert_eq!(CompareFunction::from_wgpu(value.to_wgpu()), value);
        }
        for value in [
            BlendFactor::Zero,
            BlendFactor::One,
            BlendFactor::Src,
            BlendFactor::OneMinusSrc,
            BlendFactor::SrcAlpha,
            BlendFactor::OneMinusSrcAlpha,
            BlendFactor::Dst,
            BlendFactor::OneMinusDst,
            BlendFactor::DstAlpha,
            BlendFactor::OneMinusDstAlpha,
            BlendFactor::SrcAlphaSaturated,
            BlendFactor::Constant,
            BlendFactor::OneMinusConstant,
            BlendFactor::Src1,
            BlendFactor::OneMinusSrc1,
            BlendFactor::Src1Alpha,
            BlendFactor::OneMinusSrc1Alpha,
        ] {
            assert_eq!(BlendFactor::from_wgpu(value.to_wgpu()), value);
        }
        for value in [
            BlendOperation::Add,
            BlendOperation::Subtract,
            BlendOperation::ReverseSubtract,
            BlendOperation::Min,
            BlendOperation::Max,
        ] {
            assert_eq!(BlendOperation::from_wgpu(value.to_wgpu()), value);
        }
        for value in [
            TextureFormat::R8Unorm,
            TextureFormat::R8Snorm,
            TextureFormat::R8Uint,
            TextureFormat::R8Sint,
            TextureFormat::R16Uint,
            TextureFormat::R16Sint,
            TextureFormat::R16Unorm,
            TextureFormat::R16Snorm,
            TextureFormat::R16Float,
            TextureFormat::Rg8Unorm,
            TextureFormat::Rg8Snorm,
            TextureFormat::Rg8Uint,
            TextureFormat::Rg8Sint,
            TextureFormat::R32Uint,
            TextureFormat::R32Sint,
            TextureFormat::R32Float,
            TextureFormat::Rg16Uint,
            TextureFormat::Rg16Sint,
            TextureFormat::Rg16Unorm,
            TextureFormat::Rg16Snorm,
            TextureFormat::Rg16Float,
            TextureFormat::Rgba8Unorm,
            TextureFormat::Rgba8UnormSrgb,
            TextureFormat::Rgba8Snorm,
            TextureFormat::Rgba8Uint,
            TextureFormat::Rgba8Sint,
            TextureFormat::Bgra8Unorm,
            TextureFormat::Bgra8UnormSrgb,
            TextureFormat::Rgb9e5Ufloat,
            TextureFormat::Rgb10a2Uint,
            TextureFormat::Rgb10a2Unorm,
            TextureFormat::Rg11b10Ufloat,
            TextureFormat::R64Uint,
            TextureFormat::Rg32Uint,
            TextureFormat::Rg32Sint,
            TextureFormat::Rg32Float,
            TextureFormat::Rgba16Uint,
            TextureFormat::Rgba16Sint,
            TextureFormat::Rgba16Unorm,
            TextureFormat::Rgba16Snorm,
            TextureFormat::Rgba16Float,
            TextureFormat::Rgba32Uint,
            TextureFormat::Rgba32Sint,
            TextureFormat::Rgba32Float,
            TextureFormat::Stencil8,
            TextureFormat::Depth16Unorm,
            TextureFormat::Depth24Plus,
            TextureFormat::Depth24PlusStencil8,
            TextureFormat::Depth32Float,
            TextureFormat::Depth32FloatStencil8,
            TextureFormat::NV12,
            TextureFormat::P010,
            TextureFormat::Bc1RgbaUnorm,
            TextureFormat::Bc1RgbaUnormSrgb,
            TextureFormat::Bc2RgbaUnorm,
            TextureFormat::Bc2RgbaUnormSrgb,
            TextureFormat::Bc3RgbaUnorm,
            TextureFormat::Bc3RgbaUnormSrgb,
            TextureFormat::Bc4RUnorm,
            TextureFormat::Bc4RSnorm,
            TextureFormat::Bc5RgUnorm,
            TextureFormat::Bc5RgSnorm,
            TextureFormat::Bc6hRgbUfloat,
            TextureFormat::Bc6hRgbFloat,
            TextureFormat::Bc7RgbaUnorm,
            TextureFormat::Bc7RgbaUnormSrgb,
            TextureFormat::Etc2Rgb8Unorm,
            TextureFormat::Etc2Rgb8UnormSrgb,
            TextureFormat::Etc2Rgb8A1Unorm,
            TextureFormat::Etc2Rgb8A1UnormSrgb,
            TextureFormat::Etc2Rgba8Unorm,
            TextureFormat::Etc2Rgba8UnormSrgb,
            TextureFormat::EacR11Unorm,
            TextureFormat::EacR11Snorm,
            TextureFormat::EacRg11Unorm,
            TextureFormat::EacRg11Snorm,
        ] {
            assert_eq!(TextureFormat::from_wgpu(value.to_wgpu()), value);
        }
        for block in [
            AstcBlock::B4x4,
            AstcBlock::B5x4,
            AstcBlock::B5x5,
            AstcBlock::B6x5,
            AstcBlock::B6x6,
            AstcBlock::B8x5,
            AstcBlock::B8x6,
            AstcBlock::B8x8,
            AstcBlock::B10x5,
            AstcBlock::B10x6,
            AstcBlock::B10x8,
            AstcBlock::B10x10,
            AstcBlock::B12x10,
            AstcBlock::B12x12,
        ] {
            for channel in [AstcChannel::Unorm, AstcChannel::UnormSrgb, AstcChannel::Hdr] {
                let format = TextureFormat::Astc { block, channel };
                assert_eq!(TextureFormat::from_wgpu(format.to_wgpu()), format);
            }
        }
    }

    #[test]
    fn usage_mappings_cover_all_native_flags_and_their_combinations() {
        assert_eq!(TextureUsages::all().to_wgpu(), wgpu::TextureUsages::all());
        for bits in 0..=TextureUsages::all().bits() {
            if let Some(value) = TextureUsages::from_bits(bits) {
                assert_eq!(TextureUsages::from_wgpu(value.to_wgpu()), value);
            }
        }
        assert_eq!(BufferUsages::all().to_wgpu(), wgpu::BufferUsages::all());
        for bits in 0..=BufferUsages::all().bits() {
            if let Some(value) = BufferUsages::from_bits(bits) {
                assert_eq!(BufferUsages::from_wgpu(value.to_wgpu()), value);
            }
        }
    }

    #[test]
    fn pass_depth_and_blend_states_keep_their_native_meaning() {
        assert!(depth_stencil(PassState::FULLSCREEN).is_none());
        let state = depth_stencil(PassState::OPAQUE).unwrap();
        assert_eq!(state.format, wgpu::TextureFormat::Depth32Float);
        assert_eq!(state.depth_compare, Some(wgpu::CompareFunction::Less));
        assert_eq!(state.depth_write_enabled, Some(true));
        assert_eq!(
            BlendState::ALPHA_BLENDING.to_wgpu(),
            wgpu::BlendState::ALPHA_BLENDING
        );
        assert_eq!(
            BlendState::PREMULTIPLIED_ALPHA_BLENDING.to_wgpu(),
            wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING
        );
    }
}

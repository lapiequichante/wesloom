//! The pixel probe (plan6 E2): what every resource a frame writes holds at
//! one point of the target.
//!
//! Generic over the pass list: no pass or view is special. During the
//! probed frame a copy is recorded after every pass that writes a probed
//! resource, so a transient whose memory a later pass reuses is read
//! before it is overwritten, and the copy after the last write wins.
//! - A texture is read at the same fraction of its own size, so a
//!   half-resolution bloom level reads the same point. Each array layer
//!   and cube face is read at that point, and a depth texture is copied
//!   whole, which is all a copy of one allows. Volumes are not read.
//! - A buffer has no point: its first [`BUFFER_BYTES`] bytes are read as
//!   words.
//!
//! The decoding is pure and tested without a device.

use crate::graph::{ResourcePool, Schedule};
use crate::pass::{Dimension, PassDesc, ResourceShape};
use crate::types::{TextureFormat, WgpuType};

/// One resource's value under the probe: a texel of one texture layer, or
/// the head of a buffer.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ProbedTexel {
    /// The resource's label.
    pub resource: String,
    /// Its format, or `None` for a buffer.
    pub format: Option<TextureFormat>,
    /// The texel read, in the resource's own pixels; `[0, 0]` for a buffer.
    pub texel: [u32; 2],
    /// The array layer or cube face read; 0 for a 2D texture and a buffer.
    pub layer: u32,
    /// The texture's size, or a buffer's `[bytes, 1]`.
    pub size: [u32; 2],
    /// The stored value, or `None` for a format the probe does not decode
    /// (block-compressed, packed shared-exponent, stencil). A buffer reads
    /// as [`TexelValue::Uint`] words.
    pub value: Option<TexelValue>,
}

/// A texel's channels, as stored: normalized formats read as floats, a
/// depth as its 0..1 value.
#[derive(Clone, Debug, PartialEq)]
pub enum TexelValue {
    /// Float, normalized and depth formats.
    Float(Vec<f32>),
    /// Unsigned integer formats, and buffer words.
    Uint(Vec<u32>),
    /// Signed integer formats.
    Sint(Vec<i32>),
}

/// How much of a buffer the probe reads.
pub const BUFFER_BYTES: u64 = 256;

/// The probe of one frame, while it records.
pub struct TexelProbe {
    at: [f32; 2],
    buffer: wgpu::Buffer,
    rows: Vec<Row>,
}

/// Where one resource layer's value lands in the readback buffer.
struct Row {
    resource: usize,
    label: String,
    /// `None` for a buffer.
    format: Option<TextureFormat>,
    layer: u32,
    offset: u64,
    texel: [u32; 2],
    size: [u32; 2],
    /// How many bytes were copied: a texel's, or a buffer's head.
    bytes: u32,
    written: bool,
    /// A depth texture, which a copy can only take whole: its own buffer,
    /// one padded row per texel row.
    whole: Option<(wgpu::Buffer, u32)>,
}

/// One row per resource layer: a copy's buffer offset has to be aligned,
/// and 256 covers every format's texel and a buffer's head.
const ROW: u64 = 256;

impl TexelProbe {
    /// Probe `at`, a fraction of the target (0..1, y down), over the
    /// textures and buffers of `resources`.
    pub(crate) fn new(
        device: &wgpu::Device,
        at: [f32; 2],
        resources: &[crate::pass::ResourceDesc],
    ) -> Self {
        let blank = |index: usize, label: &str| Row {
            resource: index,
            label: label.to_string(),
            format: None,
            layer: 0,
            offset: 0,
            texel: [0, 0],
            size: [0, 0],
            bytes: 0,
            written: false,
            whole: None,
        };
        let rows: Vec<Row> = resources
            .iter()
            .enumerate()
            .flat_map(|(index, desc)| match desc.shape {
                ResourceShape::Texture {
                    dimension: Dimension::D2 | Dimension::D2Array | Dimension::Cube,
                    format,
                    layers,
                    ..
                } if texel_bytes(format).is_some() => (0..layers.max(1))
                    .map(|layer| Row {
                        format: Some(format),
                        layer,
                        ..blank(index, &desc.label)
                    })
                    .collect(),
                ResourceShape::Buffer { size, .. } if size >= 4 => vec![Row {
                    size: [u32::try_from(size).unwrap_or(u32::MAX), 1],
                    bytes: (size.min(BUFFER_BYTES) & !3) as u32,
                    ..blank(index, &desc.label)
                }],
                _ => Vec::new(),
            })
            .enumerate()
            .map(|(index, row)| Row {
                offset: index as u64 * ROW,
                ..row
            })
            .collect();
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wxsl pixel probe"),
            size: (rows.len() as u64).max(1) * ROW,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        TexelProbe { at, buffer, rows }
    }

    /// Copy what `pass` wrote of every probed resource.
    pub(crate) fn after_pass(
        &mut self,
        device: &wgpu::Device,
        pass: &PassDesc,
        encoder: &mut wgpu::CommandEncoder,
        pool: &ResourcePool,
        schedule: &Schedule,
        frame: u64,
    ) {
        for id in pass.written() {
            for row in self
                .rows
                .iter_mut()
                .filter(|row| row.resource == id.index())
            {
                let Some(slot) = schedule.slot(id, frame, 0) else {
                    continue;
                };
                if row.format.is_none() {
                    let Some(buffer) = pool.buffer(slot) else {
                        continue;
                    };
                    encoder.copy_buffer_to_buffer(
                        buffer,
                        0,
                        &self.buffer,
                        row.offset,
                        u64::from(row.bytes),
                    );
                    row.written = true;
                    continue;
                }
                let Some(texture) = pool.texture(slot) else {
                    continue;
                };
                record_texel(device, encoder, &self.buffer, self.at, row, texture);
            }
        }
    }

    /// Wait for the frame and decode what it wrote, in resource order.
    pub(crate) fn read(self, device: &wgpu::Device) -> Vec<ProbedTexel> {
        let buffers = core::iter::once(&self.buffer).chain(
            self.rows
                .iter()
                .filter_map(|row| row.whole.as_ref().map(|(buffer, _)| buffer)),
        );
        for buffer in buffers {
            buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        }
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        let Ok(mapped) = self.buffer.slice(..).get_mapped_range() else {
            return Vec::new();
        };
        self.rows
            .iter()
            .filter(|row| row.written)
            .map(|row| {
                let bytes = row.bytes as usize;
                let value = match (&row.whole, row.format) {
                    (_, None) => {
                        let start = row.offset as usize;
                        Some(TexelValue::Uint(
                            mapped[start..start + bytes]
                                .chunks_exact(4)
                                .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
                                .collect(),
                        ))
                    }
                    (Some((buffer, padded)), Some(format)) => {
                        buffer.slice(..).get_mapped_range().ok().and_then(|whole| {
                            let start = row.texel[1] as usize * *padded as usize
                                + row.texel[0] as usize * bytes;
                            decode(format, whole.get(start..start + bytes)?)
                        })
                    }
                    (None, Some(format)) => {
                        let start = row.offset as usize;
                        decode(format, &mapped[start..start + bytes])
                    }
                };
                ProbedTexel {
                    resource: row.label.clone(),
                    format: row.format,
                    texel: row.texel,
                    layer: row.layer,
                    size: row.size,
                    value,
                }
            })
            .collect()
    }
}

/// Copy one texture layer's texel under `at` into the row: one texel into
/// the shared buffer, or a depth layer whole into its own.
fn record_texel(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    shared: &wgpu::Buffer,
    at: [f32; 2],
    row: &mut Row,
    texture: &wgpu::Texture,
) {
    let size = [texture.width(), texture.height()];
    let format = row.format.expect("a texture row");
    row.size = size;
    row.texel = texel_at(at, size);
    row.bytes = texel_bytes(format).unwrap_or(4);
    row.written = true;
    if texture.format().is_depth_stencil_format() {
        let padded = (row.bytes * size[0]).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wxsl pixel probe depth"),
            size: u64::from(padded) * u64::from(size[1]),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: row.layer,
                },
                aspect: wgpu::TextureAspect::DepthOnly,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(size[1]),
                },
            },
            wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
        );
        row.whole = Some((buffer, padded));
        return;
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: row.texel[0],
                y: row.texel[1],
                z: row.layer,
            },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: shared,
            layout: wgpu::TexelCopyBufferLayout {
                offset: row.offset,
                bytes_per_row: None,
                rows_per_image: None,
            },
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
}

/// The texel under `at` (a fraction of the target) in a texture of `size`.
pub fn texel_at(at: [f32; 2], size: [u32; 2]) -> [u32; 2] {
    core::array::from_fn(|axis| {
        let extent = size[axis].max(1);
        ((at[axis].clamp(0.0, 1.0) * extent as f32) as u32).min(extent - 1)
    })
}

/// How many bytes one texel of `format` takes in a copy, for the formats
/// [`decode`] reads.
pub fn texel_bytes(format: TextureFormat) -> Option<u32> {
    use wgpu::TextureFormat as F;
    Some(match format.to_wgpu() {
        F::R8Unorm | F::R8Snorm | F::R8Uint | F::R8Sint => 1,
        F::R16Uint | F::R16Sint | F::R16Unorm | F::R16Snorm | F::R16Float => 2,
        F::Rg8Unorm | F::Rg8Snorm | F::Rg8Uint | F::Rg8Sint | F::Depth16Unorm => 2,
        F::R32Uint | F::R32Sint | F::R32Float | F::Depth32Float => 4,
        F::Rg16Uint | F::Rg16Sint | F::Rg16Unorm | F::Rg16Snorm | F::Rg16Float => 4,
        F::Rgba8Unorm | F::Rgba8UnormSrgb | F::Rgba8Snorm | F::Rgba8Uint | F::Rgba8Sint => 4,
        F::Bgra8Unorm | F::Bgra8UnormSrgb | F::Rgb10a2Unorm | F::Rg11b10Ufloat => 4,
        F::Rg32Uint | F::Rg32Sint | F::Rg32Float => 8,
        F::Rgba16Uint | F::Rgba16Sint | F::Rgba16Unorm | F::Rgba16Snorm | F::Rgba16Float => 8,
        F::Rgba32Uint | F::Rgba32Sint | F::Rgba32Float => 16,
        _ => return None,
    })
}

/// Decode one texel of `format` from the bytes a copy wrote.
pub fn decode(format: TextureFormat, bytes: &[u8]) -> Option<TexelValue> {
    use wgpu::TextureFormat as F;
    if bytes.len() < texel_bytes(format)? as usize {
        return None;
    }
    let u16s = || {
        bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
    };
    let u32s = || {
        bytes
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
    };
    Some(match format.to_wgpu() {
        F::R8Unorm | F::Rg8Unorm | F::Rgba8Unorm | F::Rgba8UnormSrgb => {
            let count = texel_bytes(format)? as usize;
            TexelValue::Float(
                bytes[..count]
                    .iter()
                    .map(|&b| f32::from(b) / 255.0)
                    .collect(),
            )
        }
        F::Bgra8Unorm | F::Bgra8UnormSrgb => TexelValue::Float(
            [2, 1, 0, 3]
                .iter()
                .map(|&i| f32::from(bytes[i]) / 255.0)
                .collect(),
        ),
        F::R8Snorm | F::Rg8Snorm | F::Rgba8Snorm => {
            let count = texel_bytes(format)? as usize;
            TexelValue::Float(
                bytes[..count]
                    .iter()
                    .map(|&b| (f32::from(b as i8) / 127.0).max(-1.0))
                    .collect(),
            )
        }
        F::R8Uint | F::Rg8Uint | F::Rgba8Uint => {
            let count = texel_bytes(format)? as usize;
            TexelValue::Uint(bytes[..count].iter().map(|&b| u32::from(b)).collect())
        }
        F::R8Sint | F::Rg8Sint | F::Rgba8Sint => {
            let count = texel_bytes(format)? as usize;
            TexelValue::Sint(bytes[..count].iter().map(|&b| i32::from(b as i8)).collect())
        }
        F::R16Float | F::Rg16Float | F::Rgba16Float => TexelValue::Float(u16s().map(f16).collect()),
        F::R16Unorm | F::Rg16Unorm | F::Rgba16Unorm | F::Depth16Unorm => {
            TexelValue::Float(u16s().map(|v| f32::from(v) / 65535.0).collect())
        }
        F::R16Snorm | F::Rg16Snorm | F::Rgba16Snorm => TexelValue::Float(
            u16s()
                .map(|v| (f32::from(v as i16) / 32767.0).max(-1.0))
                .collect(),
        ),
        F::R16Uint | F::Rg16Uint | F::Rgba16Uint => {
            TexelValue::Uint(u16s().map(u32::from).collect())
        }
        F::R16Sint | F::Rg16Sint | F::Rgba16Sint => {
            TexelValue::Sint(u16s().map(|v| i32::from(v as i16)).collect())
        }
        F::R32Float | F::Rg32Float | F::Rgba32Float | F::Depth32Float => {
            TexelValue::Float(u32s().map(f32::from_bits).collect())
        }
        F::R32Uint | F::Rg32Uint | F::Rgba32Uint => TexelValue::Uint(u32s().collect()),
        F::R32Sint | F::Rg32Sint | F::Rgba32Sint => {
            TexelValue::Sint(u32s().map(|v| v as i32).collect())
        }
        F::Rgb10a2Unorm => {
            let v = u32s().next()?;
            TexelValue::Float(vec![
                (v & 0x3ff) as f32 / 1023.0,
                ((v >> 10) & 0x3ff) as f32 / 1023.0,
                ((v >> 20) & 0x3ff) as f32 / 1023.0,
                (v >> 30) as f32 / 3.0,
            ])
        }
        F::Rg11b10Ufloat => {
            let v = u32s().next()?;
            TexelValue::Float(vec![
                small_float(v & 0x7ff, 6),
                small_float((v >> 11) & 0x7ff, 6),
                small_float(v >> 22, 5),
            ])
        }
        _ => return None,
    })
}

/// An IEEE half-precision float.
fn f16(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = i32::from((bits >> 10) & 0x1f);
    let mantissa = f32::from(bits & 0x3ff);
    sign * match exponent {
        0 => mantissa * 2f32.powi(-24),
        31 if mantissa == 0.0 => f32::INFINITY,
        31 => f32::NAN,
        _ => (1.0 + mantissa / 1024.0) * 2f32.powi(exponent - 15),
    }
}

/// An unsigned float with a five-bit exponent and `mantissa` bits, as
/// `Rg11b10Ufloat` packs them.
fn small_float(bits: u32, mantissa: u32) -> f32 {
    let exponent = (bits >> mantissa) as i32;
    let fraction = (bits & ((1 << mantissa) - 1)) as f32 / (1u32 << mantissa) as f32;
    match exponent {
        0 => fraction * 2f32.powi(-14),
        31 => f32::INFINITY,
        _ => (1.0 + fraction) * 2f32.powi(exponent - 15),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_decoded_format_has_its_texel_size_and_decodes_known_bytes() {
        let half = |value: f32| {
            // 1.0 = 0x3c00, 0.5 = 0x3800, -2.0 = 0xc000.
            match value {
                1.0 => 0x3c00u16,
                0.5 => 0x3800,
                _ => 0xc000,
            }
            .to_le_bytes()
        };
        let mut rgba16 = Vec::new();
        for value in [1.0, 0.5, -2.0, 1.0] {
            rgba16.extend(half(value));
        }
        assert_eq!(
            decode(TextureFormat::Rgba16Float, &rgba16),
            Some(TexelValue::Float(vec![1.0, 0.5, -2.0, 1.0]))
        );
        assert_eq!(
            decode(TextureFormat::Rgba8Unorm, &[255, 0, 51, 255]),
            Some(TexelValue::Float(vec![1.0, 0.0, 0.2, 1.0]))
        );
        assert_eq!(
            decode(TextureFormat::Bgra8Unorm, &[0, 0, 255, 255]),
            Some(TexelValue::Float(vec![1.0, 0.0, 0.0, 1.0]))
        );
        assert_eq!(
            decode(TextureFormat::Depth32Float, &0.25f32.to_le_bytes()),
            Some(TexelValue::Float(vec![0.25]))
        );
        assert_eq!(
            decode(TextureFormat::R8Uint, &[7]),
            Some(TexelValue::Uint(vec![7]))
        );
        assert_eq!(
            decode(TextureFormat::Rg32Sint, &[255, 255, 255, 255, 2, 0, 0, 0]),
            Some(TexelValue::Sint(vec![-1, 2]))
        );
        // 1.0 in each channel: exponent 15, no fraction.
        let packed = (15u32 << 6) | ((15u32 << 6) << 11) | ((15u32 << 5) << 22);
        assert_eq!(
            decode(TextureFormat::Rg11b10Ufloat, &packed.to_le_bytes()),
            Some(TexelValue::Float(vec![1.0, 1.0, 1.0]))
        );
        // Too few bytes, and a format it does not read.
        assert_eq!(decode(TextureFormat::Rgba32Float, &[0; 4]), None);
        assert_eq!(texel_bytes(TextureFormat::Depth24Plus), None);
        assert_eq!(texel_bytes(TextureFormat::Bc1RgbaUnorm), None);
    }

    #[test]
    fn the_point_lands_at_the_same_fraction_of_any_size() {
        assert_eq!(texel_at([0.5, 0.5], [512, 512]), [256, 256]);
        assert_eq!(texel_at([0.5, 0.5], [256, 128]), [128, 64]);
        assert_eq!(texel_at([1.0, 1.0], [512, 512]), [511, 511]);
        assert_eq!(texel_at([-1.0, 2.0], [8, 8]), [0, 7]);
    }
}

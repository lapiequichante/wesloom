//! The flakes of the car's paint, generated on the GPU in place of the
//! 128² map me_car comes with, which repeats visibly: a compute shader
//! (`shaders/flakes.wgsl`) writes a random flake a texel and the normal
//! map's mip chain, as the renderer would build it from the pixels
//! (ADR 0094). The paint keeps the file's uv transform and sampler.

use wgpu::util::DeviceExt as _;
use wxsl::render::GpuContext;

/// The side of the map, texels.
pub const SIZE: u32 = 2048;
/// How far a flake leans, the most its normal's tangent part reaches: the
/// file's own map leans about as far, half of its texels past 0.35.
const TILT: f32 = 0.5;

/// Generate the map, `size`² (a power of two), every mip level written.
pub fn generate(gpu: &GpuContext, size: u32) -> wgpu::TextureView {
    let device = &gpu.device;
    let levels = size.ilog2() + 1;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("drive flakes"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("drive flakes"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/flakes.wgsl").into()),
    });
    let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty,
        count: None,
    };
    let storage = |read_only| wgpu::BindingType::Buffer {
        ty: wgpu::BufferBindingType::Storage { read_only },
        has_dynamic_offset: false,
        min_binding_size: None,
    };
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("drive flakes"),
        entries: &[
            entry(
                0,
                wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
            ),
            entry(
                1,
                wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
            ),
            entry(2, storage(true)),
            entry(3, storage(false)),
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("drive flakes"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = |entry_point| {
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("drive flakes"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some(entry_point),
            compilation_options: Default::default(),
            cache: None,
        })
    };
    let (full, reduce) = (pipeline("full"), pipeline("reduce"));

    // The means go to and fro between two buffers: odd levels in the
    // first, even ones in the second.
    let means = |side: u32| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("drive flake means"),
            size: u64::from(side.max(1) * side.max(1)) * 16,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        })
    };
    let means = [means(size / 2), means(size / 4)];

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("drive flakes"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("drive flakes"),
            timestamp_writes: None,
        });
        for level in 0..levels {
            let side = size >> level;
            let params = [side, level, TILT.to_bits(), 0];
            let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("drive flake level"),
                contents: &params.map(u32::to_le_bytes).concat(),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor {
                base_mip_level: level,
                mip_level_count: Some(1),
                ..Default::default()
            });
            let written = &means[(level as usize + 1) % 2];
            let read = &means[level as usize % 2];
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("drive flakes"),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: params.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: read.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: written.as_entire_binding(),
                    },
                ],
            });
            pass.set_pipeline(if level == 0 { &full } else { &reduce });
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(side.div_ceil(8), side.div_ceil(8), 1);
        }
    }
    gpu.queue.submit([encoder.finish()]);
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Level `level` of `view`'s texture, RGBA8.
    fn read(gpu: &GpuContext, view: &wgpu::TextureView, level: u32) -> Vec<u8> {
        let texture = view.texture();
        let side = texture.width() >> level;
        let row = (side * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(row * side),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: level,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: side,
                height: side,
                depth_or_array_layers: 1,
            },
        );
        gpu.queue.submit([encoder.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| ());
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        let mapped = buffer.slice(..).get_mapped_range().unwrap();
        mapped
            .chunks(row as usize)
            .flat_map(|line| line[..side as usize * 4].to_vec())
            .collect()
    }

    #[test]
    fn the_flakes_lean_within_their_tilt_and_their_mips_spread_as_the_renderer_would() {
        let Ok(gpu) = pollster::block_on(GpuContext::headless()) else {
            eprintln!("no adapter; skipped");
            return;
        };
        let size = 64;
        let view = generate(&gpu, size);
        let decode = |byte: u8| byte as f32 / 255.0 * 2.0 - 1.0;
        let mut sum = glam::Vec3::ZERO;
        for texel in read(&gpu, &view, 0).chunks(4) {
            let n = glam::Vec3::new(decode(texel[0]), decode(texel[1]), decode(texel[2]));
            assert!((n.length() - 1.0).abs() < 0.02, "{n}");
            assert!(n.truncate().length() < TILT + 0.01, "{n}");
            assert_eq!(texel[3], 0);
            sum += n;
        }
        // The last level covers every flake: the mean's direction and the
        // spread `normal_mip_chain` encodes for its length.
        let mean = sum / (size * size) as f32;
        let r = mean.length();
        let spread = (2.0 * (1.0 - r * r) / (r * (3.0 - r * r))).sqrt();
        let last = read(&gpu, &view, size.ilog2());
        let direction = mean / r;
        for (channel, value) in [direction.x, direction.y, direction.z]
            .into_iter()
            .enumerate()
        {
            assert!(
                (decode(last[channel]) - value).abs() < 0.02,
                "{last:?} {mean}"
            );
        }
        assert!(
            (last[3] as f32 / 255.0 - spread).abs() < 0.02,
            "{last:?} {spread}"
        );
        assert!(mean.truncate().length() < 0.05, "{mean}");
    }
}

//! Getting a `wgpu` device, and rendering without a window.
//!
//! [`GpuContext`] is a thin convenience over adapter and device requests —
//! nothing here is required to use the rest of the crate with a device you
//! obtained yourself. [`OffscreenTarget`] is more than convenience: it makes
//! the renderer testable, since it can render a frame and read the pixels
//! back with no surface, no window and no compositor.

use crate::error::RenderError;

/// A `wgpu` device and queue, with the adapter they came from.
pub struct GpuContext {
    /// The instance the adapter came from.
    pub instance: wgpu::Instance,
    /// The adapter in use. Keep it around: `AdapterInfo` is the first thing
    /// worth printing when a shader misbehaves on one machine only.
    pub adapter: wgpu::Adapter,
    /// The device.
    pub device: wgpu::Device,
    /// The queue.
    pub queue: wgpu::Queue,
}

impl GpuContext {
    /// Request an adapter and device from `instance`.
    ///
    /// Pass the surface that will be presented to, if any, so the adapter is
    /// guaranteed to be able to present to it.
    pub async fn new(
        instance: wgpu::Instance,
        compatible_surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Self, RenderError> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface,
                ..Default::default()
            })
            .await
            .map_err(|_| RenderError::NoAdapter)?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("wxsl"),
                ..Default::default()
            })
            .await
            .map_err(RenderError::NoDevice)?;
        Ok(GpuContext {
            instance,
            adapter,
            device,
            queue,
        })
    }

    /// A context with no surface, for offscreen rendering and tests.
    pub async fn headless() -> Result<Self, RenderError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        Self::new(instance, None).await
    }

    /// Block until the GPU has finished everything submitted so far.
    pub fn wait(&self) {
        // A poll error here means the device is lost, which every subsequent
        // call will report too; there is nothing useful to do about it at
        // this level.
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
    }
}

/// A texture to render into when there is no window, plus readback.
pub struct OffscreenTarget {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
}

impl OffscreenTarget {
    /// The format offscreen targets use.
    ///
    /// Plain `Rgba8Unorm`, not `Rgba8UnormSrgb`: the ABI's shading function
    /// encodes sRGB itself, so what lands in the texture is already display
    /// ready and can be written straight to a PNG.
    pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

    /// Create a `width` x `height` render target.
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wxsl offscreen target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: Self::FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        OffscreenTarget {
            texture,
            view,
            width,
            height,
            format: Self::FORMAT,
        }
    }

    /// The view to render into.
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The texture format.
    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Copy the rendered image back to the CPU as tightly packed RGBA8.
    ///
    /// Blocks until the GPU is done. Buffer rows have to be padded to
    /// `COPY_BYTES_PER_ROW_ALIGNMENT` for the copy, so the padding is
    /// stripped on the way out — callers get `width * height * 4` bytes.
    pub fn read_rgba8(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Vec<u8> {
        let unpadded_row = self.width * 4;
        let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_row = unpadded_row.div_ceil(alignment) * alignment;

        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wxsl readback"),
            size: u64::from(padded_row) * u64::from(self.height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("wxsl readback"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);

        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        let _ = device.poll(wgpu::PollType::wait_indefinitely());

        let mapped = buffer
            .slice(..)
            .get_mapped_range()
            .expect("readback buffer is mapped after a blocking poll");
        let mut pixels = Vec::with_capacity((unpadded_row * self.height) as usize);
        for row in mapped.chunks(padded_row as usize) {
            pixels.extend_from_slice(&row[..unpadded_row as usize]);
        }
        drop(mapped);
        buffer.unmap();
        pixels
    }
}

//! A dynamic texture atlas: one texture the whole UI samples from.
//!
//! Glyph distance fields and small images share one `Rgba8Unorm` texture, so
//! that a frame of node editor — thousands of glyphs, some icons, a few
//! panels — is one bind group and, clipping aside, one draw call. Without
//! that, each string would be its own pipeline state change.
//!
//! Packing is shelf-based: rectangles are placed left to right on rows whose
//! height is set by the first rectangle to open them. It wastes some space
//! next to a tall entry, and in exchange it is a dozen lines with no
//! rebalancing, no fragmentation bookkeeping and no allocation — which is the
//! right trade for a workload whose entries are all glyph-sized.
//!
//! The atlas does not grow. When it fills up, [`Atlas::insert_rgba8`] fails
//! with [`crate::RenderError::AtlasFull`] rather than silently dropping the
//! entry, and the caller decides: a UI can log it, a game can repack. See
//! [`Atlas::clear`] for the cheap way out.

use glam::Vec2;

use crate::error::RenderError;
use crate::ui::msdf::MsdfBitmap;

/// Where an entry ended up: its pixel rectangle, and the texture coordinates
/// that address it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AtlasRegion {
    /// Left edge in pixels.
    pub x: u32,
    /// Top edge in pixels.
    pub y: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Texture coordinate of the top-left corner.
    pub uv_min: Vec2,
    /// Texture coordinate of the bottom-right corner.
    pub uv_max: Vec2,
}

impl AtlasRegion {
    /// The sub-region `fraction` of the way into this one, as a 0..1 rect of
    /// this region — for a nine-slice, or for a glyph's inset.
    pub fn subregion(&self, min: Vec2, max: Vec2) -> AtlasRegion {
        let span = self.uv_max - self.uv_min;
        AtlasRegion {
            uv_min: self.uv_min + span * min,
            uv_max: self.uv_min + span * max,
            ..*self
        }
    }
}

/// One row of the shelf packer.
#[derive(Clone, Copy, Debug)]
struct Shelf {
    /// Top edge of the row.
    y: u32,
    /// Height of the row, fixed by the first entry placed on it.
    height: u32,
    /// How far along the row is used.
    used: u32,
}

/// The packing arithmetic, with no `wgpu` state in it.
///
/// Split out from [`Atlas`] so the allocator can be tested on a machine with
/// no adapter — which is most CI machines, and the reason ADR 0013 asks for
/// everything testable to be factored this way.
#[derive(Clone, Debug)]
struct ShelfPacker {
    size: u32,
    shelves: Vec<Shelf>,
    next_y: u32,
    used_pixels: u64,
    entries: usize,
}

impl ShelfPacker {
    fn new(size: u32) -> Self {
        ShelfPacker {
            size,
            shelves: Vec::new(),
            next_y: 0,
            used_pixels: 0,
            entries: 0,
        }
    }

    fn clear(&mut self) {
        self.shelves.clear();
        self.next_y = 0;
        self.used_pixels = 0;
        self.entries = 0;
    }

    fn occupancy(&self) -> f32 {
        self.used_pixels as f32 / (self.size as f32 * self.size as f32)
    }

    /// Find room for a `width` x `height` rectangle.
    ///
    /// Entries are padded by one pixel on the right and bottom so that
    /// bilinear sampling at an entry's edge cannot pick up its neighbour —
    /// the classic atlas bleed, and on a distance field it shows up as a
    /// stray mark rather than a soft edge.
    fn allocate(&mut self, width: u32, height: u32) -> Result<AtlasRegion, RenderError> {
        let padded_width = width + 1;
        let padded_height = height + 1;
        if padded_width > self.size || padded_height > self.size {
            return Err(RenderError::AtlasFull { width, height });
        }

        let mut placement = None;
        for shelf in &mut self.shelves {
            if padded_height <= shelf.height && shelf.used + padded_width <= self.size {
                placement = Some((shelf.used, shelf.y));
                shelf.used += padded_width;
                break;
            }
        }
        let (x, y) = match placement {
            Some(found) => found,
            None => {
                // Round a new shelf's height up, so that the next entry of a
                // similar size can share the row instead of opening another.
                let shelf_height = padded_height.next_multiple_of(4);
                if self.next_y + shelf_height > self.size {
                    return Err(RenderError::AtlasFull { width, height });
                }
                let y = self.next_y;
                self.next_y += shelf_height;
                self.shelves.push(Shelf {
                    y,
                    height: shelf_height,
                    used: padded_width,
                });
                (0, y)
            }
        };

        self.used_pixels += u64::from(padded_width) * u64::from(padded_height);
        self.entries += 1;
        let scale = 1.0 / self.size as f32;
        Ok(AtlasRegion {
            x,
            y,
            width,
            height,
            uv_min: Vec2::new(x as f32, y as f32) * scale,
            uv_max: Vec2::new((x + width) as f32, (y + height) as f32) * scale,
        })
    }
}

/// The glyph and image atlas.
pub struct Atlas {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    packer: ShelfPacker,
}

impl Atlas {
    /// The atlas format.
    ///
    /// `Rgba8Unorm` and not `Rgba8UnormSrgb`: the three channels of a
    /// distance field are distances, not colours, and gamma-decoding them
    /// would bend the field. Images that go in the atlas are therefore
    /// expected to be in the target's colour space already, which is the same
    /// rule the UI pass follows for vertex colours.
    pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

    /// A square atlas `size` pixels on a side.
    ///
    /// 2048 is a good default: it holds around fifteen hundred glyph fields at
    /// the sizes [`crate::ui::GlyphCache`] generates, and every WebGPU
    /// implementation supports it.
    pub fn new(device: &wgpu::Device, size: u32) -> Self {
        let size = size.max(64);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wxsl ui atlas"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: Self::FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Atlas {
            texture,
            view,
            packer: ShelfPacker::new(size),
        }
    }

    /// The view to bind for sampling.
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// The texture, for a caller that needs to bind it itself.
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    /// Side length in pixels.
    pub fn size(&self) -> u32 {
        self.packer.size
    }

    /// How many entries have been packed.
    pub fn len(&self) -> usize {
        self.packer.entries
    }

    /// Whether nothing has been packed yet.
    pub fn is_empty(&self) -> bool {
        self.packer.entries == 0
    }

    /// The fraction of the atlas's area that entries occupy, `0.0..=1.0`.
    ///
    /// Worth putting on a debug overlay: it is the only warning before
    /// [`crate::RenderError::AtlasFull`].
    pub fn occupancy(&self) -> f32 {
        self.packer.occupancy()
    }

    /// Forget every entry, so the space can be reused.
    ///
    /// Does not touch the texture's contents: whatever was there is simply no
    /// longer addressed by anything, and will be overwritten. Every
    /// [`AtlasRegion`] handed out before this call is now meaningless, so the
    /// caller must drop its own lookup tables at the same time — which is
    /// exactly what [`crate::ui::GlyphCache`] does when the MSDF backend
    /// changes and every glyph has to be regenerated.
    pub fn clear(&mut self) {
        self.packer.clear();
    }

    /// Pack a `width` x `height` RGBA8 image and upload it.
    ///
    /// `pixels` is tightly packed, `width * height * 4` bytes.
    pub fn insert_rgba8(
        &mut self,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        pixels: &[u8],
    ) -> Result<AtlasRegion, RenderError> {
        assert_eq!(
            pixels.len(),
            (width as usize) * (height as usize) * 4,
            "atlas entry is {width}x{height} but carries {} bytes",
            pixels.len()
        );
        let region = self.packer.allocate(width, height)?;
        if width > 0 && height > 0 {
            // Rows are padded to the copy alignment. `write_texture` would
            // accept a tight layout on most backends, but the padded one is
            // valid everywhere and a glyph upload is rare enough that the
            // copy costs nothing measurable.
            let unpadded = width * 4;
            let padded = unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
                * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let mut staged = vec![0u8; (padded * height) as usize];
            for (row, source) in pixels.chunks_exact(unpadded as usize).enumerate() {
                let start = row * padded as usize;
                staged[start..start + unpadded as usize].copy_from_slice(source);
            }
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: region.x,
                        y: region.y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &staged,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
        }
        Ok(region)
    }

    /// Pack a generated distance field, filling in the alpha the field does
    /// not carry.
    pub fn insert_msdf(
        &mut self,
        queue: &wgpu::Queue,
        bitmap: &MsdfBitmap,
    ) -> Result<AtlasRegion, RenderError> {
        let mut rgba = Vec::with_capacity(bitmap.pixels.len() / 3 * 4);
        for texel in bitmap.pixels.chunks_exact(3) {
            rgba.extend_from_slice(texel);
            rgba.push(255);
        }
        self.insert_rgba8(queue, bitmap.width, bitmap.height, &rgba)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_share_a_shelf_until_the_row_is_full() {
        let mut packer = ShelfPacker::new(64);
        let first = packer.allocate(20, 20).expect("room for the first");
        let second = packer.allocate(20, 20).expect("room for the second");
        assert_eq!(first.y, second.y, "both fit on the first shelf");
        assert!(second.x >= first.x + first.width, "and do not overlap");

        let third = packer.allocate(30, 20).expect("room for a third");
        assert!(third.y > first.y, "the shelf was full, so a new one opened");
    }

    #[test]
    fn uv_coordinates_address_the_pixels_that_were_packed() {
        let mut packer = ShelfPacker::new(128);
        let region = packer.allocate(32, 16).expect("room");
        let scale = 1.0 / 128.0;
        assert_eq!(
            region.uv_min,
            Vec2::new(region.x as f32, region.y as f32) * scale
        );
        assert_eq!(
            region.uv_max,
            Vec2::new((region.x + 32) as f32, (region.y + 16) as f32) * scale
        );
        // A subregion of a region stays inside it.
        let half = region.subregion(Vec2::ZERO, Vec2::splat(0.5));
        assert!(half.uv_max.x < region.uv_max.x);
        assert_eq!(half.uv_min, region.uv_min);
    }

    #[test]
    fn packed_entries_never_overlap() {
        // The property that matters, checked on a small atlas with mixed
        // sizes: a bleed between two glyphs is a visible mark on screen.
        let mut packer = ShelfPacker::new(256);
        let mut placed: Vec<AtlasRegion> = Vec::new();
        for step in 0..40u32 {
            let width = 7 + (step * 5) % 40;
            let height = 9 + (step * 3) % 25;
            let Ok(region) = packer.allocate(width, height) else {
                break;
            };
            for other in &placed {
                let disjoint = region.x + region.width <= other.x
                    || other.x + other.width <= region.x
                    || region.y + region.height <= other.y
                    || other.y + other.height <= region.y;
                assert!(disjoint, "{region:?} overlaps {other:?}");
            }
            assert!(region.x + region.width <= 256 && region.y + region.height <= 256);
            placed.push(region);
        }
        assert!(placed.len() > 10, "only {} entries packed", placed.len());
    }

    #[test]
    fn a_full_atlas_says_so_instead_of_overlapping_entries() {
        let mut packer = ShelfPacker::new(64);
        // Too big for the atlas at all.
        assert!(matches!(
            packer.allocate(100, 10),
            Err(RenderError::AtlasFull { .. })
        ));
        // And filling it up legitimately.
        let mut count = 0;
        while packer.allocate(30, 30).is_ok() {
            count += 1;
            assert!(count < 100, "the atlas never filled up");
        }
        assert!(count >= 2, "a 64px atlas holds at least two 30px entries");
        assert!(packer.occupancy() > 0.4);

        packer.clear();
        assert_eq!(packer.entries, 0);
        assert_eq!(packer.occupancy(), 0.0);
        assert!(packer.allocate(30, 30).is_ok(), "clearing frees the space");
    }
}

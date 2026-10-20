//! The colour a bounced ray picks up where it lands, from glTF's base
//! colour (factor times texture, sampled at the hit's UV) and metallic.
//! The diffuse part is the base colour times `1 - metallic`. The specular
//! part is the reflectance at normal incidence, `mix(0.04, base,
//! metallic)`, spread as if it were diffuse: low-order SH cannot carry
//! the lobe, but leaving it out makes a metal send nothing back.
//!
//! The same file says where a surface is cut out: an alpha-masked
//! material lets a ray through where its alpha is under the cutoff.

use std::path::Path;

use crate::PrtError;
use wxsl_gltf::ImportedScene;

/// Every material's diffuse colour, sampled on the CPU.
#[derive(Clone, Debug, Default)]
pub struct Albedo {
    /// Per document material.
    materials: Vec<Surface>,
    images: Vec<Option<Image>>,
    /// When no material is known: every surface this colour.
    fallback: [f32; 3],
    /// Whether the specular reflectance is added to the diffuse colour.
    specular: bool,
}

#[derive(Clone, Debug)]
struct Surface {
    base: [f32; 3],
    /// The base colour factor's alpha.
    alpha: f32,
    /// `alphaMode: MASK`'s cutoff.
    cutoff: Option<f32>,
    metallic: f32,
    base_texture: Option<TextureRef>,
    /// Metallic in the blue channel, as glTF lays it out.
    metallic_texture: Option<TextureRef>,
}

#[derive(Clone, Copy, Debug)]
struct TextureRef {
    image: usize,
    /// UV set 0 or 1.
    set: usize,
    /// Rows of rotation times scale, then the offset, as the importer
    /// applies `KHR_texture_transform`.
    rows: [[f32; 2]; 2],
    offset: [f32; 2],
    wrap: [Wrap; 2],
}

#[derive(Clone, Copy, Debug)]
enum Wrap {
    Repeat,
    Mirror,
    Clamp,
}

/// RGBA8 texels, row-major from the top.
#[derive(Clone, Debug)]
struct Image {
    width: usize,
    height: usize,
    texels: Vec<[u8; 4]>,
}

impl Albedo {
    /// Every surface the same diffuse colour.
    pub fn uniform(color: [f32; 3]) -> Self {
        Albedo {
            fallback: color,
            ..Albedo::default()
        }
    }

    /// Read the materials of the glTF file `imported` came from, through
    /// [`ImportedScene::material_sources`]. An image that cannot be
    /// decoded samples as white.
    pub fn load(path: impl AsRef<Path>, imported: &ImportedScene) -> Result<Self, PrtError> {
        let path = path.as_ref();
        let failed = |reason: String| PrtError(format!("{}: {reason}", path.display()));
        let gltf::Gltf { document, blob } =
            gltf::Gltf::open(path).map_err(|error| failed(error.to_string()))?;
        let buffers = gltf::import_buffers(&document, path.parent(), blob)
            .map_err(|error| failed(error.to_string()))?;
        let images = document
            .images()
            .map(|image| {
                gltf::image::Data::from_source(image.source(), path.parent(), &buffers)
                    .ok()
                    .and_then(decode)
            })
            .collect();
        let materials = imported
            .material_sources
            .iter()
            .map(|source| {
                let material = source.and_then(|index| document.materials().nth(index));
                match material {
                    Some(material) => surface(&material),
                    // The spec's default material: white, metallic 1.
                    None => Surface {
                        base: [1.0; 3],
                        alpha: 1.0,
                        cutoff: None,
                        metallic: 1.0,
                        base_texture: None,
                        metallic_texture: None,
                    },
                }
            })
            .collect();
        Ok(Albedo {
            materials,
            images,
            fallback: [1.0; 3],
            specular: true,
        })
    }

    /// Drop the metallic weighting and the specular part: each surface
    /// bounces its base colour, factor times texture, metals included.
    pub fn base_color_only(mut self) -> Self {
        for surface in &mut self.materials {
            surface.metallic = 0.0;
            surface.metallic_texture = None;
        }
        self.specular = false;
        self
    }

    /// Whether `material` is alpha-masked, so a hit on it must ask
    /// [`Albedo::covers`].
    pub(crate) fn masked(&self, material: usize) -> bool {
        self.materials
            .get(material)
            .is_some_and(|surface| surface.cutoff.is_some())
    }

    /// Whether `material` is there at the two UV sets: its alpha, factor
    /// times texture, reaches the mask's cutoff.
    pub(crate) fn covers(&self, material: usize, uv: [[f32; 2]; 2]) -> bool {
        let Some(surface) = self.materials.get(material) else {
            return true;
        };
        let Some(cutoff) = surface.cutoff else {
            return true;
        };
        let texture = self
            .texel(surface.base_texture, uv)
            .map_or(1.0, |texel| f32::from(texel[3]) / 255.0);
        surface.alpha * texture >= cutoff
    }

    /// The colour `material` sends back at the two UV sets: diffuse plus
    /// specular, as the module says.
    pub fn sample(&self, material: usize, uv: [[f32; 2]; 2]) -> [f32; 3] {
        let Some(surface) = self.materials.get(material) else {
            return self.fallback;
        };
        let mut color = surface.base;
        if let Some(texel) = self.texel(surface.base_texture, uv) {
            for (channel, value) in color.iter_mut().zip(texel) {
                *channel *= SRGB[value as usize];
            }
        }
        let mut metallic = surface.metallic;
        if let Some(texel) = self.texel(surface.metallic_texture, uv) {
            metallic *= f32::from(texel[2]) / 255.0;
        }
        let metallic = metallic.clamp(0.0, 1.0);
        let specular = f32::from(u8::from(self.specular));
        color.map(|channel| {
            channel * (1.0 - metallic) + specular * (0.04 + (channel - 0.04) * metallic)
        })
    }

    /// The nearest texel `texture` shows at `uv`.
    fn texel(&self, texture: Option<TextureRef>, uv: [[f32; 2]; 2]) -> Option<[u8; 4]> {
        let texture = texture?;
        let image = self.images.get(texture.image)?.as_ref()?;
        let [u, v] = uv[texture.set];
        let [row_x, row_y] = texture.rows;
        let mapped = [
            row_x[0] * u + row_x[1] * v + texture.offset[0],
            row_y[0] * u + row_y[1] * v + texture.offset[1],
        ];
        let column = wrap(mapped[0], image.width, texture.wrap[0]);
        let row = wrap(mapped[1], image.height, texture.wrap[1]);
        Some(image.texels[row * image.width + column])
    }
}

fn surface(material: &gltf::Material<'_>) -> Surface {
    let pbr = material.pbr_metallic_roughness();
    let [r, g, b, a] = pbr.base_color_factor();
    Surface {
        base: [r, g, b],
        alpha: a,
        cutoff: (material.alpha_mode() == gltf::material::AlphaMode::Mask)
            .then(|| material.alpha_cutoff().unwrap_or(0.5)),
        metallic: pbr.metallic_factor(),
        base_texture: pbr.base_color_texture().map(|info| texture_ref(&info)),
        metallic_texture: pbr
            .metallic_roughness_texture()
            .map(|info| texture_ref(&info)),
    }
}

fn texture_ref(info: &gltf::texture::Info<'_>) -> TextureRef {
    let mut set = info.tex_coord();
    let (mut rows, mut offset) = ([[1.0, 0.0], [0.0, 1.0]], [0.0, 0.0]);
    if let Some(transform) = info.texture_transform() {
        let (sin, cos) = transform.rotation().sin_cos();
        let [sx, sy] = transform.scale();
        rows = [[cos * sx, -sin * sy], [sin * sx, cos * sy]];
        offset = transform.offset();
        set = transform.tex_coord().unwrap_or(set);
    }
    let sampler = info.texture().sampler();
    let wrap = |mode: gltf::texture::WrappingMode| match mode {
        gltf::texture::WrappingMode::Repeat => Wrap::Repeat,
        gltf::texture::WrappingMode::MirroredRepeat => Wrap::Mirror,
        gltf::texture::WrappingMode::ClampToEdge => Wrap::Clamp,
    };
    TextureRef {
        image: info.texture().source().index(),
        set: set.min(1) as usize,
        rows,
        offset,
        wrap: [wrap(sampler.wrap_s()), wrap(sampler.wrap_t())],
    }
}

/// The texel index `coordinate` lands on along an axis of `size`.
fn wrap(coordinate: f32, size: usize, mode: Wrap) -> usize {
    let scaled = coordinate * size as f32;
    let index = match mode {
        Wrap::Repeat => scaled.floor().rem_euclid(size as f32),
        Wrap::Mirror => {
            let period = scaled.floor().rem_euclid(2.0 * size as f32);
            if period < size as f32 {
                period
            } else {
                2.0 * size as f32 - 1.0 - period
            }
        }
        Wrap::Clamp => scaled.floor(),
    };
    (index.max(0.0) as usize).min(size - 1)
}

/// Any 8- or 16-bit layout as RGBA8; float images are not textures glTF
/// colours come in.
fn decode(data: gltf::image::Data) -> Option<Image> {
    use gltf::image::Format;
    let (channels, wide) = match data.format {
        Format::R8 => (1, false),
        Format::R8G8 => (2, false),
        Format::R8G8B8 => (3, false),
        Format::R8G8B8A8 => (4, false),
        Format::R16 => (1, true),
        Format::R16G16 => (2, true),
        Format::R16G16B16 => (3, true),
        Format::R16G16B16A16 => (4, true),
        _ => return None,
    };
    let size = if wide { 2 } else { 1 };
    let texels = data
        .pixels
        .chunks_exact(channels * size)
        .map(|texel| {
            // The high byte of a 16-bit value, little-endian.
            let channel = |index: usize| texel[index * size + size - 1];
            match channels {
                1 => [channel(0), channel(0), channel(0), 255],
                2 => [channel(0), channel(0), channel(0), channel(1)],
                3 => [channel(0), channel(1), channel(2), 255],
                _ => [channel(0), channel(1), channel(2), channel(3)],
            }
        })
        .collect();
    Some(Image {
        width: data.width as usize,
        height: data.height as usize,
        texels,
    })
}

/// sRGB-encoded bytes to linear.
static SRGB: std::sync::LazyLock<[f32; 256]> = std::sync::LazyLock::new(|| {
    std::array::from_fn(|byte| {
        let encoded = byte as f32 / 255.0;
        if encoded <= 0.04045 {
            encoded / 12.92
        } else {
            ((encoded + 0.055) / 1.055).powf(2.4)
        }
    })
});

#[cfg(test)]
mod tests {
    use super::*;

    fn checker() -> Albedo {
        // 2×1: red then blue, sampled through a texture repeated twice.
        Albedo {
            materials: vec![Surface {
                base: [1.0, 1.0, 0.5],
                alpha: 1.0,
                cutoff: None,
                metallic: 0.0,
                base_texture: Some(TextureRef {
                    image: 0,
                    set: 1,
                    rows: [[2.0, 0.0], [0.0, 1.0]],
                    offset: [0.0, 0.0],
                    wrap: [Wrap::Repeat, Wrap::Clamp],
                }),
                metallic_texture: None,
            }],
            images: vec![Some(Image {
                width: 2,
                height: 1,
                texels: vec![[255, 0, 0, 255], [0, 0, 255, 0]],
            })],
            fallback: [0.0; 3],
            specular: false,
        }
    }

    #[test]
    fn a_texture_is_sampled_at_the_uv_set_and_transform_it_names() {
        let albedo = checker();
        let sample = |u: f32| albedo.sample(0, [[0.9, 0.9], [u, 0.5]]);
        // u 0.1 → 0.2 of the texture: red; 0.3 → 0.6: blue; 0.6 → 1.2,
        // repeated to 0.2: red again.
        assert_eq!(sample(0.1), [1.0, 0.0, 0.0]);
        assert_eq!(sample(0.3), [0.0, 0.0, 0.5]);
        assert_eq!(sample(0.6), [1.0, 0.0, 0.0]);
    }

    #[test]
    fn metal_has_no_diffuse_colour_and_srgb_bytes_decode_linear() {
        let mut albedo = checker();
        albedo.materials[0].metallic = 0.75;
        assert_eq!(albedo.sample(0, [[0.0; 2], [0.1, 0.5]]), [0.25, 0.0, 0.0]);
        let albedo = albedo.base_color_only();
        assert_eq!(albedo.sample(0, [[0.0; 2], [0.1, 0.5]]), [1.0, 0.0, 0.0]);
        assert!((SRGB[128] - 0.2158).abs() < 1e-3);
        assert_eq!(Albedo::uniform([0.5; 3]).sample(3, [[0.0; 2]; 2]), [0.5; 3]);
        assert_eq!(wrap(-0.25, 4, Wrap::Mirror), 0);
        assert_eq!(wrap(1.1, 4, Wrap::Mirror), 3);
        assert_eq!(wrap(-3.0, 4, Wrap::Clamp), 0);
    }

    #[test]
    fn a_metal_bounces_its_reflectance_and_a_mask_cuts_where_alpha_is_low() {
        let mut albedo = checker();
        albedo.specular = true;
        albedo.materials[0].metallic = 1.0;
        assert_eq!(albedo.sample(0, [[0.0; 2], [0.1, 0.5]]), [1.0, 0.0, 0.0]);
        albedo.materials[0].metallic = 0.0;
        let dielectric = albedo.sample(0, [[0.0; 2], [0.1, 0.5]]);
        assert!(dielectric
            .iter()
            .zip([1.04, 0.04, 0.04])
            .all(|(a, b)| (a - b).abs() < 1e-6));

        // Red is opaque, blue's alpha is 0.
        assert!(!albedo.masked(0) && albedo.covers(0, [[0.0; 2], [0.3, 0.5]]));
        albedo.materials[0].cutoff = Some(0.5);
        assert!(albedo.masked(0));
        assert!(albedo.covers(0, [[0.0; 2], [0.1, 0.5]]));
        assert!(!albedo.covers(0, [[0.0; 2], [0.3, 0.5]]));
    }
}

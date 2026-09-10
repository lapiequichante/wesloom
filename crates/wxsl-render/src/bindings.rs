//! The `wgpu` side of what a material declares: [`MaterialBindings`] fills
//! group 1, and [`BindingLayouts`] hands out the layouts for groups 1 and
//! 2.
//!
//! `wxsl-core` computes a [`MaterialInterface`] from the graph — which
//! parameters, at which offsets, and which textures at which bindings — and
//! everything here works *through* that table rather than beside it. There
//! is no `#[repr(C)]` mirror to keep in step, because there is nothing to
//! mirror: the offsets exist in exactly one place, and both the generated
//! WGSL and the bytes written here come from it
//! ([ADR 0023](../../../docs/adr/0023-a-material-declares-its-resources.md)).
//!
//! # Group 2 is not filled here
//!
//! A material may *declare* the block it expects in the application's
//! group, and [`BindingLayouts::layouts`] hands out the matching
//! `BindGroupLayout`. What goes in it is the application's, and so is the
//! bind group: it comes in on [`crate::draw::DrawItem::user`]. `wgpu`
//! validates that the two agree, so there is no checking code here to
//! drift.

use std::collections::BTreeMap;
use std::collections::HashMap;

use wxsl_core::abi;
use wxsl_core::node::{Value, ValueType};
use wxsl_core::resources::MaterialInterface;

use crate::error::RenderError;
use crate::material::Material;

/// One material's group-1 resources: its parameter buffer, its textures
/// and its samplers.
///
/// Owned by the application, not by the renderer, and handed to a draw
/// through [`crate::draw::DrawItem::with_bindings`] — the same arrangement
/// as meshes and materials, and for the same reason: two objects sharing a
/// material with different textures is ordinary, and the renderer has no
/// business deciding which.
///
/// Editing a parameter is [`MaterialBindings::set`] plus
/// [`MaterialBindings::upload`], which is a buffer write and nothing else.
/// No variant is compiled, no pipeline is built, and `cache_stats()` does
/// not move — which is the point of a `param` node existing at all.
pub struct MaterialBindings {
    interface: MaterialInterface,
    signature: String,
    layout: wgpu::BindGroupLayout,
    /// The host-side copy of the parameter buffer, written through the
    /// computed layout. Empty when the material declares no parameters.
    data: Vec<u8>,
    buffer: Option<wgpu::Buffer>,
    textures: BTreeMap<String, wgpu::TextureView>,
    samplers: BTreeMap<String, wgpu::Sampler>,
    bind_group: Option<wgpu::BindGroup>,
    /// Parameters changed since the last upload.
    dirty: bool,
    /// A texture or sampler changed, so the bind group itself is stale.
    stale: bool,
}

impl MaterialBindings {
    /// Create the buffer and the empty resource table for `interface`.
    ///
    /// The parameter buffer starts at the values the graph's `param` nodes
    /// declared, so a material draws as authored before the application
    /// has set anything. Textures and samplers start unbound, and
    /// [`MaterialBindings::upload`] says so by name until they are not.
    ///
    /// `layout` must be the one [`BindingLayouts`] answers for the same
    /// interface; [`crate::renderer::Renderer::material_bindings`] is the
    /// call that gets that right for you.
    pub fn new(
        device: &wgpu::Device,
        interface: &MaterialInterface,
        layout: &wgpu::BindGroupLayout,
    ) -> Self {
        let data = interface.params.filled(&interface.defaults);
        let buffer = (!data.is_empty()).then(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("wxsl material parameters"),
                size: data.len() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        MaterialBindings {
            signature: interface.signature(),
            interface: interface.clone(),
            layout: layout.clone(),
            data,
            buffer,
            textures: BTreeMap::new(),
            samplers: BTreeMap::new(),
            bind_group: None,
            dirty: true,
            stale: true,
        }
    }

    /// The interface these bindings were built for.
    pub fn interface(&self) -> &MaterialInterface {
        &self.interface
    }

    /// The shape these bindings satisfy — what a pipeline built to draw
    /// with them was laid out for.
    pub fn signature(&self) -> &str {
        &self.signature
    }

    /// Set parameter `name`.
    ///
    /// Writes into the host copy at the computed offset; the GPU sees it
    /// at the next [`MaterialBindings::upload`]. Wrong name or wrong type
    /// is an error naming both, rather than bytes at the wrong offset.
    pub fn set(&mut self, name: &str, value: Value) -> Result<(), RenderError> {
        self.interface
            .params
            .write(&mut self.data, name, value)
            .map_err(|reason| RenderError::MaterialParameter {
                name: name.to_string(),
                reason: reason.to_string(),
            })?;
        self.dirty = true;
        Ok(())
    }

    /// Read a parameter's current host-side value back.
    ///
    /// Through the same offsets it was written at, so this is a real
    /// round-trip rather than a remembered copy — which is what makes it
    /// worth having in a test.
    pub fn get(&self, name: &str) -> Option<Value> {
        self.interface.params.read(&self.data, name)
    }

    /// Bind a texture to a declared `texture.*` node's name.
    pub fn set_texture(&mut self, name: &str, view: &wgpu::TextureView) -> Result<(), RenderError> {
        self.expect(name, |ty| ty != ValueType::Sampler)?;
        self.textures.insert(name.to_string(), view.clone());
        self.stale = true;
        Ok(())
    }

    /// Bind a sampler to a declared `texture.sampler` node's name.
    pub fn set_sampler(&mut self, name: &str, sampler: &wgpu::Sampler) -> Result<(), RenderError> {
        self.expect(name, |ty| ty == ValueType::Sampler)?;
        self.samplers.insert(name.to_string(), sampler.clone());
        self.stale = true;
        Ok(())
    }

    fn expect(&self, name: &str, wanted: impl Fn(ValueType) -> bool) -> Result<(), RenderError> {
        match self.interface.resource(name) {
            Some(entry) if wanted(entry.ty) => Ok(()),
            Some(entry) => Err(RenderError::MaterialResourceKind {
                name: name.to_string(),
                declared: entry.ty.to_string(),
            }),
            None => Err(RenderError::UndeclaredMaterialResource {
                name: name.to_string(),
            }),
        }
    }

    /// Push whatever changed to the GPU, building the bind group the first
    /// time everything is there.
    ///
    /// Cheap and idempotent, so calling it every frame is fine: with
    /// nothing changed it does nothing at all.
    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<(), RenderError> {
        if let (true, Some(buffer)) = (self.dirty, self.buffer.as_ref()) {
            queue.write_buffer(buffer, 0, &self.data);
            self.dirty = false;
        }
        if !self.stale {
            return Ok(());
        }
        // A bind group has no holes: every declared texture and sampler
        // has to be there before there is anything to build. Named, so
        // "the cube is black" is "you never bound `albedo`" rather than a
        // hunt.
        for resource in &self.interface.resources {
            let name = resource.name.as_str();
            let bound = match resource.ty {
                ValueType::Sampler => self.samplers.contains_key(name),
                _ => self.textures.contains_key(name),
            };
            if !bound {
                return Err(RenderError::UnboundMaterialResource {
                    name: name.to_string(),
                    kind: resource.ty.to_string(),
                });
            }
        }
        let mut entries: Vec<wgpu::BindGroupEntry> = Vec::new();
        if let Some(buffer) = self.buffer.as_ref() {
            entries.push(wgpu::BindGroupEntry {
                binding: abi::BINDING_MATERIAL_PARAMS,
                resource: buffer.as_entire_binding(),
            });
        }
        for resource in &self.interface.resources {
            let name = resource.name.as_str();
            entries.push(wgpu::BindGroupEntry {
                binding: resource.binding,
                resource: match resource.ty {
                    ValueType::Sampler => wgpu::BindingResource::Sampler(&self.samplers[name]),
                    _ => wgpu::BindingResource::TextureView(&self.textures[name]),
                },
            });
        }
        self.bind_group = (!entries.is_empty()).then(|| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("wxsl material"),
                layout: &self.layout,
                entries: &entries,
            })
        });
        self.stale = false;
        Ok(())
    }

    /// The bind group, or `None` before the first successful
    /// [`MaterialBindings::upload`] — and for a material whose group is
    /// empty, which needs none.
    pub fn bind_group(&self) -> Option<&wgpu::BindGroup> {
        self.bind_group.as_ref()
    }
}

/// The bind group layouts for groups 1 and 2, cached by interface shape.
///
/// Keyed on [`MaterialInterface::signature`] rather than on the material,
/// because two materials declaring the same parameters and textures are
/// interchangeable to `wgpu`: they share a layout, and therefore a pipeline
/// layout, and therefore a pipeline. The signature is a *shape* and never
/// a value, so changing a parameter cannot land in a different bucket.
#[derive(Default)]
pub struct BindingLayouts {
    entries: HashMap<String, GroupLayouts>,
}

/// The two layouts a material's interface implies. Either may be absent:
/// a material with no parameters and no textures needs no group 1, and one
/// declaring no application block needs no group 2.
#[derive(Clone, Default)]
pub struct GroupLayouts {
    /// `abi::GROUP_MATERIAL`.
    pub material: Option<wgpu::BindGroupLayout>,
    /// `abi::GROUP_USER`.
    pub user: Option<wgpu::BindGroupLayout>,
}

impl BindingLayouts {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// The layouts for `interface`, created on first use.
    pub fn layouts(
        &mut self,
        device: &wgpu::Device,
        interface: &MaterialInterface,
    ) -> &GroupLayouts {
        self.entries
            .entry(interface.signature())
            .or_insert_with(|| GroupLayouts {
                material: (!interface.material_group_is_empty())
                    .then(|| material_layout(device, interface)),
                user: interface.user.as_ref().map(|_| user_layout(device)),
            })
    }

    /// How many distinct shapes have been seen.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing has been cached.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The layout of `abi::GROUP_MATERIAL` for one interface.
///
/// Visible to both stages: a parameter can drive a vertex offset (M5) as
/// easily as a colour, and there is nothing to gain by finding that out
/// as a validation error later.
fn material_layout(device: &wgpu::Device, interface: &MaterialInterface) -> wgpu::BindGroupLayout {
    let mut entries: Vec<wgpu::BindGroupLayoutEntry> = Vec::new();
    if !interface.params.is_empty() {
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: abi::BINDING_MATERIAL_PARAMS,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        });
    }
    for resource in &interface.resources {
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: resource.binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: match resource.ty {
                ValueType::Sampler => {
                    wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)
                }
                ValueType::TextureCube => wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::Cube,
                    multisampled: false,
                },
                _ => wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
            },
            count: None,
        });
    }
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("wxsl material"),
        entries: &entries,
    })
}

/// The layout of `abi::GROUP_USER`: one uniform buffer, and nothing this
/// crate knows the contents of.
///
/// The material states the *shape* of the block in its generated WGSL, and
/// the application binds a buffer against this layout. `wgpu` checks the
/// two agree — which is why there is no checking code here, and why a
/// mismatch is a validation error naming the binding rather than a wrong
/// picture.
fn user_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("wxsl user"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: abi::BINDING_USER_BLOCK,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

/// The size, in bytes, of the buffer an application must bind for
/// `material`'s declared block — or `None` if it declares none.
///
/// The application owns the contents and this crate never reads them, but
/// it does know how big they are, because it computed the layout. Handing
/// that out is what lets an application create the buffer without
/// re-deriving offsets of its own.
pub fn user_block_size(material: &Material) -> Option<u64> {
    material
        .interface()
        .user
        .as_ref()
        .map(|block| u64::from(block.layout.size()))
}

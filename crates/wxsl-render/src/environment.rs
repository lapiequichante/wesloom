//! wgpu buffers and bindings for shared environment data (ADR 0048).

use crate::pass::PassView;
use std::collections::BTreeMap;
use wxsl_core::abi;

pub use wxsl_frame::environment::*;

// Capacity is backend allocation policy, not a frame-layout constraint.
const INITIAL_INSTANCE_CAPACITY: usize = 64;

/// The buffers and bind group for the frame group ([`abi::GROUP_FRAME`]).
///
/// One instance is shared by every pass in a frame: the bindings are the
/// same for forward, for the deferred material pass and for the deferred
/// lighting pass, so the layout is created once and reused.
///
/// # Why the transforms are a storage buffer
///
/// ADR 0010 put the object transform in this group as a uniform, to be
/// addressed by a dynamic offset "when a multi-draw scene comes". It came,
/// and a dynamic offset lost: one storage buffer is one binding and one
/// upload for the whole frame, indexed in the shader by
/// `@builtin(instance_index)`, and it is the shape a culling pass can write
/// indices into later. The one capability it costs is
/// `DownlevelFlags::VERTEX_STORAGE`, which the WebGPU baseline satisfies
/// and WebGL does not — and WebGL is not a target
/// ([ADR 0021](../../../docs/adr/0021-a-declarative-render-graph-and-a-scene-document.md)).
pub struct FrameBindings {
    /// Every point of view of the frame, one [`CameraUniform`] each at
    /// [`FrameBindings::view_stride`] apart, addressed by a dynamic
    /// offset. The camera is first; see [`PassView`].
    views: wgpu::Buffer,
    view_stride: u32,
    scene: wgpu::Buffer,
    instances: wgpu::Buffer,
    /// The same rows as last frame had them (`abi::BINDING_PREVIOUS_INSTANCES`)
    /// — the velocity stage's second transform, filled beside the current
    /// ones and grown with them.
    previous_instances: wgpu::Buffer,
    capacity: usize,
    /// The shadow maps currently bound, and where they came from — the
    /// pool's generation and slot — so that rebinding the same texture is
    /// free. `None` while the placeholder is bound.
    shadow_source: Option<(u64, usize)>,
    shadow_view: wgpu::TextureView,
    /// What [`ShadowMaps::Detached`] binds instead.
    shadow_placeholder: wgpu::TextureView,
    shadow_sampler: wgpu::Sampler,
    /// Kept alive because the views above may be of it.
    _shadow_fallback: wgpu::Texture,
    /// The environment-BRDF table, baked once into this texture by the
    /// `brdf_lut` compute effect and read by every `ambient_environment`
    /// call (ADR 0039). Owned here rather than by a pass list, because it
    /// is frame-group infrastructure like the shadow maps are.
    environment_lut: wgpu::Texture,
    environment_lut_view: wgpu::TextureView,
    environment_lut_sampler: wgpu::Sampler,
    /// Whether the bake has run. It runs before the first pass of the
    /// first frame, so nothing ever samples the table unwritten — the
    /// flag is what keeps it from running a second time.
    environment_lut_baked: bool,
    /// One buffer and bind group per *declared attribute* row shape,
    /// keyed by [`BufferLayout::signature`]. The empty shape — a material
    /// declaring none — is always present and is what a pass with no
    /// material behind it binds.
    groups: BTreeMap<String, InstanceGroup>,
    layout: wgpu::BindGroupLayout,
}

/// One attribute row shape's buffer and frame bind groups.
struct InstanceGroup {
    buffer: wgpu::Buffer,
    /// In rows, not bytes.
    capacity: usize,
    stride: usize,
    bound: wgpu::BindGroup,
    detached: wgpu::BindGroup,
}

/// Whether a frame bind group points at the real shadow maps.
///
/// Two groups per shape rather than one, for a `wgpu` rule with no way
/// around it: a texture may not be sampled and written in the same pass,
/// and a shadow pass has the frame group bound while drawing into the very
/// texture its binding 4 points at. The shader never reads it — a shadow
/// stage compiles no shading — but the usage tracker works on bind groups,
/// not on what the shader does with them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShadowMaps {
    /// The maps this frame rendered. What every pass that shades binds.
    Bound,
    /// A one-texel placeholder. What a pass *writing* the maps binds.
    Detached,
}

impl FrameBindings {
    /// Create the buffers, layout and bind group.
    pub fn new(device: &wgpu::Device) -> Self {
        // One uniform per view, each at an offset the hardware will accept
        // as a dynamic one.
        let alignment = device.limits().min_uniform_buffer_offset_alignment;
        let size = size_of::<CameraUniform>() as u32;
        let view_stride = size.div_ceil(alignment.max(1)) * alignment.max(1);
        let views = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wxsl views"),
            size: u64::from(view_stride) * PassView::COUNT as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let scene = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wxsl scene"),
            size: size_of::<SceneUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let capacity = INITIAL_INSTANCE_CAPACITY;
        let instances = instance_buffer(
            device,
            "wxsl instances",
            size_of::<InstanceTransform>(),
            capacity,
        );
        let previous_instances = instance_buffer(
            device,
            "wxsl previous instances",
            size_of::<InstanceTransform>(),
            capacity,
        );

        // A one-texel slice per light, cleared to nothing and never
        // rendered into: a pipeline with no shadow passes still declares
        // the bindings, and an unfilled binding is a validation error
        // rather than a black frame.
        let shadow_fallback = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wxsl shadow fallback"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: abi::MAX_LIGHTS as u32,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let shadow_placeholder = shadow_fallback.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        // Until a pipeline with shadow passes hands its texture over, the
        // placeholder is what everything reads: an empty depth map is a
        // fully lit scene.
        let shadow_view = shadow_placeholder.clone();
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("wxsl shadow sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            // `Less`: a fragment nearer than what the light saw is lit, so
            // the comparison returns 1 where the sample survives.
            compare: Some(wgpu::CompareFunction::Less),
            ..Default::default()
        });

        let environment_lut = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wxsl environment brdf lut"),
            size: wgpu::Extent3d {
                width: abi::ENVIRONMENT_LUT_SIZE,
                height: abi::ENVIRONMENT_LUT_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // The format the bake's `texture_storage_2d` declares.
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let environment_lut_view =
            environment_lut.create_view(&wgpu::TextureViewDescriptor::default());
        let environment_lut_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("wxsl environment lut sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let buffer = |binding: u32, storage: bool, dynamic: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: if storage {
                    wgpu::BufferBindingType::Storage { read_only: true }
                } else {
                    wgpu::BufferBindingType::Uniform
                },
                has_dynamic_offset: dynamic,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wxsl frame bindings"),
            entries: &[
                // Dynamic, because which point of view a pass renders from
                // is the pass's business and the shader's `camera` is
                // whichever one it named. A pipeline with no shadow passes
                // binds offset zero and pays nothing.
                buffer(abi::BINDING_CAMERA, false, true),
                buffer(abi::BINDING_SCENE, false, false),
                buffer(abi::BINDING_INSTANCES, true, false),
                // The velocity stage's other half: vertex-only, because
                // nothing in a fragment stage has a reason to know where
                // last frame's vertices were.
                wgpu::BindGroupLayoutEntry {
                    binding: abi::BINDING_PREVIOUS_INSTANCES,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Always here, whether or not the material being drawn
                // declares anything: a frame group whose shape changed
                // per material would invalidate every pipeline layout
                // built against it.
                buffer(abi::BINDING_INSTANCE_ATTRIBUTES, true, false),
                wgpu::BindGroupLayoutEntry {
                    binding: abi::BINDING_SHADOW_MAPS,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: abi::BINDING_SHADOW_SAMPLER,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: abi::BINDING_ENVIRONMENT_LUT,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: abi::BINDING_ENVIRONMENT_SAMPLER,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let mut bindings = FrameBindings {
            views,
            view_stride,
            scene,
            instances,
            previous_instances,
            capacity,
            shadow_source: None,
            shadow_view,
            shadow_placeholder,
            shadow_sampler,
            _shadow_fallback: shadow_fallback,
            environment_lut,
            environment_lut_view,
            environment_lut_sampler,
            environment_lut_baked: false,
            groups: BTreeMap::new(),
            layout,
        };
        // The shape a material declaring nothing wants: no attributes at
        // all, and a one-row placeholder to fill the binding with.
        bindings.ensure(device, BASE_SHAPE, 0, 1);
        bindings
    }

    /// How many instances fit without reallocating.
    pub fn instance_capacity(&self) -> usize {
        self.capacity
    }

    /// How many distinct *declared* attribute row shapes are held.
    ///
    /// Zero for a frame whose materials declare no per-instance
    /// attributes: the placeholder every such material binds is not a
    /// shape anybody asked for.
    pub fn instance_shapes(&self) -> usize {
        self.groups.keys().filter(|key| *key != BASE_SHAPE).count()
    }

    /// Byte offset of one view in the views buffer — the dynamic offset a
    /// pass rendering from it binds with.
    pub fn view_offset(&self, view: PassView) -> u32 {
        self.view_stride * view.slot() as u32
    }

    /// Whether the environment-BRDF table still needs baking.
    ///
    /// Answered once: the caller that takes `true` is expected to write
    /// the table and say so with [`FrameBindings::mark_environment_baked`].
    pub fn needs_environment_bake(&self) -> bool {
        !self.environment_lut_baked
    }

    /// A view of the environment-BRDF table, for the bake to write
    /// through as a storage texture.
    pub fn environment_lut_view(&self) -> wgpu::TextureView {
        self.environment_lut
            .create_view(&wgpu::TextureViewDescriptor::default())
    }

    /// Record that the table has been written, so the bake does not run
    /// again. It is a pure function of its own coordinates, so once is
    /// exactly right — the same reasoning the `once` execution policy
    /// applies to the pass form of this bake (ADR 0035).
    pub fn mark_environment_baked(&mut self) {
        self.environment_lut_baked = true;
    }

    /// Bind `texture` as the shadow maps.
    ///
    /// `source` says where the view came from — the resource pool's
    /// generation and slot — and rebinding the same one is free, which
    /// matters because this is called every frame and rebuilding the bind
    /// groups is not.
    pub fn set_shadow_maps(
        &mut self,
        device: &wgpu::Device,
        source: (u64, usize),
        texture: &wgpu::TextureView,
    ) {
        if self.shadow_source == Some(source) {
            return;
        }
        self.shadow_source = Some(source);
        self.shadow_view = texture.clone();
        self.rebuild_groups(device);
    }

    /// Rebuild every frame bind group from the buffers currently held.
    fn rebuild_groups(&mut self, device: &wgpu::Device) {
        let keys: Vec<String> = self.groups.keys().cloned().collect();
        for key in keys {
            let attributes = self.groups[&key].buffer.clone();
            let bound = self.build_group(device, &attributes, ShadowMaps::Bound);
            let detached = self.build_group(device, &attributes, ShadowMaps::Detached);
            if let Some(group) = self.groups.get_mut(&key) {
                group.bound = bound;
                group.detached = detached;
            }
        }
    }

    /// One frame bind group over `attributes`.
    fn build_group(
        &self,
        device: &wgpu::Device,
        attributes: &wgpu::Buffer,
        shadows: ShadowMaps,
    ) -> wgpu::BindGroup {
        let shadow_view = match shadows {
            ShadowMaps::Bound => &self.shadow_view,
            ShadowMaps::Detached => &self.shadow_placeholder,
        };
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wxsl frame bindings"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: abi::BINDING_CAMERA,
                    // The bound range is one view, not the whole array:
                    // that is what a dynamic offset indexes.
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.views,
                        offset: 0,
                        size: wgpu::BufferSize::new(size_of::<CameraUniform>() as u64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: abi::BINDING_SCENE,
                    resource: self.scene.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: abi::BINDING_INSTANCES,
                    resource: self.instances.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: abi::BINDING_PREVIOUS_INSTANCES,
                    resource: self.previous_instances.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: abi::BINDING_INSTANCE_ATTRIBUTES,
                    resource: attributes.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: abi::BINDING_SHADOW_MAPS,
                    resource: wgpu::BindingResource::TextureView(shadow_view),
                },
                wgpu::BindGroupEntry {
                    binding: abi::BINDING_SHADOW_SAMPLER,
                    resource: wgpu::BindingResource::Sampler(&self.shadow_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: abi::BINDING_ENVIRONMENT_LUT,
                    resource: wgpu::BindingResource::TextureView(&self.environment_lut_view),
                },
                wgpu::BindGroupEntry {
                    binding: abi::BINDING_ENVIRONMENT_SAMPLER,
                    resource: wgpu::BindingResource::Sampler(&self.environment_lut_sampler),
                },
            ],
        })
    }

    /// Create or grow one shape's buffer, rebuilding its bind group when
    /// the buffer moves.
    fn ensure(&mut self, device: &wgpu::Device, key: &str, stride: usize, rows: usize) {
        let grown = match self.groups.get(key) {
            Some(group) if group.capacity >= rows && group.stride == stride => return,
            // Double until it fits, so a scene that grows by one object
            // per frame does not reallocate every frame.
            Some(group) => {
                let mut capacity = group.capacity.max(1);
                while capacity < rows {
                    capacity *= 2;
                }
                capacity
            }
            None => rows.max(INITIAL_INSTANCE_CAPACITY),
        };
        let buffer = instance_buffer(device, "wxsl instance attributes", stride, grown);
        let bound = self.build_group(device, &buffer, ShadowMaps::Bound);
        let detached = self.build_group(device, &buffer, ShadowMaps::Detached);
        self.groups.insert(
            key.to_string(),
            InstanceGroup {
                buffer,
                capacity: grown,
                stride,
                bound,
                detached,
            },
        );
    }

    /// Grow the transform arrays and rebuild every bind group that points
    /// at them.
    fn grow_instances(&mut self, device: &wgpu::Device, rows: usize) {
        if rows <= self.capacity {
            return;
        }
        let mut capacity = self.capacity.max(1);
        while capacity < rows {
            capacity *= 2;
        }
        self.capacity = capacity;
        self.instances = instance_buffer(
            device,
            "wxsl instances",
            size_of::<InstanceTransform>(),
            capacity,
        );
        self.previous_instances = instance_buffer(
            device,
            "wxsl previous instances",
            size_of::<InstanceTransform>(),
            capacity,
        );
        self.rebuild_groups(device);
    }

    /// Upload `environment` and every instance transform of the frame —
    /// this frame's, and what each draw says the previous frame's was
    /// beside them.
    ///
    /// Grows the storage buffers (and rebuilds the bind group) when the
    /// frame has more instances than any before it, which is the only time
    /// either is touched.
    pub fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        environment: &Environment,
        transforms: &[InstanceTransform],
        previous: &[InstanceTransform],
        rows: &InstanceRows,
    ) {
        // Every point of view the frame has, camera first: one write per
        // view rather than one buffer per view, so a pass switches between
        // them with a dynamic offset and nothing is rebound.
        for (slot, view) in environment.views().iter().enumerate() {
            queue.write_buffer(
                &self.views,
                u64::from(self.view_stride) * slot as u64,
                bytemuck::bytes_of(view),
            );
        }
        queue.write_buffer(&self.scene, 0, bytemuck::bytes_of(&environment.uniform()));

        self.grow_instances(device, transforms.len());
        if !transforms.is_empty() {
            queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(transforms));
        }
        // The previous frame's rows, always written: a draw that declared
        // no previous transform contributes its current one, so the array
        // is never a frame behind what its consumer assumes.
        if !previous.is_empty() {
            queue.write_buffer(&self.previous_instances, 0, bytemuck::cast_slice(previous));
        }

        for (key, set) in rows.sets() {
            self.ensure(device, key, set.layout().size() as usize, set.len());
            if set.is_empty() {
                continue;
            }
            let Some(group) = self.groups.get(key) else {
                continue;
            };
            queue.write_buffer(&group.buffer, 0, set.bytes());
        }
    }

    /// The bind group layout, for building pipeline layouts.
    ///
    /// One layout for every shape: the instance binding declares no
    /// minimum size, so a wider row is the same layout with a longer
    /// buffer behind it.
    pub fn layout(&self) -> &wgpu::BindGroupLayout {
        &self.layout
    }

    /// The bind group to set at [`abi::GROUP_FRAME`] for a material whose
    /// declared per-instance attributes have shape `signature`.
    ///
    /// Falls back to the empty shape, which is right for a pass with no
    /// material behind it and harmless for one whose rows were never
    /// uploaded — such a draw has nothing to read.
    pub fn instance_group(&self, signature: &str, shadows: ShadowMaps) -> &wgpu::BindGroup {
        self.groups
            .get(signature)
            .or_else(|| self.groups.get(BASE_SHAPE))
            .map(|group| match shadows {
                ShadowMaps::Bound => &group.bound,
                ShadowMaps::Detached => &group.detached,
            })
            .expect("the empty attribute shape is created up front")
    }

    /// The bind group for a material that declares no per-instance
    /// attributes.
    pub fn bind_group(&self, shadows: ShadowMaps) -> &wgpu::BindGroup {
        self.instance_group(BASE_SHAPE, shadows)
    }
}

/// [`BufferLayout::signature`] of a layout with no fields — what a
/// material declaring no per-instance attributes asks for.
const BASE_SHAPE: &str = "";

fn instance_buffer(
    device: &wgpu::Device,
    label: &str,
    stride: usize,
    capacity: usize,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        // Never zero: a zero-sized binding is a validation error, and an
        // empty frame — or a material that declares no attributes — is a
        // perfectly ordinary thing for an editor to draw.
        size: (capacity.max(1) * stride.max(4)) as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

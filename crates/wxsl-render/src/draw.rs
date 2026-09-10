//! [`DrawItem`] and [`DrawList`]: what a frame draws, flat.
//!
//! The renderer is scene-graph-free on purpose — batching, culling and
//! sorting belong to the application (ADR 0005, and unchanged by ADR 0021).
//! What it takes is a list: geometry, a material, a transform, and the
//! [`Tags`] the material was authored with. A geometry pass then draws a
//! *tag expression* over that list, so the material says what it is and the
//! pass says what it draws, with neither introspecting the other.
//!
//! Turning a `wxsl_core::scene::Scene` into one of these is the `wxsl`
//! facade's job, not this crate's: a scene names its meshes by primitive or
//! by file, and resolving a file is an I/O decision an engine makes rather
//! than a renderer.

use std::collections::BTreeMap;

use glam::Mat4;
use wxsl_core::node::Value;
use wxsl_core::scene::{TagExpr, Tags};

use crate::bindings::MaterialBindings;
use crate::environment::{InstanceRows, InstanceTransform};
use crate::error::RenderError;
use crate::material::Material;
use crate::mesh::Mesh;

/// The per-instance attributes one draw supplies, by name.
///
/// The other half of a declared per-instance attribute: the graph says
/// what it needs and at what type, and this is where the value for *this
/// object* comes from. It sits beside the transform because that is
/// exactly what it is — one more field of the same row
/// ([ADR 0024](../../../docs/adr/0024-a-material-declares-the-geometry-it-requires.md)).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InstanceAttributes {
    values: BTreeMap<String, Value>,
}

impl InstanceAttributes {
    /// Nothing supplied — what a draw of a material declaring none uses.
    pub const EMPTY: &'static InstanceAttributes = &InstanceAttributes {
        values: BTreeMap::new(),
    };

    /// An empty set.
    pub fn new() -> Self {
        InstanceAttributes::default()
    }

    /// Supply `name`.
    pub fn set(&mut self, name: impl Into<String>, value: Value) -> &mut Self {
        self.values.insert(name.into(), value);
        self
    }

    /// Supply `name`, by value, for building one inline.
    pub fn with(mut self, name: impl Into<String>, value: Value) -> Self {
        self.values.insert(name.into(), value);
        self
    }

    /// What was supplied for `name`.
    pub fn get(&self, name: &str) -> Option<Value> {
        self.values.get(name).copied()
    }

    /// Whether nothing was supplied.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// One thing to draw.
///
/// Borrows rather than owns: a draw list is built fresh every frame from
/// meshes and materials the application already holds, and copying either
/// per frame would be a strange thing to make it pay for.
#[derive(Clone, Copy)]
pub struct DrawItem<'a> {
    /// The geometry.
    pub mesh: &'a Mesh,
    /// The material, compiled per pass into whichever variant that pass's
    /// path needs.
    pub material: &'a Material,
    /// Object-to-world transform.
    pub transform: Mat4,
    /// What this draw *is*, for a pass to select on.
    pub tags: &'a Tags,
    /// The material's own bind group: its uniform parameters, its
    /// textures and its samplers (`abi::GROUP_MATERIAL`).
    ///
    /// Per *draw* rather than per material, because two objects sharing a
    /// material with different textures is ordinary — and because the
    /// renderer holds no resources of the application's, by the same rule
    /// that keeps it scene-graph-free. A material declaring neither a
    /// parameter nor a texture needs none.
    pub bindings: Option<&'a MaterialBindings>,
    /// The bind group for the block the material *expects the application
    /// to supply* (`abi::GROUP_USER`).
    ///
    /// The material hands out the layout
    /// ([`crate::renderer::Renderer::user_layout`]) and the application
    /// hands back a group. Nothing here knows what is in it: `wgpu`
    /// checks that the shape matches, so there is no validation of ours
    /// to keep in sync
    /// ([ADR 0023](../../../docs/adr/0023-a-material-declares-its-resources.md)).
    pub user: Option<&'a wgpu::BindGroup>,
    /// The per-instance attributes this draw supplies.
    ///
    /// Required whenever the material declares one, and checked when the
    /// frame is compiled rather than read as zeroes.
    pub attributes: &'a InstanceAttributes,
}

impl<'a> DrawItem<'a> {
    /// An untagged draw of `mesh` with `material`, at the origin.
    pub fn new(mesh: &'a Mesh, material: &'a Material) -> Self {
        DrawItem {
            mesh,
            material,
            transform: Mat4::IDENTITY,
            tags: Tags::EMPTY,
            bindings: None,
            user: None,
            attributes: InstanceAttributes::EMPTY,
        }
    }

    /// Place it.
    pub fn with_transform(mut self, transform: Mat4) -> Self {
        self.transform = transform;
        self
    }

    /// Tag it.
    pub fn with_tags(mut self, tags: &'a Tags) -> Self {
        self.tags = tags;
        self
    }

    /// Draw it with these material bindings. Required whenever the
    /// material declares a parameter, a texture or a sampler.
    pub fn with_bindings(mut self, bindings: &'a MaterialBindings) -> Self {
        self.bindings = Some(bindings);
        self
    }

    /// Supply the application's own bind group. Required whenever the
    /// material declares a block it expects to find there.
    pub fn with_user(mut self, group: &'a wgpu::BindGroup) -> Self {
        self.user = Some(group);
        self
    }

    /// Supply the per-instance attributes the material declares.
    pub fn with_attributes(mut self, attributes: &'a InstanceAttributes) -> Self {
        self.attributes = attributes;
        self
    }

    /// The transform half of the row this draw contributes to the frame's
    /// instance buffer.
    pub fn instance(&self) -> InstanceTransform {
        InstanceTransform::new(self.transform)
    }
}

/// Everything to draw this frame, in submission order.
///
/// The order is the instance-buffer order too: a draw's index in this list
/// is the `@builtin(instance_index)` its vertices read their transform at,
/// which is what lets one upload serve every pass in the frame.
#[derive(Clone, Default)]
pub struct DrawList<'a> {
    items: Vec<DrawItem<'a>>,
}

impl<'a> DrawList<'a> {
    /// An empty list.
    pub fn new() -> Self {
        DrawList::default()
    }

    /// Add a draw, returning its instance index.
    pub fn push(&mut self, item: DrawItem<'a>) -> u32 {
        self.items.push(item);
        self.items.len() as u32 - 1
    }

    /// The draws, in submission order.
    pub fn items(&self) -> &[DrawItem<'a>] {
        &self.items
    }

    /// How many draws.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether there is nothing to draw.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The draws a pass asking for `selector` should issue, with their
    /// instance indices.
    pub fn select<'s>(
        &'s self,
        selector: &'s TagExpr,
    ) -> impl Iterator<Item = (u32, &'s DrawItem<'a>)> + 's {
        self.items
            .iter()
            .enumerate()
            .filter(move |(_, item)| selector.matches(item.tags))
            .map(|(index, item)| (index as u32, item))
    }

    /// Every draw's transform, in instance-buffer order.
    pub fn transforms(&self) -> Vec<InstanceTransform> {
        self.items.iter().map(DrawItem::instance).collect()
    }

    /// Every draw's declared per-instance attributes, grouped by the row
    /// shape its material asked for.
    ///
    /// The transform is not in here: that array is ABI and
    /// [`DrawList::transforms`] builds it. What is here is written
    /// *through the computed layout*, never as a `#[repr(C)]` memcpy,
    /// because there is no Rust struct to memcpy from.
    ///
    /// A draw that does not supply an attribute its material declares is
    /// an error here, before a pass is opened, naming the material, the
    /// attribute and the draw.
    pub fn instance_rows(&self) -> Result<InstanceRows, RenderError> {
        let mut rows = InstanceRows::new();
        if self.items.is_empty() {
            return Ok(rows);
        }
        for item in &self.items {
            rows.reserve(item.material.instance_layout(), self.items.len());
        }
        for (index, item) in self.items.iter().enumerate() {
            let layout = item.material.instance_layout().clone();
            if layout.is_empty() {
                continue;
            }
            let Some(row) = rows.row_mut(&layout, index) else {
                continue;
            };
            for field in item.material.instance_attributes() {
                let name = field.name.as_str();
                let Some(value) = item.attributes.get(name) else {
                    return Err(RenderError::MissingInstanceAttribute {
                        material: item.material.name.clone(),
                        attribute: name.to_string(),
                        draw: index,
                    });
                };
                layout.write(row, name, value).map_err(|error| {
                    RenderError::InstanceAttributeType {
                        material: item.material.name.clone(),
                        attribute: name.to_string(),
                        reason: error.to_string(),
                    }
                })?;
            }
        }
        Ok(rows)
    }
}

impl<'a> FromIterator<DrawItem<'a>> for DrawList<'a> {
    fn from_iter<I: IntoIterator<Item = DrawItem<'a>>>(iter: I) -> Self {
        DrawList {
            items: iter.into_iter().collect(),
        }
    }
}

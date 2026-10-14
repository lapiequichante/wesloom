//! The scene mode (ADR 0082): the open scene on the GPU, and its three
//! panels: the viewport, the outliner and the inspector.
//!
//! The document and its edits are `scene`'s. This module uploads what the
//! edits changed at the next frame, through the preview's own renderer, and
//! turns gestures into commands. The camera is session state, so dragging
//! it is not an undo step.

use glam::{Mat4, Vec2, Vec3};
use wxsl_core::graph::Graph;
use wxsl_core::lighting::{LightingSet, DEFAULT_MODELS};
use wxsl_core::node::{NodeRegistry, Value, ValueType};
use wxsl_render::scene::{mesh_data, SceneImage, SceneResources};
use wxsl_render::ui::draw::{Color, Rect};
use wxsl_render::ui::input::{Key, MouseButton};
use wxsl_render::{Environment, MeshData};

use crate::app::Requests;
use crate::command::Command;
use crate::preview::{preview_environment, Preview};
use crate::scene::{self, Bounds, Orbit, SceneChanges, SceneDoc};
use crate::ui::{Align, Id, Ui};

/// The open scene and everything the editor keeps about it.
#[derive(Default)]
pub(crate) struct SceneMode {
    /// The document; `None` until a scene is opened.
    pub doc: Option<SceneDoc>,
    /// The selected instance.
    pub selected: Option<usize>,
    /// The scene material the material canvas edits.
    pub slot: Option<usize>,
    /// The material canvas's own graph, while it edits a scene material.
    pub loose: Option<Graph>,
    pub orbit: Orbit,
    /// The outliner's filter.
    pub filter: String,
    /// What the next frame uploads or recompiles.
    pub pending: SceneChanges,
    /// Material parameter writes for the next frame: material, name, value.
    pub params: Vec<(usize, String, Value)>,
    /// Why the scene is not drawing, if it is not.
    pub error: Option<String>,
    /// Frame the camera on the scene once it loads.
    pub frame_next: bool,
    /// The renderer's lighting set before a scene replaced it, put back
    /// when the scene closes.
    lighting_before: Option<LightingSet>,
    resources: Option<SceneResources>,
    /// The geometry of each document mesh, on the CPU, for picking.
    meshes: Vec<MeshData>,
    bounds: Vec<Option<Bounds>>,
}

impl SceneMode {
    /// Bring the GPU side up to date with the document. Returns whether
    /// the renderer's lighting set changed, which the material preview
    /// must be rebuilt for.
    pub fn sync(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        preview: &mut Preview,
        registry: &NodeRegistry,
    ) -> bool {
        let pending = std::mem::take(&mut self.pending);
        let params = std::mem::take(&mut self.params);
        if self.doc.is_none() {
            self.resources = None;
            self.meshes.clear();
            self.bounds.clear();
            self.error = None;
            return match self.lighting_before.take() {
                Some(set) => preview.renderer_mut().set_lighting(set).is_ok(),
                None => false,
            };
        }
        let mut relit = false;
        // A scene that failed to load is tried again on the next edit,
        // which may be the one that fixes it.
        let loading = pending.reload
            || self.resources.is_none() && (self.error.is_none() || !pending.is_empty());
        let result = if loading {
            self.resources = None;
            self.load(device, queue, preview, registry, &mut relit)
        } else {
            self.update(device, preview, registry, &pending)
        };
        let result = result.and_then(|()| {
            let Some(resources) = self.resources.as_mut() else {
                return Ok(());
            };
            for (material, name, value) in params {
                if let Some(bindings) = resources.bindings_mut(material) {
                    bindings
                        .set(&name, value)
                        .map_err(|error| error.to_string())?;
                }
            }
            resources
                .upload(device, queue)
                .map_err(|error| error.to_string())
        });
        match result {
            Err(error) => self.error = Some(error),
            Ok(()) if loading || !pending.is_empty() => self.error = None,
            Ok(()) => {}
        }
        relit
    }

    /// Resolve, upload and compile the whole scene.
    fn load(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        preview: &mut Preview,
        registry: &NodeRegistry,
        relit: &mut bool,
    ) -> Result<(), String> {
        let doc = self.doc.as_ref().expect("checked by the caller");
        self.meshes = doc
            .document
            .meshes
            .iter()
            .map(|entry| {
                mesh_data(&entry.name, &entry.source, doc.base.as_deref(), &doc.meshes)
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<_, _>>()?;
        self.bounds = self.meshes.iter().map(Bounds::of).collect();
        if let Some(set) = needed_lighting(&doc.document, preview.renderer().lighting())? {
            if self.lighting_before.is_none() {
                self.lighting_before = Some(preview.renderer().lighting().clone());
            }
            preview
                .renderer_mut()
                .set_lighting(set)
                .map_err(|error| error.to_string())?;
            *relit = true;
        }
        let features = preview.renderer().pipeline_config().features.clone();
        let lighting = preview.renderer().lighting().clone();
        let mut resources = SceneResources::load_resolved(
            device,
            &doc.document,
            registry,
            doc.base.as_deref(),
            &lighting,
            &features,
            &self.meshes,
        )
        .map_err(|error| error.to_string())?;
        resources.set_images(
            doc.images
                .iter()
                .map(|bytes| SceneImage::Encoded(bytes.clone()))
                .collect(),
        );
        resources
            .create_bindings(device, queue, preview.renderer_mut())
            .map_err(|error| error.to_string())?;
        for material in 0..doc.document.materials.len() {
            fill_placeholders(&mut resources, doc, material, preview);
        }
        fill_attributes(&mut resources, doc);
        self.resources = Some(resources);
        if std::mem::take(&mut self.frame_next) {
            if let Some(bounds) = scene::scene_bounds(&doc.document, &self.bounds) {
                self.orbit = Orbit::framing(bounds);
            }
        }
        Ok(())
    }

    /// Redo only what `pending` names.
    fn update(
        &mut self,
        device: &wgpu::Device,
        preview: &mut Preview,
        registry: &NodeRegistry,
        pending: &SceneChanges,
    ) -> Result<(), String> {
        let (Some(doc), Some(resources)) = (self.doc.as_ref(), self.resources.as_mut()) else {
            return Ok(());
        };
        if pending.instances {
            resources
                .set_instances(&doc.document)
                .map_err(|error| error.to_string())?;
        }
        let features = preview.renderer().pipeline_config().features.clone();
        let lighting = preview.renderer().lighting().clone();
        for &material in &pending.materials {
            resources
                .set_material(
                    device,
                    preview.renderer_mut(),
                    material,
                    &doc.document.materials[material],
                    registry,
                    &lighting,
                    &features,
                )
                .map_err(|error| error.to_string())?;
            fill_placeholders(resources, doc, material, preview);
        }
        if pending.instances || !pending.materials.is_empty() {
            fill_attributes(resources, doc);
        }
        Ok(())
    }

    /// The draws, if the scene is loaded.
    pub fn resources(&self) -> Option<&SceneResources> {
        self.resources.as_ref()
    }

    /// The camera and lights for a viewport of `aspect`: the preview's
    /// lighting unless the document brings its own lights.
    pub fn environment(&self, aspect: f32, time: f32) -> Environment {
        let mut environment = preview_environment(time);
        environment.camera = self.orbit.camera(aspect);
        let lights = self
            .doc
            .as_ref()
            .and_then(|doc| wxsl_render::scene::environment(&doc.document, aspect))
            .map(|scene| scene.lights)
            .unwrap_or_default();
        if !lights.is_empty() {
            environment.lights = lights;
        }
        environment
    }

    /// The instance under a viewport point.
    pub fn pick(&self, viewport: Rect, point: Vec2) -> Option<usize> {
        let doc = self.doc.as_ref()?;
        if self.meshes.len() != doc.document.meshes.len() {
            return None;
        }
        let camera = self
            .orbit
            .camera(viewport.width() / viewport.height().max(1.0));
        let ndc = Vec2::new(
            (point.x - viewport.min.x) / viewport.width() * 2.0 - 1.0,
            1.0 - (point.y - viewport.min.y) / viewport.height() * 2.0,
        );
        let (origin, direction) = scene::ray(&camera, ndc);
        scene::pick(&doc.document, &self.meshes, &self.bounds, origin, direction)
            .map(|(index, _)| index)
    }

    /// Frame the selected instance, or the whole scene.
    pub fn frame(&mut self) {
        let Some(doc) = self.doc.as_ref() else {
            return;
        };
        let bounds = match self
            .selected
            .and_then(|index| doc.document.instances.get(index))
        {
            Some(instance) => self
                .bounds
                .get(instance.mesh)
                .copied()
                .flatten()
                .map(|bounds| {
                    bounds.transformed(Mat4::from_cols_array(
                        &doc.document.instance_world(instance),
                    ))
                }),
            None => scene::scene_bounds(&doc.document, &self.bounds),
        };
        if let Some(bounds) = bounds {
            let (yaw, pitch) = (self.orbit.yaw, self.orbit.pitch);
            self.orbit = Orbit {
                yaw,
                pitch,
                ..Orbit::framing(bounds)
            };
        }
    }

    /// The selected instance's world bounds, for the viewport's outline.
    fn selected_bounds(&self) -> Option<Bounds> {
        let doc = self.doc.as_ref()?;
        let instance = doc.document.instances.get(self.selected?)?;
        let bounds = self.bounds.get(instance.mesh).copied().flatten()?;
        Some(bounds.transformed(Mat4::from_cols_array(
            &doc.document.instance_world(instance),
        )))
    }
}

/// The lighting set a scene draws under: every model a material names,
/// and the library's default for a material naming none — not the
/// renderer's set widened, which could overflow the G-buffer budget the
/// scene itself fits (ADR 0080). `None` when the renderer already runs it.
fn needed_lighting(
    scene: &wxsl_core::scene::Scene,
    current: &LightingSet,
) -> Result<Option<LightingSet>, String> {
    let mut models = Vec::new();
    for material in &scene.materials {
        let model = match material.config.model.as_deref() {
            Some(name) => *DEFAULT_MODELS
                .iter()
                .chain(current.models())
                .find(|model| model.name == name)
                .ok_or_else(|| {
                    format!(
                        "material `{}` names lighting model `{name}`, which this build does not have",
                        material.name
                    )
                })?,
            None => *LightingSet::default().default_model(),
        };
        if !models
            .iter()
            .any(|known: &wxsl_core::lighting::LightingModel| known.name == model.name)
        {
            models.push(model);
        }
    }
    if models.is_empty() {
        return Ok(None);
    }
    let set = LightingSet::new(models).map_err(|error| error.to_string())?;
    Ok((set != *current).then_some(set))
}

/// Bind the checker to every texture and sampler material `index`
/// declares and the document leaves unbound, so it draws rather than
/// failing its upload.
fn fill_placeholders(
    resources: &mut SceneResources,
    doc: &SceneDoc,
    index: usize,
    preview: &Preview,
) {
    let declared: Vec<_> = resources.materials()[index]
        .interface()
        .resources
        .iter()
        .map(|resource| (resource.name.to_string(), resource.ty))
        .collect();
    let bound = &doc.document.materials[index].bindings.textures;
    let (placeholder, sampler) = preview.placeholder();
    let Some(bindings) = resources.bindings_mut(index) else {
        return;
    };
    for (name, ty) in declared {
        if bound.contains_key(&name) {
            continue;
        }
        let _ = match ty {
            ValueType::Sampler => bindings.set_sampler(&name, sampler),
            _ => bindings.set_texture(&name, placeholder),
        };
    }
}

/// One for every per-instance attribute an instance's material declares,
/// as the preview does: the document has no values for them.
fn fill_attributes(resources: &mut SceneResources, doc: &SceneDoc) {
    for (index, instance) in doc.document.instances.iter().enumerate() {
        let fields: Vec<_> = resources.materials()[instance.material]
            .instance_attributes()
            .iter()
            .filter_map(|field| {
                let value = field.ty.splat(1.0).or_else(|| field.ty.zero())?;
                Some((field.name.to_string(), value))
            })
            .collect();
        if let Some(attributes) = resources.instance_attributes_mut(index) {
            *attributes = wxsl_render::InstanceAttributes::new();
            for (name, value) in fields {
                attributes.set(&name, value);
            }
        }
    }
}

/// The viewport: the scene as the preview's renderer drew it. Dragging
/// orbits, scrolling zooms, a click selects.
pub(crate) fn viewport(
    ui: &mut Ui<'_>,
    rect: Rect,
    mode: &mut SceneMode,
    preview: &Preview,
    requests: &mut Requests,
) {
    let theme = *ui.theme();
    ui.draw().rect(rect, theme.palette.canvas);
    let Some(doc) = mode.doc.as_ref() else {
        ui.label(
            rect,
            "no scene: drop a .glb, .gltf or .scene.json here",
            theme.palette.text_dim,
            Align::Center,
        );
        return;
    };
    if mode.resources.is_some() {
        ui.draw().image(
            rect,
            preview.texture(),
            Vec2::ZERO,
            Vec2::ONE,
            Color::WHITE,
            0.0,
        );
    }
    if let Some(error) = &mode.error {
        let (strip, _) = rect.split_top(theme.metrics.row_height * 1.4);
        ui.draw().rect(strip, theme.palette.panel.with_alpha(0.9));
        ui.small_label(
            strip.shrink(theme.metrics.padding * 0.5),
            error,
            theme.palette.error,
            Align::Left,
        );
    }

    // The selection, as the screen box around its world bounds.
    if let Some(bounds) = mode.selected_bounds() {
        let camera = mode.orbit.camera(rect.width() / rect.height().max(1.0));
        let view_proj = camera.view_proj();
        let corners: Vec<Vec2> = (0..8)
            .filter_map(|corner| {
                let point = Vec3::new(
                    if corner & 1 == 0 {
                        bounds.min.x
                    } else {
                        bounds.max.x
                    },
                    if corner & 2 == 0 {
                        bounds.min.y
                    } else {
                        bounds.max.y
                    },
                    if corner & 4 == 0 {
                        bounds.min.z
                    } else {
                        bounds.max.z
                    },
                );
                let clip = view_proj * point.extend(1.0);
                (clip.w > 0.0).then(|| {
                    let ndc = clip.truncate() / clip.w;
                    Vec2::new(
                        rect.min.x + (ndc.x + 1.0) * 0.5 * rect.width(),
                        rect.min.y + (1.0 - ndc.y) * 0.5 * rect.height(),
                    )
                })
            })
            .collect();
        if corners.len() == 8 {
            let min = corners.iter().fold(Vec2::splat(f32::MAX), |a, b| a.min(*b));
            let max = corners.iter().fold(Vec2::splat(f32::MIN), |a, b| a.max(*b));
            let outline = Rect::from_min_max(min.max(rect.min), max.min(rect.max));
            if !outline.is_empty() {
                ui.draw().round_rect_border(
                    outline,
                    theme.metrics.radius,
                    theme.metrics.outline_width * 2.0,
                    theme.palette.accent,
                );
            }
        }
    }
    let name = |index: usize| {
        let instance = &doc.document.instances[index];
        if instance.name.is_empty() {
            format!("instance {index}")
        } else {
            instance.name.clone()
        }
    };
    if let Some(index) = mode
        .selected
        .filter(|&index| index < doc.document.instances.len())
    {
        let (_, strip) = rect.split_bottom(theme.metrics.row_height);
        ui.small_label(
            strip.shrink(theme.metrics.padding * 0.5),
            &name(index),
            theme.palette.text,
            Align::Left,
        );
    }

    let response = ui.interact(Id::new("scene.viewport"), rect);
    if response.dragging && response.drag_delta != Vec2::ZERO {
        mode.orbit.turn(response.drag_delta);
    }
    if response.hovered {
        let scroll = ui.input.scroll();
        if scroll.y != 0.0 {
            mode.orbit.zoom(scroll.y * 0.015);
        }
    }
    let pointer = ui.input.pointer_or_zero();
    let still = (pointer - ui.input.press_position(MouseButton::Left)).length() < 4.0;
    if response.drag_released && still {
        requests.commands.push(Command::SelectInstance {
            instance: mode.pick(rect, pointer),
        });
    }
    if !ui.state.is_editing() {
        if let Some(index) = mode.selected {
            if ui.input.key_pressed_plain(Key::Delete) || ui.input.key_pressed_plain(Key::Backspace)
            {
                requests.commands.push(Command::RemoveInstances {
                    instances: vec![index],
                });
            }
        }
    }
}

/// The outliner: the node hierarchy with each node's instances, or, while
/// filtering, every instance whose name, mesh, material or tags match.
pub(crate) fn outliner_panel(
    ui: &mut Ui<'_>,
    rect: Rect,
    mode: &mut SceneMode,
    requests: &mut Requests,
) {
    let theme = *ui.theme();
    let metrics = theme.metrics;
    let inner = ui.panel(rect, Some("scene"));
    let Some(doc) = mode.doc.as_ref() else {
        ui.small_label(
            inner,
            "open a scene to see it here",
            theme.palette.text_dim,
            Align::Left,
        );
        return;
    };
    let scene = &doc.document;
    let (search, rest) = inner.split_top(metrics.row_height);
    ui.text_field(Id::new("scene.filter"), search, &mut mode.filter);
    let list = Rect::from_min_max(
        Vec2::new(rest.min.x, rest.min.y + metrics.row_gap),
        rest.max,
    );
    if list.is_empty() {
        return;
    }

    // Rows: (depth, label, instance it selects).
    let mut rows: Vec<(usize, String, Option<usize>)> = Vec::new();
    let instance_label = |index: usize| {
        let instance = &scene.instances[index];
        let material = scene
            .materials
            .get(instance.material)
            .map_or("?", |material| material.name.as_str());
        if instance.name.is_empty() {
            format!("instance {index} · {material}")
        } else {
            format!("{} · {material}", instance.name)
        }
    };
    let needle = mode.filter.trim().to_lowercase();
    if needle.is_empty() {
        fn walk(
            scene: &wxsl_core::scene::Scene,
            parent: Option<usize>,
            depth: usize,
            rows: &mut Vec<(usize, String, Option<usize>)>,
            label: &dyn Fn(usize) -> String,
        ) {
            for (index, _) in scene
                .instances
                .iter()
                .enumerate()
                .filter(|(_, instance)| instance.node == parent)
            {
                rows.push((depth, label(index), Some(index)));
            }
            for (index, node) in scene
                .nodes
                .iter()
                .enumerate()
                .filter(|(_, node)| node.parent == parent)
            {
                let name = if node.name.is_empty() {
                    format!("node {index}")
                } else {
                    node.name.clone()
                };
                rows.push((depth, name, None));
                // A cycle is refused by `Scene::validate`; the depth cap is
                // the belt to its braces.
                if depth < 64 {
                    walk(scene, Some(index), depth + 1, rows, label);
                }
            }
        }
        walk(scene, None, 0, &mut rows, &instance_label);
    } else {
        for (index, instance) in scene.instances.iter().enumerate() {
            let material = scene.materials.get(instance.material);
            let mesh = scene.meshes.get(instance.mesh);
            let matches = instance.name.to_lowercase().contains(&needle)
                || mesh.is_some_and(|mesh| mesh.name.to_lowercase().contains(&needle))
                || material.is_some_and(|material| material.name.to_lowercase().contains(&needle))
                || scene
                    .instance_tags(instance)
                    .iter()
                    .any(|tag| tag.to_lowercase().contains(&needle));
            if matches {
                rows.push((0, instance_label(index), Some(index)));
            }
        }
    }

    let row_height = metrics.row_height;
    let content = Vec2::new(list.width(), rows.len() as f32 * row_height);
    let area = ui.scroll_area(Id::new("scene.outliner"), list, content);
    let origin = area.origin();
    let first = ((area.offset.y / row_height).floor() as usize).saturating_sub(1);
    let visible = (list.height() / row_height).ceil() as usize + 2;
    for (position, (depth, label, instance)) in rows.iter().enumerate().skip(first).take(visible) {
        let row = Rect::from_min_size(
            Vec2::new(origin.x, origin.y + position as f32 * row_height),
            Vec2::new(list.width() - metrics.scrollbar_width, row_height - 2.0),
        );
        let indent = *depth as f32 * metrics.padding;
        let text = Rect::from_min_max(row.min + Vec2::new(indent + 4.0, 0.0), row.max);
        match instance {
            Some(index) => {
                let response = ui.interact(Id::new("scene.row").with(*index as u64), row);
                let selected = mode.selected == Some(*index);
                if selected || response.hovered {
                    ui.draw().round_rect(
                        row,
                        metrics.radius,
                        if selected {
                            theme.palette.accent.with_alpha(0.30)
                        } else {
                            theme.palette.control
                        },
                    );
                }
                if response.clicked {
                    requests.commands.push(Command::SelectInstance {
                        instance: Some(*index),
                    });
                }
                ui.truncated_label(text, label, theme.palette.text, Align::Left);
            }
            None => {
                ui.truncated_label(text, label, theme.palette.text_dim, Align::Left);
            }
        }
    }
    area.end(ui);
}

/// The inspector: the selected instance, its material's configuration, and
/// the environment's lights.
pub(crate) fn inspector_panel(
    ui: &mut Ui<'_>,
    rect: Rect,
    mode: &SceneMode,
    requests: &mut Requests,
) {
    let theme = *ui.theme();
    let metrics = theme.metrics;
    let inner = ui.panel(rect, Some("inspector"));
    let Some(doc) = mode.doc.as_ref() else {
        return;
    };
    let scene = &doc.document;
    let row = metrics.row_height + metrics.row_gap;
    let lights = scene
        .environment
        .as_ref()
        .map_or(0, |environment| environment.lights.len());
    let selected = mode.selected.filter(|&index| index < scene.instances.len());
    let rows = 2 + if selected.is_some() { 12 } else { 1 } + lights * 3;
    let width = inner.width() - metrics.scrollbar_width;
    let area = ui.scroll_area(
        Id::new("scene.inspector"),
        inner,
        Vec2::new(inner.width(), rows as f32 * row),
    );
    let mut y = area.origin().y;
    let mut next = || {
        let rect = Rect::from_min_size(
            Vec2::new(inner.min.x, y),
            Vec2::new(width, metrics.row_height),
        );
        y += row;
        rect
    };
    let labeled = |ui: &mut Ui<'_>, rect: Rect, label: &str| {
        let (left, right) = rect.split_left(width * 0.3);
        ui.small_label(left, label, theme.palette.text_dim, Align::Left);
        right
    };
    ui.small_label(
        next(),
        &format!(
            "{} · {} instances · {} materials",
            scene.name,
            scene.instances.len(),
            scene.materials.len()
        ),
        theme.palette.text,
        Align::Left,
    );

    match selected {
        None => {
            ui.small_label(
                next(),
                "click an instance in the viewport or the outliner",
                theme.palette.text_dim,
                Align::Left,
            );
        }
        Some(index) => {
            let instance = &scene.instances[index];
            let field = labeled(ui, next(), "name");
            let mut name = instance.name.clone();
            if ui
                .text_field(Id::new("scene.name").with(index as u64), field, &mut name)
                .changed
            {
                requests.commands.push(Command::SetInstanceName {
                    instance: index,
                    name,
                });
            }
            let field = labeled(ui, next(), "mesh");
            let mesh = scene
                .meshes
                .get(instance.mesh)
                .map_or("?", |mesh| mesh.name.as_str());
            ui.truncated_label(field, mesh, theme.palette.text, Align::Left);

            // Translation, rotation in degrees, scale: three drags a row.
            let (translation, degrees, scale) = scene::decompose(&instance.transform);
            let mut parts = [translation, degrees, scale];
            let mut changed = false;
            for (row, (part, (label, speed))) in parts
                .iter_mut()
                .zip([("position", 0.01), ("rotation", 0.5), ("scale", 0.01)])
                .enumerate()
            {
                let field = labeled(ui, next(), label);
                let third = field.width() / 3.0;
                for (axis, value) in part.iter_mut().enumerate() {
                    let cell = Rect::from_min_size(
                        Vec2::new(field.min.x + third * axis as f32, field.min.y),
                        Vec2::new(third - 2.0, field.height()),
                    );
                    let id = Id::new("scene.transform").with((index * 9 + row * 3 + axis) as u64);
                    changed |= ui.drag_value(id, cell, value, speed, None).changed;
                }
            }
            if changed {
                let [translation, degrees, scale] = parts;
                requests.commands.push(Command::SetTransform {
                    instance: index,
                    transform: scene::compose(translation, degrees, scale),
                });
            }

            // The material: which one, and its configuration.
            let field = labeled(ui, next(), "material");
            let names: Vec<String> = scene
                .materials
                .iter()
                .map(|material| material.name.clone())
                .collect();
            let options: Vec<(&str, bool)> =
                names.iter().map(|name| (name.as_str(), true)).collect();
            let mut chosen = instance.material;
            if ui
                .dropdown(Id::new("scene.material"), field, &options, &mut chosen)
                .changed
            {
                requests.commands.push(Command::SetInstanceMaterial {
                    instance: index,
                    material: chosen,
                });
            }
            let material = &scene.materials[instance.material];
            let field = labeled(ui, next(), "model");
            ui.truncated_label(
                field,
                material.config.model.as_deref().unwrap_or("default"),
                theme.palette.text,
                Align::Left,
            );
            let field = labeled(ui, next(), "tags");
            let mut tags = material.config.tags.iter().collect::<Vec<_>>().join(", ");
            if ui
                .text_field(
                    Id::new("scene.tags").with(instance.material as u64),
                    field,
                    &mut tags,
                )
                .changed
            {
                requests.commands.push(Command::SetMaterialTags {
                    material: instance.material,
                    tags: tags
                        .split(',')
                        .map(str::trim)
                        .filter(|tag| !tag.is_empty())
                        .map(str::to_string)
                        .collect(),
                });
            }
            let field = labeled(ui, next(), "render order");
            let mut order = material.config.render_order as f32;
            if ui
                .drag_value(
                    Id::new("scene.order").with(instance.material as u64),
                    field,
                    &mut order,
                    0.05,
                    None,
                )
                .changed
                && order.round() as i32 != material.config.render_order
            {
                requests.commands.push(Command::SetRenderOrder {
                    material: instance.material,
                    order: order.round() as i32,
                });
            }
            let field = labeled(ui, next(), "max layers");
            let mut layers = material.config.max_layers as f32;
            if ui
                .drag_value(
                    Id::new("scene.layers").with(instance.material as u64),
                    field,
                    &mut layers,
                    0.05,
                    Some((0.0, 8.0)),
                )
                .changed
                && layers.round() as i32 != material.config.max_layers
            {
                requests.commands.push(Command::SetMaxLayers {
                    material: instance.material,
                    layers: layers.round() as i32,
                });
            }
            if ui
                .button(
                    Id::new("scene.edit_material"),
                    next(),
                    "edit the material graph",
                )
                .clicked
            {
                requests.mode = Some(crate::pipeline::CanvasMode::Material);
            }
        }
    }

    if let Some(environment) = scene.environment.as_ref() {
        for (index, light) in environment.lights.iter().enumerate() {
            let title = if light.name.is_empty() {
                format!("light {index}")
            } else {
                format!("light: {}", light.name)
            };
            ui.small_label(next(), &title, theme.palette.text, Align::Left);
            let field = labeled(ui, next(), "colour");
            let third = field.width() / 3.0;
            let mut color = light.color;
            let mut changed = false;
            for (channel, value) in color.iter_mut().enumerate() {
                let cell = Rect::from_min_size(
                    Vec2::new(field.min.x + third * channel as f32, field.min.y),
                    Vec2::new(third - 2.0, field.height()),
                );
                changed |= ui
                    .drag_value(
                        Id::new("scene.light.color").with((index * 3 + channel) as u64),
                        cell,
                        value,
                        0.005,
                        Some((0.0, 1.0)),
                    )
                    .changed;
            }
            let field = labeled(ui, next(), "intensity");
            let mut intensity = light.intensity;
            changed |= ui
                .drag_value(
                    Id::new("scene.light.intensity").with(index as u64),
                    field,
                    &mut intensity,
                    0.05,
                    Some((0.0, f32::MAX)),
                )
                .changed;
            if changed {
                requests.commands.push(Command::SetLight {
                    light: index,
                    color,
                    intensity,
                });
            }
        }
    }
    area.end(ui);
}

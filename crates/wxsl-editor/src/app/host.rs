//! The editor as commands see it: [`Host`] over the editor's documents
//! (ADR 0081).

use super::*;

/// The editor as commands see it (ADR 0081).
impl Host for Editor {
    fn graph(&self, document: Document) -> Option<&Graph> {
        match document {
            Document::Material => Some(&self.graph),
            Document::Pipeline => self.pipeline_graph(),
        }
    }

    fn open(&mut self, document: Document) -> (&mut Graph, &NodeRegistry) {
        match document {
            Document::Material => (&mut self.graph, &self.registry),
            Document::Pipeline => {
                let state = self
                    .pipeline
                    .get_or_insert_with(|| PipelineCanvas::open_on_preset(StockPipeline::Deferred));
                (&mut state.graph, &state.registry)
            }
        }
    }

    fn replace(&mut self, document: Document, graph: Graph) -> Graph {
        let (slot, _) = self.open(document);
        std::mem::replace(slot, graph)
    }

    fn pass_param(&self, pass: &str, name: &str) -> Option<wxsl_core::node::Value> {
        self.preview.pass_param(pass, name)
    }

    fn set_pass_param(
        &mut self,
        pass: &str,
        name: &str,
        value: wxsl_core::node::Value,
    ) -> Result<(), String> {
        self.preview
            .set_pass_param(pass, name, value)
            .map_err(|error| error.to_string())
    }

    fn write_material_param(
        &mut self,
        name: &str,
        value: wxsl_core::node::Value,
    ) -> Result<(), String> {
        if let Some(slot) = self.scene.slot {
            self.scene.params.push((slot, name.to_string(), value));
        }
        self.preview
            .set_material_param(name, value)
            .map_err(|error| error.to_string())
    }

    fn changed(&mut self, document: Document, rebuild: bool) {
        self.modified = true;
        match document {
            Document::Material => {
                self.dirty |= rebuild;
                // The canvas edits a copy of a scene material: write it
                // back, and recompile that one material in the scene.
                if let (Some(slot), Some(doc)) = (self.scene.slot, self.scene.doc.as_mut()) {
                    if let Some(entry) = doc.document.materials.get_mut(slot) {
                        entry.graph = self.graph.clone();
                        if rebuild {
                            self.scene.pending.materials.insert(slot);
                        }
                    }
                }
            }
            Document::Pipeline => {
                if let Some(state) = self.pipeline.as_mut() {
                    state.dirty |= rebuild;
                }
            }
        }
    }

    fn scene(&self) -> Option<&SceneDoc> {
        self.scene.doc.as_ref()
    }

    fn scene_mut(&mut self) -> (&mut Option<SceneDoc>, &NodeRegistry) {
        (&mut self.scene.doc, &self.registry)
    }

    fn scene_changed(&mut self, changes: SceneChanges) {
        self.modified = true;
        self.scene.pending.merge(changes);
        let (instances, materials) = self.scene.doc.as_ref().map_or((0, 0), |doc| {
            (doc.document.instances.len(), doc.document.materials.len())
        });
        if self.scene.selected.is_some_and(|index| index >= instances) {
            self.scene.selected = None;
        }
        // The material canvas follows the scene's copy of what it edits,
        // or gets its own graph back when that material is gone.
        match self.scene.slot {
            Some(slot) if slot < materials => {
                let graph =
                    &self.scene.doc.as_ref().expect("counted").document.materials[slot].graph;
                if *graph != self.graph {
                    self.graph = graph.clone();
                    self.dirty = true;
                }
            }
            Some(_) => self.open_slot(None),
            None => {}
        }
    }

    fn material_slot(&self) -> Option<usize> {
        self.scene.slot
    }

    fn open_slot(&mut self, slot: Option<usize>) {
        if slot == self.scene.slot {
            return;
        }
        match slot {
            Some(index) => {
                let Some(graph) = self
                    .scene
                    .doc
                    .as_ref()
                    .and_then(|doc| doc.document.materials.get(index))
                    .map(|entry| entry.graph.clone())
                else {
                    return;
                };
                let previous = std::mem::replace(&mut self.graph, graph);
                if self.scene.slot.is_none() {
                    self.scene.loose = Some(previous);
                }
            }
            None => {
                if let Some(loose) = self.scene.loose.take() {
                    self.graph = loose;
                }
            }
        }
        self.scene.slot = slot;
        self.canvas.selection.clear();
        self.dirty = true;
        self.first_frame = true;
    }

    fn select_instance(&mut self, instance: Option<usize>) -> Result<(), CommandError> {
        let doc = self.scene.doc.as_ref().ok_or(CommandError::NoScene)?;
        let material = match instance {
            Some(index) => Some(
                doc.document
                    .instances
                    .get(index)
                    .ok_or(CommandError::NoSuchInstance(index))?
                    .material,
            ),
            None => None,
        };
        self.scene.selected = instance;
        if material.is_some() {
            self.open_slot(material);
        }
        Ok(())
    }

    fn view(&mut self, command: &Command) -> Result<(), CommandError> {
        let render = |error: RenderError| CommandError::Render(error.to_string());
        match command {
            Command::SetCanvasMode { mode } => self.set_canvas_mode(*mode),
            Command::SetPipeline { pipeline } => {
                self.pending_pipeline = Some(if pipeline == "document" {
                    None
                } else {
                    Some(
                        StockPipeline::parse(pipeline)
                            .ok_or_else(|| CommandError::NoSuchPipeline(pipeline.clone()))?,
                    )
                });
            }
            Command::SetDebugView { view } => {
                let choice = DebugChoice::all(&self.preview)
                    .into_iter()
                    .find(|(found, available)| *available && found.label() == view)
                    .map(|(found, _)| found)
                    .ok_or_else(|| CommandError::NoSuchDebugView(view.clone()))?;
                self.show_debug_view(choice).map_err(CommandError::Render)?;
            }
            Command::ProbePixel { x, y } => {
                if !(0.0..=1.0).contains(x) || !(0.0..=1.0).contains(y) {
                    return Err(CommandError::OutsideViewport { x: *x, y: *y });
                }
                self.probe_request = Some([*x, *y]);
            }
            Command::StepFrame { passes, show } => {
                let Some(count) = *passes else {
                    return self.step(StepRequest::Stop).map_err(render);
                };
                self.step(StepRequest::Start).map_err(render)?;
                self.step(StepRequest::To(count, None)).map_err(render)?;
                if let Some(label) = show {
                    let id = self
                        .preview
                        .stepping()
                        .and_then(|view| {
                            view.resources
                                .into_iter()
                                .find(|(_, found, drawable)| *drawable && found == label)
                        })
                        .map(|(id, _, _)| id)
                        .ok_or_else(|| CommandError::NoSuchResource(label.clone()))?;
                    self.step(StepRequest::To(count, Some(id)))
                        .map_err(render)?;
                }
            }
            _ => unreachable!("not a view command"),
        }
        Ok(())
    }
}

//! Running commands, and undoing them by snapshot (ADR 0081).
//!
//! Before a command runs, every document it names is copied, along with
//! the old value of each pass parameter it sets. A failure puts them back,
//! which is what makes a batch atomic; an undo puts them back too, and
//! keeps what it replaced as the redo. Nothing here needs a device: the
//! editor is one [`Host`], and the tests have another.
//!
//! The scene (ADR 0082) is a third document. A material snapshot also
//! records which scene material the canvas had open, so an undo after the
//! selection moved restores the right one.

use wxsl_core::{Graph, NodeRegistry, Value};

use crate::command::{edit_graph, Command, CommandError, Document, Outcome};
use crate::scene::{self, SceneChanges, SceneDoc};

/// How many steps the history keeps.
const DEPTH: usize = 200;

/// What a command runs against.
pub(crate) trait Host {
    /// A document, if it exists.
    fn graph(&self, document: Document) -> Option<&Graph>;
    /// A document to edit, opened if it never was, and its registry.
    fn open(&mut self, document: Document) -> (&mut Graph, &NodeRegistry);
    /// Put `graph` in place of a document. Returns the one it replaced.
    fn replace(&mut self, document: Document, graph: Graph) -> Graph;
    /// A running pass's effect parameter.
    fn pass_param(&self, pass: &str, name: &str) -> Option<Value>;
    /// Write a running pass's effect parameter.
    fn set_pass_param(&mut self, pass: &str, name: &str, value: Value) -> Result<(), String>;
    /// Write a material parameter to the running material's buffer.
    fn write_material_param(&mut self, name: &str, value: Value) -> Result<(), String>;
    /// A document changed; `rebuild` when what it compiles to did.
    fn changed(&mut self, document: Document, rebuild: bool);
    /// The open scene.
    fn scene(&self) -> Option<&SceneDoc>;
    /// The scene to edit, and the registry an import builds against.
    fn scene_mut(&mut self) -> (&mut Option<SceneDoc>, &NodeRegistry);
    /// The scene changed.
    fn scene_changed(&mut self, changes: SceneChanges);
    /// The scene material the material canvas edits, if any.
    fn material_slot(&self) -> Option<usize>;
    /// Edit scene material `slot` in the material canvas, or with `None`
    /// the graph the canvas had before.
    fn open_slot(&mut self, slot: Option<usize>);
    /// Select an instance: session state, not an undo step.
    fn select_instance(&mut self, instance: Option<usize>) -> Result<(), CommandError>;
}

/// The state a step replaced.
#[derive(Clone, Debug, Default)]
struct Snapshot {
    material: Option<Graph>,
    /// The scene material the canvas had open, with `material`.
    slot: Option<usize>,
    pipeline: Option<Graph>,
    /// The scene, with `None` inside for "no scene was open".
    scene: Option<Option<SceneDoc>>,
    /// `(pass, name, value)`, in the order they were first set.
    pass_params: Vec<(String, String, Value)>,
}

impl Snapshot {
    /// Whether it holds nothing to put back.
    fn is_empty(&self) -> bool {
        self.material.is_none()
            && self.pipeline.is_none()
            && self.scene.is_none()
            && self.pass_params.is_empty()
    }

    /// The state of everything `self` holds, as it is now.
    fn current(&self, host: &impl Host) -> Snapshot {
        Snapshot {
            material: self
                .material
                .as_ref()
                .and_then(|_| host.graph(Document::Material).cloned()),
            slot: host.material_slot(),
            pipeline: self
                .pipeline
                .as_ref()
                .and_then(|_| host.graph(Document::Pipeline).cloned()),
            scene: self.scene.as_ref().map(|_| host.scene().cloned()),
            pass_params: self
                .pass_params
                .iter()
                .filter_map(|(pass, name, _)| {
                    let value = host.pass_param(pass, name)?;
                    Some((pass.clone(), name.clone(), value))
                })
                .collect(),
        }
    }

    /// Put everything back.
    fn restore(self, host: &mut impl Host) {
        // The scene first: the material canvas may be editing one of its
        // materials, which the scene's restore resynchronizes.
        if let Some(scene) = self.scene {
            let changes = SceneChanges::between(host.scene(), scene.as_ref());
            *host.scene_mut().0 = scene;
            host.scene_changed(changes);
        }
        if let Some(graph) = self.material {
            if host.material_slot() != self.slot {
                host.open_slot(self.slot);
            }
            host.replace(Document::Material, graph);
            host.changed(Document::Material, true);
        }
        if let Some(graph) = self.pipeline {
            host.replace(Document::Pipeline, graph);
            host.changed(Document::Pipeline, true);
        }
        for (pass, name, value) in self.pass_params.into_iter().rev() {
            // The value was accepted once, so the pass took it.
            let _ = host.set_pass_param(&pass, &name, value);
        }
    }
}

/// The undo and redo stacks.
#[derive(Debug, Default)]
pub(crate) struct History {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The merge key of the gesture the top undo step belongs to, while
    /// that gesture lasts.
    open: Option<String>,
}

/// What a command changed, gathered across a batch.
#[derive(Default)]
struct Changed {
    rebuilt: [bool; 2],
    scene: SceneChanges,
}

impl History {
    /// How many steps can be undone.
    pub(crate) fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    /// How many steps can be redone.
    pub(crate) fn redo_depth(&self) -> usize {
        self.redo.len()
    }

    /// The current gesture is over: the next command is a new step.
    pub(crate) fn seal(&mut self) {
        self.open = None;
    }

    /// Run `command`. A `gesture` merges into the previous step when it
    /// repeats its target (see [`Command::merge_key`]).
    pub(crate) fn execute(
        &mut self,
        host: &mut impl Host,
        command: &Command,
        gesture: bool,
    ) -> Result<Outcome, CommandError> {
        match command {
            Command::Undo => return self.step(host, true).map(|()| Outcome::default()),
            Command::Redo => return self.step(host, false).map(|()| Outcome::default()),
            _ => {}
        }
        let mut before = Snapshot::default();
        if scene::mentions_scene(command) {
            before.scene = Some(host.scene().cloned());
        }
        for document in [Document::Material, Document::Pipeline] {
            if command.mentions(document) {
                let graph = host.open(document).0.clone();
                match document {
                    Document::Material => {
                        before.material = Some(graph);
                        before.slot = host.material_slot();
                    }
                    Document::Pipeline => before.pipeline = Some(graph),
                }
            }
        }
        let mut outcome = Outcome::default();
        let mut changed = Changed::default();
        if let Err(error) = run(host, command, &mut outcome, &mut before, &mut changed) {
            before.restore(host);
            return Err(error);
        }
        if !changed.scene.is_empty() {
            host.scene_changed(changed.scene);
        }
        for (document, graph, rebuild) in [
            (Document::Material, &before.material, changed.rebuilt[0]),
            (Document::Pipeline, &before.pipeline, changed.rebuilt[1]),
        ] {
            if graph.is_some() {
                host.changed(document, rebuild);
            }
        }
        // A selection or a save leaves nothing to undo.
        if before.is_empty() {
            return Ok(outcome);
        }

        let key = gesture.then(|| command.merge_key()).flatten();
        if key.is_none() || key != self.open {
            self.undo.push(before);
            if self.undo.len() > DEPTH {
                self.undo.remove(0);
            }
        }
        self.open = key;
        self.redo.clear();
        Ok(outcome)
    }

    /// Undo (`back`) or redo one step.
    fn step(&mut self, host: &mut impl Host, back: bool) -> Result<(), CommandError> {
        let (from, to) = if back {
            (&mut self.undo, &mut self.redo)
        } else {
            (&mut self.redo, &mut self.undo)
        };
        let snapshot = from.pop().ok_or(if back {
            CommandError::NothingToUndo
        } else {
            CommandError::NothingToRedo
        })?;
        to.push(snapshot.current(host));
        snapshot.restore(host);
        self.open = None;
        Ok(())
    }
}

/// Run one command, recording in `before` the old value of every pass
/// parameter it sets, and in `changed` what needs redoing.
fn run(
    host: &mut impl Host,
    command: &Command,
    outcome: &mut Outcome,
    before: &mut Snapshot,
    changed: &mut Changed,
) -> Result<(), CommandError> {
    match command {
        Command::Batch { commands } => {
            for (index, command) in commands.iter().enumerate() {
                run(host, command, outcome, before, changed).map_err(|error| {
                    CommandError::InBatch {
                        index,
                        error: Box::new(error),
                    }
                })?;
            }
        }
        Command::SetPassParam { pass, name, value } => {
            let old = host.pass_param(pass, name);
            host.set_pass_param(pass, name, *value)
                .map_err(CommandError::Render)?;
            if let Some(old) = old {
                if !before
                    .pass_params
                    .iter()
                    .any(|(p, n, _)| p == pass && n == name)
                {
                    before.pass_params.push((pass.clone(), name.clone(), old));
                }
            }
        }
        Command::Undo | Command::Redo => return Err(CommandError::HistoryInBatch),
        Command::SelectInstance { instance } => host.select_instance(*instance)?,
        Command::SaveScene { path } => host
            .scene()
            .ok_or(CommandError::NoScene)?
            .save(std::path::Path::new(path))?,
        _ if scene::mentions_scene(command) => {
            let (scene, registry) = host.scene_mut();
            let changes = scene::edit_scene(scene, registry, command)?;
            changed.scene.merge(changes);
        }
        _ => {
            let document = if command.mentions(Document::Pipeline) {
                Document::Pipeline
            } else {
                Document::Material
            };
            let (graph, registry) = host.open(document);
            edit_graph(graph, registry, command, outcome)?;
            if let Command::SetMaterialParam { name, value } = command {
                host.write_material_param(name, *value)
                    .map_err(CommandError::Render)?;
            }
            changed.rebuilt[document as usize] |= command.rebuilds();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use wxsl_core::node::GenericParam;
    use wxsl_core::{MacroValue, Node, NodeDefinition, NodeId, Socket, SocketRef, ValueType};

    use super::*;

    /// The documents and a table of pass parameters, without a device.
    /// Opening a scene material works as the editor's does: the canvas
    /// edits a copy, written back on every change.
    struct TestHost {
        material: Graph,
        pipeline: Option<Graph>,
        registry: NodeRegistry,
        pass_params: BTreeMap<(String, String), Value>,
        rebuilds: usize,
        scene: Option<SceneDoc>,
        slot: Option<usize>,
        loose: Option<Graph>,
        selected: Option<usize>,
    }

    impl Host for TestHost {
        fn graph(&self, document: Document) -> Option<&Graph> {
            match document {
                Document::Material => Some(&self.material),
                Document::Pipeline => self.pipeline.as_ref(),
            }
        }
        fn open(&mut self, document: Document) -> (&mut Graph, &NodeRegistry) {
            let graph = match document {
                Document::Material => &mut self.material,
                Document::Pipeline => self.pipeline.get_or_insert_with(|| Graph::new("pipeline")),
            };
            (graph, &self.registry)
        }
        fn replace(&mut self, document: Document, graph: Graph) -> Graph {
            match document {
                Document::Material => std::mem::replace(&mut self.material, graph),
                Document::Pipeline => self.pipeline.replace(graph).unwrap_or_default(),
            }
        }
        fn pass_param(&self, pass: &str, name: &str) -> Option<Value> {
            self.pass_params
                .get(&(pass.to_string(), name.to_string()))
                .copied()
        }
        fn set_pass_param(&mut self, pass: &str, name: &str, value: Value) -> Result<(), String> {
            let slot = self
                .pass_params
                .get_mut(&(pass.to_string(), name.to_string()))
                .ok_or_else(|| format!("no parameter {pass}.{name}"))?;
            *slot = value;
            Ok(())
        }
        fn write_material_param(&mut self, _: &str, _: Value) -> Result<(), String> {
            Ok(())
        }
        fn changed(&mut self, document: Document, rebuild: bool) {
            self.rebuilds += usize::from(rebuild);
            if let (Document::Material, Some(slot), Some(scene)) =
                (document, self.slot, self.scene.as_mut())
            {
                scene.document.materials[slot].graph = self.material.clone();
            }
        }
        fn scene(&self) -> Option<&SceneDoc> {
            self.scene.as_ref()
        }
        fn scene_mut(&mut self) -> (&mut Option<SceneDoc>, &NodeRegistry) {
            (&mut self.scene, &self.registry)
        }
        fn scene_changed(&mut self, _: SceneChanges) {
            let count = self
                .scene
                .as_ref()
                .map_or(0, |scene| scene.document.materials.len());
            match self.slot {
                Some(slot) if slot < count => {
                    self.material = self.scene.as_ref().unwrap().document.materials[slot]
                        .graph
                        .clone();
                }
                Some(_) => self.open_slot(None),
                None => {}
            }
        }
        fn material_slot(&self) -> Option<usize> {
            self.slot
        }
        fn open_slot(&mut self, slot: Option<usize>) {
            match slot {
                Some(slot) => {
                    let graph = self.scene.as_ref().unwrap().document.materials[slot]
                        .graph
                        .clone();
                    let previous = std::mem::replace(&mut self.material, graph);
                    if self.slot.is_none() {
                        self.loose = Some(previous);
                    }
                }
                None => {
                    if let Some(loose) = self.loose.take() {
                        self.material = loose;
                    }
                }
            }
            self.slot = slot;
        }
        fn select_instance(&mut self, instance: Option<usize>) -> Result<(), CommandError> {
            let scene = self.scene.as_ref().ok_or(CommandError::NoScene)?;
            let material = match instance {
                Some(index) => Some(
                    scene
                        .document
                        .instances
                        .get(index)
                        .ok_or(CommandError::NoSuchInstance(index))?
                        .material,
                ),
                None => None,
            };
            self.selected = instance;
            if material.is_some() {
                self.open_slot(material);
            }
            Ok(())
        }
    }

    fn host() -> TestHost {
        let mut registry = NodeRegistry::new();
        registry.register(
            NodeDefinition::builder("test.add", "Add")
                .generic_param(GenericParam::new(
                    "T",
                    vec![ValueType::F32, ValueType::Vec3],
                ))
                .input(Socket::new("a", ValueType::F32).generic("T"))
                .input(Socket::new("b", ValueType::F32).generic("T"))
                .output(Socket::new("out", ValueType::F32).generic("T"))
                .expr("{a} + {b}"),
        );
        registry.register(
            NodeDefinition::builder("test.three", "Three")
                .output(Socket::new("out", ValueType::Vec3))
                .expr("vec3f(1.0)"),
        );
        TestHost {
            material: Graph::new("material"),
            pipeline: None,
            registry,
            pass_params: BTreeMap::from([(
                ("bloom".to_string(), "strength".to_string()),
                Value::F32(0.5),
            )]),
            rebuilds: 0,
            scene: None,
            slot: None,
            loose: None,
            selected: None,
        }
    }

    /// Two cubes, each with its own material.
    fn two_cubes() -> SceneDoc {
        use wxsl_core::scene::{Instance, MaterialEntry, MeshEntry, MeshSource, Scene};
        let mut scene = Scene::new("two cubes");
        scene.add_mesh(MeshEntry::new("cube", MeshSource::Cube { size: 1.0 }));
        for name in ["red", "blue"] {
            let mut graph = Graph::new(name);
            graph.add_node("test.three");
            scene.add_material(MaterialEntry::new(name, graph));
        }
        scene.add_instance(Instance::new(0, 0).with_name("left"));
        scene.add_instance(Instance::new(0, 1).with_name("right"));
        SceneDoc::new(scene, None)
    }

    fn add(history: &mut History, host: &mut TestHost, def: &str) -> NodeId {
        let command = Command::AddNode {
            document: Document::Material,
            node: Node::new(def),
        };
        history.execute(host, &command, false).unwrap().added[0]
    }

    /// Every command, applied, undone and redone, leaves exactly the state
    /// it should: the round trip ADR 0081 promises.
    #[test]
    fn every_command_undoes_to_the_state_before_and_redoes_to_the_state_after() {
        let mut host = host();
        let mut history = History::default();
        let a = add(&mut history, &mut host, "test.add");
        let b = add(&mut history, &mut host, "test.add");
        let three = add(&mut history, &mut host, "test.three");
        let commands = [
            Command::Connect {
                document: Document::Material,
                from: SocketRef::new(three, "out"),
                to: SocketRef::new(a, "a"),
            },
            Command::Connect {
                document: Document::Material,
                from: SocketRef::new(a, "out"),
                to: SocketRef::new(b, "a"),
            },
            Command::SetGeneric {
                document: Document::Material,
                node: b,
                param: "T".into(),
                ty: ValueType::F32,
            },
            Command::SetParam {
                document: Document::Material,
                node: b,
                socket: "b".into(),
                value: Value::F32(2.0),
            },
            Command::SetMacro {
                document: Document::Material,
                name: "QUALITY".into(),
                value: Some(MacroValue::Int(2)),
            },
            Command::MoveNodes {
                document: Document::Material,
                positions: vec![(a, [10.0, 20.0]), (b, [30.0, 40.0])],
            },
            Command::SetLabel {
                document: Document::Material,
                node: a,
                label: Some("sum".into()),
            },
            Command::SetColor {
                document: Document::Material,
                node: a,
                color: Some([1.0, 0.0, 0.0]),
            },
            Command::Disconnect {
                document: Document::Material,
                to: SocketRef::new(a, "a"),
            },
            Command::TidyLayout {
                document: Document::Material,
            },
            Command::RemoveNodes {
                document: Document::Material,
                nodes: vec![a],
            },
            Command::AddNode {
                document: Document::Pipeline,
                node: Node::new("test.three"),
            },
            Command::SetPassParam {
                pass: "bloom".into(),
                name: "strength".into(),
                value: Value::F32(0.9),
            },
        ];
        for command in &commands {
            let before = (
                host.material.clone(),
                host.pipeline.clone(),
                host.pass_params.clone(),
            );
            history.execute(&mut host, command, false).unwrap();
            let after = (
                host.material.clone(),
                host.pipeline.clone(),
                host.pass_params.clone(),
            );
            assert_ne!(before, after, "{command:?} changed nothing");
            history.execute(&mut host, &Command::Undo, false).unwrap();
            let undone = (
                host.material.clone(),
                host.pipeline.clone(),
                host.pass_params.clone(),
            );
            assert_eq!(undone.0, before.0, "undo of {command:?}");
            assert_eq!(undone.2, before.2, "undo of {command:?}");
            history.execute(&mut host, &Command::Redo, false).unwrap();
            let redone = (
                host.material.clone(),
                host.pipeline.clone(),
                host.pass_params.clone(),
            );
            assert_eq!(redone, after, "redo of {command:?}");
        }
    }

    #[test]
    fn a_failed_batch_changes_nothing_and_records_nothing() {
        let mut host = host();
        let mut history = History::default();
        let a = add(&mut history, &mut host, "test.add");
        let before = host.material.clone();
        let depth = history.undo_depth();
        let error = history
            .execute(
                &mut host,
                &Command::Batch {
                    commands: vec![
                        Command::SetLabel {
                            document: Document::Material,
                            node: a,
                            label: Some("first".into()),
                        },
                        Command::SetPassParam {
                            pass: "bloom".into(),
                            name: "strength".into(),
                            value: Value::F32(1.0),
                        },
                        Command::RemoveNodes {
                            document: Document::Material,
                            nodes: vec![NodeId(99)],
                        },
                    ],
                },
                false,
            )
            .unwrap_err();
        assert_eq!(
            error,
            CommandError::InBatch {
                index: 2,
                error: Box::new(CommandError::NoSuchNode(NodeId(99))),
            }
        );
        assert_eq!(host.material, before);
        assert_eq!(host.pass_param("bloom", "strength"), Some(Value::F32(0.5)));
        assert_eq!(history.undo_depth(), depth);
    }

    #[test]
    fn a_gesture_is_one_step_until_it_is_sealed() {
        let mut host = host();
        let mut history = History::default();
        let a = add(&mut history, &mut host, "test.add");
        let start = host.material.clone();
        let depth = history.undo_depth();
        for x in 0..5 {
            let command = Command::MoveNodes {
                document: Document::Material,
                positions: vec![(a, [x as f32, 0.0])],
            };
            history.execute(&mut host, &command, true).unwrap();
        }
        assert_eq!(history.undo_depth(), depth + 1);
        history.seal();
        let command = Command::MoveNodes {
            document: Document::Material,
            positions: vec![(a, [9.0, 0.0])],
        };
        history.execute(&mut host, &command, true).unwrap();
        assert_eq!(history.undo_depth(), depth + 2);
        history.execute(&mut host, &Command::Undo, false).unwrap();
        history.execute(&mut host, &Command::Undo, false).unwrap();
        assert_eq!(host.material, start);
        // Commands from outside the UI never merge.
        for _ in 0..2 {
            history.execute(&mut host, &command, false).unwrap();
        }
        assert_eq!(history.undo_depth(), depth + 2);
    }

    #[test]
    fn a_new_change_clears_the_redo_and_an_empty_history_says_so() {
        let mut host = host();
        let mut history = History::default();
        assert_eq!(
            history.execute(&mut host, &Command::Undo, false),
            Err(CommandError::NothingToUndo)
        );
        add(&mut history, &mut host, "test.add");
        history.execute(&mut host, &Command::Undo, false).unwrap();
        assert_eq!(history.redo_depth(), 1);
        add(&mut history, &mut host, "test.three");
        assert_eq!(
            history.execute(&mut host, &Command::Redo, false),
            Err(CommandError::NothingToRedo)
        );
    }

    #[test]
    fn a_refused_command_names_its_cause() {
        let mut host = host();
        let mut history = History::default();
        let a = add(&mut history, &mut host, "test.add");
        let cases = [
            (
                Command::AddNode {
                    document: Document::Material,
                    node: Node::new("test.missing"),
                },
                CommandError::UnknownDefinition("test.missing".into()),
            ),
            (
                Command::Disconnect {
                    document: Document::Material,
                    to: SocketRef::new(a, "a"),
                },
                CommandError::NotConnected(SocketRef::new(a, "a")),
            ),
            (
                Command::SetParam {
                    document: Document::Material,
                    node: a,
                    socket: "c".into(),
                    value: Value::F32(1.0),
                },
                CommandError::UnknownSocket(SocketRef::new(a, "c")),
            ),
            (
                Command::SetMaterialParam {
                    name: "roughness".into(),
                    value: Value::F32(1.0),
                },
                CommandError::UnknownMaterialParam("roughness".into()),
            ),
        ];
        for (command, expected) in cases {
            assert_eq!(history.execute(&mut host, &command, false), Err(expected));
        }
        assert!(matches!(
            history.execute(
                &mut host,
                &Command::Connect {
                    document: Document::Material,
                    from: SocketRef::new(a, "out"),
                    to: SocketRef::new(a, "a"),
                },
                false
            ),
            Err(CommandError::Graph(_))
        ));
    }

    #[test]
    fn a_transcript_replays_to_the_same_graph() {
        let text = r#"
            // Two nodes and a wire.
            {"command":"add_node","node":{"def":"test.three"}}
            {"command":"add_node","node":{"def":"test.add","position":[200.0,0.0]}}
            {"command":"connect","from":{"node":1,"socket":"out"},"to":{"node":2,"socket":"a"}}
            {"command":"set_label","node":2,"label":"sum"}
            {"command":"undo"}
            {"command":"redo"}
        "#;
        let commands = Command::parse_transcript(text).unwrap();
        assert_eq!(commands.len(), 6);
        assert_eq!(
            Command::parse_transcript(&Command::to_transcript(&commands)).unwrap(),
            commands
        );
        let mut host = host();
        let mut history = History::default();
        for command in &commands {
            history.execute(&mut host, command, false).unwrap();
        }
        let sum = host.material.node(NodeId(2)).unwrap();
        assert_eq!(sum.label.as_deref(), Some("sum"));
        assert_eq!(sum.generics.get("T"), Some(&ValueType::Vec3));
        assert_eq!(host.material.edges().len(), 1);

        let error =
            Command::parse_transcript("{\"command\":\"undo\"}\n{\"command\":\"fly\"}").unwrap_err();
        assert_eq!(error.line, 2);
    }

    #[test]
    fn every_scene_command_undoes_and_redoes() {
        let mut host = host();
        host.scene = Some(two_cubes());
        let mut history = History::default();
        let moved = crate::scene::compose([1.0, 2.0, 3.0], [0.0, 45.0, 0.0], [1.0; 3]);
        let commands = [
            Command::SetTransform {
                instance: 1,
                transform: moved,
            },
            Command::SetInstanceMaterial {
                instance: 0,
                material: 1,
            },
            Command::SetInstanceName {
                instance: 0,
                name: "front".into(),
            },
            Command::SetMaterialTags {
                material: 0,
                tags: vec!["transparent".into(), "glass".into()],
            },
            Command::SetRenderOrder {
                material: 1,
                order: 4,
            },
            Command::SetMaxLayers {
                material: 1,
                layers: 2,
            },
            Command::RemoveInstances {
                instances: vec![1, 0, 1],
            },
            Command::CloseScene,
        ];
        for command in &commands {
            let before = host.scene.as_ref().map(|scene| scene.document.clone());
            history.execute(&mut host, command, false).unwrap();
            let after = host.scene.as_ref().map(|scene| scene.document.clone());
            assert_ne!(before, after, "{command:?} changed nothing");
            history.execute(&mut host, &Command::Undo, false).unwrap();
            let undone = host.scene.as_ref().map(|scene| scene.document.clone());
            assert_eq!(undone, before, "undo of {command:?}");
            history.execute(&mut host, &Command::Redo, false).unwrap();
            let redone = host.scene.as_ref().map(|scene| scene.document.clone());
            assert_eq!(redone, after, "redo of {command:?}");
            history.execute(&mut host, &Command::Undo, false).unwrap();
        }
    }

    #[test]
    fn a_selection_is_not_a_step_and_an_undo_edits_the_material_it_was_taken_on() {
        let mut host = host();
        host.scene = Some(two_cubes());
        let loose = host.material.clone();
        let mut history = History::default();
        history
            .execute(
                &mut host,
                &Command::SelectInstance { instance: Some(0) },
                false,
            )
            .unwrap();
        assert_eq!(history.undo_depth(), 0, "a selection is not an undo step");
        assert_eq!(host.material.name(), "red");
        let red = host.material.clone();
        history
            .execute(
                &mut host,
                &Command::SetLabel {
                    document: Document::Material,
                    node: NodeId(1),
                    label: Some("tint".into()),
                },
                false,
            )
            .unwrap();
        let scene = |host: &TestHost, index: usize| {
            host.scene.as_ref().unwrap().document.materials[index]
                .graph
                .clone()
        };
        assert_eq!(scene(&host, 0), host.material, "the scene's copy follows");

        // The selection moves to the blue cube; the undo restores red.
        history
            .execute(
                &mut host,
                &Command::SelectInstance { instance: Some(1) },
                false,
            )
            .unwrap();
        assert_eq!(host.material.name(), "blue");
        history.execute(&mut host, &Command::Undo, false).unwrap();
        assert_eq!(host.slot, Some(0));
        assert_eq!(scene(&host, 0), red);
        assert_eq!(host.material, red);

        // Closing the scene gives the canvas its own graph back.
        history
            .execute(&mut host, &Command::CloseScene, false)
            .unwrap();
        assert_eq!(host.slot, None);
        assert_eq!(host.material, loose);
    }

    #[test]
    fn a_scene_command_without_a_scene_or_a_target_names_the_cause() {
        let mut host = host();
        let mut history = History::default();
        let set = Command::SetInstanceName {
            instance: 5,
            name: "x".into(),
        };
        assert_eq!(
            history.execute(&mut host, &set, false),
            Err(CommandError::NoScene)
        );
        host.scene = Some(two_cubes());
        assert_eq!(
            history.execute(&mut host, &set, false),
            Err(CommandError::NoSuchInstance(5))
        );
        assert_eq!(
            history.execute(
                &mut host,
                &Command::SetLight {
                    light: 0,
                    color: [1.0; 3],
                    intensity: 1.0
                },
                false
            ),
            Err(CommandError::NoSuchLight(0))
        );
        assert_eq!(
            history.execute(
                &mut host,
                &Command::SetInstanceMaterial {
                    instance: 0,
                    material: 9
                },
                false
            ),
            Err(CommandError::NoSuchMaterial(9))
        );
        assert_eq!(history.undo_depth(), 0);
    }

    #[test]
    fn a_saved_scene_reads_back_and_an_imported_one_is_refused() {
        let directory = std::env::temp_dir().join(format!("wxsl-scene-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("two.scene.json");
        let mut host = host();
        host.scene = Some(two_cubes());
        let mut history = History::default();
        let save = Command::SaveScene {
            path: path.display().to_string(),
        };
        history.execute(&mut host, &save, false).unwrap();
        let saved = host.scene.clone().unwrap().document;
        history
            .execute(&mut host, &Command::CloseScene, false)
            .unwrap();
        history
            .execute(
                &mut host,
                &Command::OpenScene {
                    path: path.display().to_string(),
                },
                false,
            )
            .unwrap();
        assert_eq!(host.scene.as_ref().unwrap().document, saved);

        host.scene.as_mut().unwrap().document.meshes[0].source =
            wxsl_core::scene::MeshSource::Embedded { index: 0 };
        assert!(matches!(
            history.execute(&mut host, &save, false),
            Err(CommandError::Scene(_))
        ));
        let _ = std::fs::remove_dir_all(&directory);
    }
}

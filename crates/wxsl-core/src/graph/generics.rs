//! Generic sockets: how a node's type parameters resolve from its wires,
//! retype when a wire asks for another type, and are forgotten when the
//! wires that set them go.

use super::*;

impl Graph {
    /// A socket's effective type: `socket.ty` when it is neither generic nor
    /// combined, this node instance's resolution of [`Socket::generic`], or
    /// [`Socket::combine`]'s rule applied to two resolutions — `None` if
    /// whichever of those applies has not been resolved yet (or, for a
    /// combined socket, both are resolved but do not combine).
    ///
    /// A method rather than a free function on [`Socket`] because resolution
    /// is per graph *node*, not something a bare `Socket` (shared across
    /// every instance of its [`NodeDefinition`]) can answer alone. `codegen`
    /// is the other caller within this crate: once [`Graph::validate`] has
    /// passed, every generic or combined socket codegen touches resolves to
    /// `Some`. Public for the editor, which needs the same answer to colour
    /// a port or label an inspector row by what a socket actually carries,
    /// not the placeholder `socket.ty` shown before any instance resolved it.
    pub fn effective_type(&self, node: NodeId, socket: &Socket) -> Option<ValueType> {
        effective_type_of(socket, &self.nodes.get(&node)?.generics)
    }

    /// Resolve `node`'s generic parameter `param` to `ty`, then propagate
    /// that resolution across any edge already touching another socket that
    /// shares it — on `node` itself (every one of its own sockets sharing
    /// `param` sees the new value immediately, since resolution is stored
    /// once per node) and, transitively, on whatever `node` was already
    /// wired to through such a socket.
    ///
    /// Does not re-check those existing edges against the newly resolved
    /// type: nothing could have connected an incompatible one to a socket
    /// that was still unresolved in the first place — [`Graph::connect`]
    /// only reaches this once every edge already touching `param` agrees —
    /// so there is nothing to reconcile, only more of the graph to inform.
    pub(super) fn resolve_generic(
        &mut self,
        registry: &NodeRegistry,
        node: NodeId,
        param: &str,
        ty: ValueType,
    ) {
        let mut queue = VecDeque::from([(node, param.to_string())]);
        while let Some((node, param)) = queue.pop_front() {
            let Some(current) = self.nodes.get(&node) else {
                continue;
            };
            if current.generics.get(&param) == Some(&ty) {
                continue; // already settled, by this call or an earlier one
            }
            let Some(def) = registry.get(&current.def).cloned() else {
                continue;
            };
            self.nodes
                .get_mut(&node)
                .expect("looked up above")
                .generics
                .insert(param.clone(), ty);

            let shares_param = |socket: &&Socket| {
                socket
                    .generic
                    .as_ref()
                    .is_some_and(|name| name.as_str() == param)
            };
            for socket in def.inputs.iter().filter(shares_param) {
                let reference = SocketRef::new(node, socket.name.as_str());
                if let Some(edge) = self.edge_into(&reference) {
                    self.enqueue_generic_neighbor(
                        registry,
                        &edge.from,
                        Direction::Output,
                        &mut queue,
                    );
                }
            }
            for socket in def.outputs.iter().filter(shares_param) {
                for edge in self
                    .edges_from(node)
                    .filter(|e| e.from.socket == socket.name.as_str())
                {
                    let to = edge.to.clone();
                    self.enqueue_generic_neighbor(registry, &to, Direction::Input, &mut queue);
                }
            }

            for companion in self.seedable_companions(&def, node, &param, ty) {
                queue.push_back((node, companion));
            }
            self.retype_params(registry, node);
        }
    }

    /// The change to `node`'s generic parameters that would make `socket`
    /// carry `wanted`, or `None` if there is no such change.
    ///
    /// This is what makes "plug an `f32` into a socket showing `vec3f`"
    /// retype the node instead of being refused. A socket's type is either
    /// fixed (nothing to retype), one parameter (set it), or two combined
    /// by a [`TypeRule`] (set whichever of them makes the rule give
    /// `wanted` — one alone where that suffices, both otherwise).
    ///
    /// A plan is only returned when it breaks nothing: every parameter it
    /// touches must allow the type, every [`Socket::combine`] socket on the
    /// node must still derive a type, and every edge already touching the
    /// node must still join two sockets of the same type. That last
    /// condition is the whole difference between retyping and vandalism —
    /// the node adapts where it can, and a genuine conflict with wiring
    /// that already exists is still [`GraphError::TypeMismatch`], not a
    /// wire silently dropped.
    pub(super) fn plan_retype(
        &self,
        registry: &NodeRegistry,
        node: NodeId,
        def: &NodeDefinition,
        socket: &Socket,
        wanted: ValueType,
    ) -> Option<Vec<(String, ValueType)>> {
        let candidates: Vec<Vec<String>> = if let Some(param) = &socket.generic {
            vec![vec![param.as_str().to_string()]]
        } else if let Some(combined) = &socket.combine {
            let (a, b) = (
                combined.a.as_str().to_string(),
                combined.b.as_str().to_string(),
            );
            vec![vec![a.clone()], vec![b.clone()], vec![a, b]]
        } else {
            return None;
        };

        let current = self
            .nodes
            .get(&node)
            .map(|instance| instance.generics.clone())
            .unwrap_or_default();
        candidates.into_iter().find_map(|params| {
            if !params.iter().all(|param| {
                def.generic(param)
                    .is_some_and(|declared| declared.allowed.contains(&wanted))
            }) {
                return None;
            }
            let mut hypothetical = current.clone();
            for param in &params {
                hypothetical.insert(param.clone(), wanted);
            }
            if effective_type_of(socket, &hypothetical) != Some(wanted) {
                return None;
            }
            self.hypothesis_holds(registry, node, def, &hypothetical)
                .then(|| params.into_iter().map(|param| (param, wanted)).collect())
        })
    }

    /// Whether `hypothetical` is a resolution `node` could actually take:
    /// no combined socket is left with two types its rule cannot combine,
    /// and every edge already touching the node still joins two sockets of
    /// the same type.
    ///
    /// Neither check treats "not resolved yet" as a conflict. A combined
    /// socket with only one of its two parameters known is simply not
    /// derived yet, and an edge whose *other* end is unresolved is one
    /// [`Graph::resolve_generic`] propagates into — which is how a chain of
    /// generic nodes resolves together.
    fn hypothesis_holds(
        &self,
        registry: &NodeRegistry,
        node: NodeId,
        def: &NodeDefinition,
        hypothetical: &BTreeMap<String, ValueType>,
    ) -> bool {
        for socket in def.inputs.iter().chain(&def.outputs) {
            let Some(combined) = &socket.combine else {
                continue;
            };
            let (Some(&a), Some(&b)) = (
                hypothetical.get(combined.a.as_str()),
                hypothetical.get(combined.b.as_str()),
            ) else {
                continue;
            };
            if combined.rule.apply(a, b).is_none() {
                return false;
            }
        }
        for edge in &self.edges {
            let (mine, theirs, direction) = if edge.to.node == node {
                (def.input(&edge.to.socket), &edge.from, Direction::Output)
            } else if edge.from.node == node {
                (def.output(&edge.from.socket), &edge.to, Direction::Input)
            } else {
                continue;
            };
            let Some(mine) = mine else { continue };
            let (Some(becomes), Ok(Some(other))) = (
                effective_type_of(mine, hypothetical),
                self.socket_type(registry, theirs, direction),
            ) else {
                continue;
            };
            if becomes != other {
                return false;
            }
        }
        true
    }

    /// Bring `node`'s pinned parameters in line with its resolved types:
    /// convert any whose type no longer matches its socket, and pin one on
    /// every unconnected input that would otherwise have no value at all.
    ///
    /// Both halves exist so that resolving a type leaves a node *usable*
    /// rather than merely typed. A stale parameter is
    /// [`GraphError::ParamTypeMismatch`] — picking `vec3f` on a
    /// `math.add` whose operands were pinned to `0.5` has to carry them to
    /// `vec3f(0.5)`, not report an error — and a generic input has no fixed
    /// default to fall back on ([`Socket::default`] is always `None`
    /// there), so without a pinned value it is
    /// [`GraphError::MissingInput`] and the inspector has nothing to show.
    /// Only inputs nothing feeds get a value; a connected one keeps
    /// whatever the author last typed, converted, for when it is unplugged.
    pub(super) fn retype_params(&mut self, registry: &NodeRegistry, node: NodeId) {
        let Ok(def) = self.definition(registry, node).cloned() else {
            return;
        };
        let mut updates: Vec<(String, Value)> = Vec::new();
        for socket in &def.inputs {
            let Some(ty) = self.effective_type(node, socket) else {
                continue;
            };
            let name = socket.name.as_str();
            let pinned = self
                .nodes
                .get(&node)
                .and_then(|n| n.params.get(name))
                .copied();
            match pinned {
                Some(value) if value.ty() != ty => {
                    if let Some(converted) = value.converted_to(ty) {
                        updates.push((name.to_string(), converted));
                    }
                }
                Some(_) => {}
                None => {
                    let fed = socket.default_for(ty).is_some()
                        || socket.optional
                        || self.edge_into(&SocketRef::new(node, name)).is_some();
                    if !fed {
                        if let Some(value) = ty.splat(0.0).or_else(|| ty.zero()) {
                            updates.push((name.to_string(), value));
                        }
                    }
                }
            }
        }
        if let Some(instance) = self.nodes.get_mut(&node) {
            for (name, value) in updates {
                instance.params.insert(name, value);
            }
        }
    }

    /// The other parameters on `node` that resolving `param` to `ty` should
    /// carry along: for every [`Socket::combine`] socket reading `param`,
    /// the parameter on its other side.
    ///
    /// This is what keeps a two-parameter node usable from one connection.
    /// `math.multiply` declares `A` and `B` so that `f32 * vec3f` is
    /// expressible at all, but the overwhelmingly common case is two
    /// operands of the *same* type — so wiring a `vec3f` into `a` resolves
    /// `B` to `vec3f` as well, and the node is immediately complete instead
    /// of sitting on an unresolved `B` until the second operand is touched
    /// too. It is a default, not a constraint: pinning `B` afterwards
    /// ([`Graph::set_generic`]) or wiring something else into `b` changes it
    /// like any other resolution.
    ///
    /// A companion is only seeded when nothing else has a say — it is
    /// unresolved, `ty` is one of the types it allows, and *no* remaining
    /// edge touches a socket that reads it. That last condition is what
    /// keeps a guess from overriding evidence: if `b` is already wired to a
    /// chain of not-yet-anchored generic nodes, the type that chain
    /// eventually adopts decides `B`, not `A`.
    fn seedable_companions(
        &self,
        def: &NodeDefinition,
        node: NodeId,
        param: &str,
        ty: ValueType,
    ) -> Vec<String> {
        let mut companions: Vec<String> = Vec::new();
        for socket in def.inputs.iter().chain(&def.outputs) {
            let Some(combined) = &socket.combine else {
                continue;
            };
            let other = match (combined.a.as_str(), combined.b.as_str()) {
                (a, b) if a == param => b,
                (a, b) if b == param => a,
                _ => continue,
            };
            if other == param
                || companions.iter().any(|seen| seen == other)
                || self.generic_type(node, other).is_some()
                || !def
                    .generic(other)
                    .is_some_and(|declared| declared.allowed.contains(&ty))
                || self.param_has_an_edge(def, node, other)
            {
                continue;
            }
            companions.push(other.to_string());
        }
        companions
    }

    /// Whether any edge on `node` touches a socket whose effective type
    /// reads generic parameter `param` — the test for "something other than
    /// a guess has a say in what this parameter is".
    ///
    /// A [`Socket::combine`] socket counts: an edge on `multiply`'s output
    /// constrains `A` and `B` jointly (whatever they become, the rule
    /// applied to them has to stay equal to the other end's type) even
    /// though it cannot resolve either one by itself.
    fn param_has_an_edge(&self, def: &NodeDefinition, node: NodeId, param: &str) -> bool {
        let reads_param = |socket: &&Socket| {
            socket
                .referenced_params()
                .any(|name| name.as_str() == param)
        };
        def.inputs.iter().filter(reads_param).any(|socket| {
            self.edge_into(&SocketRef::new(node, socket.name.as_str()))
                .is_some()
        }) || def.outputs.iter().filter(reads_param).any(|socket| {
            self.edge_from(&SocketRef::new(node, socket.name.as_str()))
                .is_some()
        })
    }

    /// If `socket` (on the side named by `direction`) is itself generic,
    /// queue its `(node, parameter)` for [`Graph::resolve_generic`] to visit.
    fn enqueue_generic_neighbor(
        &self,
        registry: &NodeRegistry,
        socket: &SocketRef,
        direction: Direction,
        queue: &mut VecDeque<(NodeId, String)>,
    ) {
        let Ok(def) = self.definition(registry, socket.node) else {
            return;
        };
        let found = match direction {
            Direction::Input => def.input(&socket.socket),
            Direction::Output => def.output(&socket.socket),
        };
        if let Some(param) = found.and_then(|s| s.generic.as_ref()) {
            queue.push_back((socket.node, param.as_str().to_string()));
        }
    }

    /// Explicitly resolve `node`'s generic parameter `param` to `ty`.
    ///
    /// For a node nothing has connected yet (so [`Graph::connect`] never got
    /// a chance to adopt a type for it), or to change what an already-wired
    /// one resolved to. Repinning to a type incompatible with an edge
    /// already touching a socket that shares `param` **disconnects that
    /// edge** rather than rejecting the repin outright — changing what a
    /// node is supposed to invalidate wiring that no longer fits, the same
    /// way it always would have if `math.add.f32` and `math.add.vec3f` were
    /// still two different node kinds.
    ///
    /// Returns the edges that were disconnected as a result, so a caller
    /// (the editor) can report it rather than have wiring vanish silently.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::UnknownDefinition`]/[`GraphError::UnknownNode`]
    /// for a bad `node`, or [`GraphError::InvalidGenericType`] if `param`
    /// does not declare `ty` as one of its allowed types (including if
    /// `param` is not a generic parameter this node's definition declares
    /// at all, reported with an empty allowed list).
    pub fn set_generic(
        &mut self,
        registry: &NodeRegistry,
        node: NodeId,
        param: &str,
        ty: ValueType,
    ) -> Result<Vec<Edge>, GraphError> {
        let def = self.definition(registry, node)?.clone();
        let allowed = def
            .generic(param)
            .map(|declared| declared.allowed.clone())
            .unwrap_or_default();
        if !allowed.contains(&ty) {
            return Err(GraphError::InvalidGenericType {
                node,
                param: param.to_string(),
                ty,
                allowed,
            });
        }

        // Every socket this repin touches: directly (`Socket::generic` names
        // `param`) or as one of the two operands a combined socket derives
        // from (`Socket::combine` names `param` as either one) — repinning
        // `A` can just as well break `multiply`'s output as it can break
        // `a` itself.
        let references_param = |socket: &&Socket| {
            socket
                .referenced_params()
                .any(|name| name.as_str() == param)
        };
        // What every referencing socket would resolve to *after* the repin,
        // not before — a hypothetical map with only `param` changed, so a
        // combined socket's other parameter (still whatever it already was)
        // is combined against the *new* value being pinned.
        let mut hypothetical = self
            .nodes
            .get(&node)
            .map(|instance| instance.generics.clone())
            .unwrap_or_default();
        hypothetical.insert(param.to_string(), ty);

        // Every input this repin would leave mismatched: an existing edge
        // whose *other* end's effective type is no longer what this socket
        // would become. Collected before mutating anything, since
        // disconnecting is decided against the state before the repin.
        let mut mismatched = Vec::new();
        for socket in def.inputs.iter().filter(references_param) {
            let reference = SocketRef::new(node, socket.name.as_str());
            if let Some(edge) = self.edge_into(&reference) {
                let other = self
                    .socket_type(registry, &edge.from, Direction::Output)
                    .unwrap_or(None);
                if other != effective_type_of(socket, &hypothetical) {
                    mismatched.push(reference);
                }
            }
        }
        for socket in def.outputs.iter().filter(references_param) {
            for edge in self
                .edges_from(node)
                .filter(|edge| edge.from.socket == socket.name.as_str())
            {
                let other = self
                    .socket_type(registry, &edge.to, Direction::Input)
                    .unwrap_or(None);
                if other != effective_type_of(socket, &hypothetical) {
                    mismatched.push(edge.to.clone());
                }
            }
        }

        let disconnected = mismatched
            .into_iter()
            .filter_map(|input| self.disconnect(registry, &input))
            .collect();

        self.nodes
            .get_mut(&node)
            .expect("checked to exist by `definition` above")
            .generics
            .insert(param.to_string(), ty);
        // Picking one of `multiply`'s two operand types by hand should leave
        // the node usable, exactly as wiring one of them does — see
        // `Graph::seedable_companions` and `Graph::retype_params`.
        for companion in self.seedable_companions(&def, node, param, ty) {
            self.resolve_generic(registry, node, &companion, ty);
        }
        self.retype_params(registry, node);
        Ok(disconnected)
    }

    /// This node instance's resolved concrete type for generic parameter
    /// `param`, or `None` if it has not been resolved (or the node has no
    /// such parameter). For the editor: what to show next to a "pick a
    /// type" control, and whether to show one at all.
    pub fn generic_type(&self, node: NodeId, param: &str) -> Option<ValueType> {
        self.nodes.get(&node)?.generics.get(param).copied()
    }

    /// Bring `node`'s generic resolutions back in line with its remaining
    /// edges, after one was removed: forget every parameter no remaining
    /// edge has a say in — falling back to
    /// [`NodeDefinition::default_generics`] rather than to nothing — then
    /// re-seed the companions of those that survived.
    ///
    /// Forgetting is what stops a node that adopted, say, `vec3f` from a
    /// since-removed wire from staying `vec3f` with nothing connected to
    /// say so; unwiring a node completely returns it to the type it had
    /// when it was placed, not to no type at all — an unresolved parameter
    /// is an error, and a node nobody has broken should not report one.
    /// Retyping on the next connection is what makes that safe, so the
    /// default is never a trap (see [`Graph::plan_retype`]). Re-seeding is
    /// what stops the
    /// forgetting from undoing a companion resolution that is still earned:
    /// unplugging `multiply`'s output leaves `a` wired and `A` resolved, and
    /// `B` — seeded from `A` in the first place — should simply be seeded
    /// again rather than left blank. See [`Graph::seedable_companions`].
    ///
    /// Deliberately local rather than transitive: two generic nodes wired
    /// together, anchored by a third concrete one, keep whatever they
    /// resolved to if the edge removed is between the *two generic* nodes
    /// (each still touches the other), even though neither is connected to
    /// the concrete anchor anymore once that edge specifically is the one
    /// that goes. A full re-derivation would need a reachability search over
    /// every remaining edge on every disconnect; this covers the case that
    /// actually gets reported (a node with no remaining connections at all
    /// stuck on a stale resolution) without paying for that.
    pub(super) fn refresh_generics(&mut self, registry: &NodeRegistry, node: NodeId) {
        let Ok(def) = self.definition(registry, node).cloned() else {
            return;
        };
        let kept: Vec<(String, ValueType)> = def
            .generics
            .iter()
            .filter(|param| self.param_has_an_edge(&def, node, param.name.as_str()))
            .filter_map(|param| {
                self.generic_type(node, param.name.as_str())
                    .map(|ty| (param.name.as_str().to_string(), ty))
            })
            .collect();
        let Some(instance) = self.nodes.get_mut(&node) else {
            return;
        };
        instance
            .generics
            .retain(|name, _| kept.iter().any(|(survivor, _)| survivor == name));
        for (param, ty) in kept {
            for companion in self.seedable_companions(&def, node, &param, ty) {
                self.resolve_generic(registry, node, &companion, ty);
            }
        }
        // Whatever neither an edge nor a seed accounts for falls back to the
        // default, so a node the author has merely unplugged is never left
        // reporting an unresolved parameter. Ordered after the re-seeding so
        // a default never pre-empts a type something still asks for.
        let defaults = def.default_generics();
        if let Some(instance) = self.nodes.get_mut(&node) {
            for (param, ty) in defaults {
                instance.generics.entry(param).or_insert(ty);
            }
        }
        self.retype_params(registry, node);
    }
}

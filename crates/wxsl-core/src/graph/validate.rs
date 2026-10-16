//! [`Graph::validate`]: every invariant checked at once, so a document
//! read from disk reports all its problems together.

use super::*;

impl Graph {
    /// Check every invariant against `registry`, reporting all problems found.
    pub fn validate(&self, registry: &NodeRegistry) -> Result<(), GraphErrors> {
        let mut errors = Vec::new();

        for (id, node) in self.nodes() {
            let Some(def) = registry.get(&node.def) else {
                errors.push(GraphError::UnknownDefinition {
                    node: id,
                    def: node.def.clone(),
                });
                continue;
            };
            // What kind of graph this is decides which half of the
            // vocabulary is available (ADR 0040). Reported here rather than
            // refused by `Graph::add`, for the reason every other typing
            // error is: a document may arrive wrong, and a node pasted from
            // a material into an effect should say what is wrong with it.
            if !def.domains.allows(self.domain) {
                errors.push(GraphError::NodeOutsideDomain {
                    node: id,
                    def: node.def.clone(),
                    domain: self.domain,
                    allowed: def.domains.to_string(),
                });
            }
            self.check_params(id, node, def, &mut errors);
            // Every generic parameter this node's definition declares must
            // be resolved for *some* instance to mean anything concrete —
            // the graph-level analogue of `MissingInput` for a mandatory
            // socket. Once `Graph::connect`/`Graph::set_generic` are the
            // only way in, this only fires for a node nothing has ever
            // touched, or a hand-edited document that bypassed both.
            for param in &def.generics {
                if !node.generics.contains_key(param.name.as_str()) {
                    errors.push(GraphError::UnresolvedGeneric {
                        node: id,
                        param: param.name.as_str().to_string(),
                    });
                }
            }
            // A combined socket (`Socket::combine`) whose two parameters
            // are both resolved, but to types its rule does not combine — e.g.
            // `multiply` fed `vec2f` and `vec3f`. Skipped when either
            // parameter is still unresolved: that is already reported above,
            // more precisely, and there is nothing sound to combine yet.
            for socket in def.inputs.iter().chain(&def.outputs) {
                let Some(combined) = &socket.combine else {
                    continue;
                };
                let (Some(&a_ty), Some(&b_ty)) = (
                    node.generics.get(combined.a.as_str()),
                    node.generics.get(combined.b.as_str()),
                ) else {
                    continue;
                };
                if combined.rule.apply(a_ty, b_ty).is_none() {
                    errors.push(GraphError::IncompatibleGenerics {
                        node: id,
                        socket: socket.name.as_str().to_string(),
                        rule: combined.rule,
                        a: (combined.a.as_str().to_string(), a_ty),
                        b: (combined.b.as_str().to_string(), b_ty),
                    });
                }
            }
        }

        self.check_declarations(registry, &mut errors);

        let mut connected_inputs: BTreeSet<SocketRef> = BTreeSet::new();
        for edge in &self.edges {
            self.check_edge(registry, edge, &mut connected_inputs, &mut errors);
        }

        // Required inputs must be fed by an edge, a parameter or a default.
        for (id, node) in self.nodes() {
            let Some(def) = registry.get(&node.def) else {
                continue;
            };
            for socket in &def.inputs {
                let name = socket.name.as_str();
                let reference = SocketRef::new(id, name);
                let fed = connected_inputs.contains(&reference)
                    || node.params.contains_key(name)
                    || self
                        .effective_type(id, socket)
                        .and_then(|ty| socket.default_for(ty))
                        .is_some()
                    || socket.optional;
                if !fed {
                    // A generic socket that is also unresolved was already
                    // reported, more precisely, by the loop above; restating
                    // it as `MissingInput` too would only be noise.
                    let unresolved_generic = socket
                        .generic
                        .as_ref()
                        .is_some_and(|param| !node.generics.contains_key(param.as_str()));
                    if !unresolved_generic {
                        errors.push(GraphError::MissingInput { socket: reference });
                    }
                }
            }
        }

        if let Err(error) = self.topological_order(None) {
            errors.push(error);
        }
        if let Err(GraphErrors(macro_errors)) = self.effective_macros(registry) {
            errors.extend(macro_errors);
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(GraphErrors(errors))
        }
    }

    /// Check everything a graph *declares* rather than computes: node
    /// settings, the names they carry, and the application block.
    ///
    /// Over every node rather than only the reachable ones, like the rest
    /// of validation — an unreachable node with a broken declaration is
    /// still broken, and it will be reachable the moment it is wired up.
    /// A freshly dropped declaring node is valid, because its setting
    /// carries a usable default.
    fn check_declarations(&self, registry: &NodeRegistry, errors: &mut Vec<GraphError>) {
        self.check_user_block(errors);
        self.check_attributes(errors);
        self.check_bakes(registry, errors);
        self.check_outputs(registry, errors);
        // One name is one binding, so two nodes naming the same parameter
        // are the same parameter — which is a feature, as long as they
        // agree about its type.
        let mut declared: BTreeMap<String, ValueType> = BTreeMap::new();

        for (id, instance) in self.nodes() {
            let Some(def) = registry.get(&instance.def) else {
                continue;
            };
            for name in instance.settings.keys() {
                if def.setting(name).is_none() {
                    errors.push(GraphError::UnknownSetting {
                        node: id,
                        setting: name.clone(),
                    });
                }
            }
            let setting_name = match def.body {
                NodeBody::Param | NodeBody::Resource | NodeBody::AttributeRead => {
                    node::SETTING_NAME
                }
                NodeBody::UserRead => node::SETTING_FIELD,
                _ => continue,
            };
            let raw = self
                .setting(registry, id, setting_name)
                .unwrap_or_default()
                .trim()
                .to_string();
            let Some(name) = WxslIdent::new(&raw) else {
                let reason = if raw.is_empty() {
                    "is empty; a declaration has to be called something"
                } else {
                    "is not a valid WXSL identifier"
                };
                errors.push(GraphError::InvalidSetting {
                    node: id,
                    setting: setting_name.to_string(),
                    value: raw,
                    reason: reason.to_string(),
                });
                continue;
            };
            // A texture becomes a module-scope `var` of that exact name, so
            // it can collide with what the generated module already uses.
            // A parameter is a struct field and cannot, but rejecting both
            // the same way is one rule to remember instead of two.
            if let Some(reason) = reserved_reason(name.as_str()) {
                errors.push(GraphError::InvalidSetting {
                    node: id,
                    setting: setting_name.to_string(),
                    value: name.as_str().to_string(),
                    reason,
                });
                continue;
            }

            let Some(socket) = def.outputs.first() else {
                continue;
            };
            match def.body {
                NodeBody::Param | NodeBody::Resource => {
                    let Some(ty) = self.effective_type(id, socket) else {
                        // An unresolved generic, already reported above.
                        continue;
                    };
                    match declared.get(name.as_str()) {
                        Some(&first) if first != ty => {
                            errors.push(GraphError::ConflictingDeclaration {
                                name: name.as_str().to_string(),
                                first,
                                second: ty,
                            });
                        }
                        _ => {
                            declared.insert(name.as_str().to_string(), ty);
                        }
                    }
                }
                NodeBody::UserRead => {
                    let Some(block) = &self.user_block else {
                        errors.push(GraphError::NoUserBlock { node: id });
                        continue;
                    };
                    let Some(field) = block
                        .fields
                        .iter()
                        .find(|field| field.name.trim() == name.as_str())
                    else {
                        errors.push(GraphError::UnknownUserField {
                            node: id,
                            field: name.as_str().to_string(),
                            declared: block
                                .fields
                                .iter()
                                .map(|field| field.name.clone())
                                .collect(),
                        });
                        continue;
                    };
                    // The one socket whose type comes from the *document*
                    // rather than from the definition or an edge, so it is
                    // the one place a resolved generic can be wrong rather
                    // than merely missing.
                    if let Some(resolved) = self.effective_type(id, socket) {
                        if resolved != field.ty {
                            errors.push(GraphError::UserFieldTypeMismatch {
                                node: id,
                                field: name.as_str().to_string(),
                                resolved,
                                declared: field.ty,
                            });
                        }
                    }
                }
                NodeBody::AttributeRead => {
                    let Some(decl) = self.attribute(name.as_str()) else {
                        errors.push(GraphError::UnknownAttribute {
                            node: id,
                            attribute: name.as_str().to_string(),
                            declared: self
                                .attributes
                                .iter()
                                .map(|decl| decl.name.clone())
                                .collect(),
                        });
                        continue;
                    };
                    // Like a user-block field, and unlike a parameter:
                    // the type is the *document's*, so a resolved generic
                    // here can be wrong rather than merely missing.
                    if let Some(resolved) = self.effective_type(id, socket) {
                        if resolved != decl.ty {
                            errors.push(GraphError::AttributeTypeMismatch {
                                node: id,
                                attribute: name.as_str().to_string(),
                                resolved,
                                declared: decl.ty,
                            });
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// One terminal of each kind at most, and nothing wired into a
    /// terminal that cannot compile in that terminal's stage.
    ///
    /// The second half is the only typing rule partitioning needs. A
    /// missing surface output is [`crate::codegen::generate`]'s to
    /// report, because a graph mid-edit legitimately has none.
    fn check_outputs(&self, registry: &NodeRegistry, errors: &mut Vec<GraphError>) {
        for (kind, found) in [
            (abi::SURFACE_OUTPUT_ID, self.surface_outputs(registry)),
            (abi::VERTEX_OUTPUT_ID, self.vertex_outputs(registry)),
            (abi::DISCARD_OUTPUT_ID, self.discard_outputs(registry)),
            (abi::FRAGMENT_OUTPUT_ID, self.fragment_outputs(registry)),
        ] {
            if found.len() > 1 {
                errors.push(GraphError::DuplicateOutput {
                    kind: kind.to_string(),
                    nodes: found,
                });
            }
        }
        self.check_varying_outputs(registry, errors);

        // The stage rules — a vertex-only node that ends up computed in
        // the fragment stage, a computed interpolant read in the stage
        // that computes it — are the analysis's to state now, because it
        // is the analysis that decides where every node runs (plan2 P9).
        // Running it here, against whatever terminals exist, is what
        // keeps the editor's problem panel and the compiler agreeing.
        if let Some(outputs) = self.outputs(registry) {
            if let Err(failures) = crate::stages::analyze(self, registry, &outputs) {
                errors.extend(failures.0);
            }
        }
    }

    /// The name a [`NodeBody::AttributeRead`] node reads, if that is what
    /// `node` is.
    pub(crate) fn attribute_read_name(
        &self,
        registry: &NodeRegistry,
        node: NodeId,
    ) -> Option<WxslIdent> {
        let def = registry.get(&self.nodes.get(&node)?.def)?;
        matches!(def.body, NodeBody::AttributeRead)
            .then(|| self.declared_name(registry, node, node::SETTING_NAME))
            .flatten()
    }

    /// Every declared interpolant is written exactly once, and every
    /// writer names one.
    ///
    /// Both halves matter and for different reasons: a declaration
    /// nothing writes spends an inter-stage location on an undefined
    /// value, and a writer naming nothing has no location to write to.
    fn check_varying_outputs(&self, registry: &NodeRegistry, errors: &mut Vec<GraphError>) {
        let mut written: BTreeMap<String, Vec<NodeId>> = BTreeMap::new();
        for node in self.varying_outputs(registry) {
            let Some(name) = self.declared_name(registry, node, node::SETTING_NAME) else {
                errors.push(GraphError::InvalidAttribute {
                    reason: format!(
                        "the interpolant output on node {} names no interpolant",
                        node.0
                    ),
                });
                continue;
            };
            match self.attribute(name.as_str()) {
                Some(decl) if decl.frequency == AttributeFrequency::Computed => {}
                Some(decl) => errors.push(GraphError::InvalidAttribute {
                    reason: format!(
                        "`{name}` is declared as a {} attribute, and the geometry \
                         supplies it — only a computed one is written by the graph",
                        decl.frequency
                    ),
                }),
                None => errors.push(GraphError::UnknownAttribute {
                    node,
                    attribute: name.as_str().to_string(),
                    declared: self
                        .attributes
                        .iter()
                        .map(|decl| decl.name.clone())
                        .collect(),
                }),
            }
            written
                .entry(name.as_str().to_string())
                .or_default()
                .push(node);
        }
        for (name, nodes) in &written {
            if nodes.len() > 1 {
                errors.push(GraphError::DuplicateOutput {
                    kind: format!("{} `{name}`", abi::VARYING_OUTPUT_ID),
                    nodes: nodes.clone(),
                });
            }
        }
        for decl in &self.attributes {
            if decl.frequency != AttributeFrequency::Computed {
                continue;
            }
            if !written.contains_key(decl.name.trim()) {
                errors.push(GraphError::InvalidAttribute {
                    reason: format!(
                        "`{}` is a computed interpolant and nothing writes it; add an \
                         `{}` node or drop the declaration",
                        decl.name.trim(),
                        abi::VARYING_OUTPUT_ID,
                    ),
                });
            }
        }
    }

    /// The declared attribute set, independent of whether anything reads
    /// it — and the one accountant for the inter-stage location budget.
    ///
    /// Everything here is checked against a *limit* rather than against
    /// another declaration, which is what makes it worth doing in one
    /// place: a graph that overruns the varying budget or the vertex
    /// buffer budget should be told which limit and by how much, not
    /// discover it as a shader-compiler error about location 16.
    fn check_attributes(&self, errors: &mut Vec<GraphError>) {
        let mut invalid = |reason: String| errors.push(GraphError::InvalidAttribute { reason });
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut per_vertex = 0usize;
        for decl in &self.attributes {
            let name = decl.name.trim();
            let Some(ident) = WxslIdent::new(name) else {
                invalid(format!("`{name}` is not a valid WXSL attribute name"));
                continue;
            };
            if !seen.insert(name) {
                invalid(format!("`{name}` is declared twice"));
            }
            if let Some(reason) = reserved_reason(ident.as_str()) {
                invalid(format!("`{name}` {reason}"));
            }
            if abi::INSTANCE_BASE_FIELDS
                .iter()
                .any(|base| base.name == name)
            {
                invalid(format!(
                    "`{name}` is a field every instance row already has, \
                     and the transform lives there",
                ));
            }
            match decl.frequency {
                AttributeFrequency::Vertex => {
                    per_vertex += 1;
                    if let Some(reason) = vertex_attribute_reason(decl.ty) {
                        invalid(format!("`{name}`: {reason}"));
                    }
                }
                AttributeFrequency::Instance => {
                    if decl.ty.is_resource() {
                        invalid(format!(
                            "`{name}` is {}, and an instance row holds values \
                             rather than resources",
                            decl.ty
                        ));
                    }
                }
                AttributeFrequency::Computed => {
                    // The same four types a per-vertex stream may be, and
                    // for the neighbouring reason: this one *is* the
                    // `@location`, and a matrix has no interpolation.
                    if !abi::INTERPOLANT_TYPES.contains(&decl.ty) {
                        invalid(format!(
                            "`{name}` is {}, and an interpolated value is a \
                             float or a float vector",
                            decl.ty
                        ));
                    }
                }
            }
        }
        if per_vertex > abi::MAX_VERTEX_ATTRIBUTES {
            invalid(format!(
                "{per_vertex} per-vertex attributes declared, and the budget is \
                 {} — each is a vertex buffer slot of its own",
                abi::MAX_VERTEX_ATTRIBUTES,
            ));
        }
        // The accountant: the base varyings, the instance index if
        // anything needs it, and one per declared per-vertex attribute.
        let geometry = GeometryInterface::new(
            self.declared_attributes(AttributeFrequency::Vertex),
            self.declared_attributes(AttributeFrequency::Instance),
            self.declared_attributes(AttributeFrequency::Computed),
        );
        let used = geometry.varyings_used();
        if used > abi::MAX_VARYING_LOCATIONS {
            invalid(format!(
                "{used} inter-stage locations used, and there are {}: \
                 {} for the shading basis{}, {} declared per-vertex \
                 attribute(s) and {} computed interpolant(s)",
                abi::MAX_VARYING_LOCATIONS,
                abi::VERTEX_OUT_FIELDS.len(),
                if geometry.instance_index_location().is_some() {
                    " plus one for the instance index"
                } else {
                    ""
                },
                geometry.vertex().len(),
                geometry.computed().len(),
            ));
        }
    }

    /// The bake declarations, independent of whether any output reaches
    /// the named node — like the attributes, a declaration that is unusable
    /// says so when it is written rather than when something first bakes.
    fn check_bakes(&self, registry: &NodeRegistry, errors: &mut Vec<GraphError>) {
        let mut invalid = |decl: &BakeDecl, reason: String| {
            errors.push(GraphError::InvalidBake {
                texture: decl.texture.clone(),
                reason,
            })
        };
        let mut seen_nodes: BTreeSet<NodeId> = BTreeSet::new();
        for decl in &self.bakes {
            if WxslIdent::new(decl.texture.trim()).is_none() {
                invalid(
                    decl,
                    format!("`{}` is not a valid WXSL identifier", decl.texture),
                );
                continue;
            }
            if decl.size[0] == 0 || decl.size[1] == 0 {
                invalid(decl, "the bake table's size has a zero side".to_string());
            }
            match decl.precision {
                // A bake writes its table through a write-only storage
                // binding, and these are the two colour precisions that
                // can. (`scalar` and `pair` are R8Unorm and Rg16Float,
                // which no baseline device stores into.)
                crate::abi::GBufferPrecision::Normalized
                | crate::abi::GBufferPrecision::HighDynamicRange => {}
                other => invalid(
                    decl,
                    format!(
                        "precision `{}` cannot be a bake target — a bake \
                         writes through storage, so it is `standard` or `hdr`",
                        other.name()
                    ),
                ),
            }
            if !seen_nodes.insert(decl.node) {
                invalid(decl, "the node is baked twice".to_string());
            }
            let Some(instance) = self.nodes.get(&decl.node) else {
                invalid(decl, "names a node the graph does not contain".to_string());
                continue;
            };
            let Some(def) = registry.get(&instance.def) else {
                // Already reported as `UnknownDefinition`; the bake has
                // nothing of its own to add.
                continue;
            };
            let socket_name = decl.socket.as_deref().map(str::trim).unwrap_or(
                def.outputs
                    .first()
                    .map(|socket| socket.name.as_str())
                    .unwrap_or(""),
            );
            let Some(socket) = def
                .outputs
                .iter()
                .find(|socket| socket.name.as_str() == socket_name)
            else {
                invalid(
                    decl,
                    format!(
                        "node {} has no output socket `{socket_name}` to bake",
                        decl.node
                    ),
                );
                continue;
            };
            if !matches!(
                socket.ty,
                ValueType::F32 | ValueType::Vec2 | ValueType::Vec3 | ValueType::Vec4
            ) {
                invalid(
                    decl,
                    format!(
                        "bakes `{}`, and a bake table stores one value per \
                         texel: a float or a float vector",
                        socket.ty
                    ),
                );
            }
            // The bake replaces the node wholesale when it is active, so
            // anything else reading its other outputs would read nothing.
            for other in &def.outputs {
                if other.name == socket.name {
                    continue;
                }
                if self
                    .edge_from(&SocketRef::new(decl.node, other.name.as_str()))
                    .is_some()
                {
                    invalid(
                        decl,
                        format!(
                            "node {} also feeds something from `{}` — a bake \
                             stands in for the whole node, so its other outputs \
                             must be unused",
                            decl.node, other.name
                        ),
                    );
                }
            }
            // A bake table and a texture node under one name would be two
            // resources at one binding.
            let collides = self.nodes().any(|(id, instance)| {
                instance.def.starts_with("texture.")
                    && self
                        .setting(registry, id, crate::node::SETTING_NAME)
                        .is_some_and(|name| name.trim() == decl.texture.trim())
            });
            if collides {
                invalid(
                    decl,
                    format!(
                        "`{}` is also the name of a declared texture — one name \
                         is one binding",
                        decl.texture
                    ),
                );
            }
        }
    }

    /// The application block's own declaration, independent of whether any
    /// node reads it: a graph that carries an unusable one should say so
    /// when it is written, not when something first reads it.
    fn check_user_block(&self, errors: &mut Vec<GraphError>) {
        let Some(block) = &self.user_block else {
            return;
        };
        let mut invalid = |reason: String| errors.push(GraphError::InvalidUserBlock { reason });
        if WxslIdent::new(block.name.trim()).is_none() {
            invalid(format!(
                "`{}` is not a valid WXSL identifier for the block itself",
                block.name
            ));
        }
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for field in &block.fields {
            let name = field.name.trim();
            if WxslIdent::new(name).is_none() {
                invalid(format!("`{name}` is not a valid WXSL field name"));
                continue;
            }
            if !seen.insert(name) {
                invalid(format!("`{name}` is declared twice"));
            }
            if field.ty.is_resource() {
                // The block is one uniform buffer. A texture in the
                // application group is the application's business, and
                // nothing here would know how to bind it.
                invalid(format!(
                    "`{name}` is {}, and a uniform block holds values rather than resources",
                    field.ty
                ));
            }
        }
    }

    fn check_params(
        &self,
        id: NodeId,
        node: &Node,
        def: &NodeDefinition,
        errors: &mut Vec<GraphError>,
    ) {
        for (name, value) in &node.params {
            match def.input(name) {
                None => errors.push(GraphError::UnknownParam {
                    node: id,
                    param: name.clone(),
                }),
                Some(socket) => {
                    // A generic socket's declared `ty` is only a
                    // placeholder; check against this instance's resolution
                    // instead. If it has none yet, `UnresolvedGeneric`
                    // already reports the underlying problem — there is no
                    // sound expected type to compare the param against.
                    if let Some(expected) = self.effective_type(id, socket) {
                        if expected != value.ty() {
                            errors.push(GraphError::ParamTypeMismatch {
                                socket: SocketRef::new(id, name),
                                supplied: value.ty(),
                                expected,
                            });
                        }
                    }
                }
            }
        }
    }

    fn check_edge(
        &self,
        registry: &NodeRegistry,
        edge: &Edge,
        connected_inputs: &mut BTreeSet<SocketRef>,
        errors: &mut Vec<GraphError>,
    ) {
        // `Ok(None)` (an unresolved generic) is deliberately not unwrapped
        // into an error here: `Graph::validate`'s own `UnresolvedGeneric`
        // pass already reports it once per node, and there is nothing sound
        // to compare an unresolved type against anyway.
        let produced = match self.socket_type(registry, &edge.from, Direction::Output) {
            Ok(ty) => ty,
            Err(error) => {
                errors.push(error);
                None
            }
        };
        let expected = match self.socket_type(registry, &edge.to, Direction::Input) {
            Ok(ty) => ty,
            Err(error) => {
                errors.push(error);
                None
            }
        };
        if let (Some(produced), Some(expected)) = (produced, expected) {
            if produced != expected {
                errors.push(GraphError::TypeMismatch {
                    from: edge.from.clone(),
                    to: edge.to.clone(),
                    produced,
                    expected,
                });
            }
        }
        if !connected_inputs.insert(edge.to.clone()) {
            errors.push(GraphError::InputAlreadyConnected {
                socket: edge.to.clone(),
            });
        }
        // `Graph::connect` refuses these, so reaching one here means a
        // hand-edited or older document.
        let constant = self
            .definition(registry, edge.to.node)
            .ok()
            .and_then(|def| def.input(&edge.to.socket))
            .is_some_and(|socket| socket.constant);
        if constant {
            errors.push(GraphError::ConstantInput {
                socket: edge.to.clone(),
            });
        }
    }

    /// A socket's effective type, or `None` if it is generic and this node
    /// instance has not resolved that parameter yet.
    ///
    /// `None` is not itself an error here: [`Graph::validate`] reports an
    /// unresolved parameter once per node (see its `UnresolvedGeneric`
    /// pass) rather than once per socket that happens to reference it, and
    /// [`Graph::connect`] treats it as "free to adopt whatever the other
    /// side is" rather than a mismatch.
    pub(super) fn socket_type(
        &self,
        registry: &NodeRegistry,
        socket: &SocketRef,
        direction: Direction,
    ) -> Result<Option<ValueType>, GraphError> {
        let def = self.definition(registry, socket.node)?;
        let found = match direction {
            Direction::Input => def.input(&socket.socket),
            Direction::Output => def.output(&socket.socket),
        };
        let found = found.ok_or_else(|| GraphError::UnknownSocket {
            socket: socket.clone(),
            direction,
        })?;
        Ok(self.effective_type(socket.node, found))
    }
}

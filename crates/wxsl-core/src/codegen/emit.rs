//! The expression emitter: one `let` per reachable node output, in
//! dependency order, split into the partitions each stage evaluates.

use super::*;

impl Emitter<'_> {
    pub(super) fn request_import(&mut self, module: &str, item: &str) {
        let module = ModulePath::new(module).expect("ABI module paths are valid");
        let item = WxslIdent::new(item).expect("ABI item names are valid");
        let items = self.imports.entry(module).or_default();
        if !items.contains(&item) {
            items.push(item);
        }
    }

    /// Compile everything `terminal` depends on into one function body.
    pub(super) fn emit_partition(
        &mut self,
        terminal: NodeId,
        stage: ShaderStage,
    ) -> Result<Partition, CodegenError> {
        let needed = self.graph.dependencies_of(terminal);
        let order = self
            .graph
            .topological_order(Some(&needed))
            .map_err(|e| CodegenError::Invalid(GraphErrors(vec![e])))?;
        self.stage = stage;
        self.bindings.clear();
        self.body.clear();
        for node in &order {
            if *node == terminal {
                continue;
            }
            self.emit_node(*node)?;
        }
        let inputs = self.emit_terminal(terminal)?;
        Ok(Partition {
            body: core::mem::take(&mut self.body),
            inputs,
        })
    }

    /// A fragment-stage partition: like [`Emitter::emit_partition`], but
    /// a node the stage analysis assigned to the vertex stage is a *cut*
    /// — the walk stops there, and its sockets read the synthesized
    /// interpolants the vertex side writes (`attrs.<name>`). This is the
    /// whole mechanism: a cut is an interpolant with no author.
    pub(super) fn emit_fragment_partition(
        &mut self,
        terminal: NodeId,
        cuts: &std::collections::BTreeMap<
            SocketRef,
            (crate::wxsl::WxslIdent, crate::node::ValueType),
        >,
    ) -> Result<Partition, CodegenError> {
        self.emit_fragment_partition_with(terminal, cuts, None)
    }

    /// [`Emitter::emit_fragment_partition`], with one more leaf: a socket
    /// bound to a given expression, whose node the walk stops at.
    fn emit_fragment_partition_with(
        &mut self,
        terminal: NodeId,
        cuts: &std::collections::BTreeMap<
            SocketRef,
            (crate::wxsl::WxslIdent, crate::node::ValueType),
        >,
        leaf: Option<(SocketRef, String)>,
    ) -> Result<Partition, CodegenError> {
        let mut stops: std::collections::BTreeSet<NodeId> =
            cuts.keys().map(|socket| socket.node).collect();
        stops.extend(self.baked.keys().copied());
        stops.extend(leaf.iter().map(|(socket, _)| socket.node));
        let needed = self.graph.dependencies_stopping_at(terminal, &stops);
        let order = self
            .graph
            .topological_order(Some(&needed))
            .map_err(|e| CodegenError::Invalid(GraphErrors(vec![e])))?;
        self.stage = ShaderStage::Fragment;
        self.bindings.clear();
        self.body.clear();
        // The cuts are leaves as far as this partition is concerned, so
        // their expressions are the interpolant reads, bound before
        // anything that consumes them is emitted.
        for (socket, (name, _)) in cuts {
            self.bindings.insert(
                socket.clone(),
                format!("{}.{}", abi::MATERIAL_ATTRIBUTES_VAR, name),
            );
        }
        // A baked term is a leaf the same way — but its expression is one
        // sample of its table, which reads only the context uv and the
        // table's own bindings, so it can be bound up front too. Its cone
        // is not walked at all: that is the whole point of the bake, and
        // the reason the baked arm's module is smaller by the subgraph.
        for term in self.baked.values() {
            let reference = SocketRef::new(term.node, term.socket.clone());
            if self.graph.edge_from(&reference).is_some() {
                self.bindings.insert(reference, term.sample_expr());
            }
        }
        if let Some((socket, expr)) = leaf {
            self.bindings.insert(socket, expr);
        }
        for node in &order {
            if *node == terminal || stops.contains(node) {
                continue;
            }
            self.emit_node(*node)?;
        }
        let inputs = self.emit_terminal(terminal)?;
        Ok(Partition {
            body: core::mem::take(&mut self.body),
            inputs,
        })
    }

    /// The fragment chain (ADR 0089): the fragment partition rooted at
    /// `terminal`, stopping at the surface output, whose shaded colour is
    /// the [`abi::SHADED_VAR`] parameter of the function it compiles into.
    pub(super) fn emit_fragment_chain(
        &mut self,
        terminal: NodeId,
        surface: NodeId,
        cuts: &std::collections::BTreeMap<
            SocketRef,
            (crate::wxsl::WxslIdent, crate::node::ValueType),
        >,
    ) -> Result<Partition, CodegenError> {
        self.emit_fragment_partition_with(
            terminal,
            cuts,
            Some((
                SocketRef::new(surface, abi::SOCKET_SHADED_COLOR),
                abi::SHADED_VAR.to_string(),
            )),
        )
    }

    /// One cut, as a vertex-stage partition of its own — the same shape a
    /// hand-wired `output.varying` compiles into, rooted at the node the
    /// analysis chose instead of at a terminal. The partition "returns"
    /// the cut socket's emitted expression, carried in
    /// `inputs` under the varying socket's name so `finish` reads it the
    /// same way it reads a manual interpolant's.
    pub(super) fn emit_cut(&mut self, socket: &SocketRef) -> Result<Partition, CodegenError> {
        let needed = self.graph.dependencies_of(socket.node);
        let order = self
            .graph
            .topological_order(Some(&needed))
            .map_err(|e| CodegenError::Invalid(GraphErrors(vec![e])))?;
        self.stage = ShaderStage::Vertex;
        self.bindings.clear();
        self.body.clear();
        for node in &order {
            if *node == socket.node {
                continue;
            }
            self.emit_node(*node)?;
        }
        self.emit_node(socket.node)?;
        let expr = self.bindings.get(socket).cloned().ok_or_else(|| {
            CodegenError::Invalid(GraphErrors(vec![GraphError::UnknownNode(socket.node)]))
        })?;
        Ok(Partition {
            body: core::mem::take(&mut self.body),
            inputs: vec![(abi::SOCKET_VARYING.to_string(), expr)],
        })
    }

    /// The struct a context read reads through in the stage being
    /// emitted.
    fn context_var(&self) -> &'static str {
        match self.stage {
            ShaderStage::Vertex => abi::VERTEX_CONTEXT_VAR,
            ShaderStage::Fragment => abi::CONTEXT_VAR,
        }
    }

    /// Import the fixed vocabulary every generated module uses.
    ///
    /// The deferred-path items are imported unconditionally; when the
    /// deferred fragment entry is dropped by conditional translation they
    /// become unused, and the compiler strips them.
    pub(super) fn request_abi_imports(
        &mut self,
        displaces: bool,
        shades: bool,
        interpolates: bool,
    ) {
        // Only what this stage's module actually mentions. A shadow
        // module for an alpha-tested material imports the context and
        // nothing else — no `Surface`, no shading function, no G-buffer.
        if shades {
            self.request_import(abi::SURFACE_MODULE, abi::SURFACE_STRUCT);
            self.request_import(abi::SURFACE_MODULE, abi::DEFAULT_SURFACE_FN);
        }
        self.request_import(abi::SURFACE_MODULE, abi::CONTEXT_STRUCT);
        // Both halves of the vertex stage read it: the displacement and
        // every computed interpolant.
        if displaces || interpolates {
            self.request_import(abi::VERTEX_MODULE, abi::VERTEX_CONTEXT_STRUCT);
        }
        if self.options.emit_entry_points {
            self.request_import(abi::VERTEX_MODULE, abi::VERTEX_IN_STRUCT);
            self.request_import(abi::VERTEX_MODULE, abi::VERTEX_OUT_STRUCT);
            if displaces {
                self.request_import(abi::VERTEX_MODULE, abi::VERTEX_CONTEXT_FN);
                self.request_import(abi::VERTEX_MODULE, abi::TRANSFORM_VERTEX_OFFSET_FN);
            } else {
                self.request_import(abi::VERTEX_MODULE, abi::TRANSFORM_VERTEX_FN);
            }
            if interpolates {
                self.request_import(abi::VERTEX_MODULE, abi::VERTEX_CONTEXT_FN);
            }
            self.request_import(abi::VERTEX_MODULE, abi::SURFACE_CONTEXT_FN);
            // The stages that read pass-group depth sample it at texel
            // centres through the frame's point sampler (ADR 0075).
            if matches!(
                self.options.stage,
                abi::MaterialStage::PEEL_FRONT
                    | abi::MaterialStage::PEEL_BACK
                    | abi::MaterialStage::PEEL_DEPTH
                    | abi::MaterialStage::PEEL_RESOLVE
                    | abi::MaterialStage::REFRACTION_DEPTH
            ) {
                self.request_import("package::wxsl::bindings", "point_sampler");
            }
            match self.options.stage.output() {
                abi::StageOutput::Color | abi::StageOutput::PeelResolve => {
                    // Under a debug view the shading function returns the
                    // view instead of lighting (plan6 E2), so every stage
                    // that shades shows it.
                    let macros = &self.options.material.macros;
                    let debug = match macros.get(crate::lighting::DEBUG_VIEW_MACRO) {
                        Some(crate::macros::MacroValue::Int(id)) => {
                            let material = match macros.get(crate::lighting::DEBUG_MATERIAL_MACRO) {
                                Some(crate::macros::MacroValue::Int(index)) => index,
                                _ => 0,
                            };
                            crate::lighting::debug_surface(id, material)
                        }
                        _ => None,
                    };
                    // The dispatch and the whole shading function come from
                    // the material's lighting model, generated here rather
                    // than imported from a fixed module: the call inside
                    // the light loop names the model.
                    let generated = debug.unwrap_or_else(|| {
                        crate::lighting::shade_surface_with(
                            &crate::lighting::Dispatch::Direct(
                                *self.options.material.lighting.model(),
                            ),
                            self.options.material.lighting.features(),
                            // This module declares the macros in effect itself;
                            // a second declaration would not compile.
                            false,
                        )
                    });
                    for (module, item) in &generated.imports {
                        self.request_import(module, item);
                    }
                    self.lighting_source = generated.source;
                }
                abi::StageOutput::GBuffer => {
                    // The struct's fields are the plan's layout — the set's
                    // requests plus any feature channels — so the pack is
                    // generated beside it instead of imported from a fixed
                    // module.
                    let forward_shaded = self.options.material.forward_shaded;
                    let mut generated = crate::lighting::pack_gbuffer(
                        self.options.material.lighting.model(),
                        self.options.material.lighting.set(),
                        self.options.material.lighting.features(),
                        forward_shaded,
                    );
                    if forward_shaded {
                        // The same shading function a forward module gets:
                        // a forward-shaded material runs its own model in
                        // this stage, and the pack stores its output
                        // (plan5 D2). The macro set the material resolved
                        // with has already dropped the shadow lookup.
                        let shaded = crate::lighting::shade_surface_with(
                            &crate::lighting::Dispatch::Direct(
                                *self.options.material.lighting.model(),
                            ),
                            self.options.material.lighting.features(),
                            false,
                        );
                        for (module, item) in &shaded.imports {
                            self.request_import(module, item);
                        }
                        generated.source.push('\n');
                        generated.source.push_str(&shaded.source);
                    }
                    for (module, item) in &generated.imports {
                        self.request_import(module, item);
                    }
                    self.lighting_source = generated.source;
                }
                abi::StageOutput::Velocity => {
                    // The whole velocity ABI comes from one module, and the
                    // fragment is position arithmetic — no surface, no
                    // shading function. The previous-frame context is only
                    // called when the graph displaces, but an unused import
                    // is stripped, and one arm per stage reads better than
                    // import accounting per shape.
                    self.request_import(abi::VELOCITY_MODULE, abi::VELOCITY_OUT_STRUCT);
                    self.request_import(abi::VELOCITY_MODULE, abi::TRANSFORM_VERTEX_VELOCITY_FN);
                    self.request_import(abi::VELOCITY_MODULE, abi::PREVIOUS_CLIP_POSITION_FN);
                    self.request_import(abi::VELOCITY_MODULE, abi::VELOCITY_CONTEXT_FN);
                    self.request_import(abi::VELOCITY_MODULE, abi::SCREEN_MOTION_FN);
                    if displaces {
                        self.request_import(abi::VELOCITY_MODULE, abi::PREVIOUS_CONTEXT_FN);
                    }
                }
                abi::StageOutput::Nothing
                | abi::StageOutput::PeelDepth
                | abi::StageOutput::RefractionSurface => {}
            }
        }
    }

    /// The trimmed value of a declaring node's `setting`.
    ///
    /// `Graph::validate` has already rejected an empty or malformed one, so
    /// reaching the error here means codegen ran on an unvalidated graph.
    fn declared_name(&self, node: NodeId, setting: &str) -> Result<String, CodegenError> {
        let value = self
            .graph
            .setting(self.registry, node, setting)
            .unwrap_or_default()
            .trim();
        if WxslIdent::new(value).is_none() {
            return Err(CodegenError::Invalid(GraphErrors(vec![
                GraphError::InvalidSetting {
                    node,
                    setting: setting.to_string(),
                    value: value.to_string(),
                    reason: "must be a valid WXSL identifier".to_string(),
                },
            ])));
        }
        Ok(value.to_string())
    }

    fn definition(&self, node: NodeId) -> Result<&NodeDefinition, CodegenError> {
        self.graph
            .definition(self.registry, node)
            .map_err(|e| CodegenError::Invalid(GraphErrors(vec![e])))
    }

    /// The WXSL expression feeding `node`'s input `socket`, or `None` if the
    /// input is optional and nothing feeds it.
    fn input_expr(&self, node: NodeId, socket_name: &str) -> Result<Option<String>, CodegenError> {
        let reference = SocketRef::new(node, socket_name);
        if let Some(edge) = self.graph.edge_into(&reference) {
            let expr = self.bindings.get(&edge.from).ok_or_else(|| {
                // Only reachable if the topological order was wrong.
                CodegenError::Invalid(GraphErrors(vec![GraphError::Cycle {
                    nodes: vec![edge.from.node, node],
                }]))
            })?;
            return Ok(Some(expr.clone()));
        }

        let def = self.definition(node)?;
        let socket = def
            .input(socket_name)
            .ok_or_else(|| CodegenError::BadTemplate {
                def: def.id.clone(),
                placeholder: socket_name.to_string(),
            })?;
        let instance = self.graph.node(node).ok_or_else(|| {
            CodegenError::Invalid(GraphErrors(vec![GraphError::UnknownNode(node)]))
        })?;

        // A generic socket's default is a scalar to spread over whatever
        // this instance resolved to (`Socket::splat_default`), so the type
        // has to be looked up before the default can be read.
        let value: Option<Value> = instance.params.get(socket_name).copied().or_else(|| {
            self.graph
                .effective_type(node, socket)
                .and_then(|ty| socket.default_for(ty))
        });
        match value {
            Some(value) => {
                let literal =
                    value
                        .wxsl_literal()
                        .ok_or_else(|| CodegenError::UnrepresentableValue {
                            socket: reference.clone(),
                        })?;
                Ok(Some(literal))
            }
            None if socket.optional => Ok(None),
            None => Err(CodegenError::Invalid(GraphErrors(vec![
                GraphError::MissingInput { socket: reference },
            ]))),
        }
    }

    pub(super) fn emit_node(&mut self, node: NodeId) -> Result<(), CodegenError> {
        let def = self.definition(node)?.clone();
        for (module, item) in def.all_imports() {
            let items = self.imports.entry(module).or_default();
            if !items.contains(&item) {
                items.push(item);
            }
        }

        match &def.body {
            NodeBody::Expr(exprs) => {
                if exprs.len() != def.outputs.len() {
                    return Err(CodegenError::OutputArityMismatch {
                        def: def.id.clone(),
                        outputs: def.outputs.len(),
                        exprs: exprs.len(),
                    });
                }
                for (socket, template) in def.outputs.iter().zip(exprs) {
                    let target = SocketRef::new(node, socket.name.as_str());
                    // Skip outputs nobody reads: they are dead code, and the
                    // `let` would be an unused-variable warning downstream.
                    if self.graph.edge_from(&target).is_none() {
                        continue;
                    }
                    let expr = self.expand_template(node, &def, template)?;
                    let name = binding_name(node, socket.name.as_str());
                    // `socket.ty` is only a placeholder on a generic socket
                    // (see `Socket::generic`); this instance's resolved type
                    // is what the emitted WGSL must actually declare.
                    // `validate()` (run before codegen starts, in
                    // `generate()`) guarantees every generic parameter this
                    // node's definition declares is resolved by now.
                    let ty = self.graph.effective_type(node, socket).expect(
                        "a validated graph resolves every generic parameter its nodes declare",
                    );
                    // A texture or a sampler is a handle, and WGSL has no
                    // `let` for one: a node handing out a binding — the
                    // screen ABI's input image — binds the name straight
                    // through, exactly as a `NodeBody::Resource` does.
                    if ty.is_resource() {
                        self.bindings.insert(target, expr);
                        continue;
                    }
                    let _ = writeln!(self.body, "    let {name}: {} = {expr};", ty.wxsl_type());
                    self.bindings.insert(target, name);
                }
            }
            NodeBody::Call(func) => {
                let mut args = Vec::with_capacity(func.params.len());
                for param in &func.params {
                    let expr = self.input_expr(node, param.name.as_str())?.ok_or_else(|| {
                        CodegenError::Invalid(GraphErrors(vec![GraphError::MissingInput {
                            socket: SocketRef::new(node, param.name.as_str()),
                        }]))
                    })?;
                    args.push(expr);
                }
                // A generic function node calls a WXSL *template*
                // (`fn safe_normalize<T: vec2f | vec3f | vec4f>`), and the
                // graph always knows the type exactly, so the type
                // arguments are written explicitly — which is the case
                // ADR 0012 says never has to guess. Declaration order is
                // the argument order.
                let type_args = self.type_arguments(node, &def)?;
                let call = format!("{}{type_args}({})", func.name, args.join(", "));
                let name = binding_name(node, "call");
                match &func.ret {
                    FunctionReturn::Value(socket) => {
                        // Generic here too: the return socket's `ty` is only
                        // a placeholder when it carries a parameter.
                        let ty = self.graph.effective_type(node, socket).unwrap_or(socket.ty);
                        let _ = writeln!(self.body, "    let {name}: {} = {call};", ty.wxsl_type());
                        self.bindings
                            .insert(SocketRef::new(node, socket.name.as_str()), name);
                    }
                    FunctionReturn::Struct {
                        name: struct_name,
                        fields,
                    } => {
                        // Bind the call once, then read the fields off it, so
                        // a multi-output function is evaluated once however
                        // many of its outputs the graph uses.
                        let _ = writeln!(self.body, "    let {name}: {struct_name} = {call};");
                        for field in fields {
                            self.bindings.insert(
                                SocketRef::new(node, field.name.as_str()),
                                format!("{name}.{}", field.name),
                            );
                        }
                    }
                }
            }
            NodeBody::ContextRead(field) | NodeBody::VertexContextRead(field) => {
                let socket =
                    def.outputs
                        .first()
                        .ok_or_else(|| CodegenError::OutputArityMismatch {
                            def: def.id.clone(),
                            outputs: 0,
                            exprs: 1,
                        })?;
                // In a bake module there is no context to read: the uv is
                // the generated function's parameter — the bake domain —
                // and every other field is the purity check's to have
                // refused before emission began.
                if self.bake_domain {
                    if matches!(def.body, NodeBody::ContextRead(_)) && field.as_str() == "uv" {
                        self.bindings
                            .insert(SocketRef::new(node, socket.name.as_str()), "uv".to_string());
                        return Ok(());
                    }
                    return Err(CodegenError::Invalid(GraphErrors(vec![
                        GraphError::InvalidBake {
                            texture: String::new(),
                            reason: "reads something a bake cannot evaluate".to_string(),
                        },
                    ])));
                }
                // The same node in either stage, reading whichever
                // context struct this partition was handed — which is
                // exactly why the vertex context is a superset of the
                // fragment one.
                self.bindings.insert(
                    SocketRef::new(node, socket.name.as_str()),
                    format!("{}.{field}", self.context_var()),
                );
            }
            NodeBody::Param => {
                let socket =
                    def.outputs
                        .first()
                        .ok_or_else(|| CodegenError::OutputArityMismatch {
                            def: def.id.clone(),
                            outputs: 0,
                            exprs: 1,
                        })?;
                // A field read off the uniform buffer, bound as an
                // expression rather than a `let`: it is one uniform load
                // wherever it is used, and giving it a name would only add
                // a line. `read_expr` rather than `material.name` because
                // a `bool` parameter is stored as a `u32` and this is not
                // the place that knows it.
                let name = self.declared_name(node, node::SETTING_NAME)?;
                let expr = self
                    .interface
                    .params
                    .read_expr(abi::MATERIAL_PARAMS_VAR, &name)
                    .ok_or_else(|| {
                        // Only reachable if `interface_of` and this walk
                        // disagreed about what is reachable.
                        CodegenError::UndeclaredParam {
                            node,
                            name: name.clone(),
                        }
                    })?;
                self.bindings
                    .insert(SocketRef::new(node, socket.name.as_str()), expr);
            }
            NodeBody::Resource => {
                let socket =
                    def.outputs
                        .first()
                        .ok_or_else(|| CodegenError::OutputArityMismatch {
                            def: def.id.clone(),
                            outputs: 0,
                            exprs: 1,
                        })?;
                // The declared variable *is* the value: a texture handle
                // is passed to `textureSample` and to functions by name.
                let name = self.declared_name(node, node::SETTING_NAME)?;
                self.bindings
                    .insert(SocketRef::new(node, socket.name.as_str()), name);
            }
            NodeBody::UserRead => {
                let socket =
                    def.outputs
                        .first()
                        .ok_or_else(|| CodegenError::OutputArityMismatch {
                            def: def.id.clone(),
                            outputs: 0,
                            exprs: 1,
                        })?;
                let field = self.declared_name(node, node::SETTING_FIELD)?;
                let user = self
                    .interface
                    .user
                    .as_ref()
                    .ok_or(CodegenError::NoUserBlock { node })?;
                let expr = user
                    .layout
                    .read_expr(user.name.as_str(), &field)
                    .ok_or_else(|| CodegenError::UndeclaredParam {
                        node,
                        name: field.clone(),
                    })?;
                self.bindings
                    .insert(SocketRef::new(node, socket.name.as_str()), expr);
            }
            NodeBody::AttributeRead => {
                let socket =
                    def.outputs
                        .first()
                        .ok_or_else(|| CodegenError::OutputArityMismatch {
                            def: def.id.clone(),
                            outputs: 0,
                            exprs: 1,
                        })?;
                let name = self.declared_name(node, node::SETTING_NAME)?;
                let geometry = &self.interface.geometry;
                // Which backing a name has is the *declaration's* business
                // and not the node's, so this is where the three
                // frequencies stop being different: a per-vertex value and
                // a computed interpolant were both interpolated into
                // `attrs`, and a per-instance one is a field of the row
                // the flat index points at.
                let interpolated = geometry.vertex_attribute(name.as_str()).is_some()
                    || geometry.computed_varying(name.as_str()).is_some();
                let expr = if interpolated {
                    Some(format!("{}.{name}", abi::MATERIAL_ATTRIBUTES_VAR))
                } else {
                    geometry.instance().field(name.as_str()).and_then(|_| {
                        geometry.instance().read_expr(
                            &format!(
                                "{}({}.{})",
                                abi::MATERIAL_INSTANCE_FN,
                                abi::MATERIAL_ATTRIBUTES_VAR,
                                abi::INSTANCE_INDEX_FIELD,
                            ),
                            name.as_str(),
                        )
                    })
                };
                let expr = expr.ok_or_else(|| CodegenError::UndeclaredAttribute {
                    node,
                    name: name.clone(),
                })?;
                self.bindings
                    .insert(SocketRef::new(node, socket.name.as_str()), expr);
            }
            NodeBody::SurfaceOutput
            | NodeBody::VertexOutput
            | NodeBody::DiscardOutput
            | NodeBody::FragmentOutput
            | NodeBody::VaryingOutput
            | NodeBody::ScreenOutput => {
                // Emitted by `emit_surface`, which needs to run last.
                unreachable!("the surface output node is emitted separately");
            }
            NodeBody::Document => {
                // A pipeline document has no surface output, so `generate`
                // stops at `NoOutputNode` long before the walk reaches it.
                // It compiles to a `RenderGraph` (in `wxsl-render`), never
                // to WXSL.
                unreachable!("a pipeline document node has no WXSL to emit");
            }
        }
        Ok(())
    }

    /// Collect the surface field assignments from the output node.
    fn emit_terminal(&mut self, node: NodeId) -> Result<Vec<(String, String)>, CodegenError> {
        let def = self.definition(node)?.clone();
        let mut assignments = Vec::new();
        for socket in &def.inputs {
            if let Some(expr) = self.input_expr(node, socket.name.as_str())? {
                assignments.push((socket.name.to_string(), expr));
            }
        }
        Ok(assignments)
    }

    /// The `<vec3f, f32>` a call to a generic function node needs, or the
    /// empty string for a definition that declares no type parameters.
    fn type_arguments(&self, node: NodeId, def: &NodeDefinition) -> Result<String, CodegenError> {
        if def.generics.is_empty() {
            return Ok(String::new());
        }
        let mut args = Vec::with_capacity(def.generics.len());
        for param in &def.generics {
            let ty = self
                .graph
                .generic_type(node, param.name.as_str())
                .ok_or_else(|| {
                    CodegenError::Invalid(GraphErrors(vec![GraphError::UnresolvedGeneric {
                        node,
                        param: param.name.as_str().to_string(),
                    }]))
                })?;
            args.push(ty.wxsl_type());
        }
        Ok(format!("<{}>", args.join(", ")))
    }

    /// Substitute `{socket}` placeholders in an expression template.
    fn expand_template(
        &self,
        node: NodeId,
        def: &NodeDefinition,
        template: &str,
    ) -> Result<String, CodegenError> {
        let mut out = String::with_capacity(template.len());
        let mut rest = template;
        while let Some(open) = rest.find('{') {
            out.push_str(&rest[..open].replace("}}", "}"));
            rest = &rest[open + 1..];
            if let Some(stripped) = rest.strip_prefix('{') {
                out.push('{');
                rest = stripped;
                continue;
            }
            let close = rest.find('}').ok_or_else(|| CodegenError::BadTemplate {
                def: def.id.clone(),
                placeholder: rest.to_string(),
            })?;
            let name = &rest[..close];
            rest = &rest[close + 1..];
            // `{$T}` is this node instance's resolved type for generic
            // parameter `T`, spelled as WGSL — what lets a template that
            // has to *name* its type (`vec3f(value)`, a `select` fallback
            // of the right width) stay one template across every type the
            // parameter allows, instead of one per type.
            if let Some(param) = name.strip_prefix('$') {
                let ty = self.graph.generic_type(node, param).ok_or_else(|| {
                    CodegenError::BadTemplate {
                        def: def.id.clone(),
                        placeholder: name.to_string(),
                    }
                })?;
                out.push_str(ty.wxsl_type());
                continue;
            }
            let expr = self
                .input_expr(node, name)?
                .ok_or_else(|| CodegenError::BadTemplate {
                    def: def.id.clone(),
                    placeholder: name.to_string(),
                })?;
            out.push_str(&as_atom(&expr));
        }
        // Trailing text, and literal `}}` escapes in it.
        out.push_str(&rest.replace("}}", "}"));
        Ok(out)
    }

    pub(super) fn finish(self, graph: &Graph, macros: &MacroSet, parts: &Partitions) -> String {
        let Emitter {
            options,
            interface,
            imports,
            lighting_source,
            ..
        } = self;
        let mut out = String::with_capacity(2048);
        write_header(&mut out, graph, macros, &imports);
        write_declarations(&mut out, &interface, macros);

        // The material function takes what the geometry supplied as a
        // second argument, and only when there is any: a graph that
        // declares nothing generates the signature it always did.
        let attributes = if interface.geometry.is_empty() {
            String::new()
        } else {
            format!(
                ", {}: {}",
                abi::MATERIAL_ATTRIBUTES_VAR,
                abi::MATERIAL_ATTRIBUTES_STRUCT
            )
        };

        // One function per partition, in stage order. Each is a complete
        // little program over its own subgraph; nothing is shared
        // between them but the module's declarations.
        if let Some(vertex) = &parts.vertex {
            let _ = write!(
                out,
                "\nfn {}({}: {}{attributes}) -> vec3f {{\n",
                abi::VERTEX_FN,
                abi::VERTEX_CONTEXT_VAR,
                abi::VERTEX_CONTEXT_STRUCT,
            );
            out.push_str(&vertex.body);
            let _ = writeln!(
                out,
                "    return {};",
                vertex.input(abi::SOCKET_POSITION_OFFSET, "vec3f(0.0, 0.0, 0.0)")
            );
            out.push_str("}\n");
        }

        // One function per computed interpolant, before the fragment
        // ones because they run first — and because reading the module
        // top to bottom should read as vertex stage then fragment stage.
        for (name, part) in &parts.varyings {
            let Some(varying) = interface.geometry.computed_varying(name.as_str()) else {
                continue;
            };
            // Unreachable — the socket is not optional — but a generated
            // function has to return something whatever the graph is.
            let zero = varying
                .ty
                .zero()
                .and_then(|value| value.wxsl_literal())
                .unwrap_or_else(|| "0.0".to_string());
            let _ = write!(
                out,
                "\nfn {}{name}({}: {}{attributes}) -> {} {{\n",
                abi::VARYING_FN_PREFIX,
                abi::VERTEX_CONTEXT_VAR,
                abi::VERTEX_CONTEXT_STRUCT,
                varying.ty.wxsl_type(),
            );
            out.push_str(&part.body);
            let _ = writeln!(
                out,
                "    return {};",
                part.input(abi::SOCKET_VARYING, &zero)
            );
            out.push_str("}\n");
        }

        if let Some(discard) = &parts.discard {
            let _ = write!(
                out,
                "\nfn {}({}: {}{attributes}) -> bool {{\n",
                abi::DISCARD_FN,
                abi::CONTEXT_VAR,
                abi::CONTEXT_STRUCT,
            );
            out.push_str(&discard.body);
            let _ = writeln!(
                out,
                "    return {};",
                discard.input(abi::SOCKET_DISCARD, "false")
            );
            out.push_str("}\n");
        }

        if let Some(fragment) = &parts.fragment {
            let _ = write!(
                out,
                "\nfn {}({}: {}{attributes}, {}: vec4f) -> vec4f {{\n",
                abi::FRAGMENT_FN,
                abi::CONTEXT_VAR,
                abi::CONTEXT_STRUCT,
                abi::SHADED_VAR,
            );
            out.push_str(&fragment.body);
            let _ = writeln!(
                out,
                "    return {};",
                fragment.input(abi::SOCKET_FRAGMENT_COLOR, abi::SHADED_VAR)
            );
            out.push_str("}\n");
        }

        if let Some(surface) = &parts.surface {
            let _ = write!(
                out,
                "\nfn {}({}: {}{attributes}) -> {} {{\n",
                options.material_fn,
                abi::CONTEXT_VAR,
                abi::CONTEXT_STRUCT,
                abi::SURFACE_STRUCT
            );
            out.push_str(&surface.body);
            let _ = writeln!(
                out,
                "    var surface: {} = {}({});",
                abi::SURFACE_STRUCT,
                abi::DEFAULT_SURFACE_FN,
                abi::CONTEXT_VAR,
            );
            for (field, expr) in &surface.inputs {
                let _ = writeln!(out, "    surface.{field} = {expr};");
            }
            out.push_str("    return surface;\n}\n");
        }

        // What the lighting model generated for this stage: the dispatch
        // and the shading function, or the G-buffer struct and pack. One
        // block, appended before the entry points, because WGSL does not
        // care about declaration order and a reader reads the material's
        // own functions first.
        if !lighting_source.is_empty() {
            out.push('\n');
            out.push_str(&lighting_source);
        }

        if options.emit_entry_points {
            write_entry_points(&mut out, options, &interface, parts);
        }
        out
    }
}

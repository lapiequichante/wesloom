//! The compiler's passes: one `PassDesc` per pass node, its attachments
//! and reads derived from the wiring.

use super::*;

impl Compiler<'_> {
    /// The tag expression a pass draws: the `tags` setting of the
    /// `source.scene` node feeding its `draws` input. The filter belongs
    /// to the draw queue, not to the pass — one source filters once, and
    /// two passes reading it draw the same things.
    fn tags(&self, pass: NodeId) -> Result<TagExpr, PipelineError> {
        let source = self
            .fed(pass, "draws")
            .expect("a validated document's pass has a draw source");
        let text = self.setting(source, doc::SETTING_TAGS);
        TagExpr::parse(&text).map_err(|reason| PipelineError::BadTags {
            node: self.label(source),
            value: text,
            reason: reason.to_string(),
        })
    }

    /// The node an input is fed from, or `None` if unfed.
    pub(super) fn fed(&self, node: NodeId, socket: &str) -> Option<NodeId> {
        self.document
            .edge_into(&SocketRef::new(node, socket))
            .map(|edge| edge.from.node)
    }

    /// The node an input is fed from *and* the output socket it is fed
    /// through — the socket name is what identifies one of a compute
    /// pass's several declared outputs.
    fn fed_via(&self, node: NodeId, socket: &str) -> Option<(NodeId, String)> {
        self.document
            .edge_into(&SocketRef::new(node, socket))
            .map(|edge| (edge.from.node, edge.from.socket.clone()))
    }

    /// Where a pass's `into` writes: the named resource, or the frame's
    /// target when `into` is unconnected.
    ///
    /// Shared by screen and colour-writing material passes, so that the
    /// rule "a chain passes along a resource, and another pass's output is
    /// nobody's to write" is one rule rather than two.
    pub(super) fn write_target(&mut self, node: NodeId) -> Result<ResourceId, PipelineError> {
        match self.fed(node, "into") {
            Some(source) => {
                if self.kind(source) != Some(doc::RESOURCE_COLOR) {
                    return Err(PipelineError::IntoFromPass {
                        node: self.label(node),
                    });
                }
                Ok(self.colors[&source])
            }
            None => {
                // A sorted-tier pass *loads* the target — it composites
                // onto what earlier passes wrote, the peel composite
                // behind it (plan5 D5) — so several target writers in
                // sequence are sound when only the first of them clears.
                self.target_writers.push((
                    self.label(node),
                    self.kind(node) == Some(doc::PASS_GEOMETRY)
                        && self.layers(node)? == LayerFilter::Sorted,
                ));
                Ok(RenderGraph::TARGET)
            }
        }
    }

    /// The colour attachment for a target a pass writes: the config's
    /// clear colour, wherever in the chain the colour is being written.
    ///
    /// The clear colour belongs to the *frame*, not to one resource. Since
    /// the display transform became a pass of its own
    /// ([ADR 0039](../../../docs/adr/0039-tonemap-is-an-effect-and-ambient-reads-the-lut.md))
    /// what the user sees as the background is whatever the head of the
    /// chain left where nothing was drawn — the lighting pass `discard`s
    /// there, and a forward pass never covers the whole screen — so an
    /// intermediate cleared to transparent would make every chained
    /// pipeline's background black regardless of the configuration. It is
    /// read as linear radiance now, like everything else in a chain.
    pub(super) fn color_attachment(&self, target: ResourceId) -> Attachment {
        Attachment::clear(target, self.config.target.clear_color)
    }

    /// Where a pass's depth comes from, what it does to it, and whether it
    /// *loads* — depth straight from a `resource.depth` node is cleared;
    /// depth from another pass's output is tested against and left alone,
    /// which is also a state change: `LessEqual`, and no depth write. The
    /// prepass idiom is one wire and it costs the pipeline nothing.
    fn depth_input(&self, node: NodeId) -> Result<Option<(DepthAttachment, bool)>, PipelineError> {
        let Some(source) = self.fed(node, "depth") else {
            return Ok(None);
        };
        match self.kind(source) {
            Some(doc::RESOURCE_DEPTH) => Ok(Some((
                DepthAttachment::clear(self.depths[&source], 1.0),
                false,
            ))),
            Some(doc::PASS_GEOMETRY) => {
                let resource = *self.pass_depths.get(&source).ok_or_else(|| {
                    PipelineError::DepthOutputWithoutDepth {
                        node: label(self.document, self.registry, source),
                    }
                })?;
                Ok(Some((
                    DepthAttachment {
                        resource,
                        clear: None,
                        store: true,
                        layer: 0,
                        mip: 0,
                    },
                    true,
                )))
            }
            other => unreachable!(
                "typing only lets a `resource.depth` or a material pass produce a \
                 depth target, not {other:?}"
            ),
        }
    }

    pub(super) fn geometry_pass(&mut self, node: NodeId) -> Result<(), PipelineError> {
        let name = self.label(node);
        let stage_text = self.setting(node, doc::SETTING_STAGE);
        let stage =
            MaterialStage::parse(&stage_text).ok_or_else(|| PipelineError::UnknownStage {
                node: name.clone(),
                stage: stage_text,
            })?;
        if stage == MaterialStage::SHADOW {
            return Err(PipelineError::UnknownStage {
                node: name,
                stage: MaterialStage::SHADOW.name().to_string(),
            });
        }
        // Front and back share `StageOutput::Color` with a lit pass, so the
        // rejection has to happen before that arm treats them as one.
        if matches!(stage, MaterialStage::PEEL_FRONT | MaterialStage::PEEL_BACK) {
            return Err(PipelineError::Peel {
                node: name,
                reason: format!(
                    "stage `{}` is drawn by `pass.peel`, which selects the transparent tag",
                    stage.name()
                ),
            });
        }

        // Depth first: a `gbuffer`-stage pass takes the G-buffer's, and may
        // not also wire a separate one.
        let gbuffer = self.fed(node, "gbuffer");
        if stage == MaterialStage::GBUFFER && gbuffer.is_none() {
            return Err(PipelineError::GbufferWithoutGbuffer { node: name });
        }
        if gbuffer.is_some() && self.fed(node, "depth").is_some() {
            return Err(PipelineError::DepthConflict { node: name });
        }
        // Copied out rather than borrowed: the arms below mutate other
        // fields of `self`.
        let gbuffer_depth = gbuffer.map(|source| self.gbuffers[&source].clone());
        let (depth, loads_depth) = match gbuffer_depth {
            Some((_, gbuffer_depth)) => {
                self.pass_depths.insert(node, gbuffer_depth);
                (Some(DepthAttachment::clear(gbuffer_depth, 1.0)), false)
            }
            None => match self.depth_input(node)? {
                None => return Err(PipelineError::PassWithoutDepth { node: name }),
                Some((depth, loads)) => {
                    self.pass_depths.insert(node, depth.resource);
                    (Some(depth), loads)
                }
            },
        };

        // Colour: a `forward_lit` pass writes where its `into` says — the
        // frame's target, or a `resource.color` it starts a chain into; a
        // `gbuffer`-stage pass writes every target the enabled set
        // requested; every other stage writes no colour.
        let color_wired = self
            .document
            .edge_from(&SocketRef::new(node, "color"))
            .is_some();
        let into_wired = self.fed(node, "into").is_some();
        let layers = self.layers(node)?;
        let blend = self.blend(node)?;
        let mut wrote = RenderGraph::TARGET;
        let colors: Vec<Attachment> = match stage.output() {
            abi::StageOutput::Color | abi::StageOutput::Velocity => {
                // A pass whose colour reaches nothing: neither handed on
                // through `color` nor written into a named target.
                if !color_wired && !into_wired {
                    return Err(PipelineError::ColorWithoutConsumer { node: name });
                }
                wrote = self.write_target(node)?;
                // A velocity target holds a pair of *signed* half floats —
                // motion is a difference, and negative — so the one
                // precision that spells that is the only one the fragment
                // output type agrees with. Everything else would fail at
                // pipeline creation with a format complaint; this names
                // the document node instead.
                if stage.output() == abi::StageOutput::Velocity {
                    let format = self
                        .graph
                        .resource_desc(wrote)
                        .and_then(|desc| desc.texture())
                        .map(|shape| match shape {
                            crate::pass::ResourceShape::Texture { format, .. } => *format,
                            crate::pass::ResourceShape::Buffer { .. } => {
                                unreachable!("typing only lets a colour target feed an `into`")
                            }
                        });
                    if format != Some(gbuffer_format(abi::GBufferPrecision::HighDynamicRangePair)) {
                        return Err(PipelineError::VelocityTargetFormat {
                            node: name,
                            format: format
                                .map(|format| format!("{format:?}"))
                                .unwrap_or_else(|| "the frame's own target".to_string()),
                        });
                    }
                    // And an unwritten texel of a velocity target means
                    // *no motion*, not the frame's radiance clear: a
                    // background that claimed the clear colour as motion
                    // would reproject every empty pixel a step sideways
                    // and bleed the history across silhouettes. The stage
                    // owns what its output means where nothing drew.
                    vec![Attachment::clear(wrote, Color::TRANSPARENT)]
                } else if layers == LayerFilter::Sorted {
                    // The sorted transparency tier composites onto what
                    // earlier passes wrote — the peel composite behind it
                    // (plan5 D5). Clearing here would throw that away,
                    // and a load orders this pass after the writer the
                    // scheduler already tracks.
                    vec![Attachment::load(wrote)]
                } else {
                    vec![self.color_attachment(wrote)]
                }
            }
            abi::StageOutput::GBuffer => {
                if color_wired || into_wired {
                    return Err(PipelineError::ColorFromColorlessStage {
                        node: name,
                        stage: stage.name().to_string(),
                    });
                }
                let (targets, _) = &self.gbuffers[&gbuffer.expect(
                    "checked above: the stage \
                 writes a G-buffer, so it must be wired to one",
                )];
                targets
                    .iter()
                    .map(|target| Attachment::clear(*target, Color::TRANSPARENT))
                    .collect()
            }
            abi::StageOutput::Nothing => {
                if color_wired || into_wired {
                    return Err(PipelineError::ColorFromColorlessStage {
                        node: name,
                        stage: stage.name().to_string(),
                    });
                }
                Vec::new()
            }
            abi::StageOutput::PeelDepth
            | abi::StageOutput::PeelResolve
            | abi::StageOutput::RefractionSurface => {
                return Err(PipelineError::Peel {
                    node: name,
                    reason: format!(
                        "stage `{}` is drawn by `{}`",
                        stage.name(),
                        if matches!(
                            stage,
                            MaterialStage::REFRACTION_DEPTH | MaterialStage::REFRACTION_SURFACE
                        ) {
                            "pass.refraction"
                        } else {
                            "pass.peel"
                        }
                    ),
                });
            }
        };

        // A pass that loads depth never writes it — the prepass already
        // did — and tests `LessEqual` rather than `Equal`, because WGSL
        // makes no promise that two pipelines running the same vertex code
        // produce bit-identical clip positions, and `Equal` turns a one-ulp
        // difference into a hole in the surface.
        let state = if loads_depth {
            PassState::OPAQUE.with_depth_test(CompareFunction::LessEqual, false)
        } else {
            PassState::OPAQUE
        };

        let tags = self.tags(node)?;
        let policy = self.policy(node)?;
        // The G-buffer a policy'd pass writes is stable storage for the
        // same reason a chain's colour target is.
        if policy != Policy::PerFrame {
            if let Some((targets, depth)) = gbuffer.as_ref().and_then(|s| self.gbuffers.get(s)) {
                for target in targets {
                    self.graph.make_stable_storage(*target);
                }
                self.graph.make_stable_storage(*depth);
            } else if let Some((depth, _)) = self.depth_input(node)? {
                self.graph.make_stable_storage(depth.resource);
            }
        }
        let sort = self.sort(node)?;
        let state = match blend {
            // A blended pass composites: its draws test against the depth
            // the opaque pass left and leave it alone. Writing depth would
            // let the first draw's z reject every farther draw after it,
            // and a painter's-algorithm tier would silently become
            // "nearest wins" (plan5 D5).
            Some(blend) => state
                .with_blend(blend)
                .with_depth_test(state.depth_compare, false),
            None => state,
        };
        let mut pass = PassDesc::geometry(name, DrawSource::Scene(tags), stage)
            .with_colors(colors)
            .with_state(state)
            .with_policy(policy)
            .with_sort(sort)
            .with_layers(layers);
        if let Some(depth) = depth {
            pass = pass.with_depth(depth);
        }
        self.graph.pass(pass);
        self.pass_colors.insert(node, wrote);
        Ok(())
    }

    pub(super) fn refraction_passes(&mut self, node: NodeId) -> Result<(), PipelineError> {
        let name = self.label(node);
        let depth = self.sampled_depth(self.fed(node, "depth").expect("validated depth"), &name)?;
        let scene = self.sampled_color(self.fed(node, "scene").expect("validated scene"), &name)?;
        let environment = self.image_source(
            self.fed(node, "environment")
                .expect("validated environment"),
            node,
        )?;
        if !matches!(
            self.graph.resource_desc(environment).map(|r| r.shape),
            Some(crate::pass::ResourceShape::Texture {
                dimension: crate::pass::Dimension::Cube,
                ..
            })
        ) {
            return Err(PipelineError::Peel {
                node: name,
                reason: "refraction environment must be a radiance cube".into(),
            });
        }
        let count = self
            .setting(node, "surface_count")
            .parse::<u32>()
            .ok()
            .filter(|n| (1..=4).contains(n))
            .ok_or_else(|| PipelineError::Peel {
                node: name.clone(),
                reason: "surface_count must be an integer in 1..=4".into(),
            })?;
        let quality = self
            .setting(node, "quality")
            .parse()
            .map_err(|reason: &str| PipelineError::Peel {
                node: name.clone(),
                reason: reason.into(),
            })?;
        let color = crate::effect::refraction::append_with_quality(
            &mut self.graph,
            scene,
            depth,
            environment,
            &name,
            count,
            quality,
        );
        let target = self.write_target(node)?;
        self.graph.pass(
            PassDesc::screen(format!("{name} output"), "wxsl.peel_over")
                .with_color(self.color_attachment(target))
                .with_reads([Read::current(scene), Read::current(color)]),
        );
        self.pass_colors.insert(node, target);
        Ok(())
    }

    pub(super) fn sampled_depth(
        &self,
        source: NodeId,
        node: &str,
    ) -> Result<ResourceId, PipelineError> {
        match self.kind(source) {
            Some(doc::RESOURCE_DEPTH) => Ok(self.depths[&source]),
            Some(doc::PASS_GEOMETRY) => {
                self.pass_depths
                    .get(&source)
                    .copied()
                    .ok_or_else(|| PipelineError::Peel {
                        node: node.to_string(),
                        reason: format!(
                            "depth comes from `{}`, which attaches none",
                            label(self.document, self.registry, source)
                        ),
                    })
            }
            _ => Err(PipelineError::Peel {
                node: node.to_string(),
                reason: "depth must come from a `resource.depth` or a material pass".into(),
            }),
        }
    }

    pub(super) fn sampled_color(
        &self,
        source: NodeId,
        node: &str,
    ) -> Result<ResourceId, PipelineError> {
        match self.kind(source) {
            Some(doc::RESOURCE_COLOR) => Ok(self.colors[&source]),
            Some(doc::PASS_GEOMETRY)
            | Some(doc::PASS_SCREEN)
            | Some(doc::PASS_PEEL)
            | Some(doc::PASS_REFRACTION) => match self.pass_colors.get(&source) {
                Some(&resource) if resource != RenderGraph::TARGET => Ok(resource),
                _ => Err(PipelineError::Peel {
                    node: node.to_string(),
                    reason: format!(
                        "the opaque colour comes from `{}`, which writes the frame target — \
                             wire that pass into a `resource.color` so the peel can sample it",
                        label(self.document, self.registry, source)
                    ),
                }),
            },
            _ => Err(PipelineError::Peel {
                node: node.to_string(),
                reason: "the opaque colour must come from a `resource.color` or a pass that \
                         writes one"
                    .into(),
            }),
        }
    }

    pub(super) fn shadow_passes(&mut self, node: NodeId) -> Result<(), PipelineError> {
        // Typing already guarantees `into` is fed and that only
        // `source.lights` feeds it, and `declare_shadow_maps` has already
        // rejected a second source — so the maps are the recorded ones.
        let (_, maps) = self
            .shadow_source
            .expect("a validated document's shadow pass names declared shadow maps");
        let tags = self.tags(node)?;
        let name = self.label(node);
        // One pass per atlas tile, always — not one per tile a light
        // happens to claim this frame. Which lights cast, and how many
        // tiles each takes, is the environment's business and changes
        // whenever the application says so. The first pass clears the
        // whole atlas on its way in; the rest load, because the tile is
        // the only part of the shared target they may touch.
        for slot in 0..abi::MAX_SHADOW_TILES as u32 {
            let tile = abi::shadow_tile(slot);
            let depth = if slot == 0 {
                DepthAttachment::clear(maps, 1.0)
            } else {
                DepthAttachment::load(maps)
            };
            self.graph.pass(
                PassDesc::geometry(
                    format!("{name} {slot}"),
                    DrawSource::Scene(tags.clone()),
                    MaterialStage::SHADOW,
                )
                .with_view(PassView::Shadow { tile: slot })
                .with_depth(depth)
                .with_viewport(ViewportRect::square(tile.x, tile.y, tile.edge))
                // Two-sided: the usual cases are an alpha-tested leaf and a
                // displaced surface, neither of them closed.
                .with_state(PassState::OPAQUE.with_cull_mode(None)),
            );
        }
        Ok(())
    }

    pub(super) fn screen_pass(&mut self, node: NodeId) -> Result<(), PipelineError> {
        let name = self.label(node);
        let effect_text = self.setting(node, doc::SETTING_EFFECT).trim().to_string();
        let known: Vec<String> = self.effects.ids();
        let effect = self
            .effects
            .get(&effect_text)
            .ok_or_else(|| PipelineError::UnknownEffect {
                node: name.clone(),
                effect: effect_text,
                known,
            })?
            .clone();
        // An effect whose wiring the fixed socket set cannot name — two
        // image inputs and up — has a derived row of its own, whose
        // sockets are the declaration. Naming it here would bind two
        // declared inputs to the one `image` socket and pretend that was
        // wiring; the mistake is named instead.
        if !effect.fits_pass_screen() {
            return Err(PipelineError::EffectInputMismatch {
                node: name.clone(),
                effect: effect.id.to_string(),
                reason: format!(
                    "it declares more images than the fixed socket set can name — \
                     place `{}{}` instead, whose sockets are the declaration",
                    doc::PASS_SCREEN_PREFIX,
                    effect.id
                ),
            });
        }

        // The reads are the effect's declared inputs, in declaration order
        // — the order the shader declares its pass-group bindings in —
        // each through the fixed socket set's one name for its kind.
        let sockets: Vec<&'static str> = effect
            .inputs
            .iter()
            .map(|input| match input.kind {
                EffectInputKind::GBuffer => "gbuffer",
                EffectInputKind::Image => "image",
                EffectInputKind::DepthImage => "depth",
                EffectInputKind::Buffer => "buffer",
            })
            .collect();
        let reads = self.screen_reads(node, &effect, &sockets)?;
        for (socket, wired) in [("gbuffer", "a G-buffer"), ("image", "an image")] {
            if effect.declares(socket) {
                continue;
            }
            if self.fed(node, socket).is_some() {
                return Err(PipelineError::EffectInputMismatch {
                    node: name.clone(),
                    effect: effect.id.to_string(),
                    reason: format!(
                        "{wired} is wired into it, which `{}` does not take",
                        effect.id
                    ),
                });
            }
        }
        if !effect
            .inputs
            .iter()
            .any(|input| input.kind == EffectInputKind::Buffer)
            && self.fed(node, "buffer").is_some()
        {
            return Err(PipelineError::EffectInputMismatch {
                node: name.clone(),
                effect: effect.id.to_string(),
                reason: format!(
                    "a storage buffer is wired into it, which `{}` does not take",
                    effect.id
                ),
            });
        }
        self.screen_pass_tail(node, &effect, reads)
    }

    /// The screen pass's derived row: same tail as [`Self::screen_pass`],
    /// but the effect named by the row itself and every declared input
    /// wired through a socket of its own — TAA's `color`, `velocity` and
    /// `history` are three sockets, not one `image` said three times.
    pub(super) fn derived_screen_pass(
        &mut self,
        node: NodeId,
        def_id: &str,
    ) -> Result<(), PipelineError> {
        let name = self.label(node);
        let effect_id = &def_id[doc::PASS_SCREEN_PREFIX.len()..];
        let known: Vec<String> = self.effects.ids();
        let effect = self
            .effects
            .get(effect_id)
            .ok_or_else(|| PipelineError::UnknownEffect {
                node: name.clone(),
                effect: effect_id.to_string(),
                known,
            })?
            .clone();
        let sockets: Vec<&'static str> = effect.inputs.iter().map(|input| input.name).collect();
        let reads = self.screen_reads(node, &effect, &sockets)?;
        self.screen_pass_tail(node, &effect, reads)
    }

    /// The pass-group reads one screen effect's declared inputs become, in
    /// declaration order — the order the shader declares its `@group(3)`
    /// bindings in. `sockets[i]` is the document socket inputs[i] is wired
    /// through: the fixed set's name for its kind on the generic
    /// `pass.screen`, the input's own name on a derived row. An image
    /// input carrying a `history` reads a resource's ring at that depth,
    /// which orders nothing — the property a temporal resolve needs to
    /// stay schedulable against its own previous output.
    fn screen_reads(
        &self,
        node: NodeId,
        effect: &Effect,
        sockets: &[&'static str],
    ) -> Result<Vec<Read>, PipelineError> {
        let name = self.label(node);
        let mut reads: Vec<Read> = Vec::new();
        let mut socket_taken: Vec<&'static str> = Vec::new();
        for (input, socket) in effect.inputs.iter().zip(sockets) {
            // Two declared inputs through one socket is the fixed set's
            // own limit speaking: on the generic pass a second buffer has
            // no socket to take, and on a derived row the sockets are the
            // inputs' own names, so this stays quiet.
            if socket_taken.contains(socket) {
                return Err(PipelineError::EffectInputMismatch {
                    node: name.clone(),
                    effect: effect.id.to_string(),
                    reason: format!(
                        "{} — but a screen pass has one `{socket}` socket, \
                         and an earlier declared input took it",
                        input.description
                    ),
                });
            }
            socket_taken.push(socket);
            match input.kind {
                EffectInputKind::DepthImage => {
                    let source = self.fed(node, socket).ok_or_else(|| {
                        PipelineError::EffectInputMismatch {
                            node: name.clone(),
                            effect: effect.id.into(),
                            reason: format!("nothing is wired into `{socket}`"),
                        }
                    })?;
                    reads.push(Read {
                        resource: self.sampled_depth(source, &name)?,
                        history: input.history,
                    });
                }
                EffectInputKind::GBuffer => match self.fed(node, socket) {
                    Some(source) => {
                        let (targets, depth) = &self.gbuffers[&source];
                        reads.extend(
                            targets
                                .iter()
                                .copied()
                                .chain(core::iter::once(*depth))
                                .map(Read::current),
                        );
                    }
                    None => {
                        return Err(PipelineError::EffectInputMismatch {
                            node: name.clone(),
                            effect: effect.id.to_string(),
                            reason: format!(
                                "{} — but no G-buffer is wired into it",
                                input.description
                            ),
                        });
                    }
                },
                EffectInputKind::Image => match self.fed(node, socket) {
                    Some(source) => reads.push(Read {
                        resource: self.image_source(source, node)?,
                        history: input.history,
                    }),
                    None => {
                        return Err(PipelineError::EffectInputMismatch {
                            node: name.clone(),
                            effect: effect.id.to_string(),
                            reason: format!(
                                "nothing is wired into `{socket}` ({})",
                                input.description.trim_end_matches('.')
                            ),
                        });
                    }
                },
                EffectInputKind::Buffer => {
                    let Some(source) = self.fed(node, socket) else {
                        return Err(PipelineError::EffectInputMismatch {
                            node: name.clone(),
                            effect: effect.id.to_string(),
                            reason: format!(
                                "nothing is wired into `{socket}` ({})",
                                input.description.trim_end_matches('.')
                            ),
                        });
                    };
                    reads.push(Read::current(self.storage_source(source, effect)?));
                }
            }
        }
        Ok(reads)
    }

    /// Everything a screen pass shares once its reads are known: the write
    /// target, the policy, the promotion a skipped frame needs, and the
    /// pass itself.
    fn screen_pass_tail(
        &mut self,
        node: NodeId,
        effect: &Effect,
        reads: Vec<Read>,
    ) -> Result<(), PipelineError> {
        let name = self.label(node);
        let target = self.write_target(node)?;
        let policy = self.policy(node)?;
        // A pass that skips frames writes stable storage, or the
        // scheduler names the mistake: a chain's colour target is
        // promoted here, so `once`-into-`resource.color` just works.
        if policy != Policy::PerFrame && target != RenderGraph::TARGET {
            self.graph.make_stable_storage(target);
        }
        self.graph.pass(
            PassDesc::screen(name, effect.id)
                .with_color(self.color_attachment(target))
                .with_reads(reads)
                .with_policy(policy),
        );
        self.pass_colors.insert(node, target);
        Ok(())
    }

    /// A compute pass: dispatch an effect that writes storage.
    ///
    /// The node is a `pass.compute.<effect>` definition derived from the
    /// effect's declaration, so its sockets *are* the contract — what the
    /// graph model typed as fed or unfed is exactly what the effect
    /// declared. Reads and writes resolve socket by socket, in declaration
    /// order, which is the order the shader declares its `@group(3)`
    /// bindings in and the order the pass group binds.
    ///
    /// A declared *output* is an input socket on the node, as `into` is on
    /// a screen pass: the wire names the storage being written, and the
    /// storage — the resource — is what any reader wires from. Compute to
    /// screen through a buffer is one `resource.buffer` feeding two passes,
    /// which is the same shape as a colour chain through `resource.color`.
    pub(super) fn compute_pass(&mut self, node: NodeId, def_id: &str) -> Result<(), PipelineError> {
        let name = self.label(node);
        let effect_id = &def_id[doc::PASS_COMPUTE_PREFIX.len()..];
        let known: Vec<String> = self.effects.ids();
        let effect = self
            .effects
            .get(effect_id)
            .ok_or_else(|| PipelineError::UnknownEffect {
                node: name.clone(),
                effect: effect_id.to_string(),
                known,
            })?;

        // Reads first: one pass-group binding per declared input, in the
        // declared order. The generated definition made every input
        // mandatory, so a validated document has each socket fed.
        let mut reads: Vec<Read> = Vec::new();
        for input in effect.inputs {
            let (source, _) = self
                .fed_via(node, input.name)
                .expect("a validated document's compute pass has every declared input fed");
            reads.push(match input.kind {
                EffectInputKind::GBuffer => {
                    let (targets, depth) = &self.gbuffers[&source];
                    reads.extend(
                        targets
                            .iter()
                            .copied()
                            .chain(core::iter::once(*depth))
                            .map(Read::current),
                    );
                    continue;
                }
                EffectInputKind::Image => Read::current(self.image_source(source, node)?),
                EffectInputKind::DepthImage => Read::current(self.sampled_depth(source, &name)?),
                EffectInputKind::Buffer => Read::current(self.storage_source(source, &effect)?),
            });
        }

        // Then writes: one storage resource per declared output, a
        // `resource.color` for a storage texture or a `resource.buffer`
        // for a buffer. Another pass's output is nobody's to write — the
        // screen rule, at the compute pass's other end.
        let mut writes: Vec<ResourceId> = Vec::new();
        for output in effect.outputs {
            let Some((source, _)) = self.fed_via(node, output.name) else {
                return Err(PipelineError::EffectInputMismatch {
                    node: name.clone(),
                    effect: effect.id.to_string(),
                    reason: format!(
                        "nothing is wired into its `{}` output ({})",
                        output.name,
                        output.description.trim_end_matches('.')
                    ),
                });
            };
            let resource = match self.kind(source) {
                Some(doc::RESOURCE_COLOR) => self.colors[&source],
                Some(doc::RESOURCE_BUFFER) => self.buffers[&source],
                Some(_) => return Err(PipelineError::WriteIntoPassOutput { node: name }),
                None => unreachable!("the edge's producer is in the document"),
            };
            writes.push(resource);
        }

        let policy = self.policy(node)?;
        // A pass that skips frames writes stable storage, or the scheduler
        // names the mistake — a `once` bake into a `resource.color` or
        // `resource.buffer` is promoted here, just as a chain's colour
        // target is for a screen pass.
        if policy != Policy::PerFrame {
            for resource in &writes {
                self.graph.make_stable_storage(*resource);
            }
        }
        // A device without compute runs the effect's fragment arm: the same
        // reads, its one storage texture written as the colour attachment
        // of a screen pass under the same label (ADR 0075).
        if !self.config.profile.compute {
            if let (Downlevel::Fragment { .. }, [output], [target]) =
                (&effect.downlevel, effect.outputs, writes.as_slice())
            {
                if output.shape == EffectOutputShape::StorageTexture {
                    let attachment = self.color_attachment(*target);
                    self.graph.pass(
                        PassDesc::screen(name, effect.id)
                            .with_color(attachment)
                            .with_reads(reads)
                            .with_policy(policy),
                    );
                    return Ok(());
                }
            }
            return Err(PipelineError::NativeOnly {
                pass: name,
                effect: effect.id.to_string(),
                profile: self.config.profile,
            });
        }
        let mut pass = PassDesc::compute(name, effect.id).with_reads(reads);
        for resource in writes {
            pass = pass.with_write(resource);
        }
        self.graph.pass(pass.with_policy(policy));
        Ok(())
    }

    /// The resource a screen effect's `image` input stands for: a
    /// `resource.color` node, or a pass's colour output — which is an
    /// intermediate when that pass wrote into one, and the *frame's
    /// target* when it did not. Sampling the target is the one
    /// un-compilable chain shape, and the error says what to wire instead.
    fn image_source(&self, source: NodeId, reader: NodeId) -> Result<ResourceId, PipelineError> {
        match self.kind(source) {
            Some(doc::RESOURCE_COLOR) | Some(doc::PASS_ENVIRONMENT) => Ok(self.colors[&source]),
            Some(doc::PASS_GEOMETRY)
            | Some(doc::PASS_SCREEN)
            | Some(doc::PASS_PEEL)
            | Some(doc::PASS_REFRACTION)
            | Some(doc::PASS_BLOOM)
            | Some(doc::PASS_DOF) => match self.pass_colors.get(&source) {
                Some(&resource) if resource != RenderGraph::TARGET => Ok(resource),
                _ => Err(PipelineError::ImageFromPass {
                    node: self.label(reader),
                    fed_by: label(self.document, self.registry, source),
                }),
            },
            other => unreachable!(
                "typing only lets a `resource.color` or a material pass produce a \
                 colour target, not {other:?}"
            ),
        }
    }

    /// The resource a buffer input stands for: a `resource.buffer`. The
    /// only producer of a storage buffer in the document vocabulary —
    /// storage is the resource's, and every reader wires from it, compute
    /// or screen.
    fn storage_source(
        &self,
        source: NodeId,
        effect: &crate::effect::Effect,
    ) -> Result<ResourceId, PipelineError> {
        match self.kind(source) {
            Some(doc::RESOURCE_BUFFER) => Ok(self.buffers[&source]),
            other => unreachable!(
                "typing only lets a `resource.buffer` feed a buffer socket, not \
                 {other:?} — and `{}` was wired from one",
                effect.id
            ),
        }
    }
}

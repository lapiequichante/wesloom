//! Dual depth peeling's expansion into geometry and composite passes
//! (ADR 0047).

use super::*;

impl Compiler<'_> {
    /// Dual depth peeling of the `transparent` tag, over the opaque colour
    /// and depth this node is wired to (ADR 0047).
    ///
    /// The layer count is `wxsl_peel_layers`, clamped to 1..=8.
    /// `wxsl_peel_native` selects the MAX-blend path. Either path draws at
    /// most eight geometry passes, and only the `transparent` tag.
    pub(super) fn peel_passes(&mut self, node: NodeId) -> Result<(), PipelineError> {
        let name = self.label(node);
        let depth_source = self
            .fed(node, "depth")
            .expect("a validated document's peel pass has depth");
        let scene_source = self
            .fed(node, "scene")
            .expect("a validated document's peel pass has a scene colour");
        let scene_depth = self.sampled_depth(depth_source, &name)?;
        let scene_color = self.sampled_color(scene_source, &name)?;
        let layers = abi::peel_layers(&self.config.macros);
        let native = abi::peel_native(&self.config.macros);

        let pair_format = TextureFormat::Rg32Float;
        // A side that peeled nothing. One layer never runs a back pass, and
        // the composite still samples a back image.
        let empty = self.graph.resource(ResourceDesc::color(
            format!("{name} empty"),
            TextureFormat::Rgba16Float,
        ));
        self.graph.pass(
            PassDesc::geometry(
                format!("{name} empty"),
                DrawSource::Scene(TagExpr::Never),
                MaterialStage::FORWARD_LIT,
            )
            .with_color(Attachment::clear(empty, Color::TRANSPARENT))
            .with_state(PassState::FULLSCREEN),
        );
        // Near bound 0, far bound 1: the first iteration's window is the
        // whole depth range in front of the opaque surface. Each later
        // window is a new resource — rewriting this one would make every
        // earlier reader wait on the later write.
        let bounds = self
            .graph
            .resource(ResourceDesc::color(format!("{name} bounds"), pair_format));
        self.graph.pass(
            PassDesc::geometry(
                format!("{name} bounds init"),
                DrawSource::Scene(TagExpr::Never),
                MaterialStage::FORWARD_LIT,
            )
            .with_color(Attachment::clear(
                bounds,
                Color {
                    r: 0.0,
                    g: 1.0,
                    b: 0.0,
                    a: 1.0,
                },
            ))
            .with_state(PassState::FULLSCREEN),
        );

        let mut bounds_read = bounds;
        let (front_accum, back_accum) = if native {
            self.peel_native(
                &name,
                layers,
                scene_depth,
                pair_format,
                &mut bounds_read,
                empty,
            )
        } else {
            self.peel_baseline(&name, layers, scene_depth, &mut bounds_read, empty)
        };

        let target = self.write_target(node)?;
        self.graph.pass(
            PassDesc::screen(format!("{name} composite"), PEEL_COMPOSITE.id)
                .with_color(self.color_attachment(target))
                .with_reads([
                    Read::current(front_accum),
                    Read::current(back_accum),
                    Read::current(scene_color),
                ]),
        );
        self.pass_colors.insert(node, target);
        Ok(())
    }

    fn peel_baseline(
        &mut self,
        name: &str,
        layers: u32,
        scene_depth: ResourceId,
        bounds_read: &mut ResourceId,
        empty: ResourceId,
    ) -> (ResourceId, ResourceId) {
        // Two geometry passes per layer, and the budget is eight, so the
        // baseline peels at most four layers. The depth pass is what makes
        // the winner order-independent: blending every fragment that passes
        // `Less` would composite whatever the draw list submitted first.
        let layers = layers.min(abi::PEEL_LAYERS_MAX as u32 / 2);
        let scratch = self.graph.resource(ResourceDesc::color(
            format!("{name} depth scratch"),
            TextureFormat::Rg32Float,
        ));
        let mut front: Option<ResourceId> = None;
        let mut back: Option<ResourceId> = None;
        let mut front_depth = empty;
        for layer in 0..layers {
            let nearer = layer % 2 == 0;
            let depth_buf = self.graph.resource(ResourceDesc::color(
                format!("{name} depth {layer}"),
                DEPTH_FORMAT,
            ));
            let written = self.peel_shaded_layer(
                &format!("{name} {}", if nearer { "front" } else { "back" }),
                layer,
                nearer,
                scene_depth,
                depth_buf,
                scratch,
                *bounds_read,
                if nearer { None } else { Some(front_depth) },
            );
            if nearer {
                front_depth = depth_buf;
                front = Some(self.fold_peel_layer(name, layer, true, front, written));
            } else {
                back = Some(self.fold_peel_layer(name, layer, false, back, written));
                if layer + 1 < layers {
                    self.advance_bounds(
                        name,
                        layer,
                        PEEL_BOUNDS.id,
                        front_depth,
                        depth_buf,
                        bounds_read,
                    );
                }
            }
        }
        (
            front.expect("a peel draws at least one layer"),
            back.unwrap_or(empty),
        )
    }

    /// `layer` under or over `accum`, into a new target.
    ///
    /// The first layer *is* the accumulator: nothing precedes it. A later
    /// layer is folded by a fullscreen pass rather than blended into the
    /// same texture, because the scheduler runs a reader after every writer
    /// of what it reads — reloading one accumulator from two passes cycles.
    fn fold_peel_layer(
        &mut self,
        name: &str,
        layer: u32,
        under: bool,
        accum: Option<ResourceId>,
        peeled: ResourceId,
    ) -> ResourceId {
        let Some(accum) = accum else {
            return peeled;
        };
        let next = self.graph.resource(ResourceDesc::color(
            format!("{name} fold {layer}"),
            TextureFormat::Rgba16Float,
        ));
        let effect = if under { PEEL_UNDER.id } else { PEEL_OVER.id };
        self.graph.pass(
            PassDesc::screen(format!("{name} fold {layer}"), effect)
                .with_color(Attachment::clear(next, Color::TRANSPARENT))
                .with_reads([Read::current(accum), Read::current(peeled)]),
        );
        next
    }

    /// The next iteration's window, in a resource nothing has written yet.
    fn advance_bounds(
        &mut self,
        name: &str,
        layer: u32,
        effect: &str,
        near: ResourceId,
        far: ResourceId,
        bounds_read: &mut ResourceId,
    ) {
        let next = self.graph.resource(ResourceDesc::color(
            format!("{name} bounds {layer}"),
            TextureFormat::Rg32Float,
        ));
        self.graph.pass(
            PassDesc::screen(format!("{name} bounds {layer}"), effect)
                .with_color(Attachment::clear(next, Color::TRANSPARENT))
                .with_reads([
                    Read::current(near),
                    Read::current(far),
                    Read::current(*bounds_read),
                ]),
        );
        *bounds_read = next;
    }

    /// One layer, order-independent: a depth pass keeps the nearest (front)
    /// or farthest (back) fragment, and the shade pass blends only the
    /// fragment whose depth equals it.
    ///
    /// `front_sample` is the front depth the back shade discards against, so
    /// a single surface is not peeled into both accumulators.
    #[allow(clippy::too_many_arguments)]
    fn peel_shaded_layer(
        &mut self,
        name: &str,
        layer: u32,
        front: bool,
        scene_depth: ResourceId,
        depth_buf: ResourceId,
        scratch: ResourceId,
        bounds: ResourceId,
        front_sample: Option<ResourceId>,
    ) -> ResourceId {
        let (clear_z, compare, stage, blend) = match front {
            true => (
                1.0,
                CompareFunction::Less,
                MaterialStage::PEEL_FRONT,
                PREMULTIPLIED_UNDER,
            ),
            false => (
                0.0,
                CompareFunction::Greater,
                MaterialStage::PEEL_BACK,
                PREMULTIPLIED_OVER,
            ),
        };
        self.graph.pass(
            PassDesc::geometry(
                format!("{name} depth {layer}"),
                transparent_draws(),
                MaterialStage::PEEL_DEPTH,
            )
            .with_color(Attachment::clear(scratch, Color::TRANSPARENT))
            .with_depth(DepthAttachment::clear(depth_buf, clear_z))
            .with_state(
                PassState::OPAQUE
                    .with_cull_mode(None)
                    .with_depth_test(compare, true),
            )
            .with_reads([Read::current(scene_depth), Read::current(bounds)])
            // Iteration `layer` spends the `layer`-th peel of every
            // object in it: a draw whose budget ran out sits the
            // iteration out (plan5 D5).
            .with_layers(LayerFilter::Peeled { layer }),
        );
        let mut reads = vec![Read::current(scene_depth), Read::current(bounds)];
        if let Some(front_depth) = front_sample {
            reads.push(Read::current(front_depth));
        }
        // Cleared, not loaded. One fragment matches the depth, so the blend
        // against transparent is the fragment, and a later layer folds it
        // in somewhere else. Loading a shared accumulator is the cycle.
        let peeled = self.graph.resource(ResourceDesc::color(
            format!("{name} layer {layer}"),
            TextureFormat::Rgba16Float,
        ));
        self.graph.pass(
            PassDesc::geometry(format!("{name} shade {layer}"), transparent_draws(), stage)
                .with_color(Attachment::clear(peeled, Color::TRANSPARENT))
                .with_depth(DepthAttachment::load(depth_buf))
                .with_state(
                    PassState::OPAQUE
                        .with_cull_mode(None)
                        .with_depth_test(CompareFunction::Equal, false)
                        .with_blend(blend),
                )
                .with_reads(reads)
                .with_layers(LayerFilter::Peeled { layer }),
        );
        peeled
    }

    fn peel_native(
        &mut self,
        name: &str,
        layers: u32,
        scene_depth: ResourceId,
        pair_format: TextureFormat,
        bounds_read: &mut ResourceId,
        empty: ResourceId,
    ) -> (ResourceId, ResourceId) {
        // An odd last layer is one front layer, not a back layer the macro
        // did not ask for. It takes the baseline's two passes.
        let tail = (layers % 2 == 1).then(|| {
            (
                self.graph.resource(ResourceDesc::color(
                    format!("{name} front depth"),
                    DEPTH_FORMAT,
                )),
                self.graph.resource(ResourceDesc::color(
                    format!("{name} depth scratch"),
                    TextureFormat::Rg32Float,
                )),
            )
        });
        let mut front: Option<ResourceId> = None;
        let mut back: Option<ResourceId> = None;
        let mut layer = 0u32;
        let mut iteration = 0u32;
        while layer < layers {
            if layer + 1 == layers {
                let (front_depth, scratch) = tail.expect("an odd count built the tail targets");
                let written = self.peel_shaded_layer(
                    &format!("{name} front"),
                    layer,
                    true,
                    scene_depth,
                    front_depth,
                    scratch,
                    *bounds_read,
                    None,
                );
                front = Some(self.fold_peel_layer(name, layer, true, front, written));
                break;
            }
            // A new pair every iteration. The resolve reads it, and the next
            // iteration must not write the same resource or that read waits
            // on the later write.
            let pair = self.graph.resource(ResourceDesc::color(
                format!("{name} depth pair {iteration}"),
                pair_format,
            ));
            self.graph.pass(
                PassDesc::geometry(
                    format!("{name} depth {iteration}"),
                    transparent_draws(),
                    MaterialStage::PEEL_DEPTH,
                )
                .with_color(Attachment::clear(pair, Color::TRANSPARENT))
                .with_state(PassState::FULLSCREEN.with_blend(MAX_BLEND))
                .with_reads([Read::current(scene_depth), Read::current(*bounds_read)])
                // The pair pass computes this iteration's two candidates;
                // a draw's budget has to cover the front one (plan5 D5).
                .with_layers(LayerFilter::Peeled { layer }),
            );
            let hdr = TextureFormat::Rgba16Float;
            let front_layer = self.graph.resource(ResourceDesc::color(
                format!("{name} front {iteration}"),
                hdr,
            ));
            let back_layer = self
                .graph
                .resource(ResourceDesc::color(format!("{name} back {iteration}"), hdr));
            self.graph.pass(
                PassDesc::geometry(
                    format!("{name} resolve {iteration}"),
                    transparent_draws(),
                    MaterialStage::PEEL_RESOLVE,
                )
                .with_colors([
                    Attachment::clear(front_layer, Color::TRANSPARENT)
                        .with_blend(PREMULTIPLIED_UNDER),
                    Attachment::clear(back_layer, Color::TRANSPARENT)
                        .with_blend(PREMULTIPLIED_OVER),
                ])
                .with_state(PassState::FULLSCREEN)
                .with_reads([Read::current(scene_depth), Read::current(pair)])
                // The front half of the resolve spends layer `layer`, the
                // back half layer `layer + 1`; a draw's budget covers
                // both or neither of a pair's layers, and the shallower
                // read is the honest filter (plan5 D5).
                .with_layers(LayerFilter::Peeled { layer }),
            );
            front = Some(self.fold_peel_layer(name, layer, true, front, front_layer));
            back = Some(self.fold_peel_layer(name, layer + 1, false, back, back_layer));
            layer += 2;
            if layer < layers {
                let next = self.graph.resource(ResourceDesc::color(
                    format!("{name} bounds {iteration}"),
                    pair_format,
                ));
                self.graph.pass(
                    PassDesc::screen(format!("{name} bounds {iteration}"), PEEL_DEPTH_BOUNDS.id)
                        .with_color(Attachment::clear(next, Color::TRANSPARENT))
                        .with_reads([Read::current(pair), Read::current(*bounds_read)]),
                );
                *bounds_read = next;
            }
            iteration += 1;
        }
        (
            front.expect("a peel draws at least one layer"),
            back.unwrap_or(empty),
        )
    }
}

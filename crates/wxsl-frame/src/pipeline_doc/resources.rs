//! The compiler's resource declarations: one engine resource (or a
//! chain of them) per `resource.*` node and per expanding pass.

use super::*;

impl Compiler<'_> {
    pub(super) fn environment_bake(&mut self, node: NodeId) -> Result<(), PipelineError> {
        let name = self.label(node);
        let invalid = |reason: &str| PipelineError::Environment {
            node: name.clone(),
            reason: reason.into(),
        };
        if self.graph.environment_maps().is_some() {
            return Err(invalid("only one environment may be declared"));
        }
        let source = self.fed(node, "image").expect("validated mandatory image");
        let source = *self.colors.get(&source).ok_or_else(|| {
            invalid("image must name a resource.color (imported HDR or a sky pass's destination)")
        })?;
        let integer = |setting: &'static str| -> Result<u32, PipelineError> {
            self.setting(node, setting)
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|v| *v > 0)
                .ok_or_else(|| invalid(&format!("{setting} must be a positive integer")))
        };
        for effect in [
            crate::effect::ibl::EQUIRECT_TO_CUBE,
            crate::effect::ibl::DIFFUSE,
            crate::effect::ibl::SPECULAR,
            crate::effect::ibl::RESAMPLE,
        ] {
            if self.effects.get(effect.id).is_none() {
                return Err(invalid(&format!("missing effect {}", effect.id)));
            }
        }
        let (size, diffuse, mips) = (
            integer(doc::SETTING_SIZE)?,
            integer("diffuse_size")?,
            integer("mips")?,
        );
        let scale = self.number(node, "radiance_scale")?;
        if !scale.is_finite() || scale <= 0.0 {
            return Err(invalid("radiance_scale must be finite and positive"));
        }
        let maps = crate::effect::ibl::append_filtered_bake(
            &mut self.graph,
            source,
            &name,
            size,
            diffuse,
            mips,
        );
        self.graph
            .declare_environment_maps(maps.diffuse, maps.specular);
        self.graph.set_environment_scale(scale);
        self.colors.insert(node, maps.radiance);
        Ok(())
    }

    /// A `pass.bloom` node: the bloom pyramid, expanded (ADR 0065).
    ///
    /// The repetition a document should not have to author — one HDR
    /// resource per level, a thresholding extract, a 2×2 box down per
    /// deeper level, a fold per shallower one, and the combine — is
    /// synthesized here into the same pass list the scheduler always saw:
    /// no new serialization, only scoped name allocation, exactly the
    /// shape `pass.environment` set. The generated labels are derived
    /// from the node's own and are the contract the host tunes through —
    /// `<label> extract` carries `threshold` and `knee`, `<label>
    /// combine` carries `strength`, both seeded from the node's settings
    /// and re-tunable live through `set_pass_param`.
    ///
    /// The level resources are the expansion's own — no document socket
    /// names them, and no pass outside the pyramid may write one. Each
    /// fold writes a resource of its own because reading a level while
    /// writing it is the read-write hazard the scheduler refuses; the
    /// folds' targets are transients and alias away like any other.
    pub(super) fn bloom_pyramid(&mut self, node: NodeId) -> Result<(), PipelineError> {
        const MAX_LEVELS: u32 = 12;
        let name = self.label(node);
        let invalid = |reason: String| PipelineError::BloomPyramid {
            node: name.clone(),
            reason,
        };
        let source = self.fed(node, "image").expect("validated mandatory image");
        let scene = *self.colors.get(&source).ok_or_else(|| {
            invalid(
                "image must name a resource.color — the pyramid thresholds linear radiance"
                    .to_string(),
            )
        })?;
        let levels = self
            .setting(node, "levels")
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|levels| (1..=MAX_LEVELS).contains(levels))
            .ok_or_else(|| invalid(format!("levels must be 1..{MAX_LEVELS}")))?;
        // The glow is linear radiance between the levels — an 8-bit
        // intermediate would clamp the highlight the pyramid is for, the
        // same honesty the HDR scene target in the bloom demos has.
        let hdr = gbuffer_format(abi::GBufferPrecision::HighDynamicRange);

        // One transient per level, half the one before: `Extent`'s
        // viewport rule — scale by a power of two, rounded up, never
        // smaller than a texel — is a pyramid's shape already.
        let mut level = Vec::with_capacity(levels as usize);
        for index in 0..levels {
            let id = self.graph.resource(
                ResourceDesc::color(format!("{name} level {index}"), hdr).with_extent(
                    Extent::Viewport {
                        scale: 0.5f32.powi(index as i32 + 1),
                    },
                ),
            );
            level.push(id);
        }

        // The extract thresholds once, at the head, at half resolution —
        // the bright part of the image is low-frequency by definition.
        let extract = PassDesc::screen(format!("{name} extract"), "wxsl.bloom_extract")
            .with_color(self.color_attachment(level[0]))
            .with_reads(vec![Read::current(scene)])
            .with_parameter(
                "threshold",
                wxsl_core::node::Value::F32(self.number(node, "threshold")?),
            )
            .with_parameter(
                "knee",
                wxsl_core::node::Value::F32(self.number(node, "knee")?),
            );
        self.graph.pass(extract);

        // The descent: one plain box down per deeper level.
        for index in 1..levels {
            let (above, below) = (level[index as usize - 1], level[index as usize]);
            self.graph.pass(
                PassDesc::screen(format!("{name} down {index}"), "wxsl.bloom_down")
                    .with_color(self.color_attachment(below))
                    .with_reads(vec![Read::current(above)]),
            );
        }

        // The ascent, deepest first: each fold takes the level below back
        // up and adds this level's own signal onto it, into a target of
        // its own — folding in place would read a level while writing it.
        // The deepest level is its own fold, so there are levels − 1.
        let mut folds: Vec<ResourceId> = Vec::with_capacity(levels.saturating_sub(1) as usize);
        for index in (0..levels.saturating_sub(1)).rev() {
            let target = self.graph.resource(
                ResourceDesc::color(format!("{name} up {index}"), hdr).with_extent(
                    Extent::Viewport {
                        scale: 0.5f32.powi(index as i32 + 1),
                    },
                ),
            );
            let below = folds.last().copied().unwrap_or(level[levels as usize - 1]);
            self.graph.pass(
                PassDesc::screen(format!("{name} up {index}"), "wxsl.bloom_up")
                    .with_color(self.color_attachment(target))
                    .with_reads(vec![
                        Read::current(below),
                        Read::current(level[index as usize]),
                    ]),
            );
            folds.push(target);
        }
        // `folds` is deepest first, so its last entry is the topmost.
        let glow = folds.last().copied().unwrap_or(level[0]);

        // The composite: the assembled glow onto the image, into the
        // wired resource or the frame's own target.
        let target = self.write_target(node)?;
        self.graph.pass(
            PassDesc::screen(format!("{name} combine"), "wxsl.bloom_combine")
                .with_color(self.color_attachment(target))
                .with_reads(vec![Read::current(scene), Read::current(glow)])
                .with_parameter(
                    "strength",
                    wxsl_core::node::Value::F32(self.number(node, "strength")?),
                ),
        );
        self.pass_colors.insert(node, target);
        Ok(())
    }

    /// A `pass.dof` node: the depth-of-field chain, expanded (ADR 0065,
    /// the pyramid's pattern at its second consumer).
    ///
    /// Three screen passes: one turns opaque depth into signed circles of
    /// confusion, one blurs the far field by them with the occlusion
    /// fade, one dilates the near field and composites it over the far
    /// result. The labels derive from the node's — `<label> circles`,
    /// `<label> far blur`, `<label> near blur` — and are the contract the
    /// host tunes through; the lens' numbers are the node's settings,
    /// seeded into the passes' parameters. The two intermediates are the
    /// expansion's own full-resolution HDR transients, invisible to the
    /// document.
    ///
    /// The depth is sampled, never attached: a depth texture cannot be
    /// both in one pass, and the chain has no depth test to make. Whatever
    /// the document wires — a `resource.depth` or a material pass's
    /// output, a depth prepass most of all — follows the same rule
    /// `pass.peel` and the background effect already do.
    pub(super) fn depth_of_field(&mut self, node: NodeId) -> Result<(), PipelineError> {
        let name = self.label(node);
        let invalid = |reason: String| PipelineError::Dof {
            node: name.clone(),
            reason,
        };
        let source = self.fed(node, "image").expect("validated mandatory image");
        let scene = *self.colors.get(&source).ok_or_else(|| {
            invalid(
                "image must name a resource.color — depth of field blurs linear radiance"
                    .to_string(),
            )
        })?;
        let depth = self.fed(node, "depth").expect("validated mandatory depth");
        let depth = self.sampled_depth(depth, &name)?;
        // The disc cap bounds every gather; nothing sensible comes of a
        // non-positive one, and the name is better than a silent no-op.
        let max_coc = self.number(node, "max_coc")?;
        if max_coc <= 0.0 || !max_coc.is_finite() {
            return Err(invalid("max_coc must be finite and positive".to_string()));
        }

        // Full-resolution transients: the discs are per-pixel data, and a
        // half-res CoC would offset every gather's answer.
        let hdr = gbuffer_format(abi::GBufferPrecision::HighDynamicRange);
        let coc = self
            .graph
            .resource(ResourceDesc::color(format!("{name} coc"), hdr));
        let far = self
            .graph
            .resource(ResourceDesc::color(format!("{name} far"), hdr));

        let mut circles = PassDesc::screen(format!("{name} circles"), "wxsl.dof_coc")
            .with_color(self.color_attachment(coc))
            .with_reads(vec![Read::current(depth)]);
        for (parameter, setting) in [
            ("near", "near"),
            ("far", "far"),
            ("focus", "focus"),
            ("f_number", "f_number"),
            ("focal", "focal"),
            ("sensor", "sensor"),
        ] {
            circles = circles.with_parameter(
                parameter,
                wxsl_core::node::Value::F32(self.number(node, setting)?),
            );
        }
        self.graph.pass(circles);

        self.graph.pass(
            PassDesc::screen(format!("{name} far blur"), "wxsl.dof_far")
                .with_color(self.color_attachment(far))
                .with_reads(vec![Read::current(scene), Read::current(coc)])
                .with_parameter("max_coc", wxsl_core::node::Value::F32(max_coc)),
        );

        let target = self.write_target(node)?;
        self.graph.pass(
            PassDesc::screen(format!("{name} near blur"), "wxsl.dof_near")
                .with_color(self.color_attachment(target))
                .with_reads(vec![
                    Read::current(scene),
                    Read::current(coc),
                    Read::current(far),
                ])
                .with_parameter("max_coc", wxsl_core::node::Value::F32(max_coc)),
        );
        self.pass_colors.insert(node, target);
        Ok(())
    }

    pub(super) fn declare_shadow_maps(&mut self, node: NodeId) -> Result<(), PipelineError> {
        if let Some((existing, _)) = self.shadow_source {
            return Err(PipelineError::TwoShadowSources {
                nodes: vec![
                    label(self.document, self.registry, existing),
                    self.label(node),
                ],
            });
        }
        let maps = self.graph.declare_shadow_maps(
            ResourceDesc::color("shadow maps", DEPTH_FORMAT)
                .with_extent(Extent::Fixed {
                    width: abi::SHADOW_ATLAS_SIZE,
                    height: abi::SHADOW_ATLAS_SIZE,
                })
                // Both because nothing in the pass list reads this resource:
                // it is sampled through the frame group, so the graph infers
                // neither the usage nor the lifetime and is told both.
                .with_usage(TextureUsages::TEXTURE_BINDING)
                .persistent(0),
        );
        self.shadow_source = Some((node, maps));
        Ok(())
    }

    pub(super) fn declare_gbuffer(&mut self, node: NodeId) -> Result<(), PipelineError> {
        // The layout is the config's plan — the enabled set's requests
        // plus any feature channels — not the document's: widening either
        // rewires nothing, because the lighting pass reads the G-buffer by
        // the shape it was generated for.
        let layout = self
            .config
            .plan()
            .map_err(|error| PipelineError::ChannelCollision {
                node: self.label(node),
                error: error.to_string(),
            })?;
        let targets: Vec<ResourceId> = layout
            .layout()
            .iter()
            .map(|entry| {
                self.graph.resource(ResourceDesc::color(
                    format!("gbuffer {}", entry.field),
                    gbuffer_format(entry.precision),
                ))
            })
            .collect();
        let depth = self
            .graph
            .resource(ResourceDesc::color("gbuffer depth", DEPTH_FORMAT));
        self.layout = Some(layout.layout().to_vec());
        self.gbuffers.insert(node, (targets, depth));
        Ok(())
    }

    pub(super) fn declare_color(&mut self, node: NodeId) -> Result<(), PipelineError> {
        let name = self.label(node);
        let precision_text = self.setting(node, doc::SETTING_PRECISION);
        let format = if precision_text.trim() == "float" {
            TextureFormat::Rgba32Float
        } else {
            gbuffer_format(
                abi::GBufferPrecision::parse(&precision_text).ok_or_else(|| {
                    PipelineError::UnknownPrecision {
                        node: name.clone(),
                        precision: precision_text,
                    }
                })?,
            )
        };

        // An imported target is the host's: sized by whoever created it,
        // so the size settings have nothing to say and are refused rather
        // than ignored — a document that says both would only ever be
        // obeyed on one of them.
        if self
            .setting(node, doc::SETTING_IMPORTED)
            .trim()
            .eq_ignore_ascii_case("true")
        {
            let size_text = self.setting(node, doc::SETTING_SIZE);
            let scale = self.setting(node, doc::SETTING_SCALE);
            let history = self.setting(node, doc::SETTING_HISTORY);
            let conflicted = [
                (
                    !size_text.trim().eq_ignore_ascii_case("viewport"),
                    ("size", size_text),
                ),
                (scale.trim() != "1", ("scale", scale)),
                (history.trim() != "0", ("history", history)),
            ];
            for (conflict, (setting, value)) in conflicted {
                if conflict {
                    return Err(PipelineError::ImportedTargetSized {
                        node: name.clone(),
                        setting: setting.to_string(),
                        value: value.trim().to_string(),
                    });
                }
            }
            let id = self.graph.resource(ResourceDesc::imported(name, format));
            self.colors.insert(node, id);
            return Ok(());
        }

        // `viewport` sizes with the frame (the `scale` setting); anything
        // spelled `WxH` is fixed pixels — the LUTs and atlases whose size
        // is a fact of their contents, not of the window.
        let size_text = self.setting(node, doc::SETTING_SIZE);
        let extent = if size_text.trim().eq_ignore_ascii_case("viewport") {
            Extent::Viewport {
                scale: self.number(node, doc::SETTING_SCALE)?,
            }
        } else {
            let (width, height) = size_text.trim().split_once(['x', 'X']).ok_or_else(|| {
                PipelineError::UnknownSize {
                    node: name.clone(),
                    size: size_text.clone(),
                }
            })?;
            let (width, height) = (width.trim(), height.trim());
            let (width, height): (u32, u32) = match (width.parse(), height.parse()) {
                (Ok(width), Ok(height)) => (width, height),
                _ => {
                    return Err(PipelineError::UnknownSize {
                        node: name.clone(),
                        size: size_text.clone(),
                    })
                }
            };
            Extent::Fixed { width, height }
        };
        let history = self.number(node, doc::SETTING_HISTORY)? as u32;
        let mut desc = ResourceDesc::color(name, format).with_extent(extent);
        desc.persistence = if history == 0 {
            Persistence::Transient
        } else {
            Persistence::Persistent { history }
        };
        let id = self.graph.resource(desc);
        self.colors.insert(node, id);
        Ok(())
    }

    pub(super) fn declare_depth(&mut self, node: NodeId) -> Result<(), PipelineError> {
        let scale = self.number(node, doc::SETTING_SCALE)?;
        let id = self.graph.resource(
            ResourceDesc::color(self.label(node), DEPTH_FORMAT)
                .with_extent(Extent::Viewport { scale }),
        );
        self.depths.insert(node, id);
        Ok(())
    }

    /// A storage buffer: `bytes` big, persistent when `history` says so.
    /// The scheduler treats it as a resource that never aliases and orders
    /// the passes around it, exactly as it does for attachments — the
    /// document adds nothing the engine did not already reason about.
    pub(super) fn declare_buffer(&mut self, node: NodeId) -> Result<(), PipelineError> {
        let name = self.label(node);
        let bytes_text = self.setting(node, doc::SETTING_BYTES);
        let bytes: u64 = bytes_text
            .trim()
            .parse()
            .map_err(|_| PipelineError::BadBufferSize {
                node: name.clone(),
                value: bytes_text.clone(),
            })?;
        if bytes == 0 {
            return Err(PipelineError::BadBufferSize {
                node: name.clone(),
                value: bytes_text,
            });
        }
        let history = self.number(node, doc::SETTING_HISTORY)? as u32;
        let mut desc = ResourceDesc::buffer(name, bytes);
        desc.persistence = if history == 0 {
            Persistence::Transient
        } else {
            Persistence::Persistent { history }
        };
        let id = self.graph.resource(desc);
        self.buffers.insert(node, id);
        Ok(())
    }

    /// A numeric setting, with the setting's name in the error.
    fn number(&self, node: NodeId, setting: &'static str) -> Result<f32, PipelineError> {
        let text = self.setting(node, setting);
        text.trim().parse().map_err(|_| PipelineError::BadNumber {
            node: self.label(node),
            setting,
            value: text,
        })
    }

    /// The pass's execution policy, from the `policy` setting. The
    /// default setting text parses to `per frame`, so documents that never
    /// mention it compile exactly as they always did.
    pub(super) fn policy(&self, node: NodeId) -> Result<Policy, PipelineError> {
        let text = self.setting(node, doc::SETTING_POLICY);
        Policy::parse(&text).ok_or_else(|| PipelineError::UnknownPolicy {
            node: self.label(node),
            policy: text,
        })
    }

    /// How the pass orders its draws, from the `sort` setting. The
    /// default setting text parses to `none` — submission order — so
    /// documents that never mention it draw exactly as they always did
    /// (plan5 D3/D4).
    pub(super) fn sort(&self, node: NodeId) -> Result<SortOrder, PipelineError> {
        let text = self.setting(node, doc::SETTING_SORT);
        SortOrder::parse(&text).ok_or_else(|| PipelineError::UnknownSort {
            node: self.label(node),
            sort: text,
        })
    }

    /// The tier of the source's draws this pass takes, by each material's
    /// `max_layers` (plan5 D5). Only the authorable shapes parse; the
    /// `Peeled` filters are `pass.peel`'s own expansion.
    pub(super) fn layers(&self, node: NodeId) -> Result<LayerFilter, PipelineError> {
        let text = self.setting(node, doc::SETTING_LAYERS);
        LayerFilter::parse(&text).ok_or_else(|| PipelineError::UnknownLayers {
            node: self.label(node),
            layers: text,
        })
    }

    /// How the pass's fragments land in its target.
    pub(super) fn blend(
        &self,
        node: NodeId,
    ) -> Result<Option<crate::types::BlendState>, PipelineError> {
        let text = self.setting(node, doc::SETTING_BLEND);
        match text.trim().replace('_', " ").to_ascii_lowercase().as_str() {
            "" | "opaque" => Ok(None),
            "alpha over" => Ok(Some(ALPHA_OVER)),
            other => Err(PipelineError::UnknownBlend {
                node: self.label(node),
                blend: other.to_string(),
            }),
        }
    }
}

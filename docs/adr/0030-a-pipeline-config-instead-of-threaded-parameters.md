# 0030. One `PipelineConfig` instead of parameters threaded by hand

Date: 2026-09-12

Status: Accepted

## Context

Adding the lighting set (ADR 0028) meant touching seven sites for one
concept: `StockPipeline::graph`'s signature, `deferred_graph`,
`Renderer::new`, `rebuild`, `set_lighting`, the variant-cache keys and
the tests. The pattern was general — every future pipeline knob (screen
effects, execution policies, target policy) would cost the same
seven-site change — and it is what made a "small" feature a
milestone-shaped event. The knobs are facts about *one* thing, the
pipeline being run, but the code carried them as independent parameters.

## Decision

`wxsl_render::pipeline::PipelineConfig` is the one value every stock
pass list is built under:

```rust
pub struct PipelineConfig {
    pub target: TargetConfig,   // size, format, clear colour
    pub lighting: LightingSet,  // the G-buffer's shape (ADR 0028)
}
```

`StockPipeline::graph(&self, config: &PipelineConfig)` builds a pass
list from it; the `Renderer` holds one and mutates it — `set_pipeline`,
`set_lighting` and `resize` are now small mutations of `self.config` —
and `rebuild` reads it. A future knob is a field here plus the places
that genuinely need to read it, not a new parameter threaded through
every signature.

`forward_graph` and `deferred_graph` stay as free functions taking what
they take. They are the *reference* pass lists: the parity tests future
presets are held to want them callable without a config, and their
signatures name exactly the facts each one consumes.

## Alternatives considered

* **A trait `PipelineDescription` implemented by stock pipelines.**
  Rejected for now: it adds an interface before there is a second
  implementor-shaped thing (pipeline *documents*, plan2 P3, will be the
  second implementor and will use this config as one of its inputs).
  When documents land, this struct is what they compile *with*.
* **Keep separate parameters and add more as needed.** This is the
  status quo that produced the seven-site change; rejected by its own
  track record.

## Consequences

* `Renderer::set_lighting` remains the validated entry point (module
  presence, attachment budget); the config field itself is
  renderer-private, so nothing can bypass the checks by writing it.
* The variant cache and bind-group derivations still read the same facts
  — through the renderer's accessors, which now read the config.
* When plan2 P3 (pipeline documents) lands, `PipelineConfig` is the
  *knobs* half of the pair: the document is the shape, the config is the
  settings a caller supplies beside it.

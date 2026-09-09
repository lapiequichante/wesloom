# 0020. A node definition is derived from its WXSL source

Date: 2026-09-09

Status: Accepted

## Context

Every function node in the library was written twice. Once as WXSL —
`shaders/lighting/pbr_direct.wxsl`, the real thing, with the body — and once
as a `WxslFunction` descriptor in `wxsl-stdlib/src/registry.rs` restating
its module, its name, its parameters and their types and its return shape.
The Rust half added nothing the file did not already say; it existed because
nothing read the file.

That cost was tolerable at 28 files and about to stop being. The pipeline
plan's next milestones each add a pile of nodes — texture sampling (M3),
lighting models (M5), postprocess (M6) — so doing this after them means
writing every one of those twice too. And the duplication had a failure mode
of its own: a descriptor could disagree with its function, and
`every_called_function_lives_in_a_module_this_crate_ships` existed to catch
exactly that, checking that the module a descriptor named existed and
declared a function of that name. It could not check the *parameters*; a
descriptor with the wrong socket order or a missing argument compiled fine
here and failed in the shader compiler later, pointing at generated code.

The convention that makes derivation possible already held: **one `fn` per
file**, which `shaders/README.md` and `docs/architecture.md` both state as
the authoring rule, and which all 28 files followed. More of a node than the
signature was already in the source, too — `fbm3.wxsl` declares its own
`@macro const` defaults, and `pbr_direct_split.wxsl` pairs a `struct` with
the function that returns it, which is exactly `FunctionReturn::Struct`.

Two things a `.wxsl` file did *not* say: what a node is called in a palette,
and what an unconnected socket falls back to.

## Decision

**The `.wxsl` file is the node definition.** `wxsl_lang::node_from_source(source,
module_path)` parses one file and answers with the `NodeDefinition` the
graph, the editor and codegen need. Nothing about a function's interface is
restated in Rust.

What comes from where:

| In the file | Becomes |
|---|---|
| the module path, from the file's place in the tree | the node id and category — `package::math::safe_normalize` is `math.safe_normalize`, category `math` |
| `fn safe_normalize<T: vec2f \| vec3f \| vec4f>(…)` | one `GenericParam` per type parameter, whose bound list *is* `GenericParam::allowed` (ADR 0015, ADR 0018) |
| each parameter, in order | one input `Socket` |
| the return type | one output socket named `out` — or one per field, when it names a struct declared in the same file |
| `@macro const WXSL_FBM_OCTAVES: i32 = 5;` | a `MacroDef` with that default |
| the first line of the leading comment block | the label |
| the paragraph after it | the documentation |
| a parameter's trailing comment | that socket's doc, and its default after `@default` |
| an `@macro const`'s trailing comment | that macro's doc |

**The metadata a file cannot state in code is stated in comments, not in new
syntax.** Real attributes (`@label`, `@default`) would touch the lexer, the
LALRPOP grammar, `cond`, `resolve`, `mono` and `emit` — every pass would
carry them only to ignore them, for metadata the compiler has no use for. A
comment convention keeps node metadata out of the language it is not part
of. The two conventions:

* the leading comment block is a **label line**, a blank line, then a
  **documentation paragraph**. Whatever follows that paragraph is for
  somebody reading the source — how the body works, which paper the
  technique is from — and never reaches the editor.
* a parameter's own trailing `//` comment is its documentation, with
  `@default <value>` at the end. One number spreads over the socket's type
  (`@default 0.5` on a `vec3f` is `vec3f(0.5)`), which is the only form a
  generic socket can take; several are that type's components in order
  (`@default 0.0, 1.0, 0.0`). No `@default` means the input is mandatory.
  Any other `@word` is reported as a typo rather than ignored, because a
  silently-dropped `@defualt` leaves a socket mandatory for no visible
  reason.

Parameters are therefore written **one per line**: a comment after the
second parameter on a shared line has nothing to say which of them it
belongs to, so the derivation bounds each parameter's comment by the next
one's position and a shared line yields nothing.

**The derivation is a public `wxsl-lang` API with two callers.** The library
is derived at *build* time: `wxsl-stdlib/build.rs` calls `node_from_source`
over every file under `shaders/<category>/` and writes the results out as
Rust source that `registry.rs` includes, so the shipped crate pays nothing
at runtime and `wxsl-lang` is a **build**-dependency — `cargo tree -p
wxsl-stdlib -e normal` still shows only `wxsl-core`, and a consumer who
wants the node library does not compile a shader compiler to get it. The
editor will call the same function at runtime, on a file the user just
wrote, which is what the WXSL code editor (plan milestone M10) needs to make
a saved buffer appear in the palette. One implementation of the convention,
two callers.

`wxsl-lang` gains a dependency on `wxsl-core` for this. The arrow points
*into* `wxsl-core` like every other one (ADR 0002), and `wxsl-core` still
knows nothing about the compiler.

**Operators stay hand-written.** `math.add`, `convert.split`, `logic.select`
and the rest are inline WXSL expressions with no `.wxsl` file to derive from
— there is no source to be the definition. The split in `registry.rs` was
always between operators and functions; now only one side of it is Rust.

## Alternatives considered

**Attributes in the language.** `@label("Safe normalize")`, `@default(0.5)`.
Rejected: it makes the compiler carry, validate and re-emit metadata it has
no use for, through six passes, and every one of them becomes a place the
metadata can be lost. The language is for describing computation.

**Derive at runtime, with `wxsl-lang` as an ordinary dependency of
`wxsl-stdlib`.** Simpler — no build script, no generated file — and rejected
for what it does to the dependency graph: every consumer of the node library
would compile the WXSL compiler, including the `--no-default-features` build
whose whole point is that it is the graph model and nothing else.

**A build script that parses the files itself.** Rejected outright: it makes
the convention have two implementations, and the second one (the editor's,
at runtime) is the one that has to agree with the first without being able
to test against it. Writing the derivation as an API with a build-time caller
rather than as a build script is the entire difference between this
milestone and M10 fitting together and M10 needing its own parser.

**Keep the descriptors and check them against the sources in a test.** This
is what the deleted test was, in weaker form. It catches disagreement but
does not remove the second copy, so every new node still costs two edits and
the check can only ever verify what it can see — names, not defaults, not
docs, not socket order beyond what the WXSL declares.

## Consequences

`registry.rs` lost about 400 lines and nine `pub fn`s
(`color_nodes`, `lighting_nodes`, `generative_nodes`, `sdf_nodes`,
`space_nodes`, `animation_nodes`, `distort_nodes`, `math_function_nodes`,
`safe_normalize_nodes`). Nothing outside the crate called them. Adding a
function node is now one file and no Rust at all.

`every_called_function_lives_in_a_module_this_crate_ships` is **deleted** —
not because the drift it caught stopped mattering, but because it stopped
being expressible: the descriptor *is* the file. Two tests take its place,
and they check the directions that are still open:
`every_function_node_is_derived_from_a_shader_this_crate_ships` (every
shipped non-ABI module became a node, and no derived node points anywhere
else) and `the_generated_table_is_what_the_derivation_answers_now`, which
re-derives every node from the embedded source at test time and compares it
with the generated table. The second one is what makes the build script's
serializer trustworthy: the serializer is mechanical — `NodeDefinition` in,
Rust source out, no knowledge of the convention — but it is still code
between the shader and the registry, so it gets an agreement test, the same
way the two MSDF implementations do (ADR 0014).

`MacroDef::doc` changed from `&'static str` to `String`, because a macro
declaration can now come from a file read at runtime.

What a `.wxsl` file looks like changed, and every one of the 28 was rewritten
to match: a label line was added at the top of each comment block, the
implementation notes moved below the documentation paragraph, and every
parameter list became one-per-line with a `@default`. `shaders/README.md`
carries the rules.

Two things to know for later:

* **A new socket type is now a language-visible change.** When M3 adds
  texture and sampler sockets, `node_from_source`'s type mapping is where
  `texture_2d<f32>` becomes a socket type — one place, but it has to be
  edited alongside `ValueType`.
* **A node's documentation is now the file's second paragraph, exactly.** A
  doc worth editing is edited in the shader. This is the one piece of node
  metadata that got *shorter* in the move; the prose the registry used to
  carry was written for the editor, and where it was better than the file's
  own it was moved into the file rather than dropped.

If the derivation's rules change, `shaders/README.md`, `wxsl-lang`'s
`node` module docs and this ADR are the three places that state them.

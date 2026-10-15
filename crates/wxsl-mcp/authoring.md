# Writing materials and WXSL

For an agent driving the editor through the MCP server (ADR 0083), and for
anyone writing a function in the editor's code panel. The node reference is
the `wxsl://nodes` resource (`docs/reference/nodes.md` in the repository);
the shader ABI is `wxsl://abi`.

Every snippet below is checked: the WXSL ones are saved with
`function_save` and the JSON ones are run, in order, against the `editor`
example's starting graph, by `crates/wxsl/tests/mcp.rs` and by
`wxsl-mcp`'s own tests.

## A material is a graph

A material is a graph of nodes wired into one `output.surface` node. Its
inputs are the surface: `base_color`, `metallic`, `roughness`, `normal`,
`emissive`, `occlusion`, `alpha` and a few more. An input left unconnected
keeps its default. Colours are linear, never sRGB; the `tonemap` pass at
the end of the frame is the only display transform.

Look before editing: `graph_get` gives every node with its `id` and `def`,
and the edges. In the starting graph, the `output.surface` node is `15`.
`nodes_search` finds a node by words in its id or label, and the
reference says what its sockets are called and what they take.

Edit with `graph_edit`: a batch of commands, applied atomically, as one
undo step. A batch can name a node it adds with `"ref"` and refer to it as
`"$name"` later in the same batch. This one replaces the material with
steel and rust patches:

```json tool
{"name": "graph_edit", "arguments": {"commands": [
  {"command": "remove_nodes", "nodes": [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14,
    16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26]},
  {"command": "add_node", "ref": "p", "node": {"def": "input.world_position"}},
  {"command": "add_node", "ref": "scale", "node": {"def": "math.multiply",
    "generics": {"A": "vec3f", "B": "f32"}, "params": {"b": {"f32": 4.0}}}},
  {"command": "add_node", "ref": "noise", "node": {"def": "generative.fbm3"}},
  {"command": "add_node", "ref": "mask", "node": {"def": "math.smootherstep",
    "generics": {"T": "f32"}, "params": {"edge0": {"f32": 0.47}, "edge1": {"f32": 0.51}}}},
  {"command": "add_node", "ref": "color", "node": {"def": "math.mix",
    "generics": {"T": "vec3f"},
    "params": {"a": {"vec3f": [0.56, 0.57, 0.58]}, "b": {"vec3f": [0.3, 0.08, 0.02]}}}},
  {"command": "add_node", "ref": "metal", "node": {"def": "math.mix",
    "generics": {"T": "f32"}, "params": {"a": {"f32": 1.0}, "b": {"f32": 0.0}}}},
  {"command": "connect", "from": {"node": "$p", "socket": "out"},
    "to": {"node": "$scale", "socket": "a"}},
  {"command": "connect", "from": {"node": "$scale", "socket": "out"},
    "to": {"node": "$noise", "socket": "p"}},
  {"command": "connect", "from": {"node": "$noise", "socket": "out"},
    "to": {"node": "$mask", "socket": "x"}},
  {"command": "connect", "from": {"node": "$mask", "socket": "out"},
    "to": {"node": "$color", "socket": "t"}},
  {"command": "connect", "from": {"node": "$mask", "socket": "out"},
    "to": {"node": "$metal", "socket": "t"}},
  {"command": "connect", "from": {"node": "$color", "socket": "out"},
    "to": {"node": 15, "socket": "base_color"}},
  {"command": "connect", "from": {"node": "$metal", "socket": "out"},
    "to": {"node": 15, "socket": "metallic"}}
]}}
```

The answer lists the ids the refs got and the diagnostics after
recompiling. A refused command refuses the whole batch and changes nothing.

* **Values** are tagged by type: `{"f32": 0.5}`, `{"vec3f": [1, 0.5, 0]}`,
  `{"bool": true}`, `{"i32": 4}`. A bare number is refused.
* **Generic sockets** (`T`, `A`, `B` in the reference) take the type of
  what is connected. Pin one with `"generics"` on the node, or with
  `set_generic` afterwards. Types are WGSL's: `f32`, `vec2f`, `vec3f`,
  `vec4f`, `mat3x3f`, `mat4x4f`, `bool`, `i32`, `u32`.
* **Pinned inputs** go in `"params"` when the node is added, or through
  `set_param`. Connecting an input replaces its pinned value.
* **A value the user will tweak** is a `param.value` node: its `name`
  setting is what the host writes, and changing it costs no recompile.

Then look: `view_screenshot` with `"viewport": true` shows the preview
alone. `problems_list` gives every diagnostic. `history` lists the steps,
yours marked as the agent's, and undoes them.

```json tool
{"name": "view_screenshot", "arguments": {"viewport": true, "max_width": 256}}
```

## A function is a node

When no node does what you need, write one. `function_save` takes the
source of one WXSL function and registers it as `user.<fn name>`, in the
palette for this session. Saving the same name again replaces it, and
graphs using it recompile.

```wxsl
// Rust albedo
//
// The base colour of rust over steel: bare steel where `mask` is 0, rust
// where it is 1, the rust going from dark pits to orange flakes with
// `detail`.

fn rust_albedo(
    mask: f32,   // 0 = bare steel, 1 = rust. @default 0.0
    detail: f32, // A second noise in 0..1 that varies the rust. @default 0.5
) -> vec3f {
    let steel = vec3f(0.56, 0.57, 0.58);
    let pits = vec3f(0.08, 0.025, 0.008);
    let flakes = vec3f(0.32, 0.09, 0.025);
    return mix(steel, mix(pits, flakes, detail), mask);
}
```

The source is the node, so its shape matters:

* **A label line, a blank comment line, one paragraph.** The label names
  the node, the paragraph is its documentation. Comments after that
  paragraph are for readers of the source only.
* **One `fn`.** Its parameters are the inputs, in order. Its return value
  is the output `out`; a struct declared in the same source gives one
  output per field.
* **One parameter per line**, each with its own comment ending in
  `@default <value>`: one number spreads over every component, several are
  the components in order (`@default 1.0, 0.5, 0.0`). A parameter with no
  `@default` is a mandatory input.
* **WGSL types and builtins**: `f32`, `vec3f`, `mix`, `clamp`, `select`.
  There are no implicit conversions: `x * 2` with `x: f32` is an error,
  `x * 2.0` is not.
* **Pure.** A function sees only its parameters. Position, normal, UV and
  time come into the graph through `input.*` nodes and are wired in.
* **Library functions** are imported by module path, the module being
  the node's id with `package::` in front, and the function named after
  the file:

```wxsl
// Soft mask
//
// A 0..1 mask that ramps up between `low` and `high`, with no crease
// where it meets 0 and 1.

import package::math::smootherstep::smootherstep;

fn soft_mask(
    value: f32, // What to threshold. @default 0.5
    low: f32,   // Where the mask starts rising. @default 0.4
    high: f32,  // Where it reaches 1. @default 0.6
) -> f32 {
    return smootherstep<f32>(low, high, value);
}
```

  A generic library function is called with its type written out, as
  `smootherstep<f32>` here.
* **Generic over widths**, when the same body works for every width:
  `fn f<T: f32 | vec3f>(x: T, …) -> T`. It is checked when a node first
  uses it rather than when it is saved.
* **A knob that changes the code**, like a loop bound, is a macro:
  `@macro const OCTAVES: i32 = 4; // How many octaves.` It shows up as a
  setting of the graph, which `macros_set` pins.

Add the node with `add_node` like any other. A ref lasts as long as its
batch, so a later batch uses the ids the first one's answer listed under
`refs`; here `mask` became `30`:

```json tool
{"name": "graph_edit", "arguments": {"commands": [
  {"command": "add_node", "ref": "albedo", "node": {"def": "user.rust_albedo"}},
  {"command": "connect", "from": {"node": 30, "socket": "out"},
    "to": {"node": "$albedo", "socket": "mask"}},
  {"command": "connect", "from": {"node": "$albedo", "socket": "out"},
    "to": {"node": 15, "socket": "base_color"}}
]}}
```

## Reading the errors

A refused save says which line, and why. The common ones:

```wxsl refused
// Glow
//
// Brightens a value.

fn glow(
    x: float, // @default 1.0
) -> float {
    return x * 2.0;
}
```

```text
`float` is not a type a node socket can carry
```

WXSL's types are WGSL's: `f32`, not `float`; `vec3f`, not `vec3` or
`float3`.

```wxsl refused
// Glow
//
// Brightens a value.

fn glow(
    x: f32, // @defualt 1.0
) -> f32 {
    return x * 2.0;
}
```

```text
`@defualt` is not a socket annotation; the only one is `@default`
```

```wxsl refused
fn glow(
    x: f32, // @default 1.0
) -> f32 {
    return x * 2.0;
}
```

```text
a node source opens with a comment block
```

A source that parses can still be wrong. Then the device's validator
answers, quoting the WGSL the function compiled to, which is your source
with the comments gone. The caret marks the expression, and the last
lines say what is wrong with it:

```wxsl refused
// Glow
//
// Brightens a colour.

fn glow(
    color: vec3f, // @default 1.0
) -> f32 {
    return color * 2.0;
}
```

```text
does not match the declared return type
```

The return type is `f32` and `color * 2.0` is a `vec3f`. Other validator
messages to expect: `no definition in scope for identifier` (a typo, or a
library function not imported), and `Operation Multiply can't work with`
(an `f32` times an `i32`: write `2.0`, not `2`).

In a `graph_edit`, the answer names the command by its index in the batch:

* `no node definition` — the `def` is not an id in the reference;
* `no input #15.metalic` — a socket name, misspelt;
* `type mismatch connecting #1.out (vec3f) to #15.metallic (f32)` —
  convert first: `convert.split.vec3f` takes a vector apart,
  `convert.combine.vec3f` builds one, `convert.splat` spreads a scalar;
* ``no node was added as `mask` before this command`` — a ref used
  outside the batch that named it. The ids are in that batch's answer,
  under `refs`.

After an edit, `diagnostics.material` holds what the material's
compilation said, empty when it is clean. A function that broke the
material is named there.

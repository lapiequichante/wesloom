# Using WXSL from C and C++

The device-free half of WXSL (document compilation, scheduling, layouts, the
WXSL compiler) is Rust. A C or C++ application reaches it through a small,
data-only C ABI: JSON documents in; plans, WGSL and computed layouts out
([ADR 0049](adr/0049-share-the-core-through-a-data-only-c-abi.md)). It never
reimplements those rules.

There are three ways to use it:

| You want | Use | Rust at runtime? |
|---|---|---|
| A renderer for shipped content | The Dawn renderer, on an offline **bundle** exported by `bake_docs` | no |
| A renderer that compiles at runtime | The Dawn renderer plus `wxsl::Compiler` | yes, loaded dynamically |
| Your own renderer | The C ABI directly (`wxsl.h`) | yes, loaded dynamically |

The rest of this document covers:

- [the headers](#headers);
- [the C ABI](#the-c-abi): calls, ownership, requests and responses;
- [the C++ classes](#the-c-classes) `wxsl::Renderer` and `wxsl::Compiler`.

Building and testing the Dawn backend is described in
[`dawn/README.md`](../dawn/README.md).

## Headers

| Header | What | Generated from |
|---|---|---|
| `crates/wxsl-ffi/include/wxsl.h` | The C ABI: functions, `WxslBytes`, `WxslField`, status codes | `crates/wxsl-ffi/src/ffi.rs` |
| `dawn/include/wxsl_host.h` | Fixed host layouts: scene uniform, lights, vertices, UI and MSDF buffers | `wxsl_core::host` ([ADR 0050](adr/0050-host-shared-layouts-are-generated.md)) |
| `dawn/include/renderer.hpp` | `wxsl::Renderer`, the headless Dawn renderer | hand-written |
| `dawn/include/compiler.hpp` | `wxsl::Compiler`, the runtime compiler | hand-written |

Never edit the generated headers by hand. Their comments use `/** */`, so
Doxygen can read all four if you want HTML.

## The C ABI

Build the library with `cargo build -p wxsl-ffi --lib`. This produces
`libwxsl_ffi.so`, `libwxsl_ffi.dylib` or `wxsl_ffi.dll`. The library links
no GPU, window or editor code.

### A minimal call

The snippet below is adapted from `dawn/tests/abi_smoke.c`, which CI runs.
It assumes the symbols were loaded with `dlopen` and `dlsym` (or
`LoadLibrary` and `GetProcAddress`) into variables of the
`wxsl_fn_<name>` types the header declares.

```c
#include "wxsl.h"

if (wxsl_abi_version() != WXSL_ABI_VERSION) { /* wrong library */ }

const char *request = "{\"pipeline\": <a pipeline document>}";
WxslBytes input = {(const uint8_t *)request, strlen(request)};
WxslResult *result = wxsl_compile_pipeline(WXSL_ABI_VERSION, input);

WxslBytes json = wxsl_result_json(result);   /* {"version":1,"abi":1,"status":...,"data":...} */
if (wxsl_result_status(result) != WXSL_STATUS_SUCCESS) {
    fwrite(json.data, 1, json.len, stderr); /* data.message says why */
}
/* ... copy what you need ... */
wxsl_result_free(result);                  /* invalidates every view */
```

### Ownership and threading

- **Check the version first.** Call `wxsl_abi_version()` and compare it to
  `WXSL_ABI_VERSION` before anything else. Every call also takes the
  version, and refuses a mismatch before it reads the request. This C ABI
  version and the shader/document `abi` revision are separate contracts,
  even though both are currently 1.
- **Inputs are borrowed.** They are UTF-8 JSON byte spans, not
  NUL-terminated, and only need to stay readable during the call.
- **Every call returns its own result**, even on failure. Check
  `wxsl_result_status`, then read `wxsl_result_json`.
- **Outputs are views into the result:** WGSL, parameter bytes and field
  rows. Copy what you need: `wxsl_result_free` releases the result once and
  invalidates **every** view. Never free a view yourself.
- **Null handling:** the accessors of a null result return empty views or
  `WXSL_STATUS_INVALID_ARGUMENT`, and `wxsl_result_free(NULL)` does
  nothing. A null input span that claims a length, or an oversized span, is
  refused.
- **Undefined behaviour:** dangling pointers, a double free and stale views
  are caller errors, not diagnostics.
- **Panics:** a Rust panic becomes `WXSL_STATUS_INTERNAL_ERROR`; an
  allocator abort does not. The library refuses to build with
  `panic=abort`.
- **Threads:** independent calls may run concurrently; there is no global
  state. Do not use or free one result from two threads at once.

### Status codes

| Status | Meaning |
|---|---|
| `SUCCESS` | `data` holds the output |
| `INVALID_ARGUMENT` | null or oversized input span, or null result |
| `ABI_MISMATCH` | the caller's version is not the library's; nothing was read |
| `INVALID_DOCUMENT` | the request or a document in it did not parse or validate, or named something unknown |
| `COMPILE_ERROR` | WXSL, codegen or pipeline compilation failed |
| `INCOMPATIBLE` | the scene does not fit the setup; `data.incompatibilities` lists every mismatch |
| `INTERNAL_ERROR` | a contained panic: a bug in the library |

### Requests

Every request may carry `version` (the request schema, currently 1) and
`abi` (the shader ABI). A missing field means "this build's". A newer
schema or a foreign ABI revision is refused by name. Nested graph and scene
documents keep their own version checks.

| Function | Required | Optional |
|---|---|---|
| `wxsl_compile_pipeline` | `pipeline`: a pipeline document | `config` |
| `wxsl_compile_shader` | exactly one of `root` (a module path) or `effect` (a shipped effect id) | `library`, `macros`, `config` |
| `wxsl_compile_material` | `graph`: a surface graph; `stage`: a stage name such as `forward_lit` or `gbuffer` | `material`, `config`, `library` |
| `wxsl_check_setup` | `pipeline` and `scene` documents | `config`, `library` |
| `wxsl_import_gltf` | `path`: a `.gltf` or `.glb` file | none |
| `wxsl_import_glb` | the request *is* the GLB's bytes, not JSON | none |

**`config`** describes the frame the request is for. Every field is
optional:

| Field | Default | Example |
|---|---|---|
| `target` | 800×600 `rgba8unorm` | `{"width":1280,"height":720,"format":"rgba16float"}` |
| `profile` | full | `{"compute":false,"storage_buffers":false}` for WebGL2 |
| `lighting_models` | `["wxsl.pbr"]` | `["pbr","clearcoat"]` |
| `features` | none | `["subsurface"]` |
| `macros` | none | `{"NAME":{"int":3}}` |

Formats are lowercase neutral names (`rgba16float`), and other enum values
are snake_case. Width and height must be nonzero.

**`material`** is a `MaterialConfig` as the scene document spells it, for
example `{"model":"wxsl.pbr","macros":{...}}`. Its features come from
`config.features`, the pipeline's plan, not from the material.

**Macros** go in three different places:

| Macros for | Go in |
|---|---|
| a material | `material.macros` |
| a raw or effect shader | `macros` |
| pipeline expansion | `config.macros` |

**`library`** overlays modules on the embedded standard library:

- `modules` maps module paths to WXSL source;
- `node_modules` lists the paths that are also derived as function nodes.

There are no callbacks, and no file access except `wxsl_import_gltf`
reading the file it names. Custom effects and lighting models cannot be
registered through the ABI yet.

### Responses

`wxsl_result_json` is always an envelope:

```json
{"version": 1, "abi": 1, "status": "success", "data": { ... }}
```

On failure, `data.message` holds the diagnostic. An `incompatible` result
carries the complete `data.incompatibilities` list.

| Call | `data` holds | Other views |
|---|---|---|
| pipeline | see the list below | none |
| shader | for `root`, the module path; for `effect`, the effect's declaration: inputs, outputs, parameters and their layout | WGSL; for an effect, parameter bytes and field rows |
| material | see the list below | WGSL, parameter bytes, field rows |
| setup check | `incompatibilities` (empty on success) and the setup's `capabilities`: stages, effects, lighting models, G-buffer layout | none |
| glTF import | see below | meshes and images |

**A pipeline's `data`:**

- `target`;
- `graph`: passes and resources;
- `schedule`;
- the declarations and parameter layouts of the effects it uses.

How to read the schedule:

- Resource ids index `graph.resources`.
- `schedule.order` indexes `graph.passes`, so never reorder that array.
- `schedule.allocations` maps each resource to a slot, `imported` or
  `{"ring":{"base":…,"length":…}}`.
- `schedule.slots` lists the physical textures and buffers, with their
  usage flags.

No scheduler is needed in C++.

**A material's `data`:**

- `interface`: bindings, vertex slots and locations, instance stride, user
  and parameter layouts, defaults;
- the entry-point names;
- the layout signature;
- the variant key, as a decimal string so that all 64 bits survive JSON
  tools.

**`WxslField` rows** describe the computed buffers. Each row gives the
field's name and host storage type, its group and binding, its offset, size
and alignment, and its buffer's size and alignment. The `buffer` id is one
of:

- 0: material parameters;
- 1: user block;
- 2: instance attributes;
- 3: effect parameters.

A `bool` is stored as a `u32`; the interface JSON keeps the logical type.
Matrices keep WGSL's column padding: a `mat3x3f` is 48 bytes. Parameter
bytes are already padded to the layout.

Camera, light and vertex structs never cross this ABI; they are the fixed
layouts in `wxsl_host.h`.

**A glTF import's `data`**
([ADR 0080](adr/0080-gltf-import-produces-a-scene-document-with-generated-graphs.md)):

- `scene`: a scene document (v2). Its nodes keep the file's hierarchy,
  and each material is a generated `gltf_pbr` graph to compile with
  `wxsl_compile_material`, whose texture slots `bindings.textures` maps
  to the document's `textures`;
- `lighting_models`: the set to pass as `config.lighting_models`;
- `directory`: what `file` textures are relative to;
- `warnings`: everything the import could not map, by name.

`wxsl_result_meshes` returns `wxsl_result_mesh_count` `WxslMesh` rows,
indexed by the scene's `embedded` mesh sources. Each has `vertices` as
`WxslVertex` rows (48 bytes), `indices` as `uint32_t`, and the optional
`colors` (`float[4]`) and `uv1` (`float[2]`) streams, empty when absent.
`wxsl_result_images` returns the embedded images, still PNG or JPEG,
indexed by `embedded` texture sources; decoding them and building mips is
the host's job.

## The C++ classes

Both classes throw `std::runtime_error` on failure, and their headers
document each method.

### `wxsl::Renderer`

A headless Dawn renderer over a bundle directory exported by `wxsl-ffi`'s
`bake_docs` example. The bundle holds plans, WGSL, layouts and upload bytes.
Mesh generation, draw selection, scheduling, layouts and shader compilation
already happened in Rust.

```cpp
#include "renderer.hpp"

wxsl::Renderer renderer("target/dawn-assets/pbr");
std::vector<uint8_t> rgba = renderer.render("deferred");  // width() * height() * 4 bytes
```

- **Materials:**
  - `upload_material_params` changes parameter values without
    recompiling;
  - `compile_material` replaces a material through a `Compiler`, as long
    as its layout is unchanged; a failed edit keeps the previous material.
- **Environment:** `upload_environment_image` imports application-decoded
  linear RGB under an authored resource label; pass the scale it returns
  to `set_environment_scale`.
- **Spotlights:** `upload_light_cookies` uploads cookie layers.
- **Pass policies:** `mark_pass` and `pass_run_count` drive and observe
  `on demand` passes.

The renderer does not support file meshes, application uniform blocks,
extra vertex streams or windows yet.

### `wxsl::Compiler`

Loads the `wxsl_ffi` library dynamically
([ADR 0052](adr/0052-runtime-compilation-stays-behind-the-data-abi.md))
and wraps the four calls:

```cpp
#include "compiler.hpp"

wxsl::Compiler compiler("target/debug/libwxsl_ffi.dylib");
auto compiled = compiler.compile(wxsl::Operation::Material, {
    {"graph", graph_document},
    {"stage", "forward_lit"},
    {"material", {{"model", "wxsl.pbr"}}},
});
// compiled->wgsl, compiled->params, compiled->fields, compiled->metadata["data"]
```

Results are copied out of the foreign result before it is freed. A cache
holds up to 64 successful requests. An instance has a single owner, so use
one per worker thread.

Passing a `Compiler` to the `Renderer` constructor makes it check the scene
and setup, then recompile every shader instead of reading the bundle's
WGSL.

## Keeping it working

`bash dawn/tests/run_abi_smoke.sh` builds the library and compiles a C11
and a C++11 harness against the header, with no GPU or Dawn needed. The
harness checks:

- dynamic loading;
- both presets;
- byte-exact WGSL for every material stage, against Rust;
- the field tables;
- the version and capability refusals.

`cargo test -p wxsl-ffi` pins the generated header and runs the release
gate ([ADR 0077](adr/0077-gate-a-release-on-every-shipped-shader-through-the-c-abi.md)).

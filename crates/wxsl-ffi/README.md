# wxsl-ffi

Device-free C ABI (ADR 0049). Build the cdylib with `cargo build -p wxsl-ffi
--lib`; include the generated `include/wxsl.h`. It links core, frame, compiler
and the standard vocabulary, never wgpu or the editor. `wxsl-render` is only
a dev-dependency for parity tests/fixtures.

## Calls and ownership

Check `wxsl_abi_version() == WXSL_ABI_VERSION` before calling anything else.
Each operation also takes that version and rejects a mismatch **before
reading the request**. The C ABI version and shader/document `abi` revision
are separate contracts, even though both currently equal 1.

Inputs are borrowed UTF-8 JSON byte spans (not NUL-terminated). They must be
readable during the call. Each call returns its own immutable `WxslResult`,
even on failure. Check `wxsl_result_status`, then read `wxsl_result_json`
for metadata or diagnostics. WGSL, parameter bytes and field tables are
separate borrowed views. Copy them if needed beyond `wxsl_result_free`,
which releases the result once and invalidates **every** view. Do not free
the views yourself or access/free a result concurrently. Independent calls
may run concurrently; there is no global context or thread-local last error.

Null result accessors return empty views/InvalidArgument; free(null) is a
no-op. Null nonempty or oversized input spans are refused. Other dangling
or unreadable pointers, double-free and stale views are caller errors, not
recoverable diagnostics. Rust panics during processing become InternalError;
allocator aborts do not. The crate requires `panic=unwind` (Cargo's default)
and rejects `panic=abort` builds so this guarantee cannot silently disappear.

## Request shapes

Every request optionally carries `version` (request schema, currently 1)
and `abi` (shader ABI). Missing fields mean this build's versions. Newer
schemas and foreign shader ABI revisions are refused by name; nested graph
and scene documents keep their existing version checks and JSON spellings.

| Function | Required fields | Optional fields |
|---|---|---|
| `wxsl_compile_pipeline` | `pipeline`: pipeline graph document | `config` |
| `wxsl_compile_shader` | exactly one of `root`: module path or `effect`: shipped id | `library`, `macros`, `config` |
| `wxsl_compile_material` | `graph`: surface graph document, `stage`: stage name | `material`: existing MaterialConfig, `config`, `library` |
| `wxsl_check_setup` | `pipeline`, `scene`: existing documents | `config`, `library` |

`config` defaults to an 800×600 `rgba8unorm` target, the `wxsl.pbr` lighting
set and no features/macros. Optional fields: `target` (all fields of
TargetConfig: width, height, format, clear_color), `lighting_models` (shipped
names, e.g. `["pbr", "clearcoat"]`), `features` (e.g. `["subsurface"]`),
`macros` (the graph's existing `{"NAME":{"int":3}}` spelling).
Formats use lowercase neutral enum names (`rgba16float`); graphics-state
and plan enum variants use snake_case. G-buffer metadata is serialized
from the core's actual table. Width/height must be nonzero.

`library.modules` is a map of module paths to WXSL source, overlaid on the
embedded standard library. `library.node_modules` lists paths also derived
as function nodes with `wxsl_lang::node_from_source` (ADR 0020). It supports
new function nodes and replacement of existing ones without callbacks or
file access. The first cut does not accept custom effect registrations or
custom lighting-model descriptors. Raw WXSL can still use any supplied module.

The material request uses `config.features` as the pipeline's feature plan,
not an authored MaterialConfig field (which deliberately skips features on
the document wire). Material macros go in `material.macros`; raw/effect shader
macros go in `macros`; pipeline expansion macros go in `config.macros`.
Only generated lighting effects inherit shader macros, as on the Rust path.

## Outputs

Metadata has `{"version":1,"abi":1,"status":"success","data":...}`.
Failures carry `data.message`; a capability refusal carries the complete
`data.incompatibilities` list (Incompatible status), not only the first error.
The setup check also validates/compiles/schedules the pipeline and validates
scene references. It does not ask an adapter for hardware capabilities.

A pipeline's data contains `target`, the shared `graph`, its `schedule` and
the used effects' declarations/parameter layouts. Resource ids index
`graph.resources`; schedule order entries index `graph.passes` (do not reorder
the pass array). `schedule.allocations` indexes resources and preserves
`imported` or `{"ring":{"base":...,"length":...}}`; `schedule.slots` is
the physical allocation list, including inferred usage flags as symbolic
names. History reads retain their history count. No scheduler is needed in C++.

A material returns the actual `interface` (including resource bindings,
vertex slots/locations, instance stride, user/parameter layouts and defaults),
entry-point names, layout signature and variant key. The key is a decimal
string to retain all 64 bits through JSON tools. WGSL is a UTF-8 byte view;
material/effect initial parameter bytes are already padded by BufferLayout.

`WxslField` rows are computed from BufferLayout: name, host storage type,
group/binding, field offset/size/alignment and enclosing buffer size/alignment.
Buffer ids: 0 material params, 1 user block, 2 instance attributes, 3 effect
params. Boolean storage is `u32`; `interface` JSON retains the logical type.
Empty layouts have no field rows; their size is still available in metadata.
Matrices retain WGSL column padding (`mat3x3f`: 48 bytes). No camera/light
struct mirror crosses this ABI; fixed layouts are generated separately by
`wxsl-core::host` (ADR 0050), including `dawn/include/wxsl_host.h`.

## Verification and header updates

```sh
cargo test -p wxsl-ffi
cargo run -p wxsl-ffi --example export_header -- crates/wxsl-ffi/include/wxsl.h
bash dawn/tests/run_abi_smoke.sh
cargo tree -p wxsl-ffi -e normal
```

`build.rs` generates the header directly from exported Rust declarations;
unsupported signature types fail the build. The test compares the installed
copy with the generated one. Review ABI compatibility before changing a
signature or plan schema; changing field offsets computed from a material
is data, not a C struct-layout change. The C/C++ harness dynamically loads
the cdylib and compares WGSL byte-for-byte with native Rust variant fixtures.

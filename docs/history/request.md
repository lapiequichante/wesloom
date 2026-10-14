What I really want to achieve is to take the shader node editor to a much lower-level and more complete abstraction.

I don't want the node editor to only generate a single fragment shader. I want it to represent a programmable rendering pipeline, while still keeping the API data-oriented and easy to extend.

The most important architectural requirement is to separate the concepts of:

* Shader Graph: describes how values are computed.
* Render Graph: describes when passes execute and how GPU resources flow between them.
* Pipeline / Pass: describes the actual rendering or compute operation.
* Resources: textures, buffers, samplers, render targets, etc.

The shader graph should therefore be usable inside different kinds of render passes.

## Shader stages

I want explicit control over vertex and fragment stages.

There should be concepts equivalent to the important GLSL pipeline outputs, but expressed in a WGSL-friendly way:

* Vertex position/output
* Fragment color/output
* Fragment depth
* Multiple render targets
* Discard / alpha clipping where appropriate

The user should not have to manually manage WGSL varyings.

A node should have an automatic stage (`Auto`) by default, but it should also be possible to explicitly constrain it to:

* Vertex
* Fragment
* Compute

The compiler should perform stage analysis.

For example:

Texture → WorldPosition → Noise → Color

If WorldPosition is generated in the vertex stage and Noise/Color are needed in the fragment stage, the compiler should automatically generate the required vertex output/varying and fragment input.

Moving a node from vertex to fragment should therefore be a graph operation, not a manual WGSL operation.

Dependencies must be analyzed automatically. Shared nodes should be evaluated in the earliest valid stage when possible, and cross-stage values should automatically become varyings.

## Material output vs G-buffer

I don't want the material graph itself to be coupled to deferred rendering.

Instead of treating "GBuffer" as the fundamental material output, I think the fundamental abstraction should be something closer to:

MaterialOutput / SurfaceOutput

It should expose semantic channels such as:

* Position
* Normal
* Albedo
* Emission
* Depth-related information where appropriate
* Custom material channels

The deferred renderer can compile this material output into a G-buffer.

The forward renderer can compile the same material output directly into the forward lighting path without creating a G-buffer.

Conceptually:

Forward:

Material → Lighting → Color → Post Process → Final Output

Deferred:

Material → GBuffer → Lighting → Color → Post Process → Final Output

The material graph should not need `if forward` / `if deferred` logic. The renderer/compiler should decide how MaterialOutput is represented.

## G-buffer channels

The G-buffer must not be a fixed structure containing arbitrary hardcoded fields beyond the core semantics.

I want:

Position
Normal
Albedo

plus extensible typed/custom channels.

For example:

slot1 could contain Metallic/Roughness/AO.

slot2 could contain Emission.

Another material could use completely different custom channels.

Therefore G-buffer channels should have metadata/semantics and types rather than simply being anonymous `slot1`, `slot2`, etc.

The renderer should be able to determine which channels are actually required by a material/lighting pipeline.

For example:

Material A:

Normal
Albedo
Metallic/Roughness/AO

Material B:

Normal
Albedo
Emission
Subsurface

The render graph should be able to derive the required resources/attachments from the material and lighting graphs.

## Lighting

Lighting should be a separate stage from MaterialOutput.

I want to be able to build lighting graphs that consume material/G-buffer data.

For example:

GBuffer
↓
Direct Lighting
↓
IBL
↓
Emission
↓
Color Output

Lighting should therefore be represented as a graph/pass rather than being hidden inside the material shader.

## Post processing

After the final color output, I want to be able to chain arbitrary post-process operations.

For example:

Color
→ Bloom
→ Blur
→ Color Correction
→ Tone Mapping
→ Vignette
→ Final Output

These operations should be composable and data-driven.

They should not be hardcoded into the renderer.

## Render passes

The API should support arbitrary pre-passes and post-passes.

Examples:

* Shadow maps
* Depth pre-pass
* BRDF LUT generation
* Environment preprocessing
* Irradiance maps
* Prefiltered environment maps
* Compute preprocessing
* Blur passes
* Downsampling
* Upsampling
* Bloom
* Temporal effects
* Custom user-defined passes

I want a generic `RenderPass` / `ComputePass` abstraction that can consume and produce resources.

A pass should describe:

* Inputs
* Outputs
* Shader/graph
* Execution dependencies
* Resource usage
* Execution policy

For example, a BRDF LUT could be represented as a compute pass that runs once and produces a persistent texture.

The system should support execution policies such as:

* Once
* Per frame
* On resize
* On demand

## Render graph

The render graph should automatically understand dependencies between passes and resources.

For example:

ShadowPass
→ MaterialPass
→ GBuffer
→ Lighting
→ Bloom
→ ToneMapping
→ Present

But also:

BRDFPrecompute → persistent texture
↓
ShadowPass → MaterialPass → Lighting → PostProcess

The user should be able to build arbitrary graphs rather than being limited to a fixed renderer pipeline.

The render graph should distinguish between transient and persistent resources, and ideally support history resources for temporal effects.

## Compute

Compute passes should be first-class citizens.

A compute pass should be able to:

* Read textures/buffers
* Write textures/buffers
* Use a shader graph or shader representation
* Define workgroup size
* Define dispatch dimensions
* Participate in render-graph dependencies

This is required for things such as BRDF precomputation, texture preprocessing, simulations, blur, etc.

## Resources

Resources should be explicit and data-oriented:

* Texture
* Buffer
* Sampler
* Uniform data
* Storage buffer
* Storage texture
* Render attachment
* Depth attachment

Resource usage should be represented explicitly so that the render graph/compiler can reason about dependencies and GPU usage.

## Validation

The graph system should validate invalid configurations before generating WGSL whenever possible.

Examples:

* Vertex-only node used from an incompatible stage
* Invalid type connection
* Missing required output
* Circular dependency
* Resource used in an incompatible way
* Multiple writers
* Unsupported feature
* Invalid render-pass configuration

Errors should reference the graph node/port that caused the problem so that the editor can display them directly.

## Compilation architecture

Please avoid coupling graph nodes directly to WGSL strings as much as possible.

I want the architecture to conceptually look like:

Editor Graph
→ Intermediate Representation
→ Dependency Analysis
→ Stage Analysis
→ Resource Analysis
→ Pipeline / Render Graph Compilation
→ WGSL generation
→ wgpu pipeline/resources

The graph representation should remain backend-independent as much as reasonably possible.

The compiler should be responsible for deciding:

* Which nodes run in which shader stage
* Which values need varyings
* Which resources are required
* Which render targets are required
* Which passes need to exist
* Which resources are transient/persistent
* How material outputs map to forward/deferred rendering
* How shader graphs become WGSL

## Overall goal

The API should feel like a complete, low-level, data-oriented rendering framework rather than a high-level "material editor".

The node editor is the user-facing representation, but underneath it should be possible to describe an entire rendering pipeline:

Geometry
→ Vertex processing
→ Rasterization
→ Material evaluation
→ G-buffer or Forward output
→ Lighting
→ Compute/pre-pass/post-pass operations
→ Post processing
→ Final output

The most important thing is that these concepts should be composable and extensible rather than hardcoded around one specific renderer architecture.

Before implementing this, please review the existing architecture and identify where the current design prevents this model from working cleanly. I would rather refactor the core data model now than add special cases for forward/deferred rendering, G-buffers, post-processing, or compute passes later.

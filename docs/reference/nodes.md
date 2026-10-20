# Node reference

Every node a graph can place, generated from the registries. A node is added with `add_node` by its id, and its sockets are named as listed here. A socket with a default can be left unconnected. `T`, `U`… are generic: they take the type of what is connected, or the one `set_generic` picks.

- [Shader nodes](#shader-nodes) (193)
- [Pipeline nodes](#pipeline-nodes) (29)

## Shader nodes

Categories: [animation](#shader-animation), [color](#shader-color), [compare](#shader-compare), [const](#shader-const), [convert](#shader-convert), [distort](#shader-distort), [filter](#shader-filter), [generative](#shader-generative), [input](#shader-input), [lighting](#shader-lighting), [logic](#shader-logic), [math](#shader-math), [output](#shader-output), [param](#shader-param), [sample](#shader-sample), [sdf](#shader-sdf), [space](#shader-space), [texture](#shader-texture), [vector](#shader-vector).

<a id="shader-animation"></a>

### animation

#### `animation.ease_in_out_cubic` · Ease in-out (cubic)

Cubic ease over 0..1, clamped outside it.

- in `t: f32 = 0.0f`
- out `out: f32`

#### `animation.pulse` · Pulse

A 0..1 sine oscillation. Wire the `input.time` node into it.

- in `time: f32 = 0.0f`
- in `frequency: f32 = 1.0f`: Cycles per second.
- in `phase: f32 = 0.0f`: Offset in turns.
- out `out: f32`

#### `animation.skin` · Skin

Linear blend skinning from a bone texture: the vertex moved by up to four joints, weighted. Wire its outputs into the vertex output.

- in `bones: texture_2d<f32>`
- in `joints: vec4f`: The four joint indices, as floats.
- in `weights: vec4f = vec4f(1.0, 0.0, 0.0, 0.0)`: Their weights, summing to 1.
- in `frame: f32 = 0.0f`: The bone texture's row.
- in `position: vec3f`: The vertex's object-space position.
- in `normal: vec3f = vec3f(0.0, 1.0, 0.0)`: Its object-space normal.
- in `tangent: vec3f = vec3f(1.0, 0.0, 0.0)`: Its object-space tangent.
- out `offset: vec3f`
- out `normal: vec3f`
- out `tangent: vec3f`

<a id="shader-color"></a>

### color

#### `color.bloom_threshold` · Bloom threshold

Keeps the part of a radiance value above `threshold`, with a soft knee `knee` wide below it where the contribution fades in instead of stepping. Grey above the threshold passes unchanged; anything under it contributes nothing.

- in `radiance: vec3f = vec3f(1.0, 1.0, 1.0)`: Linear radiance in.
- in `threshold: f32 = 1.0f`: Where highlights begin.
- in `knee: f32 = 0.5f`: Width of the soft knee below the threshold.
- out `out: vec3f`

#### `color.hsv_to_rgb` · HSV to RGB

Hue (in turns), saturation and value to RGB.

- in `hsv: vec3f = vec3f(0.0, 1.0, 1.0)`
- out `out: vec3f`

#### `color.linear_to_oklab` · Linear sRGB to OKLab

Linear sRGB in, OKLab out: L in 0..1 for the display range, a and b unbounded and centred on grey.

- in `rgb: vec3f = vec3f(0.0, 0.0, 0.0)`: Linear sRGB.
- out `out: vec3f`

#### `color.linear_to_srgb` · Linear to sRGB

Encode linear light as sRGB, for writing to a non-`Srgb` format.

- in `color: vec3f = vec3f(1.0, 1.0, 1.0)`
- out `out: vec3f`

#### `color.luminance` · Luminance

Relative luminance of a linear colour (Rec. 709 weights).

- in `color: vec3f = vec3f(1.0, 1.0, 1.0)`
- out `out: f32`

#### `color.oklab_to_linear` · OKLab to linear sRGB

The inverse of `linear_to_oklab`: OKLab in, linear sRGB out. The result is whatever the input demands — a wide-gamut or out-of-range Lab value comes back outside sRGB, which is correct and the caller's business.

- in `lab: vec3f = vec3f(0.5, 0.0, 0.0)`: OKLab: L, a, b.
- out `out: vec3f`

#### `color.oklab_to_oklch` · OKLab to OKLCh

Cartesian to cylindrical: L through unchanged, the a/b pair as chroma and hue. The hue is in **turns** (0..1, a full circle), matching `rotate_uv` and the animation nodes' phase convention; `oklch_to_oklab` turns it back.

- in `lab: vec3f = vec3f(0.5, 0.0, 0.0)`: OKLab: L, a, b.
- out `out: vec3f`

#### `color.oklch_to_oklab` · OKLCh to OKLab

Cylindrical back to cartesian: chroma becomes the a/b radius at the given hue. The hue is in turns, as `oklab_to_oklch` writes it.

- in `lch: vec3f = vec3f(0.5, 0.0, 0.0)`: OKLCh: L, chroma, hue in turns.
- out `out: vec3f`

#### `color.rgb_to_hsv` · RGB to HSV

RGB to hue (in turns), saturation and value.

- in `rgb: vec3f = vec3f(1.0, 0.0, 0.0)`
- out `out: vec3f`

#### `color.srgb_to_linear` · sRGB to linear

Decode an sRGB colour (what a colour picker gives you) to the linear light every lighting computation expects.

- in `color: vec3f = vec3f(1.0, 1.0, 1.0)`
- out `out: vec3f`

#### `color.tonemap_aces` · ACES filmic tonemap

Maps unbounded positive radiance into 0..1 through the ACES fit: a pre-tonemap colour space transform into AP1, the RRT+ODT rational fit, and the transform back. Brighter and more saturated in the midtones than the filmic curve; desaturates highlights as they roll off.

- in `color: vec3f = vec3f(1.0, 1.0, 1.0)`: Linear radiance, unbounded positive.
- in `exposure: f32 = 1.0f`: Multiplies the radiance before the curve; 2.0 is one stop brighter.
- out `out: vec3f`

#### `color.tonemap_filmic` · Tonemap (filmic)

Filmic curve with a gentle toe, a long shoulder and highlight desaturation. Also what the shipped `tonemap` effect applies at the end of a pipeline's chain.

- in `color: vec3f = vec3f(1.0, 1.0, 1.0)`
- in `exposure: f32 = 1.0f`: Multiplies the radiance before the curve; 2.0 is one stop brighter.
- out `out: vec3f`

#### `color.tonemap_reinhard` · Tonemap (Reinhard)

Compress high dynamic range on luminance, preserving hue.

- in `color: vec3f = vec3f(1.0, 1.0, 1.0)`
- in `exposure: f32 = 1.0f`: Multiplies the radiance before the curve; 2.0 is one stop brighter.
- out `out: vec3f`

#### `color.tonemap_reinhard_jodie` · Reinhard-Jodie tonemap

Maps unbounded positive radiance into 0..1, blending the plain global Reinhard scale with a per-channel one so saturated highlights keep their hue instead of collapsing to white.

- in `color: vec3f = vec3f(1.0, 1.0, 1.0)`: Linear radiance, unbounded positive.
- in `exposure: f32 = 1.0f`: Multiplies the radiance before the curve; 2.0 is one stop brighter.
- out `out: vec3f`

<a id="shader-compare"></a>

### compare

#### `compare.equal` · Equal

Compare two scalars. Exact float comparison is rarely what you want for equality; compare a difference against a tolerance instead.

- in `a: f32 = 0.0f`
- in `b: f32 = 0.0f`
- out `out: bool`

#### `compare.greater` · Greater than

Compare two scalars. Exact float comparison is rarely what you want for equality; compare a difference against a tolerance instead.

- in `a: f32 = 0.0f`
- in `b: f32 = 0.0f`
- out `out: bool`

#### `compare.greater_equal` · Greater or equal

Compare two scalars. Exact float comparison is rarely what you want for equality; compare a difference against a tolerance instead.

- in `a: f32 = 0.0f`
- in `b: f32 = 0.0f`
- out `out: bool`

#### `compare.less` · Less than

Compare two scalars. Exact float comparison is rarely what you want for equality; compare a difference against a tolerance instead.

- in `a: f32 = 0.0f`
- in `b: f32 = 0.0f`
- out `out: bool`

#### `compare.less_equal` · Less or equal

Compare two scalars. Exact float comparison is rarely what you want for equality; compare a difference against a tolerance instead.

- in `a: f32 = 0.0f`
- in `b: f32 = 0.0f`
- out `out: bool`

#### `compare.not_equal` · Not equal

Compare two scalars. Exact float comparison is rarely what you want for equality; compare a difference against a tolerance instead.

- in `a: f32 = 0.0f`
- in `b: f32 = 0.0f`
- out `out: bool`

<a id="shader-const"></a>

### const

#### `const.bool` · Constant (boolean)

A literal boolean.

- in `value: bool = false`
- out `out: bool`

#### `const.value` · Constant

A literal value, shared by everything wired to it. A matrix constant defaults to the identity, which is the diagonal a splat default builds.

- in `value: T = 1.0 in every component`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f, mat3x3f, mat4x4f

<a id="shader-convert"></a>

### convert

#### `convert.combine.vec2f` · Combine

Build a vector from its components.

- in `x: f32 = 0.0f`
- in `y: f32 = 0.0f`
- out `out: vec2f`

#### `convert.combine.vec3f` · Combine

Build a vector from its components.

- in `x: f32 = 0.0f`
- in `y: f32 = 0.0f`
- in `z: f32 = 0.0f`
- out `out: vec3f`

#### `convert.combine.vec4f` · Combine

Build a vector from its components.

- in `x: f32 = 0.0f`
- in `y: f32 = 0.0f`
- in `z: f32 = 0.0f`
- in `w: f32 = 1.0f`
- out `out: vec4f`

#### `convert.splat` · Splat

Copy one scalar into every component.

- in `value: f32 = 0.0f`
- out `out: T`
- `T`: one of vec2f, vec3f, vec4f

#### `convert.split.vec2f` · Split

Take a vector apart into its components.

- in `v: vec2f = vec2f(0.0, 0.0)`
- out `x: f32`
- out `y: f32`

#### `convert.split.vec3f` · Split

Take a vector apart into its components.

- in `v: vec3f = vec3f(0.0, 0.0, 0.0)`
- out `x: f32`
- out `y: f32`
- out `z: f32`

#### `convert.split.vec4f` · Split

Take a vector apart into its components.

- in `v: vec4f = vec4f(0.0, 0.0, 0.0, 0.0)`
- out `x: f32`
- out `y: f32`
- out `z: f32`
- out `w: f32`

#### `convert.to_float` · To float

Convert a boolean or an integer to `f32`. `false` is 0 and `true` is 1.

- in `value: T`: The value to convert.
- out `out: f32`
- `T`: one of bool, i32, u32

<a id="shader-distort"></a>

### distort

#### `distort.swirl_uv` · Swirl UV

Rotate a coordinate around a centre by an amount that falls off with distance, so the middle twists and the edges stay put.

- in `uv: vec2f = vec2f(0.0, 0.0)`
- in `center: vec2f = vec2f(0.5, 0.5)`
- in `radius: f32 = 0.5f`: Where the swirl fades to nothing.
- in `turns: f32 = 0.25f`: How far the centre is rotated.
- out `out: vec2f`

<a id="shader-filter"></a>

### filter

#### `filter.blur_box` · Box blur

The unweighted mean over a square of (2·radius+1)² texels: the cheapest neighbourhood average, and the baseline the fancier kernels are judged against.

- in `image: texture_2d<f32>`
- in `uv: vec2f = vec2f(0.0, 0.0)`: Where to filter, in 0..1.
- in `texel: vec2f = vec2f(0.0, 0.0)`: One texel of `image` in uv units.
- out `out: vec3f`
- macro `WXSL_BOX_RADIUS` = `3`: Texels each side of the centre. A loop bound.

#### `filter.blur_gaussian` · Gaussian blur, one axis

The Gaussian-weighted mean along one direction — `(1, 0)` for horizontal, `(0, 1)` for vertical, one tap per texel of it. Separability is the whole point: an H pass then a V pass of this node costs radius taps per pixel instead of the square a 2D kernel would, which is how bloom pyramids and DOF afford their widths.

- in `image: texture_2d<f32>`
- in `uv: vec2f = vec2f(0.0, 0.0)`: Where to filter, in 0..1.
- in `texel: vec2f = vec2f(0.0, 0.0)`: One texel of `image` in uv units.
- in `axis: vec2f = vec2f(1.0, 0.0)`: Per-tap step, in texels.
- in `sigma: f32 = 2.0f`: Standard deviation, in texels.
- out `out: vec3f`
- macro `WXSL_GAUSSIAN_RADIUS` = `5`: Taps each side of the centre. A loop bound.

#### `filter.blur_kawase` · Kawase blur

Four diagonal taps at a half-texel-shifted offset. One pass of it is already a soft blur, and *repeating* it with growing offsets — the Kawase dual-filter scheme — approximates a wide Gaussian at a fraction of the taps: this is what mobile chains reach for when a separable Gaussian is still too wide.

- in `image: texture_2d<f32>`
- in `uv: vec2f = vec2f(0.0, 0.0)`: Where to filter, in 0..1.
- in `texel: vec2f = vec2f(0.0, 0.0)`: One texel of `image` in uv units.
- in `offset: f32 = 0.0f`: Tap offset in texels; grows down a Kawase chain.
- out `out: vec3f`

#### `filter.chromatic_aberration` · Chromatic aberration

Samples the red and blue channels at points pulled toward and away from the frame's centre while green stays put: the lens-dispersion fringe, cheapest of the post effects and therefore the most overused.

- in `image: texture_2d<f32>`
- in `uv: vec2f = vec2f(0.5, 0.5)`: Where to sample, 0..1.
- in `texel: vec2f = vec2f(0.0, 0.0)`: One texel of `image` in uv units.
- in `strength: f32 = 0.01f`: Pull at the frame edge, in uv units.
- out `out: vec3f`

#### `filter.circle_of_confusion` · Circle of confusion

The thin-lens blur disc a point at `depth_m` makes on the sensor, in pixels: the aperture, the focal length and the distance to the focus plane decide how far out of focus a depth is, and the sensor width and viewport width convert the physical disc into screen texels. SI units throughout — metres in, pixels out.

- in `depth_m: f32 = 10.0f`: Linear camera depth of the surface, metres.
- in `focus_m: f32 = 10.0f`: Distance to the focus plane, metres.
- in `focal_m: f32 = 0.05f`: Focal length, metres.
- in `f_number: f32 = 2.8f`: Aperture as focal ratio.
- in `sensor_width_m: f32 = 0.035f`: Sensor width, metres.
- in `viewport_width_px: f32 = 1920.0f`: Viewport width, pixels.
- out `out: f32`

#### `filter.disk_sample` · Disk sample

The `index`-th point of a deterministic `count`-point disc: the golden angle turns the index sequence into a sunflower spiral, and the square root of the index's centre-radius makes the points *area*-uniform — no clump at the centre, no ring at the rim. Same indices, same points, every backend and every run: a gather that must match a fixture or a second implementation samples this and not a clock-seeded random.

- in `index: u32 = 0u`: Which point, 0..count.
- in `count: u32 = 16u`: How many the kernel gathers.
- out `out: vec2f`

#### `filter.downsample2` · Downsample 2x

The mean of the 2×2 texel block a half-resolution pixel covers: one plain box downsample, the step every mipmap chain and blur pyramid repeats. At odd sizes the last row/column is clamped onto the edge texel, which weights it — the honest cheap answer, and why a chain that needs exact halves rounds its targets up.

- in `image: texture_2d<f32>`
- in `uv: vec2f = vec2f(0.0, 0.0)`: Where to sample, in 0..1 of the source.
- in `texel: vec2f = vec2f(0.0, 0.0)`: One texel of `image` in uv units.
- out `out: vec3f`

#### `filter.film_grain` · Film grain

Adds a deterministic per-pixel noise around zero to a radiance value: the texture a flat gradient needs to stop banding, and the vintage look when pushed harder.

- in `color: vec3f = vec3f(1.0, 1.0, 1.0)`: Linear radiance in.
- in `pixel: vec2f = vec2f(0.0, 0.0)`: This pixel's coordinates.
- in `time: f32 = 0.0f`: Seconds since the renderer started.
- in `amount: f32 = 0.05f`: Grain strength as a fraction of the value.
- out `out: vec3f`

#### `filter.fxaa` · FXAA

Fast approximate anti-aliasing: find the pixels that sit on a luminance edge and blur each one *along* that edge, so a staircase softens into a line while everything else is left exactly as it was. One pass over an image already drawn, with no depth, no history and no extra samples per primitive — which is why it is the cheapest anti-aliasing there is and why it is the first thing a screen effect gets asked for.

- in `image: texture_2d<f32>`
- in `uv: vec2f = vec2f(0.0, 0.0)`: Where to filter, in 0..1.
- in `texel: vec2f = vec2f(0.0, 0.0)`: One texel of `image` in uv units.
- in `strength: f32 = 1.0f`: How much of the blur to keep, 0..1.
- out `out: vec3f`

#### `filter.linearize_depth` · Linearize depth

The depth buffer's value back into metres, for a standard perspective projection: NDC depth in [0, 1], near plane mapping to 0, far plane to 1, not reversed-Z, finite far. Hyperbolic depth distributes precision near the camera, so the curve back is steep exactly where the near field's blur is finest — a depth-of-field chain linearizes first and measures its circles of confusion in metres after.

- in `depth: f32 = 1.0f`: Depth buffer value, 0 at the near plane, 1 at the far.
- in `near_m: f32 = 0.1f`: Near plane distance, metres.
- in `far_m: f32 = 1000.0f`: Far plane distance, metres.
- out `out: f32`

#### `filter.vignette` · Vignette

Darkens a radiance value toward the frame's edge: 1 at the centre (unchanged), falling to 0 outside `radius`, over `softness` of falloff.

- in `color: vec3f = vec3f(1.0, 1.0, 1.0)`: Linear radiance in.
- in `uv: vec2f = vec2f(0.5, 0.5)`: Where on the frame, 0..1.
- in `texel: vec2f = vec2f(0.0, 0.0)`: One texel of the image, for the aspect.
- in `radius: f32 = 0.7f`: Where the falloff starts, in half-heights.
- in `softness: f32 = 0.3f`: How far the falloff spreads.
- out `out: vec3f`

<a id="shader-generative"></a>

### generative

#### `generative.brick_mask` · Brick mask

1 inside offset bricks, 0 in the mortar between them: every other row shifted half a brick, mortar width a fraction of a brick.

- in `uv: vec2f = vec2f(0.0, 0.0)`: Texture coordinates, 0..1 per repeat.
- in `count: vec2f = vec2f(4.0, 8.0)`: Bricks across the uv.
- in `mortar: f32 = 0.05f`: Mortar width as a fraction of a brick, 0..0.5.
- out `out: f32`

#### `generative.checker` · Checker pattern

0 or 1 alternating per cell of a `cells`-by-`cells` grid over the uv.

- in `uv: vec2f = vec2f(0.0, 0.0)`: Texture coordinates, 0..1 per repeat.
- in `cells: vec2f = vec2f(8.0, 8.0)`: Cells across the uv.
- out `out: f32`

#### `generative.checker_aa` · Anti-aliased checker

The checker pattern integrated over each pixel's footprint instead of sampled: where a cell is small on screen the value slides toward the 0.5 average rather than flickering between 0 and 1, and where a cell is large the answer is the hard pattern. The closed form evaluates the triangle wave's average across the pixel box analytically — no sample loop, no `smoothstep` hack at a hand-tuned width.

- in `uv: vec2f = vec2f(0.0, 0.0)`: Texture coordinates, 0..1 per repeat.
- in `cells: vec2f = vec2f(8.0, 8.0)`: Cells across the uv.
- out `out: f32`
- fragment stage only

#### `generative.fbm3` · Fractal noise (3D)

Octaves of value noise summed at rising frequency and falling amplitude, normalized to 0..1. The octave count is the macro variable `WXSL_FBM_OCTAVES`, and `wxsl_fbm_ridged` switches the octaves to ridges — both change which code is compiled, not what a socket carries.

- in `p: vec3f = vec3f(0.0, 0.0, 0.0)`
- in `lacunarity: f32 = 2.0f`: Frequency multiplier per octave.
- in `gain: f32 = 0.5f`: Amplitude multiplier per octave.
- out `out: f32`
- macro `WXSL_FBM_OCTAVES` = `5`: How many octaves of noise to sum. A loop bound, so it is a compile-time constant.
- macro `wxsl_fbm_ridged` = `false`: Fold each octave into a ridge, for a creased look.

#### `generative.grid_mask` · Grid mask

1 on the lines of a grid, 0 in the cells — a grid whose line width is a fraction of a cell, so it stays the same width in uv terms however many cells there are.

- in `uv: vec2f = vec2f(0.0, 0.0)`: Texture coordinates, 0..1 per repeat.
- in `cells: vec2f = vec2f(8.0, 8.0)`: Cells across the uv.
- in `line_width: f32 = 0.05f`: Line width as a fraction of a cell, 0..0.5.
- out `out: f32`

#### `generative.grid_mask_aa` · Anti-aliased grid mask

`grid_mask` with the line edges settled by the pixel's footprint instead of a `step`: a one-pixel ramp across each edge, so thin lines and distant cells fade out rather than crawling as the camera moves. The width is still a fraction of a cell; the ramp is whatever one pixel of that cell is.

- in `uv: vec2f = vec2f(0.0, 0.0)`: Texture coordinates, 0..1 per repeat.
- in `cells: vec2f = vec2f(8.0, 8.0)`: Cells across the uv.
- in `line_width: f32 = 0.05f`: Line width as a fraction of a cell, 0..0.5.
- out `out: f32`
- fragment stage only

#### `generative.hash13` · Hash (3 to 1)

A deterministic pseudo-random value in 0..1 for a point.

- in `p: vec3f = vec3f(0.0, 0.0, 0.0)`
- out `out: f32`

#### `generative.simplex2` · Simplex noise (2D)

The 2D half of `simplex3`: gradient noise over the skewed triangular lattice, roughly -1..1, zero at lattice points. Cheaper than the 3D version and the right choice for a uv-space field — decals, ripples, masks.

- in `p: vec2f = vec2f(0.0, 0.0)`: Sample point.
- out `out: f32`

#### `generative.simplex3` · Simplex noise (3D)

Gradient noise over the simplex lattice — a skewing of the grid so each sample needs three corners instead of value noise's eight, which is why it has no axis-aligned bias and costs a fraction of what a same- quality grid noise would. Returns roughly -1..1, zero at lattice points.

- in `p: vec3f = vec3f(0.0, 0.0, 0.0)`: Sample point.
- out `out: f32`

#### `generative.truchet_arcs` · Truchet arcs

The classic two-arcs Truchet tile: each cell draws one of two quarter- circle pairs — which one decided per cell, so the tiles join into meandering curves with no visible tiling. 1 on the arcs, 0 between.

- in `uv: vec2f = vec2f(0.0, 0.0)`: Texture coordinates, one tile per unit.
- in `cells: vec2f = vec2f(4.0, 4.0)`: Tiles across the uv.
- in `width: f32 = 0.1f`: Arc stroke width, in cell units.
- out `out: f32`

#### `generative.value_noise3` · Value noise (3D)

Smooth 0..1 noise over a lattice, quintically interpolated.

- in `p: vec3f = vec3f(0.0, 0.0, 0.0)`
- out `out: f32`

#### `generative.worley3` · Worley noise (3D)

Cellular distance noise: the distances to the first and second nearest feature points, one jittered point per grid cell. F1 alone gives cell walls and pebbles; F2 − F1 gives the webbing between them.

- in `p: vec3f = vec3f(0.0, 0.0, 0.0)`: Sample point.
- out `out: vec2f`

<a id="shader-input"></a>

### input

#### `input.attribute` · Geometry attribute

One declared attribute: a per-vertex stream the mesh carries, a per-instance value the draw supplies, or an interpolant the graph's own vertex stage computed. The graph declares the name, the type and which of the three it is; none of that is on this node, so moving an attribute between them rewires nothing.

- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f, mat3x3f, mat4x4f, bool, i32, u32
- setting `name` = `color`: Which declared attribute to read.
- only in surface graphs

#### `input.image` · Input image

The image this effect reads: whatever the pass before it wrote. Wire it into a sampling or filtering node.

- out `out: texture_2d<f32>`: The pass's input image, at binding 0 of the pass group.
- only in screen graphs

#### `input.normal_matrix` · Normal matrix

Object to world space for directions: the inverse transpose of the model matrix's linear part. It turns a normal the vertex output replaces into world space.

- out `out: mat3x3f`: Object to world space for directions: the inverse transpose of the model matrix's linear part. It turns a normal the vertex output replaces into world space.
- only in surface graphs

#### `input.object_normal` · Object normal

The vertex's normal in object space, unit length.

- out `out: vec3f`: The vertex's normal in object space, unit length.
- only in surface graphs

#### `input.object_position` · Object position

The vertex's position in object space, before the model transform.

- out `out: vec3f`: The vertex's position in object space, before the model transform.
- only in surface graphs

#### `input.object_tangent` · Object tangent

The vertex's tangent in object space, unit length.

- out `out: vec3f`: The vertex's tangent in object space, unit length.
- only in surface graphs

#### `input.pixel` · Pixel

This fragment's pixel coordinates in the image being written.

- out `out: vec2f`: This fragment's pixel coordinates in the image being written.
- only in screen graphs

#### `input.texel` · Texel size

One texel of the input image, in UV units: 1 / its size.

- out `out: vec2f`: One texel of the input image, in UV units: 1 / its size.
- only in screen graphs

#### `input.time` · Time

Seconds since the renderer started, for animated materials.

- out `out: f32`: Seconds since the renderer started, for animated materials.
- only in surface and screen graphs

#### `input.user` · Application value

One field of the uniform block this graph expects the application to supply. The graph declares the block's fields; wxsl never knows what is in it, only its shape.

- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f, mat3x3f, mat4x4f, bool, i32, u32
- setting `field` = `value`: Which field of the application block to read.
- only in surface graphs

#### `input.uv` · UV

Interpolated texture coordinates.

- out `out: vec2f`: Interpolated texture coordinates.
- only in surface and screen graphs

#### `input.view_direction` · View direction

Unit vector from the fragment towards the camera.

- out `out: vec3f`: Unit vector from the fragment towards the camera.
- only in surface graphs

#### `input.world_bitangent` · World bitangent

Interpolated bitangent in world space, normalized.

- out `out: vec3f`: Interpolated bitangent in world space, normalized.
- only in surface graphs

#### `input.world_normal` · World normal

Interpolated geometric normal in world space, normalized.

- out `out: vec3f`: Interpolated geometric normal in world space, normalized.
- only in surface graphs

#### `input.world_position` · World position

Fragment position in world space.

- out `out: vec3f`: Fragment position in world space.
- only in surface graphs

#### `input.world_tangent` · World tangent

Interpolated tangent in world space, normalized.

- out `out: vec3f`: Interpolated tangent in world space, normalized.
- only in surface graphs

<a id="shader-lighting"></a>

### lighting

#### `lighting.ambient_environment` · Ambient environment

Analytic hemisphere ambient: sky above, bounce below, with a split-sum specular response.

- in `normal: vec3f = vec3f(0.0, 1.0, 0.0)`
- in `view_direction: vec3f = vec3f(0.0, 0.0, 1.0)`
- in `base_color: vec3f = vec3f(0.8, 0.8, 0.8)`
- in `metallic: f32 = 0.0f`
- in `roughness: f32 = 0.5f`
- in `sky_color: vec3f = vec3f(0.35, 0.45, 0.6)`
- in `ground_color: vec3f = vec3f(0.12, 0.1, 0.08)`
- in `env_brdf: vec2f = vec2f(1.0, 0.0)`
- out `out: vec3f`

#### `lighting.anisotropic_widths` · Anisotropic widths

Converts the authoring pair — a perceptual roughness and an anisotropy in 0..1 — into the two GGX alpha widths `distribution_ggx_anisotropic` and `visibility_ggx_anisotropic` want: `x` along the tangent, `y` along the bitangent.

- in `roughness: f32 = 0.5f`: Perceptual roughness, 0..1.
- in `anisotropy: f32 = 0.0f`: How stretched, 0..1.
- out `out: vec2f`

#### `lighting.charlie_distribution` · Charlie distribution

The sheen microfacet distribution: a slowly-falling lobe that spreads over the whole hemisphere instead of tightening to a mirror peak, which is what fabric needs. Companion to `distribution_ggx` for cloth.

- in `n_dot_h: f32 = 1.0f`: Cosine between normal and half vector.
- in `roughness: f32 = 0.5f`: Perceptual roughness, 0..1.
- out `out: f32`

#### `lighting.diffuse_lambert` · Lambert diffuse

Energy-normalized diffuse albedo, faded out for metals.

- in `base_color: vec3f = vec3f(0.8, 0.8, 0.8)`
- in `metallic: f32 = 0.0f`
- out `out: vec3f`

#### `lighting.distribution_ggx` · GGX distribution

Microfacet normal distribution: how much of the surface faces the half vector.

- in `n_dot_h: f32 = 1.0f`
- in `roughness: f32 = 0.5f`
- out `out: f32`

#### `lighting.distribution_ggx_anisotropic` · GGX distribution, anisotropic

The GGX microfacet distribution for a half vector expressed in the tangent frame — brushed metal, hair, worn surfaces that drag highlights along one axis.

- in `half_tangent: vec3f = vec3f(0.0, 0.0, 1.0)`: Half vector in the tangent frame.
- in `alpha_x: f32 = 0.25f`: Width along the tangent.
- in `alpha_y: f32 = 0.25f`: Width along the bitangent.
- out `out: f32`

#### `lighting.f0_to_ior` · F0 to IOR

The index of refraction a measured normal-incidence reflectance implies, against air — the inverse of `ior_to_f0`. An F0 of 0 is reported as the smallest representable IOR rather than a division by zero; an F0 of 1 would be an infinite IOR and is clamped just inside.

- in `f0: f32 = 0.04f`: Reflectance at normal incidence, 0..1.
- out `out: f32`

#### `lighting.fog_composite` · Fog composite

Folds fog into a shaded radiance: what survived the path, plus what the fog scattered in front of the surface. `inscattering` is the radiance the fog itself contributes along that path, already integrated — a uniform fog colour times the path's (1 − transmittance) is the usual stand-in.

- in `radiance: vec3f = vec3f(1.0, 1.0, 1.0)`: Shaded surface radiance.
- in `inscattering: vec3f = vec3f(0.5, 0.5, 0.5)`: Radiance the fog adds.
- in `transmission: f32 = 0.5f`: `fog_transmittance` over the same path.
- out `out: vec3f`

#### `lighting.fog_transmittance` · Fog transmittance

The fraction of light that survives a path through homogeneous fog: Beer–Lambert extinction over `distance` metres at an extinction coefficient of `extinction` per metre. Multiply radiance by it.

- in `distance: f32 = 10.0f`: Path length, metres.
- in `extinction: f32 = 0.05f`: Extinction coefficient, per metre.
- out `out: f32`

#### `lighting.fresnel_schlick` · Fresnel (Schlick)

Reflectance as a function of viewing angle.

- in `f0: vec3f = vec3f(0.04, 0.04, 0.04)`: Reflectance at normal incidence.
- in `cos_theta: f32 = 1.0f`
- out `out: vec3f`

#### `lighting.height_fog_depth` · Height fog optical depth

The optical depth of an exponential height fog along a path: density `ground_density` at the start height, falling off by `falloff` per metre upward, integrated analytically over `distance` metres of path and `height_delta` of climb.

- in `start_height: f32 = 0.0f`: Height the path starts at, metres.
- in `height_delta: f32 = 0.0f`: Height change over the path, metres.
- in `distance: f32 = 10.0f`: Path length, metres.
- in `ground_density: f32 = 0.05f`: Density at height 0, per metre.
- in `falloff: f32 = 0.1f`: Density falloff per metre of height.
- out `out: f32`

#### `lighting.hg_phase` · Henyey-Greenstein phase

The one-parameter scattering lobe for media with forward-thrown (mist, haze) or backward-thrown (dense cloud) particles: `asymmetry` of 0 is isotropic, positive throws light forward, negative backward.

- in `cos_theta: f32 = 0.0f`: Cosine of the scattering angle.
- in `asymmetry: f32 = 0.5f`: The g parameter, -0.99..0.99.
- out `out: f32`

#### `lighting.ior_to_f0` · IOR to F0

The reflectance at normal incidence for an interface between two indices of refraction: what `fresnel_schlick` takes as its F0. The direction of the ratio does not matter — glass to air and air to glass reflect identically at normal incidence.

- in `transmitted: f32 = 1.5f`: IOR on the far side.
- in `incident: f32 = 1.0f`: IOR on the near side.
- out `out: f32`

#### `lighting.iridescence` · Thin-film iridescence

The spectral reflectance of a thin film over a base surface: Snell's refraction into the film, Fresnel amplitudes at both interfaces, the phase inversions, and the two harmonic terms of the Fourier-space XYZ-sensitivity integration — the full evaluation, not a stand-in. The result is the film's RGB Fresnel reflectance. A lighting model blends its ordinary Fresnel toward this response with the film strength; the iridescent model evaluates it at the light's half-vector angle (ADR 0058). `thin_film_phase` is the one scalar term of this spectral evaluation.

- in `base_f0: vec3f = vec3f(0.04, 0.04, 0.04)`: The base surface's F0 reflectance.
- in `cos_theta: f32 = 1.0f`: Cosine of the view angle against the normal.
- in `thickness_nm: f32 = 300.0f`: Film thickness, nanometres.
- in `film_ior: f32 = 1.3f`: The film's IOR, 1..3; 1 is the air interface.
- out `out: vec3f`

#### `lighting.normal_spread_roughness` · Normal spread roughness

Roughness widened by the detail a normal map's mips averaged away. A texture in the `normal` colour space keeps, in each mip's alpha, the GGX alpha its normals' spread adds (ADR 0094); wire that alpha into `spread`. Far away, where a flake or a brushed groove shrinks below a texel, its highlight then turns into the wider, dimmer lobe it really is instead of a mirror, and the mip transitions stop showing.

- in `roughness: f32 = 0.5f`: Perceptual roughness, 0..1.
- in `spread: f32 = 0.0f`: The normal texture's alpha.
- in `strength: f32 = 1.0f`: The normal map's strength.
- out `out: f32`

#### `lighting.pbr_direct` · PBR direct light

One light's full contribution to a physically-based surface: GGX specular plus Lambert diffuse, Fresnel-balanced.

- in `normal: vec3f = vec3f(0.0, 1.0, 0.0)`: Unit shading normal, world space.
- in `view_direction: vec3f = vec3f(0.0, 0.0, 1.0)`: Unit vector towards the eye.
- in `light_direction: vec3f = vec3f(0.0, 1.0, 0.0)`: Unit vector towards the light.
- in `radiance: vec3f = vec3f(1.0, 1.0, 1.0)`: The light's radiance, already attenuated for distance.
- in `base_color: vec3f = vec3f(0.8, 0.8, 0.8)`
- in `metallic: f32 = 0.0f`
- in `roughness: f32 = 0.5f`
- out `out: vec3f`

#### `lighting.pbr_direct_split` · PBR direct light (split)

As `lighting.pbr_direct`, but keeping the diffuse and specular halves separate. The function is evaluated once however many of its outputs are used.

- in `normal: vec3f = vec3f(0.0, 1.0, 0.0)`: Unit shading normal, world space.
- in `view_direction: vec3f = vec3f(0.0, 0.0, 1.0)`: Unit vector towards the eye.
- in `light_direction: vec3f = vec3f(0.0, 1.0, 0.0)`: Unit vector towards the light.
- in `radiance: vec3f = vec3f(1.0, 1.0, 1.0)`: The light's radiance, already attenuated for distance.
- in `base_color: vec3f = vec3f(0.8, 0.8, 0.8)`
- in `metallic: f32 = 0.0f`
- in `roughness: f32 = 0.5f`
- out `diffuse: vec3f`
- out `specular: vec3f`

#### `lighting.prt_diffuse_occlusion` · PRT diffuse occlusion

The share of the environment's diffuse light that reaches a point, per channel, from a grayscale precomputed radiance transfer bake (ADR 0092). Feed it to the surface's `diffuse_occlusion`.

- in `prt0: vec4f = vec4f(0.0, 0.0, 0.0, 0.0)`: The bake's first stream.
- in `prt1: vec4f = vec4f(0.0, 0.0, 0.0, 0.0)`: The bake's second stream.
- in `normal: vec3f = vec3f(0.0, 1.0, 0.0)`: The normal the bake used: the geometric one.
- in `sh0: vec3f = vec3f(0.0, 0.0, 0.0)`: Environment radiance, band 0.
- in `sh1: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 1, m = -1 (y).
- in `sh2: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 1, m = 0 (z).
- in `sh3: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 1, m = 1 (x).
- in `sh4: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 2, m = -2 (xy).
- in `sh5: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 2, m = -1 (yz).
- in `sh6: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 2, m = 0 (3z² - 1).
- in `sh7: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 2, m = 1 (xz).
- in `sh8: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 2, m = 2 (x² - y²).
- out `out: vec3f`

#### `lighting.prt_diffuse_occlusion_color` · PRT diffuse occlusion (colour)

`lighting.prt_diffuse_occlusion` for a colour bake (ADR 0092): the transfer has one set of coefficients per channel, because the light a red panel bounces onto its neighbours is red. `wxsl-prt` packs it into four streams, `prt0` to `prt3`: red and green paired per coefficient first, then blue and the visibility as a grayscale bake lays them out. The environment is read as in the grayscale node, luminance windowed, so the only colour in the ratio is the bounces'.

- in `prt0: vec4f = vec4f(0.0, 0.0, 0.0, 0.0)`: The bake's first stream.
- in `prt1: vec4f = vec4f(0.0, 0.0, 0.0, 0.0)`: The second stream.
- in `prt2: vec4f = vec4f(0.0, 0.0, 0.0, 0.0)`: The third stream.
- in `prt3: vec4f = vec4f(0.0, 0.0, 0.0, 0.0)`: The fourth stream.
- in `normal: vec3f = vec3f(0.0, 1.0, 0.0)`: The normal the bake used: the geometric one.
- in `sh0: vec3f = vec3f(0.0, 0.0, 0.0)`: Environment radiance, band 0.
- in `sh1: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 1, m = -1 (y).
- in `sh2: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 1, m = 0 (z).
- in `sh3: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 1, m = 1 (x).
- in `sh4: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 2, m = -2 (xy).
- in `sh5: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 2, m = -1 (yz).
- in `sh6: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 2, m = 0 (3z² - 1).
- in `sh7: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 2, m = 1 (xz).
- in `sh8: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 2, m = 2 (x² - y²).
- out `out: vec3f`

#### `lighting.prt_specular_occlusion` · PRT specular occlusion

The share of the environment's reflection that reaches a point, from the ambient occlusion a precomputed radiance transfer bake stores (ADR 0092). Feed it to the surface's `specular_occlusion`.

- in `visibility: vec3f = vec3f(0.0, 0.0, 1.0)`: Bent normal times occlusion.
- in `normal: vec3f = vec3f(0.0, 1.0, 0.0)`: Shading normal.
- in `view_direction: vec3f = vec3f(0.0, 0.0, 1.0)`: From the surface towards the eye.
- in `roughness: f32 = 0.5f`: Perceptual roughness.
- out `out: f32`

#### `lighting.prt_unpack` · PRT unpack

The two values a precomputed radiance transfer bake packs into one float (ADR 0092). Each is a 12-bit code over [-4, 4]; the float holds `high * 4096 + low`, an integer a 32-bit float stores exactly, so the pair survives any vertex fetch. Returns (high, low).

- in `packed: f32 = 0.0f`: Two codes, high * 4096 + low.
- out `out: vec2f`

#### `lighting.prt_visibility` · PRT visibility

The visibility a precomputed radiance transfer bake stores beside the transfer (ADR 0092): the bent normal, the mean unblocked direction, scaled by the ambient occlusion. It is in the last stream's z and w in both modes, so this reads `prt1` of a grayscale bake and `prt3` of a colour one. Feed it to `lighting.prt_specular_occlusion`.

- in `last: vec4f = vec4f(0.0, 0.0, 0.0, 0.0)`: The bake's last stream.
- out `out: vec3f`

#### `lighting.prt_window` · PRT window

One environment coefficient as a precomputed radiance transfer reads it (ADR 0092): its luminance in every channel, times the band's window. Order-3 SH of a small opening (a car window) dotted with a low sun rings, often below zero in one channel and far above in another; the window (1, 0.6, 0.2 for bands 0, 1, 2) damps the high bands of both the transfer and the unshadowed lobe it is divided by. Taking luminance leaves the colour to the environment maps and the bounces.

- in `sh: vec3f = vec3f(0.0, 0.0, 0.0)`: An environment coefficient.
- in `band: f32 = 0.0f`: Its SH band: 0, 1 or 2.
- out `out: vec3f`

#### `lighting.rayleigh_phase` · Rayleigh phase

How Rayleigh scattering distributes light over angles: symmetric front and back, dimmest at right angles, the reason the sky is brightest toward and away from the sun and bluest to the side. The value is a probability density (per steradian), not a colour — feed it the scattering coefficient for one.

- in `cos_theta: f32 = 0.0f`: Cosine of the scattering angle.
- out `out: f32`

#### `lighting.roughness_aa` · Roughness anti-aliasing

Roughness widened by the screen-space variance of the normal: where a surface foreshortens or a curved edge packs many normals into few pixels, specular lobes alias into sparkle. This folds that variance into the roughness the GGX alpha squares, so the highlight calms exactly where the detail ran out — the standard pre-filtering move (the Kaplanyan–Dachsbacher / Akenine-Möller fitting, re-derived).

- in `normal: vec3f = vec3f(0.0, 0.0, 1.0)`: Shading normal, world space.
- in `roughness: f32 = 0.5f`: Perceptual roughness, 0..1.
- in `strength: f32 = 1.0f`: How much variance to absorb, 0..1.
- out `out: f32`
- fragment stage only

#### `lighting.sh_irradiance` · SH irradiance

The irradiance an environment given as order-3 spherical harmonics delivers to a surface facing `normal`: its radiance through the clamped cosine, unshadowed. `sh0`..`sh8` are radiance coefficients in the order `wxsl_prt::sh` uses: band 0, band 1 as (y, z, x), band 2 as (xy, yz, 3z² - 1, xz, x² - y²).

- in `normal: vec3f = vec3f(0.0, 1.0, 0.0)`: Surface normal.
- in `sh0: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 0.
- in `sh1: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 1, m = -1 (y).
- in `sh2: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 1, m = 0 (z).
- in `sh3: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 1, m = 1 (x).
- in `sh4: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 2, m = -2 (xy).
- in `sh5: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 2, m = -1 (yz).
- in `sh6: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 2, m = 0 (3z² - 1).
- in `sh7: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 2, m = 1 (xz).
- in `sh8: vec3f = vec3f(0.0, 0.0, 0.0)`: Band 2, m = 2 (x² - y²).
- out `out: vec3f`

#### `lighting.sheen_ibl_response` · Sheen IBL response

Bounded approximation of the hemispherical Charlie-sheen response. Multiply by sheen colour and environment irradiance; this is not a full BRDF.

- in `n_dot_v: f32 = 0.5f`: Cosine between normal and view.
- in `roughness: f32 = 0.5f`: Perceptual sheen roughness.
- out `out: f32`

#### `lighting.sky_single_scattering` · Single-scattering sky

Rayleigh and Mie in-scattered linear radiance in a spherical exponential atmosphere, with RGB extinction and planet shadow. Origins are planet-centred, distances kilometres, coefficients inverse kilometres. No sun disc, ground reflection, ozone or multiple scattering; exposure belongs to the caller.

- in `origin: vec3f = vec3f(0.0, 6360.01, 0.0)`: Planet-centred observer, in km.
- in `direction: vec3f = vec3f(0.0, 1.0, 0.0)`: Viewing ray, normalized internally.
- in `sun_direction: vec3f = vec3f(0.0, 1.0, 0.0)`: Direction toward the sun.
- in `sun_irradiance: vec3f = vec3f(20.0, 20.0, 20.0)`: Linear RGB incident irradiance.
- in `planet_radius: f32 = 6360.0f`: Opaque ground radius, km.
- in `atmosphere_radius: f32 = 6460.0f`: Outer atmosphere radius, km.
- in `rayleigh_height: f32 = 8.0f`: Rayleigh density scale height, km.
- in `mie_height: f32 = 1.2f`: Mie density scale height, km.
- in `rayleigh_scattering: vec3f = vec3f(0.0058, 0.0135, 0.0331)`: RGB scattering/extinction at ground, per km.
- in `mie_scattering: f32 = 0.0044f`: Achromatic Mie scattering at ground, per km.
- in `mie_absorption: f32 = 0.00044f`: Additional Mie absorption at ground, per km.
- in `mie_asymmetry: f32 = 0.8f`: HG asymmetry, clamped to -0.99..0.99.
- out `out: vec3f`
- macro `WXSL_SKY_VIEW_STEPS` = `32`: View quadrature budget, clamped to 1..128.
- macro `WXSL_SKY_SUN_STEPS` = `8`: Solar quadrature budget, clamped to 1..64.

#### `lighting.thin_film_phase` · Thin film phase

The optical phase difference a wave accumulates crossing a film of the given thickness twice — once in, once after reflecting off the film's lower interface: 4π · n · d · cosθ / λ.

- in `thickness_nm: f32 = 400.0f`: Film thickness.
- in `film_ior: f32 = 1.3f`: Index of refraction of the film.
- in `cos_inside: f32 = 1.0f`: Cosine of the angle *inside* the film.
- in `wavelength_nm: f32 = 550.0f`: Wavelength being integrated.
- out `out: f32`

#### `lighting.visibility_ggx_anisotropic` · GGX visibility, anisotropic

The Smith height-correlated visibility for a stretched GGX lobe: how much of the distribution survives microfacet shadowing and masking when the lobe is narrower along one tangent axis than the other. Pair with `distribution_ggx_anisotropic` and a Fresnel term.

- in `tangent: vec3f = vec3f(1.0, 0.0, 0.0)`: Surface tangent, unit length.
- in `bitangent: vec3f = vec3f(0.0, 1.0, 0.0)`: Surface bitangent, unit length.
- in `normal: vec3f = vec3f(0.0, 0.0, 1.0)`: Surface normal, unit length.
- in `view: vec3f = vec3f(0.0, 0.0, 1.0)`: Direction from the surface to the eye.
- in `light: vec3f = vec3f(0.0, 0.0, 1.0)`: Direction from the surface to the light.
- in `alpha_x: f32 = 0.25f`: Width along the tangent.
- in `alpha_y: f32 = 0.25f`: Width along the bitangent.
- out `out: f32`

#### `lighting.visibility_neubelt` · Neubelt visibility

The visibility (shadowing-masking) term the sheen lobe pairs with: a cheap approximation that stays well-behaved at grazing angles, where the Smith terms of a mirror BRDF go to extremes.

- in `n_dot_v: f32 = 0.5f`: Cosine between normal and view.
- in `n_dot_l: f32 = 0.5f`: Cosine between normal and light.
- out `out: f32`

#### `lighting.visibility_smith` · Smith visibility

Height-correlated masking and shadowing, already divided by the microfacet BRDF's denominator.

- in `n_dot_v: f32 = 1.0f`
- in `n_dot_l: f32 = 1.0f`
- in `roughness: f32 = 0.5f`
- out `out: f32`

<a id="shader-logic"></a>

### logic

#### `logic.and` · And

True when both inputs are true.

- in `a: bool = false`
- in `b: bool = false`
- out `out: bool`

#### `logic.not` · Not

Invert a boolean.

- in `a: bool = false`
- out `out: bool`

#### `logic.or` · Or

True when either input is true.

- in `a: bool = false`
- in `b: bool = false`
- out `out: bool`

#### `logic.select` · Select

Pick one of two values by a condition. Both inputs are evaluated — this is a data selection, not a branch.

- in `if_false: T = 0.0 in every component`
- in `if_true: T = 1.0 in every component`
- in `condition: bool = false`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

<a id="shader-math"></a>

### math

#### `math.absolute` · Absolute

Drop the sign, component-wise.

- in `a: T`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.add` · Add

Sum of two operands. Either may be a scalar spread over the other's components, and two matrices of the same shape add element-wise.

- in `a: A`
- in `b: B`
- out `out: A + B`
- `A`: one of f32, vec2f, vec3f, vec4f, mat3x3f, mat4x4f
- `B`: one of f32, vec2f, vec3f, vec4f, mat3x3f, mat4x4f

#### `math.arccosine` · Arccosine

Inverse cosine, in radians. Input outside [-1, 1] gives NaN.

- in `a: T`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.arcsine` · Arcsine

Inverse sine, in radians. Input outside [-1, 1] gives NaN.

- in `a: T`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.arctangent` · Arctangent

Inverse tangent, in radians.

- in `a: T`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.arctangent2` · Arctangent 2

Angle of the vector (x, y) in radians, over the full circle. Component-wise for the vector types, which makes it a field of angles rather than one angle.

- in `y: T = 0.0 in every component`
- in `x: T = 1.0 in every component`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.ceil` · Ceil

Round up, component-wise.

- in `a: T`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.clamp` · Clamp

Constrain to a range, component-wise.

- in `x: T = 0.0 in every component`
- in `low: T = 0.0 in every component`
- in `high: T = 1.0 in every component`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.cosine` · Cosine

Cosine of an angle in radians.

- in `a: T`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.divide` · Divide

Quotient of two operands, either of which may be a scalar spread over the other's components. Division by zero yields an infinity, which will spread; guard the divisor if it can reach zero.

- in `a: A`
- in `b: B`
- out `out: A + B`
- `A`: one of f32, vec2f, vec3f, vec4f
- `B`: one of f32, vec2f, vec3f, vec4f

#### `math.exponential` · Exponential

e raised to the input, component-wise.

- in `a: T`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.exponential_2` · Exponential (base 2)

2 raised to the input, component-wise.

- in `a: T`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.floor` · Floor

Round down, component-wise.

- in `a: T`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.fraction` · Fraction

The fractional part, always in [0, 1).

- in `a: T`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.inverse_lerp` · Inverse lerp

Where a value sits between two others, as a 0..1 factor — the inverse of `math.mix`. Components where `a == b` give 0.

- in `a: T = 0.0 in every component`
- in `b: T = 1.0 in every component`
- in `value: T = 0.5 in every component`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.inverse_square_root` · Inverse square root

1/sqrt, component-wise, as a single instruction.

- in `a: T`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.logarithm` · Logarithm

Natural log, component-wise.

- in `a: T`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.logarithm_2` · Logarithm (base 2)

Base-2 log, component-wise.

- in `a: T`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.maximum` · Maximum

Component-wise larger of the two.

- in `a: T`
- in `b: T`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.minimum` · Minimum

Component-wise smaller of the two.

- in `a: T`
- in `b: T`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.mix` · Mix

Linear blend: `a` at t=0, `b` at t=1, extrapolating outside. `t` is a scalar, which WGSL's `mix` accepts against operands of any width.

- in `a: T = 0.0 in every component`
- in `b: T = 1.0 in every component`
- in `t: f32 = 0.5f`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.modulo` · Modulo

Floating-point remainder, keeping the sign of the dividend. For a periodic wrap use `math.wrap` instead.

- in `a: A`
- in `b: B`
- out `out: A + B`
- `A`: one of f32, vec2f, vec3f, vec4f
- `B`: one of f32, vec2f, vec3f, vec4f

#### `math.multiply` · Multiply

Product of two operands. Either may be a scalar spread over the other's components, and a matrix may multiply a vector of its own size — which is how a basis from `space.tangent_basis` is applied to a direction — or another matrix of the same shape.

- in `a: A`
- in `b: B`
- out `out: A * B`
- `A`: one of f32, vec2f, vec3f, vec4f, mat3x3f, mat4x4f
- `B`: one of f32, vec2f, vec3f, vec4f, mat3x3f, mat4x4f

#### `math.negate` · Negate

Flip the sign, component-wise.

- in `a: T`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.power` · Power

`a` raised to `b`, component-wise. Undefined for a negative base with a fractional exponent.

- in `a: T`
- in `b: T`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.ray_sphere` · Ray sphere intersection

Where a ray meets a sphere centred at the origin: the two path lengths along `direction` where it enters and leaves. A negative near root means the origin is inside; both negative means the sphere is behind.

- in `origin: vec3f = vec3f(0.0, 0.0, 0.0)`: Ray origin, sphere at the origin.
- in `direction: vec3f = vec3f(0.0, 0.0, -1.0)`: Ray direction.
- in `radius: f32 = 1.0f`: Sphere radius.
- out `out: vec2f`

#### `math.remap` · Remap

Map a value from [in_min, in_max] onto [out_min, out_max]. Not clamped: feed the result through `math.clamp` if the input can leave its stated range. A zero-width input range gives `out_min`.

- in `value: T = 0.0 in every component`
- in `in_min: T = 0.0 in every component`
- in `in_max: T = 1.0 in every component`
- in `out_min: T = 0.0 in every component`
- in `out_max: T = 1.0 in every component`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.round` · Round

Round to nearest, halves to even.

- in `a: T`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.safe_normalize` · Safe normalize

Normalize, returning zero instead of NaN for a zero-length input. A NaN here spreads through everything downstream and shows up as black or missing pixels far from its cause.

- in `v: T = 0.0 in every component`
- out `out: T`
- `T`: one of vec2f, vec3f, vec4f

#### `math.saturate` · Saturate

Clamp to [0, 1], component-wise.

- in `a: T`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.screen_width` · Screen-space width

How much one pixel of screen travel changes a value — the graph-facing spelling of `fwidth`. The caller-side recipe an anti-aliased width wants: feed a distance field's `screen_width` into `sdf_coverage`'s `width`, and the coverage ramp is always about one pixel wide however the surface foreshortens. A constant width aliases exactly where the field crosses texel boundaries steeply.

- in `value: f32 = 0.0f`: The quantity whose pixel-to-pixel change matters.
- out `out: f32`
- fragment stage only

#### `math.sign` · Sign

-1, 0 or 1 per component.

- in `a: T`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.sine` · Sine

Sine of an angle in radians.

- in `a: T`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.smootherstep` · Smootherstep

Quintic ramp between two edges, with a continuous second derivative — no crease where the ramp meets the flat parts.

- in `edge0: T = 0.0 in every component`
- in `edge1: T = 1.0 in every component`
- in `x: T = 0.5 in every component`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.smoothstep` · Smoothstep

Hermite ramp between two edges. For a continuous second derivative use `math.smootherstep`.

- in `edge0: T = 0.0 in every component`
- in `edge1: T = 1.0 in every component`
- in `x: T = 0.5 in every component`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.square_root` · Square root

Component-wise square root; negative inputs give NaN.

- in `a: T`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.step` · Step

0 below the edge, 1 at or above it, component-wise.

- in `edge: T = 0.5 in every component`
- in `x: T = 0.0 in every component`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.subtract` · Subtract

Difference of two operands, with the same mixing rules as `math.add`.

- in `a: A`
- in `b: B`
- out `out: A + B`
- `A`: one of f32, vec2f, vec3f, vec4f, mat3x3f, mat4x4f
- `B`: one of f32, vec2f, vec3f, vec4f, mat3x3f, mat4x4f

#### `math.tangent` · Tangent

Tangent of an angle in radians.

- in `a: T`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.truncate` · Truncate

Drop the fractional part, towards zero.

- in `a: T`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

#### `math.wrap` · Wrap

Wrap a value into [low, high), the way a repeating texture coordinate does. Unlike `math.modulo` this is correct for negative inputs: wrapping -0.25 into 0..1 gives 0.75.

- in `value: T = 0.0 in every component`
- in `low: T = 0.0 in every component`
- in `high: T = 1.0 in every component`
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f

<a id="shader-output"></a>

### output

#### `output.discard` · Discard

Throws the fragment away when true, writing neither colour nor depth. This is the only part of a material a depth or shadow pass compiles, so keep what feeds it cheap.

- in `discard: bool = false` (optional): Discard the fragment when true.
- only in surface graphs

#### `output.fragment` · Fragment output

The colour this material writes, in place of its shaded surface. Feed it from the surface output's `color`; nodes in between change the lit colour. Linear: the pipeline's tonemap still applies.

- in `color: vec4f = vec4f(0.0, 0.0, 0.0, 1.0)` (optional): Linear radiance and alpha.
- only in surface graphs

#### `output.screen` · Screen output

The colour one fullscreen pass writes. Unconnected inputs keep the value shown in the editor, so a graph driving only `color` is a complete effect.

- in `color: vec3f = vec3f(0.0, 0.0, 0.0)` (optional): Linear radiance, unless this is the pass that encodes for a display.
- in `alpha: f32 = 1.0f` (optional): Alpha written to the target.
- only in screen graphs

#### `output.surface` · Surface output

The material a graph produces. Unconnected inputs keep their default: the geometric normal for `normal`, the value shown in the editor for the rest.

- in `ior: f32 = 1.5f` (optional): Optical index of refraction; 1 means no bending. Read by pass.refraction.
- in `attenuation_distance: f32 = 1.0f` (optional): World distance at which transmitted white becomes base_color (Beer absorption).
- in `base_color: vec3f = vec3f(0.8, 0.8, 0.8)` (optional): Linear-space diffuse albedo (metals: reflectance tint).
- in `metallic: f32 = 0.0f` (optional): 0 = dielectric, 1 = conductor.
- in `roughness: f32 = 0.5f` (optional): Perceptual roughness; squared into the GGX alpha.
- in `normal: vec3f` (optional): Shading normal in world space. Unconnected: geometric normal.
- in `emissive: vec3f = vec3f(0.0, 0.0, 0.0)` (optional): Light emitted by the surface, added after shading.
- in `occlusion: f32 = 1.0f` (optional): Ambient occlusion multiplier, 1 = unoccluded.
- in `alpha: f32 = 1.0f` (optional): Opacity. The deferred path ignores it (the G-buffer is opaque).
- in `iridescence_strength: f32 = 0.0f` (optional): Thin-film specular weight, 0..1; read by the iridescent model.
- in `iridescence_thickness: f32 = 300.0f` (optional): Thin-film thickness in nanometres; zero disables the film.
- in `iridescence_ior: f32 = 1.3f` (optional): Thin-film index of refraction, 1..3.
- in `sheen_color: vec3f = vec3f(0.0, 0.0, 0.0)` (optional): Linear RGB sheen reflectance, 0..1; black disables the layer.
- in `sheen_roughness: f32 = 0.5f` (optional): Perceptual roughness of the Charlie sheen layer, 0..1.
- in `clearcoat: f32 = 0.0f` (optional): Clear-coat strength, 0..1 (glTF clearcoat); read by the glTF clear-coat model.
- in `clearcoat_roughness: f32 = 0.0f` (optional): Perceptual roughness of the clear coat, 0..1.
- in `clearcoat_normal: vec3f` (optional): Clear-coat normal in world space. Unconnected: geometric normal.
- in `diffuse_occlusion: vec3f = vec3f(1.0, 1.0, 1.0)` (optional): Share of the environment maps' diffuse light that arrives, per channel (PRT).
- in `specular_occlusion: f32 = 1.0f` (optional): Share of the environment maps' reflection that arrives, 1 = unoccluded.
- in `transmission: f32 = 1.0f` (optional): Share of light pass.refraction lets through; the rest is the glass's diffuse base colour.
- in `thickness: f32 = 1.0f` (optional): 0 is thin-walled glass for pass.refraction: one interface, no bending, tinted by base_color.
- out `color: vec4f`: The surface lit by the pipeline: linear radiance and alpha. Only an `output.fragment` chain can read it.
- only in surface graphs

#### `output.varying` · Interpolant output

Computes one declared interpolant in the vertex stage, to be read per fragment. The graph declares the name and the type; this says what goes in it. Each interpolant costs one of the 16 inter-stage locations, and the graph is told when it runs out.

- in `value: T` (optional): What the vertex stage computes for this interpolant.
- `T`: one of f32, vec2f, vec3f, vec4f
- setting `name` = `value`: Which declared interpolant this writes.
- only in surface graphs

#### `output.vertex` · Vertex output

Moves the vertex, in object space, before it is transformed. The one place a material graph runs in the vertex stage — so everything feeding it is compiled there too, including into a shadow pass, which is what makes a displaced object cast a displaced shadow.

- in `position_offset: vec3f = vec3f(0.0, 0.0, 0.0)` (optional): Object-space offset added to the vertex position.
- in `normal: vec3f` (optional): Object-space normal replacing the mesh's; unconnected, the mesh's.
- in `tangent: vec3f` (optional): Object-space tangent replacing the mesh's, its bitangent sign kept; unconnected, the mesh's.
- only in surface graphs

<a id="shader-param"></a>

### param

#### `param.value` · Parameter

A value the host can change without recompiling: a field of this material's uniform buffer. Unlike a constant, editing it costs a buffer write rather than a new shader variant. The `value` socket is its starting value, and two parameters with the same name are one parameter.

- in `value: T = 0.0 in every component` (a pinned value, never connected)
- out `out: T`
- `T`: one of f32, vec2f, vec3f, vec4f, mat3x3f, mat4x4f, bool, i32, u32
- setting `name` = `amount`: The name the host writes this parameter by, and the field name in the generated struct.
- only in surface graphs

<a id="shader-sample"></a>

### sample

#### `sample.load_2d` · Load a 2D texture

Read a texel out of a texture at a UV coordinate, with no sampler and no filtering: the uv is scaled to the texture's size, clamped to it, and the texel there is returned from the base mip level.

- in `tex: texture_2d<f32>`
- in `uv: vec2f = vec2f(0.0, 0.0)`
- out `out: vec4f`

#### `sample.texture_2d` · Sample a 2D texture

Read a colour out of a texture at a UV coordinate, with filtering and mip selection chosen by the sampler. Wire a `texture.texture_2d` node into `tex` and a `texture.sampler` node into `samp`.

- in `tex: texture_2d<f32>`
- in `samp: sampler`
- in `uv: vec2f = vec2f(0.0, 0.0)`
- out `out: vec4f`
- fragment stage only

<a id="shader-sdf"></a>

### sdf

#### `sdf.box` · SDF box

Signed distance to an axis-aligned box at the origin.

- in `p: vec3f = vec3f(0.0, 0.0, 0.0)`
- in `half_extents: vec3f = vec3f(0.5, 0.5, 0.5)`
- out `out: f32`

#### `sdf.capsule` · SDF capsule

Signed distance to the capsule around a 2D segment: every point within `radius` of the segment from `a` to `b` is inside. A degenerate segment (a == b) is a circle.

- in `p: vec2f = vec2f(0.0, 0.0)`: Sample point.
- in `a: vec2f = vec2f(-0.3, 0.0)`: Segment start.
- in `b: vec2f = vec2f(0.3, 0.0)`: Segment end.
- in `radius: f32 = 0.2f`
- out `out: f32`

#### `sdf.chamfer_union` · SDF chamfer union

Union of two fields cut by a 45-degree chamfer of width `r` along the seam, instead of smooth union's rounded blend.

- in `a: f32 = 0.0f`: First field.
- in `b: f32 = 0.0f`: Second field.
- in `r: f32 = 0.1f`: Chamfer width.
- out `out: f32`

#### `sdf.circle` · SDF circle

Signed distance to a circle in the plane: negative inside, zero on the rim, exact Euclidean distance outside.

- in `p: vec2f = vec2f(0.0, 0.0)`: Sample point.
- in `radius: f32 = 0.5f`
- out `out: f32`

#### `sdf.coverage` · SDF coverage

Anti-aliased inside/outside coverage of a distance field, as an alpha in 0..1: 1 fully inside, 0 fully outside, a linear ramp of width `w` across the boundary.

- in `distance: f32 = 0.0f`: A signed distance field.
- in `width: f32 = 0.01f`: Half the transition ramp, in distance units.
- out `out: f32`

#### `sdf.cylinder` · SDF cylinder

Signed distance to a capped cylinder around the y axis: a disc of `radius` extruded `half_height` in each direction. Exact outside.

- in `p: vec3f = vec3f(0.0, 0.0, 0.0)`: Sample point.
- in `half_height: f32 = 0.5f`: Distance from the mid-plane to each cap.
- in `radius: f32 = 0.5f`
- out `out: f32`

#### `sdf.equilateral_triangle` · SDF equilateral triangle

Signed distance to an equilateral triangle in the plane, pointing up, `radius` from centre to a vertex.

- in `p: vec2f = vec2f(0.0, 0.0)`: Sample point.
- in `radius: f32 = 0.5f`: Centre to vertex.
- out `out: f32`

#### `sdf.hexagon` · SDF hexagon

Signed distance to a regular flat-topped hexagon in the plane, `radius` from centre to a corner.

- in `p: vec2f = vec2f(0.0, 0.0)`: Sample point.
- in `radius: f32 = 0.5f`: Centre to corner.
- out `out: f32`

#### `sdf.intersection` · SDF intersection

Only where both fields are inside: `max` of two distances.

- in `a: f32 = 0.0f`: First field.
- in `b: f32 = 0.0f`: Second field.
- out `out: f32`

#### `sdf.onion` · SDF onion

A shell of `thickness` around a field's boundary: the distance field of the surface itself, hollowed out.

- in `distance: f32 = 0.0f`: A signed distance field.
- in `thickness: f32 = 0.1f`: Shell width.
- out `out: f32`

#### `sdf.round` · SDF round

Inflates a field by `radius`: every surface moves outward that far, and sharp corners become rounded ones of that radius. A negative radius shrinks the field instead.

- in `distance: f32 = 0.0f`: A signed distance field.
- in `radius: f32 = 0.1f`: How far to grow (negative shrinks).
- out `out: f32`

#### `sdf.rounded_box` · SDF rounded box

Signed distance to an axis-aligned rectangle with rounded corners, in the plane. Exact outside; inside it is the bounded distance to the nearest surface, the usual convention.

- in `p: vec2f = vec2f(0.0, 0.0)`: Sample point.
- in `half_size: vec2f = vec2f(0.5, 0.5)`: Half extents, centre to face.
- in `radius: f32 = 0.1f`: Corner rounding, clamped to the smaller half extent.
- out `out: f32`

#### `sdf.smooth_intersection` · SDF smooth intersection

Intersection of two fields with a filleted seam of width `k` — the counterpart of `smooth_union` for the inside of both.

- in `a: f32 = 0.0f`: First field.
- in `b: f32 = 0.0f`: Second field.
- in `k: f32 = 0.1f`: Width of the filleted seam.
- out `out: f32`

#### `sdf.smooth_union` · SDF smooth union

Union of two fields with a rounded seam of width `k`.

- in `a: f32 = 0.0f`
- in `b: f32 = 0.0f`
- in `k: f32 = 0.1f`: Width of the rounded seam.
- out `out: f32`

#### `sdf.sphere` · SDF sphere

Signed distance to a sphere at the origin.

- in `p: vec3f = vec3f(0.0, 0.0, 0.0)`
- in `radius: f32 = 0.5f`
- out `out: f32`

#### `sdf.subtract` · SDF subtract

The first field with the second carved out of it: `a` where it is inside and `b` is outside.

- in `a: f32 = 0.0f`: The field being carved.
- in `b: f32 = 0.0f`: The field cutting.
- out `out: f32`

#### `sdf.torus` · SDF torus

Signed distance to a torus around the y axis: a ring of tube radius `tube` whose centreline has radius `ring`.

- in `p: vec3f = vec3f(0.0, 0.0, 0.0)`: Sample point.
- in `ring: f32 = 0.35f`: Centreline radius.
- in `tube: f32 = 0.15f`: Tube radius.
- out `out: f32`

#### `sdf.union` · SDF union

The outside of either field: `min` of two distances.

- in `a: f32 = 0.0f`: First field.
- in `b: f32 = 0.0f`: Second field.
- out `out: f32`

<a id="shader-space"></a>

### space

#### `space.apply_normal_map` · Apply normal map

Take a tangent-space normal into world space, with a strength control. Wire the result into the output node's `normal`.

- in `tangent_normal: vec3f = vec3f(0.0, 0.0, 1.0)`
- in `normal: vec3f = vec3f(0.0, 1.0, 0.0)`
- in `tangent: vec3f = vec3f(1.0, 0.0, 0.0)`
- in `bitangent: vec3f = vec3f(0.0, 0.0, 1.0)`
- in `strength: f32 = 1.0f`
- out `out: vec3f`

#### `space.direction_to_equirect` · Direction to equirectangular uv

Maps a unit-ish direction onto the 0..1 uv of an equirectangular environment map: longitude around the y axis, latitude from pole to pole. y is up and the seam sits on -x, behind the +z-forward convention the tangent basis builds — whether v needs flipping is a property of the texture, and this node assumes the common top-at-v=0.

- in `direction: vec3f = vec3f(0.0, 0.0, 1.0)`: Direction to map.
- out `out: vec2f`

#### `space.equirect_to_direction` · Equirectangular uv to direction

Unit direction for a y-up environment map with north at v=0 and seam on -x. Inverse of direction_to_equirect, modulo the seam and pole longitude.

- in `uv: vec2f = vec2f(0.5, 0.5)`: Equirectangular coordinate, u wraps and v clamps.
- out `out: vec3f`

#### `space.rotate_uv` · Rotate UV

Rotate a coordinate around a pivot, in turns.

- in `uv: vec2f = vec2f(0.0, 0.0)`
- in `pivot: vec2f = vec2f(0.5, 0.5)`
- in `turns: f32 = 0.0f`
- out `out: vec2f`

#### `space.tangent_basis` · Tangent basis

An arbitrary but stable orthonormal frame around a normal, as the columns (tangent, bitangent, normal).

- in `normal: vec3f = vec3f(0.0, 1.0, 0.0)`
- out `out: mat3x3f`

<a id="shader-texture"></a>

### texture

#### `texture.sampler` · Sampler

How a texture is filtered and wrapped, bound by the application. One sampler serves every texture read through it.

- out `out: sampler`
- setting `name` = `linear`: The name the application binds this sampler by.
- only in surface graphs

#### `texture.texture_2d` · Texture 2D

A 2D texture the application binds by name. Wire it into a sampling node together with a sampler.

- out `out: texture_2d<f32>`
- setting `name` = `albedo`: The name the application binds this texture by.
- only in surface graphs

#### `texture.texture_cube` · Texture cube

A cube texture the application binds by name.

- out `out: texture_cube<f32>`
- setting `name` = `environment`: The name the application binds this texture by.
- only in surface graphs

<a id="shader-vector"></a>

### vector

#### `vector.cross` · Cross product

The vector perpendicular to both inputs, right-handed.

- in `a: vec3f = vec3f(1.0, 0.0, 0.0)`
- in `b: vec3f = vec3f(0.0, 1.0, 0.0)`
- out `out: vec3f`

#### `vector.distance` · Distance

Euclidean distance between two points.

- in `a: T = 0.0 in every component`
- in `b: T = 0.0 in every component`
- out `out: f32`
- `T`: one of vec2f, vec3f, vec4f

#### `vector.dot` · Dot product

Sum of component-wise products; the cosine of the angle between two unit vectors.

- in `a: T = 0.0 in every component`
- in `b: T = 0.0 in every component`
- out `out: f32`
- `T`: one of vec2f, vec3f, vec4f

#### `vector.length` · Length

Euclidean length.

- in `v: T = 0.0 in every component`
- out `out: f32`
- `T`: one of vec2f, vec3f, vec4f

#### `vector.normalize` · Normalize

Scale to unit length. A zero-length input gives NaN; use `math.safe_normalize` where that is possible.

- in `v: T = 1.0 in every component`
- out `out: T`
- `T`: one of vec2f, vec3f, vec4f

#### `vector.reflect` · Reflect

Mirror an incident direction about a normal. Both should be unit length, and `incident` points *at* the surface.

- in `incident: T = 0.0 in every component`
- in `normal: T = 0.0 in every component`
- out `out: T`
- `T`: one of vec2f, vec3f, vec4f

#### `vector.refract` · Refract

Bend an incident direction through a surface. `eta` is the ratio of refractive indices; total internal reflection returns the zero vector.

- in `incident: T = 0.0 in every component`
- in `normal: T = 0.0 in every component`
- in `eta: f32 = 0.6666667f`
- out `out: T`
- `T`: one of vec2f, vec3f, vec4f

#### `vector.transform` · Transform by matrix

Multiply a vector by a matrix of its own size — the way to use a basis from `space.tangent_basis`, or any other frame, on a direction. `M` and `V` resolve independently, so a mat3x3f with a vec4f is reported rather than silently accepted.

- in `m: M = 1.0 in every component`
- in `v: V = 1.0 in every component`
- out `out: M * V`
- `M`: one of mat3x3f, mat4x4f
- `V`: one of vec3f, vec4f

## Pipeline nodes

Categories: [output](#pipeline-output), [pass](#pipeline-pass), [resource](#pipeline-resource), [source](#pipeline-source).

<a id="pipeline-output"></a>

### output

#### `output.present` · Present

What reaches the frame's own target. Exactly one per document.

- in `surface: color_target`: What to present.
- only in document graphs

<a id="pipeline-pass"></a>

### pass

#### `pass.bloom` · bloom pyramid

A bloom pyramid over one HDR image: a thresholding extract into a half-resolution first level, a 13-tap down per deeper level, a fold per shallower one, and the combine that adds the assembled glow back onto the image. Expands to the pyramid's level resources and passes (ADR 0065) with stable labels — `<label> extract`, `<label> down 1`, `<label> up 0`, `<label> combine` — which is what `set_pass_param` tunes through: `threshold` and `knee` on the extract, `strength` on the combine. The glow resources are HDR transients; thresholding is linear radiance, so an 8-bit intermediate would clamp the highlight the pyramid is for.

- in `image: color_target`: The image to glow from, as linear radiance.
- in `into: color_target` (optional): Where to write. Unconnected means the frame's own target.
- out `color: color_target`: What the combine wrote — the frame's target when `into` is unconnected.
- setting `levels` = `4`: How many pyramid levels, including the first: each is half the one before, starting at half resolution.
- setting `threshold` = `1`: Linear radiance where the glow begins; the knee eases it in.
- setting `knee` = `0.6`: Width of the soft threshold knee.
- setting `strength` = `0.85`: How much of the assembled glow is added back.
- only in document graphs

#### `pass.dof` · depth of field

A depth-of-field chain over one image and its opaque depth: the signed circle of confusion each pixel's depth asks for, a far-field blur with the occlusion fade (a sharp foreground never smears into the background it overlaps), and the dilated near-field composite (a foreground's blur spills over the background exactly as far as its own discs reach). Expands to three passes with stable labels (ADR 0065) — `<label> circles`, `<label> far blur`, `<label> near blur` — which is what `set_pass_param` tunes through. SI units: metres in, pixels of blur out. The depth comes from a `resource.depth` or a material pass — a depth prepass is the usual wire.

- in `image: color_target`: The image to defocus, as linear radiance.
- in `depth: depth_target`: The opaque depth the discs measure against.
- in `into: color_target` (optional): Where to write. Unconnected means the frame's own target.
- out `color: color_target`: What the near pass wrote — the frame's target when `into` is unconnected.
- setting `focus` = `10`: Where the focus plane sits, in metres. Everything there is sharp.
- setting `f_number` = `2.8`: Aperture as focal ratio.
- setting `focal` = `0.05`: Metres.
- setting `sensor` = `0.035`: Metres.
- setting `near` = `0.1`: Metres — the depth's linearization.
- setting `far` = `400`: Metres — the depth's linearization.
- setting `max_coc` = `16`: Largest disc the passes gather, in pixels.
- only in document graphs

#### `pass.environment` · environment bake

Equirectangular linear radiance to filtered lighting cubes; one environment per frame.

- in `image: color_target`
- out `radiance: color_target`
- setting `size` = `128`: Square cube face side in pixels.
- setting `diffuse_size` = `16`: Diffuse cube face side in pixels.
- setting `mips` = `8`: GGX roughness levels, including zero.
- setting `radiance_scale` = `1`: Positive restoration factor for scaled HDR uploads.
- only in document graphs

#### `pass.geometry` · material pass

Draws a material stage over a draw list. Wired to a G-buffer it writes that set's targets; wired to depth it clears it if the depth comes straight from a `resource.depth` node and tests against it (without writing) if it comes from another pass's depth output. Its colour output feeds `present`, or another pass in a chain.

- in `draws: draw_queue`: What to draw.
- in `gbuffer: gbuffer` (optional): For a `gbuffer`-stage pass: the G-buffer to write.
- in `depth: depth_target` (optional): The depth to clear or test against.
- in `into: color_target` (optional): Where a colour-writing stage writes. Unconnected means the frame's own target; wired to a `resource.color`, the pass starts an effect chain instead of ending one.
- out `color: color_target`: What the pass wrote — unconnected for a stage that writes no colour.
- out `depth: depth_target`: The depth as this pass left it, for the next pass to test against.
- setting `stage` = `forward_lit`, one of forward_lit, gbuffer, depth_only, velocity: forward_lit, gbuffer, depth_only or velocity — the material stage the pass draws.
- setting `policy` = `per frame`, one of per frame, once, on resize, on demand: How often the pass runs: per frame, once, on resize or on demand. Only a pass whose target survives frames may skip.
- setting `sort` = `none`, one of none, front to back, back to front: How the pass orders its draws: none (submission order), front to back, or back to front. Sorting reorders the draws inside this pass only.
- setting `layers` = `all`, one of all, sorted: Which draws the pass takes, by each material's max layers: all of them, or only the sorted tier (max layers zero) that no peel pass reads. Peeling itself is a `pass.peel` node.
- setting `blend` = `opaque`, one of opaque, alpha over: How fragments land in the target: opaque (no blending) or alpha over (straight-alpha source over the target), which a sorted transparent pass composites with.
- only in document graphs

#### `pass.peel` · depth peel

Order-independent transparency by dual depth peeling. Draws only instances tagged `transparent`, however the scene source that feeds other passes is filtered, and composites them over the opaque colour wired to `scene`. The layer count is the `wxsl_peel_layers` macro on the pipeline config (1 to 8, default 4), and `wxsl_peel_native` selects the float-blend path (ADR 0047).

- in `depth: depth_target`: The opaque pass's depth. Fragments behind it are not peeled.
- in `scene: color_target`: The opaque colour, a `resource.color` the peel can sample. The frame's own target cannot be both sampled and presented.
- in `into: color_target` (optional): Where the composite is written. Unconnected means the frame's own target.
- out `color: color_target`: The opaque colour with the transparent layers composited over it.
- only in document graphs

#### `pass.refraction` · refraction

Gather optical interfaces and trace IOR refraction through the refractive tag. Clear glass, with HDR fallback (ADR 0067).

- in `depth: depth_target`
- in `scene: color_target`
- in `environment: color_target`: Linear radiance cube, normally pass.environment's radiance output.
- in `into: color_target` (optional)
- out `color: color_target`
- setting `surface_count` = `2`: Whole scene crossing budget (1..4), doubled for front/back. A torus consumes two crossings, a sphere one.
- setting `quality` = `layered`, one of layered, precise: layered: fast back-to-front composition; precise: opt-in expensive ray marching.
- only in document graphs

#### `pass.screen` · screen effect

A fullscreen pass over what earlier passes wrote. The effect is named by `effect`; what it reads is what is wired into it — a G-buffer for the deferred lighting pass, a colour target for a post effect, a storage buffer for an effect drawing what compute left. With `into` unconnected it writes the frame's own target.

- in `gbuffer: gbuffer` (optional): The G-buffer, for an effect that shades one.
- in `image: color_target` (optional): A colour input, for a post effect.
- in `buffer: storage_buffer` (optional): A storage buffer, for an effect that reads one as `var<storage>` — the buffer half of a compute pass's work arriving at the screen.
- in `into: color_target` (optional): Where to write. Unconnected means the frame's own target.
- out `color: color_target`: What the effect wrote — the frame's target when `into` is unconnected.
- setting `effect` = `wxsl.deferred_lighting`: Which screen effect the pass runs.
- setting `policy` = `per frame`, one of per frame, once, on resize, on demand: How often the pass runs: per frame, once, on resize or on demand. Only a pass whose target survives frames may skip.
- only in document graphs

#### `pass.screen.wxsl.bloom_combine` · bloom combine

Add a bloom pyramid's assembled glow back onto the image. Reads: `color` (The unblurred image, as linear radiance.), `glow` (The pyramid's assembled glow, at half this pass's resolution.).

- in `into: color_target` (optional): Where to write. Unconnected means the frame's own target.
- in `color: color_target`: The unblurred image, as linear radiance.
- in `glow: color_target`: The pyramid's assembled glow, at half this pass's resolution.
- out `color: color_target`: What the effect wrote — the frame's target when `into` is unconnected.
- setting `policy` = `per frame`: How often the pass runs: per frame, once, on resize or on demand. Only a pass whose target survives frames may skip.
- only in document graphs

#### `pass.screen.wxsl.bloom_up` · bloom up

Bilinear-upsample the level below and add this level onto it. Reads: `low` (The level below, at half this pass's resolution.), `high` (This level's own signal, from the descent.).

- in `into: color_target` (optional): Where to write. Unconnected means the frame's own target.
- in `low: color_target`: The level below, at half this pass's resolution.
- in `high: color_target`: This level's own signal, from the descent.
- out `color: color_target`: What the effect wrote — the frame's target when `into` is unconnected.
- setting `policy` = `per frame`: How often the pass runs: per frame, once, on resize or on demand. Only a pass whose target survives frames may skip.
- only in document graphs

#### `pass.screen.wxsl.bloom_y` · bloom vertical

Blur a horizontal bloom along y and add it back onto the image. Reads: `color` (The unblurred image, as linear radiance.), `glow` (The horizontal pass's bright signal.).

- in `into: color_target` (optional): Where to write. Unconnected means the frame's own target.
- in `color: color_target`: The unblurred image, as linear radiance.
- in `glow: color_target`: The horizontal pass's bright signal.
- out `color: color_target`: What the effect wrote — the frame's target when `into` is unconnected.
- setting `policy` = `per frame`: How often the pass runs: per frame, once, on resize or on demand. Only a pass whose target survives frames may skip.
- only in document graphs

#### `pass.screen.wxsl.dof_coc` · dof circles of confusion

Write the signed blur disc each pixel's depth asks for, in pixels. Reads: `depth` (The opaque depth the discs measure against.).

- in `into: color_target` (optional): Where to write. Unconnected means the frame's own target.
- in `depth: depth_target`: The opaque depth the discs measure against.
- out `color: color_target`: What the effect wrote — the frame's target when `into` is unconnected.
- setting `policy` = `per frame`: How often the pass runs: per frame, once, on resize or on demand. Only a pass whose target survives frames may skip.
- only in document graphs

#### `pass.screen.wxsl.dof_far` · dof far field

Blur the far field by its circle of confusion, occlusion-faded. Reads: `color` (The image, as linear radiance.), `coc` (The signed circles of confusion, from the CoC pass.).

- in `into: color_target` (optional): Where to write. Unconnected means the frame's own target.
- in `color: color_target`: The image, as linear radiance.
- in `coc: color_target`: The signed circles of confusion, from the CoC pass.
- out `color: color_target`: What the effect wrote — the frame's target when `into` is unconnected.
- setting `policy` = `per frame`: How often the pass runs: per frame, once, on resize or on demand. Only a pass whose target survives frames may skip.
- only in document graphs

#### `pass.screen.wxsl.dof_near` · dof near field

Composite the dilated, blurred foreground over the far result. Reads: `color` (The image, as linear radiance.), `coc` (The signed circles of confusion, from the CoC pass.), `far` (The far pass's blurred background.).

- in `into: color_target` (optional): Where to write. Unconnected means the frame's own target.
- in `color: color_target`: The image, as linear radiance.
- in `coc: color_target`: The signed circles of confusion, from the CoC pass.
- in `far: color_target`: The far pass's blurred background.
- out `color: color_target`: What the effect wrote — the frame's target when `into` is unconnected.
- setting `policy` = `per frame`: How often the pass runs: per frame, once, on resize or on demand. Only a pass whose target survives frames may skip.
- only in document graphs

#### `pass.screen.wxsl.environment_background` · Environment background

Camera rays into radiance cube on untouched opaque depth, before tonemap Reads: `color` (Linear opaque colour), `depth` (Opaque standard depth), `radiance` (Unconvolved radiance cube).

- in `into: color_target` (optional): Where to write. Unconnected means the frame's own target.
- in `color: color_target`: Linear opaque colour
- in `depth: depth_target`: Opaque standard depth
- in `radiance: color_target`: Unconvolved radiance cube
- out `color: color_target`: What the effect wrote — the frame's target when `into` is unconnected.
- setting `policy` = `per frame`: How often the pass runs: per frame, once, on resize or on demand. Only a pass whose target survives frames may skip.
- only in document graphs

#### `pass.screen.wxsl.motion_blur` · motion blur

Smear the frame along its own velocity buffer. Reads: `color` (The frame, as linear radiance.), `velocity` (The velocity stage's screen motion, uv per frame.).

- in `into: color_target` (optional): Where to write. Unconnected means the frame's own target.
- in `color: color_target`: The frame, as linear radiance.
- in `velocity: color_target`: The velocity stage's screen motion, uv per frame.
- out `color: color_target`: What the effect wrote — the frame's target when `into` is unconnected.
- setting `policy` = `per frame`: How often the pass runs: per frame, once, on resize or on demand. Only a pass whose target survives frames may skip.
- only in document graphs

#### `pass.screen.wxsl.refraction` · refraction

Trace clear glass through up to eight optical interfaces. Reads: `scene` (Optical layer or linear radiance.), `depth` (Opaque or peeled depth.), `environment` (Optical layer or linear radiance.), `normal_layers` (Optical layer or linear radiance.), `optical_layers` (Optical layer or linear radiance.).

- in `into: color_target` (optional): Where to write. Unconnected means the frame's own target.
- in `scene: color_target`: Optical layer or linear radiance.
- in `depth: depth_target`: Opaque or peeled depth.
- in `environment: color_target`: Optical layer or linear radiance.
- in `normal_layers: color_target`: Optical layer or linear radiance.
- in `optical_layers: color_target`: Optical layer or linear radiance.
- out `color: color_target`: What the effect wrote — the frame's target when `into` is unconnected.
- setting `policy` = `per frame`: How often the pass runs: per frame, once, on resize or on demand. Only a pass whose target survives frames may skip.
- only in document graphs

#### `pass.screen.wxsl.refraction_bounds` · refraction bounds

Advance the optical peel's near bound. Reads: `depth` (Opaque or peeled depth.), `previous` (Optical layer or linear radiance.).

- in `into: color_target` (optional): Where to write. Unconnected means the frame's own target.
- in `depth: depth_target`: Opaque or peeled depth.
- in `previous: color_target`: Optical layer or linear radiance.
- out `color: color_target`: What the effect wrote — the frame's target when `into` is unconnected.
- setting `policy` = `per frame`: How often the pass runs: per frame, once, on resize or on demand. Only a pass whose target survives frames may skip.
- only in document graphs

#### `pass.screen.wxsl.refraction_layer` · optical layer

Compose a front/back optical pair over the farther scene. Reads: `background` (Optical layer or linear radiance.), `background_depth` (Optical layer or linear radiance.), `depth` (Opaque or peeled depth.), `environment` (Optical layer or linear radiance.), `normal_layers` (Optical layer or linear radiance.), `optical_layers` (Optical layer or linear radiance.), `surface_layers` (Optical layer or linear radiance.).

- in `into: color_target` (optional): Where to write. Unconnected means the frame's own target.
- in `background: color_target`: Optical layer or linear radiance.
- in `background_depth: color_target`: Optical layer or linear radiance.
- in `depth: depth_target`: Opaque or peeled depth.
- in `environment: color_target`: Optical layer or linear radiance.
- in `normal_layers: color_target`: Optical layer or linear radiance.
- in `optical_layers: color_target`: Optical layer or linear radiance.
- in `surface_layers: color_target`: Optical layer or linear radiance.
- out `color: color_target`: What the effect wrote — the frame's target when `into` is unconnected.
- setting `policy` = `per frame`: How often the pass runs: per frame, once, on resize or on demand. Only a pass whose target survives frames may skip.
- only in document graphs

#### `pass.screen.wxsl.refraction_layer_depth` · optical layer depth

Update nearest composed depth for the next optical pair. Reads: `background_depth` (Optical layer or linear radiance.), `depth` (Opaque or peeled depth.), `normal_layers` (Optical layer or linear radiance.), `optical_layers` (Optical layer or linear radiance.).

- in `into: color_target` (optional): Where to write. Unconnected means the frame's own target.
- in `background_depth: color_target`: Optical layer or linear radiance.
- in `depth: depth_target`: Opaque or peeled depth.
- in `normal_layers: color_target`: Optical layer or linear radiance.
- in `optical_layers: color_target`: Optical layer or linear radiance.
- out `color: color_target`: What the effect wrote — the frame's target when `into` is unconnected.
- setting `policy` = `per frame`: How often the pass runs: per frame, once, on resize or on demand. Only a pass whose target survives frames may skip.
- only in document graphs

#### `pass.screen.wxsl.refraction_scene_depth` · optical scene depth

Copy opaque depth for optical composition. Reads: `depth` (Opaque or peeled depth.).

- in `into: color_target` (optional): Where to write. Unconnected means the frame's own target.
- in `depth: depth_target`: Opaque or peeled depth.
- out `color: color_target`: What the effect wrote — the frame's target when `into` is unconnected.
- setting `policy` = `per frame`: How often the pass runs: per frame, once, on resize or on demand. Only a pass whose target survives frames may skip.
- only in document graphs

#### `pass.screen.wxsl.taa` · TAA

Blend the frame with its reprojected history: antialias, at the cost of one ring. Reads: `color` (The frame, as linear radiance.), `velocity` (The velocity stage's screen motion, uv per frame.), `history` (Last frame's resolve — the same resource `into` names.).

- in `into: color_target` (optional): Where to write. Unconnected means the frame's own target.
- in `color: color_target`: The frame, as linear radiance.
- in `velocity: color_target`: The velocity stage's screen motion, uv per frame.
- in `history: color_target`: Last frame's resolve — the same resource `into` names.
- out `color: color_target`: What the effect wrote — the frame's target when `into` is unconnected.
- setting `policy` = `per frame`: How often the pass runs: per frame, once, on resize or on demand. Only a pass whose target survives frames may skip.
- only in document graphs

#### `pass.shadow` · shadow

One shadow pass per light slot, rasterizing into its own tile of the shadow atlas; the first pass clears the whole atlas on its way in. A slot whose light casts nothing this frame is cleared and left alone — the pass list is built once and scheduled against every frame.

- in `draws: draw_queue`: What to draw.
- in `into: shadow_maps`: The shadow maps to fill.
- only in document graphs

<a id="pipeline-resource"></a>

### resource

#### `resource.buffer` · storage buffer

A storage buffer: the thing a compute pass writes and any pass's shader reads back as `var<storage>`. Its slot is never shared — a buffer aliasing bug corrupts a whole block, not a frame region.

- out `buffer: storage_buffer`: The buffer, of `bytes` bytes.
- setting `bytes` = `1024`: The buffer's size, in bytes. A read-back buffer wants a multiple of 4.
- setting `history` = `0`: Previous frames kept readable: 0 transient, 1 ping-pong, n a ring of n+1.
- only in document graphs

#### `resource.color` · colour target

A colour target an effect chain writes and reads.

- out `color: color_target`: The target, sized from `scale`.
- setting `precision` = `standard`, one of standard, hdr, float, scalar, pair: standard (8-bit), hdr (half float), float (linear rgba32float import), scalar or pair.
- setting `size` = `viewport`: `viewport` (sized by `scale`, the default) or fixed `64x64` pixels — for the things whose size is a fact of their contents, a LUT most of all.
- setting `scale` = `1`: Fraction of the frame target's size: 1.0 full, 0.5 half.
- setting `history` = `0`: Previous frames kept readable: 0 transient, 1 ping-pong, n a ring of n+1.
- setting `imported` = `false`, one of false, true: `true` if the host owns the texture — a bake table the scene's materials created and sample. Its label must match the bake's, and the size settings say nothing.
- only in document graphs

#### `resource.depth` · depth target

A depth target. The first pass to draw into it clears it; a pass taking another pass's depth output tests against what is already there.

- out `depth: depth_target`: The target, sized from `scale`.
- setting `scale` = `1`: Fraction of the frame target's size: 1.0 full, 0.5 half.
- only in document graphs

#### `resource.gbuffer` · G-buffer

The G-buffer the enabled lighting set requests: one target per request, plus the depth the lighting pass reconstructs position from. The set is a knob of the pipeline's configuration, not of the document — widening it rewires nothing.

- out `gbuffer: gbuffer`: Every target, then depth — the order a screen pass reads them in.
- only in document graphs

<a id="pipeline-source"></a>

### source

#### `source.lights` · shadow maps

The frame's shadow atlas: one tile per light slot, packed by the ABI's tile table. Its shape is the frame group's, not a setting — the lighting pass reads it by that shape.

- out `shadows: shadow_maps`: The atlas the shadow passes fill, one tile per light.
- only in document graphs

#### `source.scene` · Scene

The frame's draw list, filtered by a tag expression.

- out `draws: draw_queue`: Everything the scene offers, filtered by `tags`.
- setting `tags` = `*`: Which instances this queue carries, as a tag expression: `*`, `opaque && !outlined`.
- only in document graphs

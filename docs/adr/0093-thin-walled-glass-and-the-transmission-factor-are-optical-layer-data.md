# 93. Thin-walled glass and the transmission factor are optical-layer data

Date: 2026-10-18

Status: Accepted

Extends [ADR 0068](0068-compose-optical-peels-back-to-front-by-default.md)
(the layered composition) and [ADR 0008](0008-surface-graphs-and-a-named-shader-abi.md)
(two surface fields).

## Context

`pass.refraction` models every optical surface as one side of a closed
solid. Consecutive peeled interfaces are paired as entry and exit, and the
side of the normal tells which is which. glTF's `KHR_materials_transmission`
without `KHR_materials_volume` means something else: a thin-walled
surface. It has a single interface, no bending, and the light passing
through is tinted by the base colour.

The sample car is built from such sheets: headlight covers, windows and
light glass. Imported as solids, they paired with unrelated sheets. Where
a cover's normal faced away from the viewer, it was skipped as an exit.
The importer also ignored `transmissionFactor` below 1 with a warning, and
the base colour's alpha (coverage) did not reach the glass.

## Decision

* **Two surface fields, default 1.**
  - `transmission` is the share of light that passes through. The rest is
    the glass's diffuse base colour, lit by the radiance cube's smallest
    mip.
  - `thickness`: 0 makes the surface thin-walled.
* **The refraction surface stage writes a third RGBA16F target:** base
  colour and transmission factor. With the normal/depth (RGBA32F) and
  optics (RGBA16F) targets, that is exactly WebGPU's baseline of 32 bytes
  per sample.
* **A thin layer is marked in the optics target.** Red is -1, green holds
  the coverage (the surface's `alpha`), and the IOR is stored as facing
  the viewer whichever side was drawn. A solid layer is unchanged.
* **The layered composition treats a thin front as one interface:**
  Fresnel reflection, then `behind × base colour` weighed by transmission,
  over its coverage. The layer in its pair slot is the next sheet, composed
  first when it is thin too. The transmission factor weighs every layer's
  transmitted light against its diffuse base colour.
* **The glTF importer** writes `transmission` from `transmissionFactor`,
  and `thickness` from the volume's `thicknessFactor`, or 0 when there is
  no volume. It no longer warns about a factor below 1.

## Alternatives considered

* **Packing the factor and coverage into the existing two targets.** RGBA16F
  cannot hold the IOR and the factor in one channel with enough precision,
  and the normal channels are read as a unit vector everywhere.
* **A fourth target.** It would exceed 32 bytes per sample.
* **Blending the glass's own forward shading over the composed image.**
  That needs a new blended pass and a depth order, for a share that is
  usually small.

## Consequences

* With every default at 1, solids compose exactly as before. The IOR = 1
  identity and the existing refraction tests are unchanged.
* **A thin sheet in front of a solid's entry, within the same pair, hides
  that solid.** The pair slot is then read as the next sheet. Separate the
  layers with `surface_count` if a scene mixes them closely.
* **The precise solver (`quality=precise`) traces a thin layer as a clear
  solid.** Only the layered composition models thin walls.
* **Coverage applies only to thin layers.** A solid's alpha is still
  ignored.
* The base colour now tints thin glass, as glTF specifies, so tinted
  windows in an asset look tinted.
* A pass that draws `refraction_surface` outside `pass.refraction` must
  attach three targets. The gallery's glass probes do.

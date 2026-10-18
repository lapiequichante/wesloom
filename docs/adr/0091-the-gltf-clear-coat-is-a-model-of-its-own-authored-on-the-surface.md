# 91. The glTF clear coat is a model of its own, authored on the surface

Date: 2026-10-18

Status: Accepted

Extends [ADR 0058](0058-iridescence-is-authored-on-the-surface-and-carried-by-a-model-channel.md)
(surface fields carried by a model channel) and
[ADR 0080](0080-gltf-import-produces-a-scene-document-with-generated-graphs.md)
(the importer's clearcoat mapping). Keeps `wxsl.clearcoat` unchanged.

## Context

`wxsl.clearcoat` derives its coat from the base roughness and fixes the
coat's roughness. A graph cannot author a coat, and the importer maps
`KHR_materials_clearcoat` onto that model with a warning that the file's
factors, textures and coat normal are ignored.

## Decision

* **Three surface fields**, appended after sheen: `clearcoat` (default 0),
  `clearcoat_roughness` (default 0) and `clearcoat_normal` (unconnected:
  the geometric normal, as glTF specifies when the coat has no normal
  texture). Old graphs keep their meaning.
* **A new model, `wxsl.gltf_clearcoat`** (id 9). Its model channel holds
  strength, roughness, and the coat normal encoded octahedrally with the
  G-buffer's own normal encoding, so the full glTF coat fits in one
  `vec4`.
* **The layering is the extension's.** The coat is a GGX lobe with Smith
  visibility, a fixed 4% reflectance, and its own roughness and normal.
  The base keeps `1 - strength * F(coat normal · view)` of the light. The
  model has its own ambient, which layers the same way over the default
  ambient: the environment maps when the frame has them, the analytic
  hemisphere when not.
* **The importer maps the whole extension**: both factors, the strength
  from the red of `clearcoatTexture` and the roughness from the green of
  `clearcoatRoughnessTexture`, and `clearcoatNormalTexture` with its
  scale. It no longer warns about clearcoat.
* `wxsl.clearcoat` stays as it was.

## Alternatives considered

* **Change `wxsl.clearcoat` itself.** One coat model instead of two, but
  materials that rely on the derived coat would lose it.
* **Leave the coat normal out.** That frees two channels, but the coat
  would then share the base's normal map, which glTF does not do.

## Consequences

* The model's ambient restates the default ambient. A model module cannot
  call the generated one, so a change to `default_ambient` must be made
  here too.
* The coat data is stored at half precision. That is enough for the
  octahedral normal, as it is for the base normal.
* The node reference gains the three sockets.

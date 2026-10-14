# Sample glTF models

Reference models for the glTF importer (`wxsl::asset::import_gltf`, plan6
G2) and the `viewer` example. They are unmodified files from the Khronos
[glTF-Sample-Assets](https://github.com/KhronosGroup/glTF-Sample-Assets)
repository at commit `edc7c9e67c639d230715049ee31f9a96a6babbbe`, under
`Models/<name>/`. Each folder keeps the upstream `LICENSE.md`, `README.md`
and `metadata.json`; those files are the authoritative terms. They are not
build inputs, and no crate packages them.

| Folder | Upstream variant | Credit | License |
|---|---|---|---|
| `DamagedHelmet/` | `glTF-Binary` | Rebuild and conversion to glTF © 2018 ctxwing; earlier version of the model © 2016 theblueturtle_ | CC-BY-4.0 **and** CC-BY-NC-4.0 |
| `CarConcept/` | `glTF-Binary` | Model and textures © 2024 Darmstadt Graphics Group GmbH, by Eric Chadwick; Khronos and 3D Commerce logos © Khronos Group | CC-BY-4.0; logos under `LicenseRef-LegalMark-Khronos.txt` |
| `GlamVelvetSofa/` | `glTF-Binary` | © 2021 Wayfair, LLC, by Eric Chadwick | CC-BY-4.0 |

The upstream metadata files (`LICENSE.md`, `README.md` and
`metadata.json`) are CC-BY-4.0, © The Khronos Group.

Notes:

- **DamagedHelmet is non-commercial.** The earlier version it derives from
  is CC-BY-NC-4.0, so the model may not be used commercially. That
  includes screenshots of it in commercial material.
- **The logos are trademarks.** The Khronos and 3D Commerce logos on
  CarConcept are covered by `LicenseRef-LegalMark-Khronos.txt` (fetched
  from the same commit's `LICENSES/`), not by CC-BY. `CarConcept/LICENSE.md`
  links to it as `../../LICENSES/…`, which is the upstream layout; here it
  sits beside this file.
- **Attribution.** CC-BY requires crediting the authors above, linking
  the license and saying whether anything changed (nothing has) wherever a
  model or a render of it is shared.
- **Not covered.** None of these files is under this repository's
  MIT/Apache-2.0 license.

# S2 / S3 / S4 — guide d’implémentation et banque de snippets

État : propositions, 2026-10-09. S1 est livré (ADR 0053), **pas les lots S2–S4**.
Les 50 blocs `wxsl` sont des prototypes compilables, pas une promesse de qualité
physique, de performance ou de disponibilité dans la palette. Seul
`lighting.sheen_ibl_response` est déjà livré. Les blocs Rust/pseudo-code servent
de recettes d’intégration ; les APIs proposées sont signalées explicitement.

## 1. Sources locales, revisions et licences

`refs/` est ignoré, non committé, sans sous-modules et jamais lu par un build.
Les SHA ci-dessous permettent de retrouver les sources sans disposer des clones.
Pas d’installation npm, pas d’exécution des scripts des références.

| Clone | Révision | Licence racine vérifiée | Usage prioritaire |
|---|---|---|---|
| `refs/three.js` | `8d486d4cc1a2b420b2585ea52a3e0f783c919282` | MIT | sheen, iridescence, tonemaps, ciel, blur/bloom |
| `refs/Babylon.js` | `7cc564f9f73eb7e1dad58933382b133e353c5e71` | Apache-2.0, `license.md` | WGSL PBR, anisotropie, features, matériaux procéduraux |
| `refs/filament` | `5f83f24898dbb573cc02afd99acf855774ba7d8a` | Apache-2.0, `LICENSE` | BRDF, conservation d’énergie, IBL, couleur |
| `refs/glTF-Sample-Renderer` | `0686eb22dc86ecfdd1e0fc93a141b50edac4a706` | Apache-2.0, `LICENSE.md` | contrats glTF, sheen/iridescence/transmission, préfiltrage |
| `refs/webgl-noise` | `6abed1e77ed1e18b181627c35f688eb30c9fe75e` | MIT, `LICENSE` | simplex, classic, cellular et bruit périodique |
| `refs/postprocessing` | `9cf03cff26615636a564d8bfddf765edb8917441` | Zlib, `LICENSE.md` | S4, technique uniquement tant que Zlib n’est pas approuvée |

Une licence racine ne couvre pas automatiquement les assets, `thirdparty/`,
les tables pré-calculées ou un morceau importé ailleurs. Vérifier chaque fichier
retenu et ses propres crédits. Exemples importants : les liens vers Filament
dans les chunks three.js, les notices tierces de Babylon et du renderer Khronos.
Le code propre au workspace reste MIT OR Apache-2.0 ; un port garde les
obligations de sa source. L’attribution seule ne suffit pas.

### Index de fragments amont à ouvrir

Chaque lien pointe sur la révision clonée. Chercher le symbole, pas un numéro
de ligne susceptible de changer. Le code complet est dans `refs/` : beaucoup
plus de matière que ce guide, sans copier des milliers de lignes dans la stdlib.

- [three : PBR physique](https://github.com/mrdoob/three.js/blob/8d486d4cc1a2b420b2585ea52a3e0f783c919282/src/renderers/shaders/ShaderChunk/lights_physical_pars_fragment.glsl.js) : `D_Charlie`, `V_Neubelt`, `IBLSheenBRDF`, `D_GGX_Anisotropic`, `V_GGX_SmithCorrelated_Anisotropic`, `computeMultiscattering`.
- [three : iridescence](https://github.com/mrdoob/three.js/blob/8d486d4cc1a2b420b2585ea52a3e0f783c919282/src/renderers/shaders/ShaderChunk/iridescence_fragment.glsl.js) : `Fresnel0ToIor`, `IorToFresnel0`, `evalIridescence`, sensibilité spectrale.
- [three : tonemaps](https://github.com/mrdoob/three.js/blob/8d486d4cc1a2b420b2585ea52a3e0f783c919282/src/renderers/shaders/ShaderChunk/tonemapping_pars_fragment.glsl.js) : ACES, AgX, Neutral ; conserver toutes les étapes et les crédits.
- [three : Sky](https://github.com/mrdoob/three.js/blob/8d486d4cc1a2b420b2585ea52a3e0f783c919282/examples/jsm/objects/Sky.js) : phases Rayleigh/Mie, densité optique et luminance solaire.
- [three : GaussianBlurNode](https://github.com/mrdoob/three.js/blob/8d486d4cc1a2b420b2585ea52a3e0f783c919282/examples/jsm/tsl/display/GaussianBlurNode.js) : poids, axe, taille cible ; comparer avec `HorizontalBlurShader.js` et `VerticalBlurShader.js`.
- [three : UnrealBloomPass](https://github.com/mrdoob/three.js/blob/8d486d4cc1a2b420b2585ea52a3e0f783c919282/examples/jsm/postprocessing/UnrealBloomPass.js) : niveaux, tailles, recombinaison ; également `BloomNode.js`.
- [three : BokehShader2](https://github.com/mrdoob/three.js/blob/8d486d4cc1a2b420b2585ea52a3e0f783c919282/examples/jsm/shaders/BokehShader2.js) : CoC, focus et occlusions, pas seulement un flou.
- [Babylon : BRDF WGSL](https://github.com/BabylonJS/Babylon.js/blob/7cc564f9f73eb7e1dad58933382b133e353c5e71/packages/dev/core/src/ShadersWGSL/ShadersInclude/pbrBRDFFunctions.fx) : Charlie, visibilité, LUT, réflectance ; ne pas recopier les `#ifdef` de l’engine.
- [Babylon : anisotropie](https://github.com/BabylonJS/Babylon.js/blob/7cc564f9f73eb7e1dad58933382b133e353c5e71/packages/dev/core/src/ShadersWGSL/ShadersInclude/pbrBlockAnisotropic.fx) : repère tangent, aspect, direction et environnement.
- [Babylon : sheen](https://github.com/BabylonJS/Babylon.js/blob/7cc564f9f73eb7e1dad58933382b133e353c5e71/packages/dev/core/src/ShadersWGSL/ShadersInclude/pbrBlockSheen.fx) : couche, intensité, conservation d’énergie.
- [Babylon : ciel](https://github.com/BabylonJS/Babylon.js/blob/7cc564f9f73eb7e1dad58933382b133e353c5e71/packages/dev/materials/src/sky/sky.fragment.fx) : atmosphère ; dossiers `grid/` et `fire/` pour les textures.
- [Filament : BRDF](https://github.com/google/filament/blob/5f83f24898dbb573cc02afd99acf855774ba7d8a/shaders/src/surface_brdf.fs) : distributions, visibilité et diffuse, selon le nom du fichier de cette révision.
- [Filament : IBL](https://github.com/google/filament/tree/5f83f24898dbb573cc02afd99acf855774ba7d8a/tools/cmgen/src) : convolutions, PDF, niveaux et importance sampling.
- [Filament : couleur/post](https://github.com/google/filament/tree/5f83f24898dbb573cc02afd99acf855774ba7d8a/filament/src) : `ColorGrading.cpp`, `PostProcessManager.cpp`, chaîne et coût des formats.
- [Khronos : BRDF](https://github.com/KhronosGroup/glTF-Sample-Renderer/blob/0686eb22dc86ecfdd1e0fc93a141b50edac4a706/source/Renderer/shaders/brdf.glsl) : `D_Charlie`, visibilité et modèle glTF.
- [Khronos : iridescence](https://github.com/KhronosGroup/glTF-Sample-Renderer/blob/0686eb22dc86ecfdd1e0fc93a141b50edac4a706/source/Renderer/shaders/iridescence.glsl) : `evalIridescence`, IOR, épaisseur et film mince.
- [Khronos : préfiltrage](https://github.com/KhronosGroup/glTF-Sample-Renderer/blob/0686eb22dc86ecfdd1e0fc93a141b50edac4a706/source/shaders/ibl_filtering.frag) : GGX/Charlie, Hammersley, PDF et solid angles.
- [webgl-noise : simplex 3D](https://github.com/ashima/webgl-noise/blob/6abed1e77ed1e18b181627c35f688eb30c9fe75e/src/noise3D.glsl) : permutation, normalisation, constantes ; garder les crédits Ashima/Gustavson.
- [webgl-noise : cellular 3D](https://github.com/ashima/webgl-noise/blob/6abed1e77ed1e18b181627c35f688eb30c9fe75e/src/cellular3D.glsl) : F1/F2 ; les variantes 2×2 sont des compromis, pas des équivalents exacts.
- [webgl-noise : périodique](https://github.com/ashima/webgl-noise/blob/6abed1e77ed1e18b181627c35f688eb30c9fe75e/src/psrdnoise2D.glsl) : périodes et dérivées analytiques.
- [postprocessing : kernels](https://github.com/pmndrs/postprocessing/tree/9cf03cff26615636a564d8bfddf765edb8917441/src/materials/glsl) : `convolution.gaussian`, `.box`, `.kawase`, `.downsampling`, `.upsampling`.
- [postprocessing : effets](https://github.com/pmndrs/postprocessing/tree/9cf03cff26615636a564d8bfddf765edb8917441/src/effects/glsl) : DOF, vignette, bruit et aberration ; références Zlib, pas des ports approuvés ici.
- [OKLab, source de la définition et des matrices](https://bottosson.github.io/posts/oklab/) : utiliser la définition mathématique, ou auditer son code MIT avant un port ; distinguer sRGB linéaire et sRGB encodé.

Commandes de recherche locales :

```sh
rg -n 'Charlie|Sheen|Anisotrop|Iridescen' refs/three.js/src/renderers/shaders/ShaderChunk
rg -n 'fn .*|Charlie|Sheen' refs/Babylon.js/packages/dev/core/src/ShadersWGSL/ShadersInclude/pbrBRDFFunctions.fx
rg -n 'pdf|Hammersley|roughness' refs/glTF-Sample-Renderer/source/shaders/ibl_filtering.frag
rg --files refs/webgl-noise/src
rg --files refs/postprocessing/src/materials/glsl
```

## 2. Contrat d’intégration, valable pour chaque lot

1. Choisir **un** petit résultat utile. Noter unités, domaine, conventions et
   source/licence avant de coder. Ne pas rebaptiser une approximation en modèle exact.
2. Ajouter `shaders/<cat>/<nom>.wxsl` et son entrée `MODULES` dans `src/shaders.rs`.
   `build.rs` dérive le node, ses types et ses defaults. Aucun descriptor Rust parallèle.
3. Le label est la première ligne ; le paragraphe suivant est la documentation
   palette. Crédits et décisions viennent **après** ce paragraphe.
4. Un knob runtime est `param.value`/un paramètre d’effet. Une borne structurelle
   est `@macro const`. Ne pas utiliser `@if` pour choisir une valeur runtime.
5. Les fonctions ci-dessous supposent des entrées finies. Les clamps traitent
   les domaines dégénérés ordinaires, pas NaN/Inf provenant d’un hôte corrompu.
6. Tout reste en radiance linéaire jusqu’à l’unique transform display terminal.
   Une courbe de tonemap n’est pas nécessairement un encodeur sRGB.
7. Modèles et effects sont data-driven. Ne pas créer une nouvelle branche de
   `PassKind`, un second registry, un nouveau planner C++ ou une stdlib renderer-aware.

### Gabarit de fichier pour un port

```text
// Label
//
// Un paragraphe : résultat, unités et domaine attendu.
//
// SPDX-License-Identifier: <licence vérifiée>
// Copyright <notice exacte>
// Ported from: <URL>
// Source: <chemin>
// Revision: <SHA complet>
// Symbol: <symbole amont>
// Changes: <différences de comportement et de syntaxe>
```

Pour Apache : licence complète, indication des changements et notices
applicables. Mettre à jour les `NOTICE` racine/crate et la formule SPDX du package
si nécessaire. Pour MIT/BSD : conserver les textes et copyrights requis, y
compris dans une application qui n’embarque que WGSL compilé.

### Intégration Rust : API existante

```rust
use wxsl_core::{abi, graph::{Graph, Node}, node::Value};
let registry = wxsl_stdlib::registry();
let mut graph = Graph::new("sheen preview");
let term = graph.add(Node::new("lighting.sheen_ibl_response")
    .with_param("n_dot_v", Value::F32(0.5))
    .with_param("roughness", Value::F32(0.6)));
let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
// Preview scalaire uniquement, pas une vraie couche sheen sur le PBR.
graph.wire(&registry, (term, "out"), (output, "roughness"))?;
graph.validate(&registry)?;
```

## 3. S2 — surfaces : lots, décisions et snippets

| Lot | Livrable | Fichiers concernés | Difficulté de raisonnement |
|---|---|---|---|
| S2-A | SDF 2D + opérateurs + couverture | `shaders/sdf`, `filter`, registry domains/stages | moyenne |
| S2-B | OKLab/OKLCh + curves tonemap | `shaders/color`, tonemap terminal | moyenne |
| S2-C | Charlie/Neubelt, anisotropic GGX | `shaders/lighting` | élevée |
| S2-D | modèles cloth/sheen, iridescence | `lighting/models`, core lighting/ABI/générateurs | élevée, ADR |

Ordre conseillé : S2-A/B donnent des nodes utiles sans modifier le G-buffer ;
S2-C établit les briques ; S2-D intègre la physique et la parité des deux paths.
Le seuil « plus de 100 nodes » n’est pas une preuve de couverture : compter les
fonctions réellement nouvelles et les usages, pas les opérateurs déjà présents.

### S2.01 — utiliser le port S1 livré

```wxsl
import package::lighting::sheen_ibl_response::sheen_ibl_response;
fn preview_sheen(nv: f32, roughness: f32, sheen: vec3f) -> vec3f {
    return sheen * sheen_ibl_response(nv, roughness);
}
```

La réponse hemisphérique est un facteur IBL, pas un remplacement du BRDF
ponctuel. Ne pas additionner cette preview au PBR sans budget d’énergie.

### S2.02 — distribution Charlie, prototype mathématique

Convention : roughness perceptuelle, alpha = roughness² ; pas la convention
de tous les engines. Implémentation originale de la formule, différente du
plancher fp16 amont. Vérifier numériquement l’intégrale `D(h)·N·H`.

```wxsl
fn charlie_distribution(nh: f32, roughness: f32) -> f32 {
    let alpha = max(clamp(roughness, 0.0, 1.0) * clamp(roughness, 0.0, 1.0), 0.001);
    let n = clamp(nh, 0.0, 1.0);
    return (2.0 + 1.0 / alpha) * pow(max(1.0 - n * n, 0.0), 0.5 / alpha)
        / 6.28318530718;
}
```

### S2.03 — visibilité de Neubelt

```wxsl
fn neubelt_visibility(nv: f32, nl: f32) -> f32 {
    let v = clamp(nv, 0.0, 1.0);
    let l = clamp(nl, 0.0, 1.0);
    return min(1.0, 0.25 / max(v + l - v * l, 0.000001));
}
```

À multiplier par D, sheen RGB, radiance incidente et `max(N·L,0)` dans le modèle.
La borne ci-dessus est un garde, pas une preuve de conservation d’énergie.

### S2.04 — anisotropic GGX en repère tangent

`half_tangent` doit être un half-vector normalisé exprimé dans T/B/N ; les
alphas sont les largeurs des deux axes, **pas** des roughness perceptuelles.

```wxsl
fn anisotropic_ggx(half_tangent: vec3f, alpha_x: f32, alpha_y: f32) -> f32 {
    let ax = max(alpha_x, 0.001);
    let ay = max(alpha_y, 0.001);
    let h = half_tangent;
    let q = h.x * h.x / (ax * ax) + h.y * h.y / (ay * ay) + h.z * h.z;
    return 1.0 / max(3.14159265359 * ax * ay * q * q, 0.0000001);
}
```

### S2.05 — conversion roughness / anisotropy

Convention de départ proposée : anisotropy ∈ [0,1), orientation séparée en
tours. L’anisotropie signée doit choisir explicitement quel axe est long.

```wxsl
fn anisotropic_widths(roughness: f32, anisotropy: f32) -> vec2f {
    let r = clamp(roughness, 0.0, 1.0);
    let alpha = max(r * r, 0.001);
    let aspect = sqrt(max(1.0 - 0.9 * clamp(anisotropy, 0.0, 1.0), 0.1));
    return vec2f(alpha / aspect, alpha * aspect);
}
```

À anisotropy = 0, comparer à `distribution_ggx`. Ajouter ensuite la visibilité
anisotrope corrélée ; D seul n’est pas un modèle complet.

### S2.06 — anti-aliasing de roughness, prototype fragment-only

```wxsl
fn roughness_aa(normal: vec3f, roughness: f32, strength: f32) -> f32 {
    let dx = dpdx(normal);
    let dy = dpdy(normal);
    let variance = max(dot(dx, dx) + dot(dy, dy), 0.0) * max(strength, 0.0);
    let r = clamp(roughness, 0.0, 1.0);
    return sqrt(clamp(r * r + variance, 0.0, 1.0));
}
```

**Bloc d’architecture à résoudre avant shipping** : un appel à dérivée est
fragment-only même si sa signature n’a aucun `SurfaceContext`. Pinner la node
du demo en Fragment ne suffit pas à protéger toute la palette. Ajouter la
contrainte de stage à la dérivation/analyse existante (ADR si nouveau contrat),
refuser les chemins vertex/compute et les usages en flux non uniforme.

### S2.07 — couverture d’un SDF

```wxsl
fn sdf_coverage(distance: f32, width: f32) -> f32 {
    let w = max(abs(width), 0.000001);
    return 1.0 - smoothstep(-w, w, distance);
}
```

Calculer `width = max(fwidth(distance), epsilon)` dans un appelant fragment.
Ne pas utiliser l’alpha transparent pour remplacer un alpha-discard de shadow.

### S2.08 — cercle 2D

```wxsl
fn sdf_circle(p: vec2f, radius: f32) -> f32 {
    return length(p) - max(radius, 0.0);
}
```

### S2.09 — rectangle arrondi 2D

```wxsl
fn sdf_rounded_box(p: vec2f, half_size: vec2f, radius: f32) -> f32 {
    let size = max(half_size, vec2f(0.0));
    let r = clamp(radius, 0.0, min(size.x, size.y));
    let q = abs(p) - size + vec2f(r);
    return length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - r;
}
```

### S2.10 — capsule 2D, segment dégénéré inclus

```wxsl
fn sdf_capsule(p: vec2f, a: vec2f, b: vec2f, radius: f32) -> f32 {
    let segment = b - a;
    let t = clamp(dot(p - a, segment) / max(dot(segment, segment), 0.000001), 0.0, 1.0);
    return length(p - a - segment * t) - max(radius, 0.0);
}
```

### S2.11 — union / intersection / soustraction

Trois fichiers/nodes lors du shipping. Les valeurs sont des distances signées,
mais un opérateur CSG ne garantit pas partout une distance euclidienne exacte.

```wxsl
fn sdf_union(a: f32, b: f32) -> f32 { return min(a, b); }
fn sdf_intersection(a: f32, b: f32) -> f32 { return max(a, b); }
fn sdf_subtract(a: f32, b: f32) -> f32 { return max(a, -b); }
```

### S2.12 — onion et arrondi

```wxsl
fn sdf_onion(distance: f32, thickness: f32) -> f32 {
    return abs(distance) - max(thickness, 0.0);
}
fn sdf_round(distance: f32, radius: f32) -> f32 {
    return distance - max(radius, 0.0);
}
```

### S2.13 — smooth intersection

Réutiliser le `smooth_union` livré dans la version finale. Cette forme autonome
permet d’expérimenter sans inventer un deuxième noyau de CSG.

```wxsl
fn sdf_smooth_intersection(a: f32, b: f32, radius: f32) -> f32 {
    let k = max(radius, 0.000001);
    let h = clamp(0.5 + 0.5 * (a - b) / k, 0.0, 1.0);
    return mix(b, a, h) + k * h * (1.0 - h);
}
```

### S2.14 — union chanfrein

```wxsl
fn sdf_chamfer_union(a: f32, b: f32, radius: f32) -> f32 {
    return min(min(a, b), (a + b - max(radius, 0.0)) * 0.70710678118);
}
```

### S2.15 — sRGB linéaire → OKLab

Coefficients de la définition OKLab de Björn Ottosson (matrices mises à jour
en 2021). Traduction mathématique originale par produits scalaires, **pas une
fonction trois.js**. Racine cubique signée pour les valeurs hors gamut ; ne pas
clampper arbitrairement le HDR avant une conversion de travail.

```wxsl
fn linear_to_oklab(rgb: vec3f) -> vec3f {
    let lms = vec3f(
        dot(rgb, vec3f(0.4122214708, 0.5363325363, 0.0514459929)),
        dot(rgb, vec3f(0.2119034982, 0.6806995451, 0.1073969566)),
        dot(rgb, vec3f(0.0883024619, 0.2817188376, 0.6299787005)));
    let root = sign(lms) * pow(abs(lms), vec3f(0.33333333333));
    return vec3f(
        dot(root, vec3f(0.2104542553, 0.7936177850, -0.0040720468)),
        dot(root, vec3f(1.9779984951, -2.4285922050, 0.4505937099)),
        dot(root, vec3f(0.0259040371, 0.7827717662, -0.8086757660)));
}
```

### S2.16 — OKLab → sRGB linéaire

```wxsl
fn oklab_to_linear(lab: vec3f) -> vec3f {
    let root = vec3f(
        lab.x + 0.3963377774 * lab.y + 0.2158037573 * lab.z,
        lab.x - 0.1055613458 * lab.y - 0.0638541728 * lab.z,
        lab.x - 0.0894841775 * lab.y - 1.2914855480 * lab.z);
    let cube = root * root * root;
    return vec3f(
        dot(cube, vec3f(4.0767416621, -3.3077115913, 0.2309699292)),
        dot(cube, vec3f(-1.2684380046, 2.6097574011, -0.3413193965)),
        dot(cube, vec3f(-0.0041960863, -0.7034186147, 1.7076147010)));
}
```

Tests : noir/blanc/primaires, gamut négatif, round-trip aléatoire avec erreur
relative et absolue ; précision attendue à fixer avant le test, pas après.

### S2.17 — OKLab ↔ OKLCh, teinte en tours

```wxsl
fn oklab_to_oklch(lab: vec3f) -> vec3f {
    let chroma = length(lab.yz);
    var hue = 0.0;
    if chroma > 0.000001 { hue = fract(atan2(lab.z, lab.y) / 6.28318530718 + 1.0); }
    return vec3f(lab.x, chroma, hue);
}
fn oklch_to_oklab(lch: vec3f) -> vec3f {
    let angle = lch.z * 6.28318530718;
    return vec3f(lch.x, max(lch.y, 0.0) * cos(angle), max(lch.y, 0.0) * sin(angle));
}
```

### S2.18 — Reinhard-Jodie, courbe seulement

Prototype de la formule ; les valeurs négatives sont rejetées ici. L’encodage
display reste une autre opération dans le **même terminal** de chaîne.

```wxsl
fn reinhard_jodie(rgb: vec3f) -> vec3f {
    let c = max(rgb, vec3f(0.0));
    let luminance = dot(c, vec3f(0.2126, 0.7152, 0.0722));
    let per_channel = c / (vec3f(1.0) + c);
    return mix(c / (1.0 + luminance), per_channel, per_channel);
}
```

ACES/AgX : utiliser les fragments amont complets, tracer les crédits, déclarer
l’espace d’entrée/sortie. Une rational curve approchant ACES n’est pas le
transform ACES complet. AgX implique des matrices/gamut, un domaine log,
un contraste et des looks ; ne pas baptiser une courbe arbitraire « AgX ».

Audit du 2026-10-10 : le chunk Three.js à la révision ci-dessus renvoie à
Filament (`filament/src/ToneMapper.cpp`, révision
`5f83f24898dbb573cc02afd99acf855774ba7d8a`, Apache-2.0). Filament renvoie au
générateur [EaryChow/AgX_LUT_Gen](https://github.com/EaryChow/AgX_LUT_Gen)
pour les matrices et au billet IOLITE pour le contraste/les looks. La licence
permissive de toute cette chaîne n'a pas été établie : aucune indication de
licence trouvée dans le fichier `AgXBaseRec2020.py` ou l'inventaire du dépôt
amont consultés. Les looks sont aussi décrits dans AgX-S2O3, mais cela ne
suffit pas à autoriser la copie du reste. Conserver les références comme
technique-only tant que l'audit ADR 0053 reste incomplet. Aucun port AgX livré.

### S2.19 — IOR et F0

```wxsl
fn ior_to_f0(transmitted: f32, incident: f32) -> f32 {
    let t = max(transmitted, 0.000001);
    let i = max(incident, 0.000001);
    let ratio = (t - i) / (t + i);
    return ratio * ratio;
}
fn f0_to_ior(f0: f32) -> f32 {
    let root = sqrt(clamp(f0, 0.0, 0.9999));
    return (1.0 + root) / (1.0 - root);
}
```

### S2.20 — film mince : phase, pas un faux BRDF iridescent

```wxsl
fn thin_film_phase(thickness_nm: f32, film_ior: f32, cos_inside: f32, wavelength_nm: f32) -> f32 {
    return 12.56637061436 * max(thickness_nm, 0.0) * max(film_ior, 0.0)
        * clamp(cos_inside, 0.0, 1.0) / max(wavelength_nm, 1.0);
}
```

Une implémentation réelle reprend la réfraction Snell, les amplitudes Fresnel
aux deux interfaces, les inversions de phase et l’intégration spectrale RGB
du fichier Khronos. Ne pas remplacer cela par trois cosinus colorés additionnés.
Tests : épaisseur nulle retrouve le matériau de base, incidences rasantes,
transition film-air, énergie et forward/deferred avec les mêmes données.

### S2-D : intégrer un modèle, pas seulement un node

- Partir de `lighting/models/pbr.wxsl` et `clearcoat.wxsl`, du contrat
  `DEFAULT_MODELS`, des `GBufferChannelRequest`/features existants.
- Décrire les canaux nécessaires (sheen RGB + roughness, direction tangentielle,
  anisotropy, film thickness/IOR) avant de coder les pack/unpack générés.
- Décider si sheen est un modèle cloth distinct ou une couche PBR. Un node
  scalaire ne peut pas à lui seul changer le light loop et le G-buffer.
- Une couche supplémentaire doit atténuer l’énergie de la couche de base ;
  l’IBL doit utiliser une LUT/préfiltration compatible avec sa distribution.
- Représenter les valeurs dans `MaterialConfig`/surface et les tables existantes,
  pas dans un descriptor C++ parallèle. ADR pour le nouveau contrat/canaux.
- Étendre les corpus générés, les contrôles de budget G-buffer, la galerie
  et la liste de parité Dawn. Actuellement, les modèles custom du C ABI et les
  resources hôte avancées de Dawn restent à implémenter avant certains demos.

## 4. S3 — monde, bruit, textures, IBL

| Lot | Première sortie utile | Dépendances |
|---|---|---|
| S3-A | bruits/procédures purs | S1, maths et corpus |
| S3-B | fog + phases + ciel analytique | reconstruction profondeur/repères |
| S3-C | ciel physique à single scattering | intégration optique et sampling |
| S3-D | equirect → cube → diffuse/specular | ADR bindings/layouts + support des deux backends |

### S3.01 — hash entier déterministe

Éviter le hash `sin()` pour des goldens stricts inter-backends. Ce mixer est
un prototype original de permutation/mélange, pas une garantie cryptographique.

```wxsl
fn hash_cell(p: vec3i) -> u32 {
    var h = bitcast<u32>(p.x) * 374761393u + bitcast<u32>(p.y) * 668265263u
        + bitcast<u32>(p.z) * 2246822519u;
    h = (h ^ (h >> 13u)) * 1274126177u;
    return h ^ (h >> 16u);
}
```

`vec3i` n’est pas un type de socket existant de cette stdlib : garder ceci comme
helper interne ou proposer un ADR/types avant d’en faire un node public.

### S3.02 — convertir un hash en [0,1)

```wxsl
fn hash_to_unit(hash: u32) -> f32 {
    return f32(hash >> 8u) / 16777216.0;
}
```

### S3.03 — Worley F1/F2 : prototype 3D 27-cellules

Version originale lisible, bruit jitter borné à **0.25 autour du centre**.
Les 27 cellules sont un compromis de voisinage pour F1/F2 : ne pas promettre
F2 exact pour tous points sans une borne géométrique/un voisinage adaptatif.
Comparer au code cellular de Gustavson, aux bords et au CPU brute-force.

```wxsl
fn worley_hash(p: vec3i) -> u32 {
    var h = bitcast<u32>(p.x) * 374761393u + bitcast<u32>(p.y) * 668265263u
        + bitcast<u32>(p.z) * 2246822519u;
    h = (h ^ (h >> 13u)) * 1274126177u;
    return h ^ (h >> 16u);
}
fn worley27(p: vec3f) -> vec2f {
    let cell = vec3i(floor(p));
    var first = 1000000.0;
    var second = 1000000.0;
    for (var z: i32 = -1; z <= 1; z = z + 1) {
        for (var y: i32 = -1; y <= 1; y = y + 1) {
            for (var x: i32 = -1; x <= 1; x = x + 1) {
                let c = cell + vec3i(x, y, z);
                let h = worley_hash(c);
                let random = vec3f(f32(h & 1023u), f32((h >> 10u) & 1023u), f32((h >> 20u) & 1023u)) / 1023.0;
                let feature = vec3f(c) + vec3f(0.5) + (random - vec3f(0.5)) * 0.5;
                let d = length(feature - p);
                second = min(second, max(first, d));
                first = min(first, d);
            }
        }
    }
    return vec2f(first, second);
}
```

### S3.04 — fBM sur le bruit livré

```wxsl
import package::generative::value_noise3::value_noise3;
@macro const GUIDE_OCTAVES: i32 = 5;
fn fbm_normalized(p: vec3f, gain: f32, lacunarity: f32) -> f32 {
    var point = p;
    var amplitude = 1.0;
    var sum = 0.0;
    var total = 0.0;
    for (var i: i32 = 0; i < GUIDE_OCTAVES; i = i + 1) {
        sum = sum + amplitude * value_noise3(point);
        total = total + amplitude;
        point = point * max(lacunarity, 1.0);
        amplitude = amplitude * clamp(gain, 0.0, 1.0);
    }
    return sum / max(total, 0.000001);
}
```

Ne pas livrer ce doublon de `fbm3` tel quel : utiliser le prototype pour décider
si normalisation/gain sont une extension compatible du node existant.

### S3.05 — domain warp

```wxsl
import package::generative::value_noise3::value_noise3;
fn domain_warp(p: vec3f, strength: f32) -> vec3f {
    let displacement = vec3f(value_noise3(p), value_noise3(p + vec3f(17.0, 9.0, 3.0)),
        value_noise3(p + vec3f(4.0, 23.0, 11.0)));
    return p + (displacement - vec3f(0.5)) * strength;
}
```

Simplex/tiled : porter `noise3D.glsl`/`psrdnoise2D.glsl` après audit des helpers
et copyrights ; valider les bornes et périodes sur coordonnées négatives.
`fract(p / period)` suivi d’un bruit ordinaire n’est **pas** un bruit seamless.

### S3.06 — checker analytique

```wxsl
fn checker(uv: vec2f, cells: vec2f) -> f32 {
    let cell = floor(uv * max(cells, vec2f(1.0)));
    return (cell.x + cell.y) - 2.0 * floor((cell.x + cell.y) * 0.5);
}
```

### S3.07 — grid, largeur en fraction de cellule

```wxsl
fn grid_mask(uv: vec2f, cells: vec2f, line_width: f32) -> f32 {
    let local = fract(uv * max(cells, vec2f(1.0)));
    let border = min(local, vec2f(1.0) - local);
    return 1.0 - step(clamp(line_width, 0.0, 0.5), min(border.x, border.y));
}
```

Ajouter ensuite une variante AA avec largeur fournie ou dérivées fragment-only.

### S3.08 — brick offset + joints

```wxsl
fn brick_mask(uv: vec2f, count: vec2f, mortar: f32) -> f32 {
    let p = uv * max(count, vec2f(1.0));
    let row = floor(p.y);
    let odd = row - 2.0 * floor(row * 0.5);
    let local = fract(vec2f(p.x + 0.5 * odd, p.y));
    let border = min(local, vec2f(1.0) - local);
    return step(clamp(mortar, 0.0, 0.5), min(border.x, border.y));
}
```

### S3.09 — motif Truchet, deux arcs par tuile

```wxsl
fn truchet_arcs(uv: vec2f, flip: bool, width: f32) -> f32 {
    var p = fract(uv);
    if flip { p.x = 1.0 - p.x; }
    let a = abs(length(p) - 0.5);
    let b = abs(length(p - vec2f(1.0)) - 0.5);
    return 1.0 - step(max(width, 0.0), min(a, b));
}
```

Un hash de cellule choisit `flip` ; sa distribution et son seed doivent être
reproductibles. Pas de sampling caché d’une texture engine.

### S3.10 — fog homogène / Beer-Lambert

```wxsl
fn fog_transmittance(distance_m: f32, extinction_per_m: f32) -> f32 {
    return exp(-max(distance_m, 0.0) * max(extinction_per_m, 0.0));
}
fn fog_composite(radiance: vec3f, inscattering: vec3f, transmission: f32) -> vec3f {
    let t = clamp(transmission, 0.0, 1.0);
    return radiance * t + inscattering * (1.0 - t);
}
```

La seconde formule suppose une source homogène déjà intégrée/équivalente ;
un scattering spatialement variable demande une intégration le long du rayon.

### S3.11 — height fog : intégrale analytique d’une densité exponentielle

Le signe de `height_delta` importe. Le prototype borne l’exponentielle ; les
hauteurs très négatives saturent la densité à cette borne numérique.

```wxsl
fn height_fog_optical_depth(start_height: f32, height_delta: f32, distance_m: f32,
    ground_density: f32, falloff_per_m: f32) -> f32 {
    let k = max(falloff_per_m, 0.0);
    let a = clamp(k * height_delta, -40.0, 40.0);
    var integral = 1.0;
    if abs(a) > 0.0001 { integral = (1.0 - exp(-a)) / a; }
    return max(ground_density, 0.0) * exp(clamp(-k * start_height, -40.0, 40.0))
        * max(distance_m, 0.0) * integral;
}
```

### S3.12 — phase Rayleigh

```wxsl
fn rayleigh_phase(cos_theta: f32) -> f32 {
    let mu = clamp(cos_theta, -1.0, 1.0);
    return 3.0 * (1.0 + mu * mu) / 50.2654824574;
}
```

### S3.13 — phase Henyey-Greenstein

```wxsl
fn hg_phase(cos_theta: f32, asymmetry: f32) -> f32 {
    let g = clamp(asymmetry, -0.99, 0.99);
    let mu = clamp(cos_theta, -1.0, 1.0);
    let d = max(1.0 + g * g - 2.0 * g * mu, 0.000001);
    return (1.0 - g * g) / (12.56637061436 * d * sqrt(d));
}
```

Définir si `mu` compare les directions de propagation ou les directions vers
la lumière/caméra ; une inversion produit un halo dans la mauvaise direction.

### S3.14 — intersection rayon/sphère atmosphérique

```wxsl
fn ray_sphere(origin: vec3f, direction: vec3f, radius: f32) -> vec2f {
    let a = max(dot(direction, direction), 0.000001);
    let b = dot(origin, direction);
    let c = dot(origin, origin) - max(radius, 0.0) * max(radius, 0.0);
    let discriminant = b * b - a * c;
    if discriminant < 0.0 { return vec2f(-1.0); }
    let root = sqrt(discriminant);
    return vec2f((-b - root) / a, (-b + root) / a);
}
```

### S3.15 — coordonnées equirectangulaires

Convention proposée : Y vers le haut, origine longitudinale +X ; la texture
détermine si V doit être inversé. Tester des couleurs cardinales, pas une HDR
symétrique qui masquerait une rotation/inversion de face.

```wxsl
fn direction_to_equirect(direction: vec3f) -> vec2f {
    let norm2 = dot(direction, direction);
    if norm2 < 0.000001 { return vec2f(0.5); }
    let d = direction * inverseSqrt(norm2);
    return vec2f(atan2(d.z, d.x) / 6.28318530718 + 0.5,
        0.5 - asin(clamp(d.y, -1.0, 1.0)) / 3.14159265359);
}
```

### S3.16 — séquence Hammersley

```wxsl
fn hammersley(index: u32, count: u32) -> vec2f {
    return vec2f(f32(index) / f32(max(count, 1u)), f32(reverseBits(index)) * 2.32830643654e-10);
}
```

### S3.17 — échantillon hémisphère cosine-weighted

```wxsl
fn cosine_hemisphere(xi: vec2f) -> vec3f {
    let u = clamp(xi.x, 0.0, 1.0);
    let phi = xi.y * 6.28318530718;
    return vec3f(sqrt(u) * cos(phi), sqrt(u) * sin(phi), sqrt(1.0 - u));
}
```

L’irradiance diffuse est `PI * mean(L)` pour ce sampling ; le diffuse Lambert
redonne `albedo * mean(L)`. Ne pas multiplier par PI deux fois.

### S3.18 — importance sampling GGX, repère tangent

```wxsl
fn sample_ggx_half(xi: vec2f, perceptual_roughness: f32) -> vec3f {
    let r = clamp(perceptual_roughness, 0.0, 1.0);
    let alpha = max(r * r, 0.001);
    let a2 = alpha * alpha;
    let u = clamp(xi.y, 0.0, 0.999999);
    let cosine = sqrt((1.0 - u) / max(1.0 + (a2 - 1.0) * u, 0.000001));
    let sine = sqrt(max(1.0 - cosine * cosine, 0.0));
    let phi = xi.x * 6.28318530718;
    return vec3f(sine * cos(phi), sine * sin(phi), cosine);
}
```

Transform H vers le monde, construire L par réflexion, utiliser
`pdf_L = D(H)*N·H/(4*V·H)` et le solid angle pour le mip source. Le cube préfiltré
doit contenir des mips par roughness, pas seulement un `generateMipmap`.

### S3-C/D : plan d’intégration réel

1. Ciel single scattering : rayons planète/atmosphère, densités Rayleigh/Mie,
   extinction RGB, rayon solaire, integration à pas fixes via macro, halo et
   occultation terrestre. Tests jour/nuit, horizon, altitude, caméra extérieure.
2. Ne pas baptiser une interpolation de deux couleurs « modèle physique ».
   La phase seule ne calcule ni profondeur optique ni multiple scattering.
3. IBL : host texture HDR → six faces → irradiance diffuse + GGX specular/mips
   → LUT BRDF. L’hôte possède les textures ; policies Once/OnDemand sur stable
   storage, invalidation quand la source change.
4. ADR avant ajout des bindings environnement : tables `abi`/`host` et WXSL
   générés, tailles/slices/mips dans des descriptions neutres. Pas de mirror C++.
5. Le graphe sait déjà nommer cube/array, mais vérifier **les writes, views,
   mip ranges, sampling et imports** réellement implémentés dans les deux recorders.
   Décrire les extensions nécessaires, pas prétendre qu’un enum suffit.
6. Étendre la FFI pour les effets/modèles custom si ces shaders en ont besoin ;
   étendre le host Dawn pour les imports/textures HDR avant la parité de ce demo.
7. Tests physiques : environnement blanc constant reste constant après
   préfiltration, orientations cardinales, coutures des faces, roughness 0/1,
   energy split diffuse/specular, GPU captures aux deux backends.

## 5. S4 — screen graphs et pyramide

Ordre : petits effets purs → kernels mono-image → chaîne multi-résolution →
bloom pyramidal → DOF. SSR reste reporté à N7 et à la stabilité TAA.

### S4.01 — vignette multiplicative, radiance linéaire

```wxsl
fn vignette(uv: vec2f, aspect: f32, radius: f32, softness: f32) -> f32 {
    let p = (uv - vec2f(0.5)) * vec2f(max(aspect, 0.000001), 1.0);
    let r = max(radius, 0.0);
    return 1.0 - smoothstep(r, r + max(softness, 0.000001), length(p));
}
```

### S4.02 — bruit de grain déterministe, seed explicite

```wxsl
fn grain_hash(pixel: vec2u, frame: u32) -> f32 {
    var h = pixel.x * 374761393u + pixel.y * 668265263u + frame * 2246822519u;
    h = (h ^ (h >> 13u)) * 1274126177u;
    h = h ^ (h >> 16u);
    return f32(h >> 8u) / 16777216.0 - 0.5;
}
```

Le seed doit venir de l’hôte/doc pour les captures. Pour du grain perceptuel
post-curve, l’intégrer au transform terminal : ne pas introduire un deuxième
encodeur/display pass. Un grain pré-curve a une réponse dépendante de l’exposition.

### S4.03 — seuil doux de bloom

```wxsl
fn bloom_extract(radiance: vec3f, threshold: f32, knee: f32) -> vec3f {
    let c = max(radiance, vec3f(0.0));
    let brightness = max(c.x, max(c.y, c.z));
    let k = max(knee, 0.000001);
    let soft = clamp(brightness - threshold + k, 0.0, 2.0 * k);
    let contribution = max(brightness - threshold, soft * soft / (4.0 * k));
    return c * contribution / max(brightness, 0.000001);
}
```

Tests : seuil positif sur noir donne zéro ; seuil haut retire le glow, énergie
finie ; vérifier spatialement le highlight, pas seulement une image moyenne.

### S4.04 — box blur mono-axe, sample explicite

```wxsl
@macro const GUIDE_BOX_RADIUS: i32 = 3;
fn box_blur(image: texture_2d<f32>, filtering: sampler, uv: vec2f, texel: vec2f, axis: vec2f) -> vec3f {
    var sum = vec3f(0.0);
    var count = 0.0;
    for (var i: i32 = -GUIDE_BOX_RADIUS; i <= GUIDE_BOX_RADIUS; i = i + 1) {
        sum = sum + textureSampleLevel(image, filtering, uv + axis * texel * f32(i), 0.0).rgb;
        count = count + 1.0;
    }
    return sum / max(count, 1.0);
}
```

Le screen ABI actuel expose une image, **pas un sampler de pass**. Ce prototype
est utilisable via texture/sampler explicites ; pour un screen graph existant,
préférer `sample.load_2d` (textureLoad) et contrôler le clamp des coordonnées.
Ne pas ajouter un sampler matériel group 1 pour l’image de pass group 3.

### S4.05 — Gaussian blur mono-axe

```wxsl
@macro const GUIDE_GAUSSIAN_RADIUS: i32 = 5;
fn gaussian_blur(image: texture_2d<f32>, filtering: sampler, uv: vec2f,
    texel: vec2f, axis: vec2f, sigma: f32) -> vec3f {
    var sum = vec3f(0.0);
    var weight_sum = 0.0;
    let s = max(sigma, 0.0001);
    for (var i: i32 = -GUIDE_GAUSSIAN_RADIUS; i <= GUIDE_GAUSSIAN_RADIUS; i = i + 1) {
        let x = f32(i);
        let weight = exp(-0.5 * x * x / (s * s));
        sum = sum + weight * textureSampleLevel(image, filtering, uv + axis * texel * x, 0.0).rgb;
        weight_sum = weight_sum + weight;
    }
    return sum / max(weight_sum, 0.000001);
}
```

Pour shipping : poids générés ou pré-calculés, puis pairing bilinéaire si le
contrat sampler permet l’optimisation. Rayon ≥ 0 validé à la compilation,
sigma = 0 devrait avoir un comportement identité choisi explicitement.

### S4.06 — Kawase quatre taps

```wxsl
fn kawase_blur(image: texture_2d<f32>, filtering: sampler, uv: vec2f, texel: vec2f, offset: f32) -> vec3f {
    let d = texel * max(offset + 0.5, 0.0);
    let a = textureSampleLevel(image, filtering, uv + vec2f(-d.x, -d.y), 0.0).rgb;
    let b = textureSampleLevel(image, filtering, uv + vec2f( d.x, -d.y), 0.0).rgb;
    let c = textureSampleLevel(image, filtering, uv + vec2f(-d.x,  d.y), 0.0).rgb;
    let e = textureSampleLevel(image, filtering, uv + vec2f( d.x,  d.y), 0.0).rgb;
    return (a + b + c + e) * 0.25;
}
```

### S4.07 — downsample 2×, texelFetch exact

```wxsl
fn downsample2(image: texture_2d<f32>, pixel: vec2i) -> vec3f {
    let size = vec2i(textureDimensions(image));
    let p = pixel * 2;
    let top = max(size - vec2i(1), vec2i(0));
    let a = textureLoad(image, clamp(p, vec2i(0), top), 0).rgb;
    let b = textureLoad(image, clamp(p + vec2i(1, 0), vec2i(0), top), 0).rgb;
    let c = textureLoad(image, clamp(p + vec2i(0, 1), vec2i(0), top), 0).rgb;
    let d = textureLoad(image, clamp(p + vec2i(1, 1), vec2i(0), top), 0).rgb;
    return (a + b + c + d) * 0.25;
}
```

`vec2i` n’est pas actuellement un socket : helper interne, ou entrée UV convertie
dans la fonction publique. Tester les tailles impaires et 1×1.

### S4.08 — combinaison bloom, deux images

```wxsl
fn bloom_composite(base: vec3f, glow: vec3f, strength: f32) -> vec3f {
    return base + glow * max(strength, 0.0);
}
```

La fonction est pure, mais **le pass** doit lire deux resources distinctes.
Réutiliser la dérivation `pass.screen.<effect>` des effets multi-inputs existants,
pas une nouvelle palette. Préserver la radiance, pas de clamp à 1 avant tonemap.

### S4.09 — aberration chromatique radiale

```wxsl
fn chromatic_aberration(image: texture_2d<f32>, filtering: sampler, uv: vec2f, strength: f32) -> vec3f {
    let delta = (uv - vec2f(0.5)) * strength;
    let r = textureSampleLevel(image, filtering, uv + delta, 0.0).r;
    let g = textureSampleLevel(image, filtering, uv, 0.0).g;
    let b = textureSampleLevel(image, filtering, uv - delta, 0.0).b;
    return vec3f(r, g, b);
}
```

Définir strength en UV ou pixels, pas alternativement selon la résolution.
Clamp-to-edge proposé ; wrap produit une couture visible.

### S4.10 — CoC signé, caméra thin-lens

Échelles SI (mètres). Résultat en pixels. `depth_m` est une profondeur caméra
linéarisée positive, **pas** la valeur du depth buffer.

```wxsl
fn circle_of_confusion(depth_m: f32, focus_m: f32, focal_m: f32,
    f_number: f32, sensor_width_m: f32, viewport_width_px: f32) -> f32 {
    let focal = max(focal_m, 0.000001);
    let focus = max(focus_m, focal + 0.000001);
    let depth = max(depth_m, 0.000001);
    let aperture = focal / max(f_number, 0.0001);
    let sensor_coc = aperture * focal * (depth - focus) / (depth * (focus - focal));
    return sensor_coc * max(viewport_width_px, 1.0) / max(sensor_width_m, 0.000001);
}
```

### S4.11 — linéarisation depth WebGPU, projection perspective standard

```wxsl
fn linearize_depth(depth: f32, near_m: f32, far_m: f32) -> f32 {
    let n = max(near_m, 0.000001);
    let f = max(far_m, n + 0.000001);
    return n * f / max(f - clamp(depth, 0.0, 1.0) * (f - n), 0.000001);
}
```

Suppose NDC depth [0,1], near→0, far→1, non reversed-Z et far fini. Pour une
autre projection, reconstruire avec l’inverse réelle de la caméra. DOF nécessite
des passes distincts near/far, dilatation du CoC near et recombinaison tenant
compte de l’occlusion ; un blur variable seul fuit au travers des silhouettes.

### S4.12 — disque de sampling déterministe

```wxsl
fn disk_sample(index: u32, count: u32) -> vec2f {
    let radius = sqrt((f32(index) + 0.5) / f32(max(count, 1u)));
    let angle = f32(index) * 2.39996322973;
    return vec2f(cos(angle), sin(angle)) * radius;
}
```

Contract index < count. Ajouter un angle seed par pixel si nécessaire ; figer
le seed dans les fixtures. La distribution des taps ne résout pas l’occlusion.

### Pyramide : frontières et synthèse à concevoir

```text
scene HDR ── extract/downsample ── level0 ── down ── level1 ── down ── level2
                                      ▲                ▲               │
                                      └──── up/add ─────┴─── up/add ─────┘
scene HDR + level0 reconstruit ── tonemap/display unique ── target
```

- Première version explicite : une ressource et un pass par niveau dans le
  document existant. Tailles `max(1, ceil(size/2))`, niveau maximal borné.
- Ensuite seulement : ADR pour un helper/sous-document qui synthétise cette
  répétition dans **le même modèle Graph**. IDs, labels, paramètres et erreurs
  doivent rester stables ; aucune mini-sérialisation d’effets cachée.
- Vérifier le vocabulaire `Extent`/resource color : nommer des tailles par
  niveau n’est pas la même chose que supposer que le preset sait déjà les régler.
- La synthèse/validation reste Rust et doit traverser la FFI. Les deux backends
  consomment le même plan développé et les mêmes physical slots.
- Aucun pass n’échantillonne l’attachement qu’il écrit. Les reads doivent être
  visibles au scheduler, y compris les images basse résolution de l’upsample.
- Le niveau le plus bas a une durée de vie transitoire, pas une history ring.
  Reallocation au resize invalide les outputs stables dépendants.
- Tests CPU : structure, ordre et aliasing, tailles impaires, paramètres et
  labels ; GPU : impulse response, DC constant, conservation des poids, absence
  de bandes, bords, HDR > 1 et parité à plusieurs résolutions.

## 6. Validation, preuves et ordre de livraison

Les snippets `wxsl` de ce document sont vérifiables indépendamment :

```sh
cargo run -p wxsl-ffi --example check_shader_guide -- docs/s2-s3-s4-implementation-guide.md
cargo test -p wxsl-stdlib
cargo test -p wxsl --test graph_to_wgsl
cargo test -p wxsl-stdlib --test lighting_models
cargo test -p wxsl-frame
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Le vérificateur passe le vrai compilateur WXSL puis le parseur/validateur WGSL
Naga, sans device. Cela valide syntaxe/types, pas les résultats physiques, les
stages réellement utilisés, les inputs hôte ou la performance. Chaque prototype
livré exige des tests numériques et une capture dans sa chaîne réelle.

Pour un nouveau node : defaults et cas limites, node dérivé, reachability,
compilation tous stages compatibles, test du refus des stages incompatibles.
Pour un nouveau modèle : corpus du générateur, budgets/canaux, light sets mixtes,
modèle non activé refusé par nom, comparaisons forward/deferred.
Pour un effet : bindings/entry points/params, knob modifié sans compilation,
chaîne HDR puis tonemap, dimensions et sampling au bord.

```sh
bash dawn/tests/run_parity.sh
# Ajouter les nouveaux demos à dawn/tests/scenes.txt, sans relâcher la tolérance.
# Si le host Dawn refuse une resource, implémenter ce support avant de déclarer la parité acquise.
```

### Tickets concrets, dans l’ordre

1. S2-A : cercle/box/capsule + CSG + fixtures numériques/SDF AA.
2. Contrat fragment-only pour les dérivées ; shader checks et diagnostic clair.
3. S2-B : OKLab/OKLCh, round-trip + gamut ; tonemap curves séparées de l’encodage.
4. S2-C : Charlie/Neubelt et GGX anisotrope, intégrales/limites/isotropic fallback.
5. ADR modèle cloth/sheen + données Surface/G-buffer ; gallery et parité GPU.
6. Iridescence fidèle à la physique et à la provenance amont, pas un effet RGB cosmétique.
7. S3-A : simplex/periodic/cellular, hash entier et textures analytiques.
8. S3-B/C : fog + ciel, métriques optiques et captures jour/nuit/horizon.
9. ADR IBL ; formats/mips/imports hôte + FFI et Dawn ; prefilter/LUT puis gallery.
10. S4 : vignette/aberration et kernels, respect de l’unique display transform.
11. ADR synthèse pyramide ; down/up/combine explicites puis helper public.
12. DOF near/far avec contrôle d’occlusion ; SSR reste reporté.

Ne pas faire de ces douze tickets un refactor global. Livrer une brique et sa
preuve à la fois. Le guide n’autorise pas à contourner les ADRs restantes ni à
marquer S2/S3/S4 terminés après avoir uniquement copié les fonctions ci-dessus.

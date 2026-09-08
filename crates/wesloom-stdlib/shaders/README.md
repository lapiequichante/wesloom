# Layout

Categories mirror the shape common to granular shader libraries (LYGIA
among them) because that shape works, not because this is a port of any of
them:

```
shaders/
  animation/
  color/
  distort/
  filter/
  generative/
  lighting/
  math/
  sample/
  sdf/
  space/
```

## Authoring rule

Every function in this crate is **original code**. It's fine — encouraged,
even — to look at how other libraries and engines solve a problem
(LYGIA's category breakdown, [Babylon.js](https://github.com/BabylonJS/Babylon.js)'s
shader techniques, papers, blog posts) for the *idea*: what the function
should do, what a fast approximation looks like, what edge cases matter.
It is not fine to transcribe or lightly rename someone else's
implementation — that's a derivative work regardless of the source
license, and defeats the point of writing this from scratch (see
`docs/adr/0007-original-shader-stdlib-instead-of-a-lygia-port.md`). If a
function was written with a specific external reference in mind for the
*technique* (not the code), a one-line comment naming the reference is
good practice; it is not a substitute for the implementation being your
own.

None have been written yet — this directory is scaffolding.

// Signed distance to a sphere centred on the origin.
//
// Negative inside, zero on the surface, and an exact Euclidean distance
// outside — so it can be used directly as a ray-march step length.
fn sdf_sphere(p: vec3f, radius: f32) -> f32 {
    return length(p) - radius;
}

// The paint's flakes: a tangent-space normal map, one flake a texel, each
// leaning a random way, uniformly within `tilt` of straight out. Its mips
// are the renderer's for a normal map (ADR 0094, `normal_mip_chain`): RGB
// the mean direction of the full-size normals a texel covers, alpha
// `√(2/κ)`, the GGX alpha their spread adds. `full` writes level 0;
// `reduce` writes each level after it from the one before, carrying the
// unnormalised means in `means`.

struct Level {
    // This level's side, texels.
    size: u32,
    // 1 for the first reduction, whose children are the flakes themselves.
    level: u32,
    tilt: f32,
    _pad: u32,
}

@group(0) @binding(0) var<uniform> params: Level;
@group(0) @binding(1) var texels: texture_storage_2d<rgba8unorm, write>;
// The level before's means, and this level's.
@group(0) @binding(2) var<storage, read> finer: array<vec4f>;
@group(0) @binding(3) var<storage, read_write> means: array<vec4f>;

// Texel `at`'s flake, a unit normal.
fn flake(at: vec2u) -> vec3f {
    var h = (at.x * 0x8da6b343u) ^ (at.y * 0xd8163841u);
    var random: array<f32, 2>;
    for (var i = 0; i < 2; i++) {
        h ^= h >> 13u;
        h *= 0x5bd1e995u;
        h ^= h >> 15u;
        random[i] = f32(h & 0x00ffffffu) / 16777216.0;
    }
    let radius = params.tilt * sqrt(random[0]);
    let angle = 6.2831853 * random[1];
    let lean = radius * vec2f(cos(angle), sin(angle));
    return vec3f(lean, sqrt(1.0 - dot(lean, lean)));
}

fn encode(mean: vec3f, spread: f32) -> vec4f {
    let r = length(mean);
    let direction = select(vec3f(0.0, 0.0, 1.0), mean / r, r > 1e-6);
    return vec4f(direction * 0.5 + 0.5, spread);
}

@compute @workgroup_size(8, 8)
fn full(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= vec2u(params.size))) {
        return;
    }
    textureStore(texels, id.xy, encode(flake(id.xy), 0.0));
}

@compute @workgroup_size(8, 8)
fn reduce(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= vec2u(params.size))) {
        return;
    }
    // A mean of the four means below is the mean of the whole footprint.
    var sum = vec3f(0.0);
    for (var i = 0u; i < 4u; i++) {
        let child = id.xy * 2u + vec2u(i & 1u, i >> 1u);
        if (params.level == 1u) {
            sum += flake(child);
        } else {
            sum += finer[child.y * params.size * 2u + child.x].xyz;
        }
    }
    let mean = sum * 0.25;
    means[id.y * params.size + id.x] = vec4f(mean, 0.0);
    let r = max(length(mean), 1e-4);
    let spread = select(min(sqrt(2.0 * (1.0 - r * r) / (r * (3.0 - r * r))), 1.0), 0.0, r >= 1.0);
    textureStore(texels, id.xy, encode(mean, spread));
}

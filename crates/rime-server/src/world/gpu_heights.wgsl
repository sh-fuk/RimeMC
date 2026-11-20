// GPU version of world::generator::{hash2, value_noise, column_height}.
// Integer-only, so results are bit-identical to the CPU implementation.

struct Params {
    seed: u32,
    count: u32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> coords: array<vec2<i32>>;
@group(0) @binding(2) var<storage, read_write> heights: array<i32>;

fn hash2(x: i32, z: i32, seed: u32) -> u32 {
    var h: u32 = (bitcast<u32>(x) * 0x27d4eb2du) ^ (bitcast<u32>(z) * 0x165667b1u) ^ seed;
    h = h ^ (h >> 15u);
    h = h * 0x2c1b3c6du;
    h = h ^ (h >> 12u);
    h = h * 0x297a2d39u;
    h = h ^ (h >> 15u);
    return h;
}

fn smooth_fp(t: i32) -> i32 {
    return (t * t * (768 - 2 * t)) >> 16u;
}

fn lattice(x: i32, z: i32, seed: u32) -> i32 {
    return i32(hash2(x, z, seed) & 0xFFFFu) - 32768;
}

fn value_noise(x: i32, z: i32, shift: u32, seed: u32) -> i32 {
    let ix = x >> shift;
    let iz = z >> shift;
    let mask = (1i << shift) - 1i;
    let fx = smooth_fp(((x & mask) << 8u) >> shift);
    let fz = smooth_fp(((z & mask) << 8u) >> shift);
    let v00 = lattice(ix, iz, seed);
    let v10 = lattice(ix + 1, iz, seed);
    let v01 = lattice(ix, iz + 1, seed);
    let v11 = lattice(ix + 1, iz + 1, seed);
    let a = v00 + (((v10 - v00) * fx) >> 8u);
    let b = v01 + (((v11 - v01) * fx) >> 8u);
    return a + (((b - a) * fz) >> 8u);
}

fn column_height(x: i32, z: i32, seed: u32) -> i32 {
    let continent = (value_noise(x, z, 9u, seed ^ 0xA5A5A5A5u) * 36) >> 15u;
    let hills = (value_noise(x, z, 7u, seed) * 22) >> 15u;
    let mid = (value_noise(x, z, 5u, seed + 1u) * 8) >> 15u;
    let detail = (value_noise(x, z, 3u, seed + 2u) * 3) >> 15u;
    return clamp(64 + continent + hills + mid + detail, -56, 250);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let index = gid.x + gid.y * nwg.x * 256u;
    let chunk = index / 256u;
    if (chunk >= params.count) {
        return;
    }
    let i = index % 256u;
    let c = coords[chunk];
    let x = c.x * 16 + i32(i % 16u);
    let z = c.y * 16 + i32(i / 16u);
    heights[index] = column_height(x, z, params.seed);
}

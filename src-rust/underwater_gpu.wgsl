// Exact byte-domain AquaVision application after stitch/I-Log quantization.
// This is an integer tetrahedral ILUT, not a trilinear CUBE texture.
struct Params { size: vec2<u32>, padding: vec2<u32> };
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read_write> pixels: array<u32>;
@group(0) @binding(2) var<storage, read> taps: array<vec4<u32>>;
@group(0) @binding(3) var<storage, read_write> analysis: array<u32>;
@group(0) @binding(4) var<storage, read> lut: array<u32>;

fn rgb(value: u32) -> vec3<i32> {
    return vec3<i32>(i32(value & 255u), i32((value >> 8u) & 255u), i32((value >> 16u) & 255u));
}

fn pack(value: vec3<i32>) -> u32 {
    let bytes = vec3<u32>(clamp(value, vec3<i32>(0), vec3<i32>(255)));
    return bytes.x | (bytes.y << 8u) | (bytes.z << 16u);
}

fn resized(index: u32) -> u32 {
    let xt = taps[index % 224u];
    let yt = taps[224u + index / 224u];
    let top = rgb(pixels[yt.x * params.size.x + xt.x]) * i32(xt.z)
        + rgb(pixels[yt.x * params.size.x + xt.y]) * i32(xt.w);
    let bottom = rgb(pixels[yt.y * params.size.x + xt.x]) * i32(xt.z)
        + rgb(pixels[yt.y * params.size.x + xt.y]) * i32(xt.w);
    return pack((top * i32(yt.z) + bottom * i32(yt.w) + vec3<i32>(1 << 21)) >> vec3<u32>(22u));
}

// Four RGB pixels occupy exactly three words. 224² is divisible by four.
@compute @workgroup_size(64)
fn downsample_rgb24(@builtin(global_invocation_id) id: vec3<u32>) {
    let first = id.x * 4u;
    if first >= 224u * 224u { return; }
    let a = resized(first);
    let b = resized(first + 1u);
    let c = resized(first + 2u);
    let d = resized(first + 3u);
    let output = id.x * 3u;
    analysis[output] = a | ((b & 255u) << 24u);
    analysis[output + 1u] = (b >> 8u) | ((c & 65535u) << 16u);
    analysis[output + 2u] = (c >> 16u) | (d << 8u);
}

fn restored(input: u32) -> u32 {
    let color = vec3<u32>(rgb(input));
    let base = color >> vec3<u32>(4u);
    let fraction = vec3<i32>(color & vec3<u32>(15u));
    let origin = (base.x * 17u + base.y) * 17u + base.z;
    var first: u32;
    var second: u32;
    var weights: vec3<i32>;
    if fraction.x > fraction.y {
        if fraction.y > fraction.z {
            first = 289u; second = 306u; weights = fraction.xyz;
        } else if fraction.x > fraction.z {
            first = 289u; second = 290u; weights = fraction.xzy;
        } else {
            first = 1u; second = 290u; weights = fraction.zxy;
        }
    } else if fraction.x > fraction.z {
        first = 17u; second = 306u; weights = fraction.yxz;
    } else if fraction.y > fraction.z {
        first = 17u; second = 18u; weights = fraction.yzx;
    } else {
        first = 1u; second = 18u; weights = fraction.zyx;
    }
    let a = rgb(lut[origin]);
    let b = rgb(lut[origin + first]);
    let c = rgb(lut[origin + second]);
    let d = rgb(lut[origin + 307u]);
    let delta = (b - a) * weights.x + (c - b) * weights.y + (d - c) * weights.z;
    // Signed division truncates toward zero before adding the origin.
    return pack(a + delta / vec3<i32>(16)) | (input & 0xff000000u);
}

@compute @workgroup_size(16, 8)
fn apply_lut(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= params.size) { return; }
    let index = id.y * params.size.x + id.x;
    pixels[index] = restored(pixels[index]);
}

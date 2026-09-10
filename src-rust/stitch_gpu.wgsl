struct FrameParams {
    output_size: vec2<u32>,
    _padding0: vec2<u32>,
    output_to_camera: vec4<f32>,
    feather_dead_zone: vec4<f32>,
    lut_domain_min_size: vec4<f32>,
    lut_domain_scale: vec4<f32>,
};

struct LensParams {
    orientation: vec4<f32>,
    intrinsics: vec4<f32>,
    native: vec4<f32>,
    source: vec4<f32>,
    model_meta: vec4<u32>,
    geometry: vec4<f32>,
    coefficients0: vec4<f32>,
    coefficients1: vec4<f32>,
    coefficients2: vec4<f32>,
    coefficients3: vec4<f32>,
    compatibility: vec4<f32>,
    readout: vec4<f32>,
};

struct PreparedMasks {
    offsets: vec4<u32>,
    weights: array<f32>,
};

struct ProjectedSample {
    color: vec3<f32>,
    source: vec2<f32>,
    detail_weight: f32,
    illumination_weight: f32,
    valid: u32,
};

@group(0) @binding(0) var<uniform> frame: FrameParams;
@group(0) @binding(1) var<storage, read> lenses: array<LensParams>;
@group(0) @binding(2) var<storage, read> masks: PreparedMasks;
@group(0) @binding(3) var<storage, read_write> slopes: array<vec4<f32>>;
@group(0) @binding(4) var first_texture: texture_2d<f32>;
@group(0) @binding(5) var second_texture: texture_2d<f32>;
@group(0) @binding(6) var source_sampler: sampler;
@group(0) @binding(7) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(8) var<storage, read_write> second_stats: array<vec4<f32>>;
@group(0) @binding(9) var first_y_texture: texture_2d<f32>;
@group(0) @binding(10) var first_u_texture: texture_2d<f32>;
@group(0) @binding(11) var first_v_texture: texture_2d<f32>;
@group(0) @binding(12) var second_y_texture: texture_2d<f32>;
@group(0) @binding(13) var second_u_texture: texture_2d<f32>;
@group(0) @binding(14) var second_v_texture: texture_2d<f32>;
@group(0) @binding(15) var<storage, read_write> yuv_output: array<u32>;
@group(0) @binding(16) var<storage, read> color_lut: array<vec4<f32>>;
@group(0) @binding(17) var<storage, read> readout_poses: array<vec4<f32>>;

const PI: f32 = 3.14159265358979323846;
const SHARPNESS: f32 = 5.2;

fn finite2(value: vec2<f32>) -> bool {
    return all(value == value) && all(abs(value) < vec2<f32>(3.0e38));
}

fn rotate_vector(quaternion: vec4<f32>, value: vec3<f32>) -> vec3<f32> {
    let imaginary = quaternion.yzw;
    let first = cross(imaginary, value);
    let second = cross(imaginary, first);
    return value + 2.0 * (quaternion.x * first + second);
}

fn sharpen_alpha(value: f32) -> f32 {
    if value <= 0.5 {
        return 0.5 * pow(2.0 * value, SHARPNESS);
    }
    return 1.0 - 0.5 * pow(2.0 * (1.0 - value), SHARPNESS);
}

fn overlap_alpha(angle: f32, belt: f32) -> f32 {
    let distance_from_seam = 0.5 * PI - angle;
    if belt <= 1e-6 {
        if distance_from_seam > 1e-6 {
            return 1.0;
        }
        if distance_from_seam < -1e-6 {
            return 0.0;
        }
        return 0.5;
    }
    return clamp(distance_from_seam / belt + 0.5, 0.0, 1.0);
}

fn coefficient(lens: LensParams, index: u32) -> f32 {
    switch index {
        case 0u: { return lens.coefficients0.x; }
        case 1u: { return lens.coefficients0.y; }
        case 2u: { return lens.coefficients0.z; }
        case 3u: { return lens.coefficients0.w; }
        case 4u: { return lens.coefficients1.x; }
        case 5u: { return lens.coefficients1.y; }
        case 6u: { return lens.coefficients1.z; }
        case 7u: { return lens.coefficients1.w; }
        case 8u: { return lens.coefficients2.x; }
        case 9u: { return lens.coefficients2.y; }
        case 10u: { return lens.coefficients2.z; }
        case 11u: { return lens.coefficients2.w; }
        case 12u: { return lens.coefficients3.x; }
        default: { return 0.0; }
    }
}

fn project_omni(lens: LensParams, ray: vec3<f32>, pro: bool) -> vec3<f32> {
    let ray_norm = length(ray);
    if ray_norm <= 1e-8 {
        return vec3<f32>(0.0, 0.0, 0.0);
    }
    let denominator = ray.z + lens.native.x * ray_norm;
    if abs(denominator) <= 1e-8 {
        return vec3<f32>(0.0, 0.0, 0.0);
    }
    let position = ray.xy / denominator;
    let x2 = position.x * position.x;
    let y2 = position.y * position.y;
    let xy = position.x * position.y;
    let r2 = x2 + y2;
    let r4 = r2 * r2;
    let r6 = r4 * r2;
    var distorted: vec2<f32>;
    if pro {
        let r8 = r6 * r2;
        let r10 = r8 * r2;
        let radial = 1.0 + coefficient(lens, 0u) * r2
            + coefficient(lens, 1u) * r4
            + coefficient(lens, 2u) * r6
            + coefficient(lens, 3u) * r8
            + coefficient(lens, 4u) * r10;
        let p1 = coefficient(lens, 5u);
        let p2 = coefficient(lens, 6u);
        let p3 = coefficient(lens, 7u);
        let p4 = coefficient(lens, 8u);
        distorted = vec2<f32>(
            position.x * radial + (p1 + r2 * p3) * (r2 + 2.0 * x2)
                + 2.0 * (p2 + r2 * p4) * xy + coefficient(lens, 9u) * r2
                + coefficient(lens, 11u) * r4,
            position.y * radial + (p2 + r2 * p4) * (r2 + 2.0 * y2)
                + 2.0 * (p1 + r2 * p3) * xy + coefficient(lens, 10u) * r2
                + coefficient(lens, 12u) * r4,
        );
    } else {
        let radial_delta = coefficient(lens, 0u) * r2
            + coefficient(lens, 1u) * r4 + coefficient(lens, 2u) * r6;
        let p1 = coefficient(lens, 3u);
        let p2 = coefficient(lens, 4u);
        distorted = vec2<f32>(
            position.x + position.x * radial_delta + 2.0 * p1 * xy
                + p2 * (r2 + 2.0 * x2),
            position.y + position.y * radial_delta + 2.0 * p2 * xy
                + p1 * (r2 + 2.0 * y2),
        );
    }
    return vec3<f32>(lens.intrinsics.xy + distorted * lens.intrinsics.zw, 1.0);
}

fn project_polynomial(lens: LensParams, ray: vec3<f32>) -> vec3<f32> {
    let radial_length = length(ray.xy);
    let theta = atan2(radial_length, ray.z);
    if radial_length <= 1e-8 {
        return vec3<f32>(lens.intrinsics.xy, 1.0);
    }
    let theta2 = theta * theta;
    let distorted_theta = theta * (coefficient(lens, 0u)
        + coefficient(lens, 1u) * theta
        + coefficient(lens, 2u) * theta2
        + coefficient(lens, 3u) * theta2 * theta);
    return vec3<f32>(
        lens.intrinsics.x + lens.intrinsics.z * distorted_theta * ray.x / radial_length,
        lens.intrinsics.y + lens.intrinsics.w * distorted_theta * ray.y / radial_length,
        1.0,
    );
}

fn project_equidistant(lens: LensParams, ray: vec3<f32>) -> vec3<f32> {
    let radial_length = length(ray.xy);
    let theta = acos(clamp(ray.z, -1.0, 1.0));
    let theta2 = theta * theta;
    let distorted_theta = theta * (1.0 + lens.compatibility.x * theta2
        + lens.compatibility.y * theta2 * theta2
        + lens.compatibility.z * theta2 * theta2 * theta2);
    if distorted_theta < 0.0 {
        return vec3<f32>(0.0, 0.0, 0.0);
    }
    if radial_length <= 1e-8 {
        return vec3<f32>(lens.intrinsics.xy, 1.0);
    }
    return vec3<f32>(
        lens.intrinsics.x + lens.intrinsics.z * distorted_theta * ray.x / radial_length,
        lens.intrinsics.y - lens.intrinsics.w * distorted_theta * ray.y / radial_length,
        1.0,
    );
}

fn camera_ray_within_fov(lens: LensParams, ray: vec3<f32>) -> bool {
    if lens.model_meta.y == 0u { return true; }
    let ray_norm = length(ray);
    let half_fov = lens.geometry.x;
    if ray_norm <= 1e-8 || half_fov < 0.0 { return false; }
    if lens.model_meta.x == 1u { return atan2(length(ray.xy), ray.z) < half_fov; }
    return ray.z / ray_norm >= cos(half_fov) - 0.01;
}

fn project_camera(lens: LensParams, ray: vec3<f32>, clip: bool) -> vec3<f32> {
    if clip && !camera_ray_within_fov(lens, ray) { return vec3<f32>(0.0); }
    if lens.model_meta.y == 0u {
        return project_equidistant(lens, ray);
    }
    switch lens.model_meta.x {
        case 1u: { return project_polynomial(lens, ray); }
        case 2u: { return project_omni(lens, ray, false); }
        case 3u: { return project_omni(lens, ray, true); }
        default: { return vec3<f32>(0.0, 0.0, 0.0); }
    }
}

fn yuv_to_rgb(yuv: vec3<f32>, metadata: u32) -> vec3<f32> {
    let full_range = (metadata & 1u) != 0u;
    let matrix = (metadata >> 1u) & 3u;
    var y = yuv.x;
    var cb = yuv.y - (128.0 / 255.0);
    var cr = yuv.z - (128.0 / 255.0);
    if !full_range {
        y = (y * 255.0 - 16.0) / 219.0;
        cb = (yuv.y * 255.0 - 128.0) / 224.0;
        cr = (yuv.z * 255.0 - 128.0) / 224.0;
    }
    var color: vec3<f32>;
    if matrix == 0u {
        color = vec3<f32>(
            y + 1.402 * cr,
            y - 0.344136 * cb - 0.714136 * cr,
            y + 1.772 * cb,
        );
    } else if matrix == 2u {
        color = vec3<f32>(
            y + 1.4746 * cr,
            y - 0.164553 * cb - 0.571353 * cr,
            y + 1.8814 * cb,
        );
    } else {
        color = vec3<f32>(
            y + 1.5748 * cr,
            y - 0.187324 * cb - 0.468124 * cr,
            y + 1.8556 * cb,
        );
    }
    return clamp(color, vec3<f32>(0.0), vec3<f32>(1.0));
}

fn sample_yuv420(lens_index: u32, source: vec2<f32>) -> vec3<f32> {
    let lens = lenses[lens_index];
    let dimensions = lens.source.xy;
    let metadata = u32(round(lens.source.w));
    let chroma_centered = ((metadata >> 3u) & 1u) != 0u;
    let luma_coordinate = (source + vec2<f32>(0.5)) / dimensions;
    var chroma_source = source * 0.5;
    if chroma_centered {
        chroma_source.x = (source.x - 0.5) * 0.5;
    }
    chroma_source.y = (source.y - 0.5) * 0.5;
    let chroma_dimensions = ceil(dimensions * 0.5);
    let chroma_coordinate = (chroma_source + vec2<f32>(0.5)) / chroma_dimensions;
    var yuv: vec3<f32>;
    if lens_index == 0u {
        yuv = vec3<f32>(
            textureSampleLevel(first_y_texture, source_sampler, luma_coordinate, 0.0).r,
            textureSampleLevel(first_u_texture, source_sampler, chroma_coordinate, 0.0).r,
            textureSampleLevel(first_v_texture, source_sampler, chroma_coordinate, 0.0).r,
        );
    } else {
        yuv = vec3<f32>(
            textureSampleLevel(second_y_texture, source_sampler, luma_coordinate, 0.0).r,
            textureSampleLevel(second_u_texture, source_sampler, chroma_coordinate, 0.0).r,
            textureSampleLevel(second_v_texture, source_sampler, chroma_coordinate, 0.0).r,
        );
    }
    return yuv_to_rgb(yuv, metadata);
}

fn sample_source_unmasked(lens_index: u32, source: vec2<f32>) -> vec3<f32> {
    let lens = lenses[lens_index];
    let dimensions = lens.source.xy;
    if lens.source.z >= 0.5 {
        return sample_yuv420(lens_index, source);
    }
    let coordinate = (source + vec2<f32>(0.5)) / dimensions;
    if lens_index == 0u {
        return textureSampleLevel(first_texture, source_sampler, coordinate, 0.0).rgb;
    }
    return textureSampleLevel(second_texture, source_sampler, coordinate, 0.0).rgb;
}

// Retain partially supported bilinear footprints without admitting masked RGB
// taps. Feather alpha remains a separate interpolated distance-field weight.
fn sample_source(lens_index: u32, source: vec2<f32>) -> vec3<f32> {
    if masks.offsets[lens_index] == 0xffffffffu { return sample_source_unmasked(lens_index, source); }
    let low = floor(source);
    let high = ceil(source);
    let fraction = source - low;
    let positions = array<vec2<f32>, 4>(low, vec2<f32>(high.x, low.y), vec2<f32>(low.x, high.y), high);
    let weights = array<f32, 4>((1.0-fraction.x)*(1.0-fraction.y), fraction.x*(1.0-fraction.y),
        (1.0-fraction.x)*fraction.y, fraction.x*fraction.y);
    var color = vec3<f32>(0.0);
    var support = 0.0;
    for (var tap = 0u; tap < 4u; tap += 1u) {
        if weights[tap] > 0.0 && mask_pixel_weight(lens_index, vec2<u32>(positions[tap])) > 0.0 {
            color += sample_source_unmasked(lens_index, positions[tap]) * weights[tap];
            support += weights[tap];
        }
    }
    if support > 0.0 { return color / support; }
    return vec3<f32>(0.0);
}

fn mask_pixel_weight(lens_index: u32, source: vec2<u32>) -> f32 {
    return masks.weights[masks.offsets[lens_index] + source.y * u32(lenses[lens_index].source.x) + source.x];
}

fn mask_weight(lens_index: u32, source: vec2<f32>) -> f32 {
    if masks.offsets[lens_index] == 0xffffffffu { return 1.0; }
    let maximum = lenses[lens_index].source.xy - vec2<f32>(1.0);
    if !finite2(source) || any(source < vec2<f32>(0.0)) || any(source > maximum) { return 0.0; }
    let low = vec2<u32>(floor(source));
    let high = vec2<u32>(ceil(source));
    let fraction = source - floor(source);
    return mix(mix(mask_pixel_weight(lens_index, low), mask_pixel_weight(lens_index, vec2<u32>(high.x, low.y)), fraction.x),
        mix(mask_pixel_weight(lens_index, vec2<u32>(low.x, high.y)), mask_pixel_weight(lens_index, high), fraction.x), fraction.y);
}

// Same bounded, shortest-arc interpolation as ReadoutPoseTable. Tables resolve
// winding before upload and use at most 129 uniformly spaced poses per lens.
fn readout_rotation(lens_index: u32, source: vec2<f32>) -> vec4<f32> {
    let lens = lenses[lens_index];
    let scan = u32(lens.readout.y);
    var coordinate = (source.y + 0.5) / lens.source.y;
    if scan >= 2u { coordinate = (source.x + 0.5) / lens.source.x; }
    var fraction = mix(lens.readout.z, lens.readout.w, clamp(coordinate, 0.0, 1.0));
    if scan == 1u || scan == 3u { fraction = 1.0 - fraction; }
    let position = clamp(fraction, 0.0, 1.0) * (lens.readout.x - 1.0);
    let index = min(u32(floor(position)), u32(lens.readout.x) - 2u);
    let amount = position - f32(index);
    let first = readout_poses[lens_index * 129u + index];
    var second = readout_poses[lens_index * 129u + index + 1u];
    var cosine = dot(first, second);
    if cosine < 0.0 { second = -second; cosine = -cosine; }
    if cosine > 0.9995 { return normalize(mix(first, second, amount)); }
    let angle = acos(clamp(cosine, -1.0, 1.0));
    return normalize((sin((1.0 - amount) * angle) * first + sin(amount * angle) * second) / sin(angle));
}

fn project_sample(lens_index: u32, direction: vec3<f32>) -> ProjectedSample {
    let lens = lenses[lens_index];
    let lens_canvas_width = lens.native.z * 0.5;
    let lens_origin = lens_canvas_width * f32(lens.model_meta.z);
    var local = vec3<f32>(0.0);
    var source = vec2<f32>(0.0);
    var previous_source = vec2<f32>(0.0);
    var capture_direction = direction;
    if lens.readout.x >= 2.0 {
        capture_direction = rotate_vector(readout_rotation(lens_index, lens.source.xy * 0.5 - vec2<f32>(0.5)), direction);
    }
    var converged = false;
    for (var iteration = 0u; iteration < 8u; iteration += 1u) {
        local = rotate_vector(lens.orientation, capture_direction);
        let projected = project_camera(lens, local, lens.readout.x < 2.0);
        if projected.z < 0.5 || !finite2(projected.xy) {
            return ProjectedSample(vec3<f32>(0.0), vec2<f32>(0.0), 0.0, 0.0, 0u);
        }
        source = vec2<f32>(
            (projected.x - lens_origin) * lens.source.x / lens_canvas_width,
            projected.y * lens.source.y / lens.native.w,
        );
        if !finite2(source) { break; }
        if lens.readout.x < 2.0 || (iteration > 0u && all(abs(source - previous_source) <= vec2<f32>(0.05))) {
            converged = true;
            break;
        }
        previous_source = source;
        capture_direction = rotate_vector(readout_rotation(lens_index, source), direction);
    }
    if !converged || !camera_ray_within_fov(lens, local) || source.x < 0.0 || source.y < 0.0
        || source.x > lens.source.x - 1.0 || source.y > lens.source.y - 1.0 {
        return ProjectedSample(vec3<f32>(0.0), source, 0.0, 0.0, 0u);
    }
    var detail_weight = 1.0;
    var illumination_weight = 1.0;
    if lens.model_meta.y != 0u {
        let blend_angle = lens.geometry.y;
        let half_fov = lens.geometry.x;
        if blend_angle < 0.0 || half_fov < 0.0 {
            return ProjectedSample(vec3<f32>(0.0), vec2<f32>(0.0), 0.0, 0.0, 0u);
        }
        let angle = acos(clamp(local.z / max(length(local), 1e-8), -1.0, 1.0));
        let detail_alpha = overlap_alpha(angle, blend_angle - PI);
        let illumination_alpha = overlap_alpha(angle, 2.0 * half_fov - PI);
        detail_weight = sharpen_alpha(detail_alpha);
        illumination_weight = illumination_alpha;
    }

    if lens.model_meta.y == 0u {
        let edge = min(min(source.x, source.y),
            min(lens.source.x - 1.0 - source.x, lens.source.y - 1.0 - source.y));
        let feather_pixels = min(lens.source.x, lens.source.y) * frame.feather_dead_zone.x;
        var edge_weight = 1.0;
        if feather_pixels > 1e-8 {
            let value = clamp(edge / feather_pixels, 0.0, 1.0);
            edge_weight = value * value * (3.0 - 2.0 * value);
        }
        detail_weight = edge_weight;
        illumination_weight = edge_weight;
    } else {
        let validity = mask_weight(lens_index, source);
        detail_weight *= validity;
        illumination_weight *= validity;
    }
    if detail_weight <= 0.0 && illumination_weight <= 0.0 {
        return ProjectedSample(vec3<f32>(0.0), source, 0.0, 0.0, 0u);
    }
    return ProjectedSample(
        sample_source(lens_index, source), source,
        detail_weight, illumination_weight, 1u,
    );
}

fn panorama_direction(column: u32, row: u32) -> vec3<f32> {
    let longitude = (f32(column) + 0.5) / f32(frame.output_size.x) * 2.0 * PI;
    let latitude = 0.5 * PI - (f32(row) + 0.5) / f32(frame.output_size.y) * PI;
    let latitude_cosine = cos(latitude);
    let panorama_ray = vec3<f32>(
        latitude_cosine * cos(longitude),
        -latitude_cosine * sin(longitude),
        sin(latitude),
    );
    return panorama_ray;
}

fn maximum_channel_difference(first: vec3<f32>, second: vec3<f32>) -> f32 {
    let difference = abs(first - second);
    return max(difference.x, max(difference.y, difference.z));
}

struct ColorMeans {
    first: vec3<f32>,
    second: vec3<f32>,
};

fn first_window_means(column: u32) -> ColorMeans {
    let width = frame.output_size.x;
    let window_width = max(width / 5u, 1u);
    let half_window = window_width / 2u;
    var first_sum = vec3<f32>(0.0);
    var second_sum = vec3<f32>(0.0);
    var count = 0.0;
    for (var offset = 0u; offset < window_width; offset += 1u) {
        let source = (column + width + offset - half_window) % width;
        let first = slopes[source * 2u];
        let second = slopes[source * 2u + 1u];
        first_sum += first.xyz;
        second_sum += second.xyz;
        count += first.w;
    }
    if count > 0.0 {
        first_sum /= count;
        second_sum /= count;
    }
    return ColorMeans(first_sum, second_sum);
}

fn second_window_means(column: u32) -> ColorMeans {
    let width = frame.output_size.x;
    let window_width = max(width / 5u, 1u);
    let half_window = window_width / 2u;
    var first_sum = vec3<f32>(0.0);
    var second_sum = vec3<f32>(0.0);
    var count = 0.0;
    for (var offset = 0u; offset < window_width; offset += 1u) {
        let source = (column + width + offset - half_window) % width;
        let first = second_stats[source * 2u];
        let second = second_stats[source * 2u + 1u];
        first_sum += first.xyz;
        second_sum += second.xyz;
        count += first.w;
    }
    if count > 0.0 {
        first_sum /= count;
        second_sum /= count;
    }
    return ColorMeans(first_sum, second_sum);
}

@compute @workgroup_size(64, 1, 1)
fn radiometry_first(@builtin(global_invocation_id) id: vec3<u32>) {
    let column = id.x;
    if column >= frame.output_size.x {
        return;
    }
    if frame.feather_dead_zone.z < 0.5 {
        slopes[column * 2u] = vec4<f32>(0.0);
        slopes[column * 2u + 1u] = vec4<f32>(0.0);
        return;
    }

    var first_sum = vec3<f32>(0.0);
    var second_sum = vec3<f32>(0.0);
    var count = 0.0;
    for (var row = 0u; row < frame.output_size.y; row += 8u) {
        // Statistics use a sphere whose north pole is the first lens axis.
        let direction = rotate_vector(
            lenses[0].orientation * vec4<f32>(1.0, -1.0, -1.0, -1.0),
            panorama_direction(column, row),
        );
        let first = project_sample(0u, direction);
        let second = project_sample(1u, direction);
        if first.valid != 0u && second.valid != 0u
            && maximum_channel_difference(first.color, second.color) * 255.0 < 255.0 {
            first_sum += first.color;
            second_sum += second.color;
            count += 1.0;
        }
    }
    slopes[column * 2u] = vec4<f32>(first_sum, count);
    slopes[column * 2u + 1u] = vec4<f32>(second_sum, count);
}

@compute @workgroup_size(64, 1, 1)
fn radiometry_second(@builtin(global_invocation_id) id: vec3<u32>) {
    let column = id.x;
    if column >= frame.output_size.x {
        return;
    }
    if frame.feather_dead_zone.z < 0.5 {
        second_stats[column * 2u] = vec4<f32>(0.0);
        second_stats[column * 2u + 1u] = vec4<f32>(0.0);
        return;
    }

    let preliminary = first_window_means(column);
    let threshold = floor(
        maximum_channel_difference(preliminary.first, preliminary.second) * 255.0 + 30.0,
    );
    var first_sum = vec3<f32>(0.0);
    var second_sum = vec3<f32>(0.0);
    var count = 0.0;
    for (var row = 0u; row < frame.output_size.y; row += 8u) {
        let direction = rotate_vector(
            lenses[0].orientation * vec4<f32>(1.0, -1.0, -1.0, -1.0),
            panorama_direction(column, row),
        );
        let first = project_sample(0u, direction);
        let second = project_sample(1u, direction);
        if first.valid != 0u && second.valid != 0u
            && maximum_channel_difference(first.color, second.color) * 255.0 < threshold {
            first_sum += first.color;
            second_sum += second.color;
            count += 1.0;
        }
    }
    second_stats[column * 2u] = vec4<f32>(first_sum, count);
    second_stats[column * 2u + 1u] = vec4<f32>(second_sum, count);
}

@compute @workgroup_size(64, 1, 1)
fn radiometry_means(@builtin(global_invocation_id) id: vec3<u32>) {
    let column = id.x;
    if column >= frame.output_size.x {
        return;
    }
    let means = second_window_means(column);
    slopes[column * 2u] = vec4<f32>(means.first, 0.0);
    slopes[column * 2u + 1u] = vec4<f32>(means.second, 0.0);
}

@compute @workgroup_size(64, 1, 1)
fn radiometry_slopes(@builtin(global_invocation_id) id: vec3<u32>) {
    let column = id.x;
    let width = frame.output_size.x;
    if column >= width {
        return;
    }
    var first_mean = vec3<f32>(0.0);
    var second_mean = vec3<f32>(0.0);
    let signed_width = i32(width);
    for (var offset = -10; offset <= 10; offset += 1) {
        let remainder = (i32(column) + offset) % signed_width;
        let wrapped = u32((remainder + signed_width) % signed_width);
        first_mean += slopes[wrapped * 2u].xyz / 21.0;
        second_mean += slopes[wrapped * 2u + 1u].xyz / 21.0;
    }
    let middle = (first_mean + second_mean) * 0.5;
    let slope_scale = 4.0 / f32(frame.output_size.y);
    var first_slope = vec3<f32>(0.0);
    var second_slope = vec3<f32>(0.0);
    if middle.x > 0.0 {
        first_slope.x = slope_scale * (1.0 - first_mean.x / middle.x);
        second_slope.x = slope_scale * (1.0 - second_mean.x / middle.x);
    }
    if middle.y > 0.0 {
        first_slope.y = slope_scale * (1.0 - first_mean.y / middle.y);
        second_slope.y = slope_scale * (1.0 - second_mean.y / middle.y);
    }
    if middle.z > 0.0 {
        first_slope.z = slope_scale * (1.0 - first_mean.z / middle.z);
        second_slope.z = slope_scale * (1.0 - second_mean.z / middle.z);
    }
    second_stats[column] = vec4<f32>(first_slope, 0.0);
    second_stats[width + column] = vec4<f32>(second_slope, 0.0);
}

fn gain(lens_index: u32, position: vec2<f32>) -> vec3<f32> {
    if frame.feather_dead_zone.z < 0.5 {
        return vec3<f32>(1.0);
    }
    let width = frame.output_size.x;
    let column = position.x - floor(position.x / f32(width)) * f32(width);
    let left = u32(floor(column)) % width;
    let right = (left + 1u) % width;
    let fraction = column - floor(column);
    let row = clamp(position.y, 0.0, f32(frame.output_size.y - 1u));
    var distance: f32;
    if lens_index == 0u {
        distance = row - frame.feather_dead_zone.y;
    } else {
        distance = f32(frame.output_size.y - 1u) - row - frame.feather_dead_zone.y;
    }
    distance = max(distance, 0.0);
    let slope = mix(
        slopes[lens_index * width + left].xyz,
        slopes[lens_index * width + right].xyz,
        fraction,
    );
    return max(vec3<f32>(0.0), vec3<f32>(1.0) + slope * distance);
}

fn camera_position(direction: vec3<f32>) -> vec2<f32> {
    // Invert the statistics remap before evaluating the lens-axis gain ramps.
    let color_direction = rotate_vector(lenses[0].orientation, direction);
    let longitude = atan2(-color_direction.y, color_direction.x);
    let colatitude = atan2(length(color_direction.xy), color_direction.z);
    return vec2<f32>(
        longitude / (2.0 * PI) * f32(frame.output_size.x) - 0.5,
        colatitude / PI * f32(frame.output_size.y) - 0.5,
    );
}

fn bilinear_mask_weight(lens_index: u32, source: vec2<f32>, maximum: vec2<f32>) -> f32 {
    return mask_weight(lens_index, min(source, maximum));
}

fn low_frequency(lens_index: u32, source: vec2<f32>) -> vec3<f32> {
    let offsets = array<f32, 5>(-64.0, -32.0, 0.0, 32.0, 64.0);
    let weights = array<f32, 5>(1.0, 4.0, 6.0, 4.0, 1.0);
    let maximum = lenses[lens_index].source.xy - vec2<f32>(1.0);
    var result = vec3<f32>(0.0);
    var total_weight = 0.0;
    for (var y = 0u; y < 5u; y += 1u) {
        for (var x = 0u; x < 5u; x += 1u) {
            let position = clamp(
                source + vec2<f32>(offsets[x], offsets[y]), vec2<f32>(0.0), maximum,
            );
            let weight = weights[x] * weights[y] * bilinear_mask_weight(lens_index, position, maximum);
            if weight <= 0.0 {
                continue;
            }
            result += sample_source(lens_index, position) * weight;
            total_weight += weight;
        }
    }
    if total_weight <= 0.0 {
        return sample_source(lens_index, source);
    }
    return result / total_weight;
}

fn quantize(color: vec3<f32>) -> u32 {
    let bytes = vec3<u32>(floor(clamp(color * 255.0, vec3<f32>(0.0), vec3<f32>(255.0)) + 0.5));
    return bytes.x | (bytes.y << 8u) | (bytes.z << 16u) | (255u << 24u);
}

fn unpack_rgb(pixel: u32) -> vec3<f32> {
    return vec3<f32>(
        f32(pixel & 255u),
        f32((pixel >> 8u) & 255u),
        f32((pixel >> 16u) & 255u),
    ) / 255.0;
}

fn lut_value(point: vec3<u32>, size: u32) -> vec3<f32> {
    return color_lut[point.x + size * (point.y + size * point.z)].xyz;
}

fn convert_color(color: vec3<f32>) -> vec3<f32> {
    let size = u32(frame.lut_domain_min_size.w);
    if size < 2u {
        return color;
    }
    let position = clamp(
        (color - frame.lut_domain_min_size.xyz) * frame.lut_domain_scale.xyz,
        vec3<f32>(0.0), vec3<f32>(1.0),
    ) * f32(size - 1u);
    let low = vec3<u32>(floor(position));
    let high = min(low + vec3<u32>(1u), vec3<u32>(size - 1u));
    let fraction = position - vec3<f32>(low);
    let c00 = mix(lut_value(low, size), lut_value(vec3<u32>(high.x, low.yz), size), fraction.x);
    let c10 = mix(lut_value(vec3<u32>(low.x, high.y, low.z), size), lut_value(vec3<u32>(high.xy, low.z), size), fraction.x);
    let c01 = mix(lut_value(vec3<u32>(low.xy, high.z), size), lut_value(vec3<u32>(high.x, low.y, high.z), size), fraction.x);
    let c11 = mix(lut_value(vec3<u32>(low.x, high.yz), size), lut_value(high, size), fraction.x);
    return mix(mix(c00, c10, fraction.y), mix(c01, c11, fraction.y), fraction.z);
}

fn panorama_rgb(x: u32, y: u32) -> vec3<f32> {
    if x >= frame.output_size.x || y >= frame.output_size.y {
        return vec3<f32>(0.0);
    }
    return unpack_rgb(output_pixels[y * frame.output_size.x + x]);
}

fn bt709_limited_luma(color: vec3<f32>) -> u32 {
    let luma = dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
    return u32(round(clamp(16.0 + 219.0 * luma, 0.0, 255.0)));
}

fn bt709_limited_chroma(color: vec3<f32>) -> vec2<u32> {
    let luma = dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
    let cb = (color.b - luma) / 1.8556;
    let cr = (color.r - luma) / 1.5748;
    return vec2<u32>(round(clamp(
        vec2<f32>(128.0 + 224.0 * cb, 128.0 + 224.0 * cr),
        vec2<f32>(0.0),
        vec2<f32>(255.0),
    )));
}

fn pack_four(values: vec4<u32>) -> u32 {
    return values.x | (values.y << 8u) | (values.z << 16u) | (values.w << 24u);
}

@compute @workgroup_size(16, 8, 1)
fn rgb_to_yuv420(@builtin(global_invocation_id) id: vec3<u32>) {
    let base_x = id.x * 8u;
    let base_y = id.y * 2u;
    if base_x >= frame.output_size.x || base_y >= frame.output_size.y {
        return;
    }
    let y_stride = frame._padding0.x;
    let chroma_stride = frame._padding0.y;
    let u_offset = y_stride * frame.output_size.y;
    let v_offset = u_offset + chroma_stride * (frame.output_size.y / 2u);

    var chroma_rgb: array<vec3<f32>, 4>;
    for (var row = 0u; row < 2u; row += 1u) {
        var luma: array<u32, 8>;
        for (var column = 0u; column < 8u; column += 1u) {
            let color = panorama_rgb(base_x + column, base_y + row);
            luma[column] = bt709_limited_luma(color);
            chroma_rgb[column / 2u] += color * 0.25;
        }
        let y_byte_offset = (base_y + row) * y_stride + base_x;
        yuv_output[y_byte_offset / 4u] = pack_four(
            vec4<u32>(luma[0], luma[1], luma[2], luma[3]),
        );
        yuv_output[(y_byte_offset + 4u) / 4u] = pack_four(
            vec4<u32>(luma[4], luma[5], luma[6], luma[7]),
        );
    }

    var u_values = vec4<u32>(0u);
    var v_values = vec4<u32>(0u);
    for (var chroma = 0u; chroma < 4u; chroma += 1u) {
        let converted = bt709_limited_chroma(chroma_rgb[chroma]);
        u_values[chroma] = converted.x;
        v_values[chroma] = converted.y;
    }
    let chroma_byte_offset = id.y * chroma_stride + id.x * 4u;
    yuv_output[(u_offset + chroma_byte_offset) / 4u] = pack_four(u_values);
    yuv_output[(v_offset + chroma_byte_offset) / 4u] = pack_four(v_values);
}

@compute @workgroup_size(16, 8, 1)
fn stitch(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= frame.output_size.x || id.y >= frame.output_size.y {
        return;
    }
    let direction = rotate_vector(frame.output_to_camera, panorama_direction(id.x, id.y));
    let position = camera_position(direction);
    let first = project_sample(0u, direction);
    let second = project_sample(1u, direction);
    var color = vec3<f32>(0.0);
    if first.valid == 0u && second.valid == 0u {
        color = vec3<f32>(0.0);
    } else if second.valid == 0u {
        color = first.color * gain(0u, position);
    } else if first.valid == 0u {
        color = second.color * gain(1u, position);
    } else {
        let first_gain = gain(0u, position);
        let second_gain = gain(1u, position);
        let first_color = first.color * first_gain;
        let second_color = second.color * second_gain;
        let first_low = low_frequency(0u, first.source) * first_gain;
        let second_low = low_frequency(1u, second.source) * second_gain;
        let detail_total = first.detail_weight + second.detail_weight;
        let illumination_total = first.illumination_weight + second.illumination_weight;
        if detail_total <= 0.0 || illumination_total <= 0.0 {
            if first.detail_weight >= second.detail_weight {
                color = first_color;
            } else {
                color = second_color;
            }
        } else {
            let high = ((first_color - first_low) * first.detail_weight
                + (second_color - second_low) * second.detail_weight) / detail_total;
            let illumination = (first_low * first.illumination_weight
                + second_low * second.illumination_weight) / illumination_total;
            color = high + illumination;
        }
    }
    let stitched = quantize(color);
    if frame.lut_domain_min_size.w >= 2.0 {
        // Match the RGB8 CPU stitch boundary before applying the same table.
        output_pixels[id.y * frame.output_size.x + id.x] = quantize(convert_color(unpack_rgb(stitched)));
    } else {
        output_pixels[id.y * frame.output_size.x + id.x] = stitched;
    }
}

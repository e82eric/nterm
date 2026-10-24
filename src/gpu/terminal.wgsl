// Storage words match Ghostty's 32-byte CellText records. The uniforms match
// the native std140 layout; packed u16/u8 fields are unpacked explicitly.
struct Globals {
    projection: mat4x4<f32>, screen: vec2<f32>, cell: vec2<f32>,
    grid: u32, pad0: u32, pad1: u32, pad2: u32,
    padding: vec4<f32>, extend: u32, contrast: f32,
    cursor: u32, cursor_color: u32, background: u32, flags: u32,
    pad3: u32, pad4: u32,
}
struct Glyph { words: array<u32, 8> }
@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var<storage, read> backgrounds: array<u32>;
@group(0) @binding(2) var<storage, read> data: array<u32>;
@group(0) @binding(3) var grayscale: texture_2d<f32>;
@group(0) @binding(4) var color_atlas: texture_2d<f32>;
@group(0) @binding(5) var image_sampler: sampler;

fn unpack2(v: u32) -> vec2<u32> { return vec2(v & 65535u, v >> 16u); }
fn unpack_color(v: u32) -> vec4<f32> {
    return vec4<f32>(f32(v & 255u), f32((v >> 8u) & 255u),
        f32((v >> 16u) & 255u), f32(v >> 24u)) / 255.0;
}
fn linear(v: vec3<f32>) -> vec3<f32> {
    return select(v / 12.92, pow((v + 0.055) / 1.055, vec3(2.4)), v > vec3(0.04045));
}
fn gamma(v: vec3<f32>) -> vec3<f32> {
    return select(v * 12.92, 1.055 * pow(max(v, vec3(0.0)), vec3(1.0 / 2.4)) - 0.055,
        v > vec3(0.0031308));
}
fn load_color(v: u32, use_linear: bool) -> vec4<f32> {
    let c = unpack_color(v);
    return vec4(select(c.rgb, linear(c.rgb), use_linear) * c.a, c.a);
}
fn luminance(v: vec3<f32>) -> f32 { return dot(v, vec3(0.2126, 0.7152, 0.0722)); }
fn contrast(fg: vec4<f32>, bg: vec4<f32>) -> vec4<f32> {
    let f = luminance(fg.rgb); let b = luminance(bg.rgb);
    if ((max(f, b) + 0.05) / (min(f, b) + 0.05) >= globals.contrast) { return fg; }
    let white = 1.05 / (b + 0.05); let black = (b + 0.05) / 0.05;
    return vec4(select(vec3(0.0), vec3(1.0), white > black), 1.0);
}
struct Fullscreen { @builtin(position) position: vec4<f32> }
@vertex fn fullscreen(@builtin(vertex_index) vertex: u32) -> Fullscreen {
    let p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var out: Fullscreen; out.position = vec4(p[vertex], 0.0, 1.0); return out;
}
@fragment fn background() -> @location(0) vec4<f32> {
    return load_color(globals.background, (globals.flags & 4u) != 0u);
}
@fragment fn cells(in: Fullscreen) -> @location(0) vec4<f32> {
    let grid = vec2<i32>(unpack2(globals.grid));
    var p = vec2<i32>(floor((in.position.xy - globals.padding.wx) / globals.cell));
    if ((p.x < 0 && (globals.extend & 1u) == 0u) ||
        (p.x >= grid.x && (globals.extend & 2u) == 0u) ||
        (p.y < 0 && (globals.extend & 4u) == 0u) ||
        (p.y >= grid.y && (globals.extend & 8u) == 0u)) { return vec4(0.0); }
    p = clamp(p, vec2(0), grid - vec2(1));
    return load_color(backgrounds[u32(p.y * grid.x + p.x)], (globals.flags & 4u) != 0u);
}
struct TextVertex {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) atlas: u32,
    @location(2) @interpolate(flat) color: vec4<f32>,
    @location(3) @interpolate(flat) bg: vec4<f32>,
}
@vertex fn text_vertex(@builtin(vertex_index) vertex: u32, @builtin(instance_index) instance: u32) -> TextVertex {
    let k = instance * 8u;
    let g = Glyph(array<u32, 8>(data[k], data[k+1u], data[k+2u], data[k+3u],
        data[k+4u], data[k+5u], data[k+6u], data[k+7u]));
    let size = vec2<f32>(f32(g.words[2]), f32(g.words[3]));
    let bearings = vec2<f32>(f32(bitcast<i32>(g.words[4] << 16u) >> 16u),
        f32(bitcast<i32>(g.words[4]) >> 16u));
    let grid = unpack2(g.words[5]);
    let corner = vec2<f32>(f32(vertex == 1u || vertex == 3u), f32(vertex >= 2u));
    let pos = globals.cell * vec2<f32>(grid) + size * corner + vec2(bearings.x, globals.cell.y - bearings.y);
    var out: TextVertex;
    out.position = globals.projection * vec4(pos, 0.0, 1.0);
    out.position.z = (out.position.z + out.position.w) * 0.5;
    out.uv = vec2<f32>(f32(g.words[0]), f32(g.words[1])) + size * corner;
    out.atlas = g.words[7] & 255u;
    out.color = load_color(g.words[6], true);
    let bg = load_color(backgrounds[grid.y * (globals.grid & 65535u) + grid.x], true);
    out.bg = bg + load_color(globals.background, true) * (1.0 - bg.a);
    let glyph_flags = (g.words[7] >> 8u) & 255u;
    if (globals.contrast > 1.0 && (glyph_flags & 1u) == 0u) { out.color = contrast(out.color, out.bg); }
    let cursor = unpack2(globals.cursor);
    if ((glyph_flags & 2u) == 0u && grid.y == cursor.y &&
        (grid.x == cursor.x || ((globals.flags & 1u) != 0u && grid.x == cursor.x + 1u))) {
        out.color = load_color(globals.cursor_color, (globals.flags & 4u) != 0u);
    }
    return out;
}
@fragment fn text_fragment(in: TextVertex) -> @location(0) vec4<f32> {
    let use_linear = (globals.flags & 4u) != 0u;
    if (in.atlas == 1u) {
        let c = textureLoad(color_atlas, vec2<i32>(in.uv), 0);
        if (use_linear || c.a == 0.0) { return c; }
        return vec4(gamma(c.rgb / c.a) * c.a, c.a);
    }
    var c = in.color;
    if (!use_linear && c.a > 0.0) { c = vec4(gamma(c.rgb / c.a) * c.a, c.a); }
    var a = textureLoad(grayscale, vec2<i32>(in.uv), 0).r;
    if ((globals.flags & 8u) != 0u) {
        let fg = luminance(c.rgb); let bg = luminance(in.bg.rgb);
        if (abs(fg - bg) > 0.001) {
            let blended = gamma(vec3(fg)).x * a + gamma(vec3(bg)).x * (1.0 - a);
            a = clamp((linear(vec3(blended)).x - bg) / (fg - bg), 0.0, 1.0);
        }
    }
    return c * a;
}

struct ImageVertex { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn image_vertex(@builtin(vertex_index) vertex: u32) -> ImageVertex {
    let grid = vec2(bitcast<f32>(data[0]), bitcast<f32>(data[1]));
    let offset = vec2(bitcast<f32>(data[2]), bitcast<f32>(data[3]));
    let source = vec4(bitcast<f32>(data[4]), bitcast<f32>(data[5]), bitcast<f32>(data[6]), bitcast<f32>(data[7]));
    let dest = vec2(bitcast<f32>(data[8]), bitcast<f32>(data[9]));
    let corner = vec2<f32>(f32(vertex == 1u || vertex == 3u), f32(vertex >= 2u));
    var out: ImageVertex;
    out.position = globals.projection * vec4(globals.cell * grid + offset + dest * corner, 1.0, 1.0);
    out.position.z = (out.position.z + out.position.w) * 0.5;
    out.uv = (source.xy + source.zw * corner) / vec2<f32>(textureDimensions(grayscale));
    return out;
}
@fragment fn image_fragment(in: ImageVertex) -> @location(0) vec4<f32> {
    var c = textureSample(grayscale, image_sampler, in.uv);
    if ((globals.flags & 4u) == 0u) { c = vec4(gamma(c.rgb), c.a); }
    return vec4(c.rgb * c.a, c.a);
}
@fragment fn background_image(in: Fullscreen) -> @location(0) vec4<f32> {
    let opacity = bitcast<f32>(data[0]); let info = data[1] & 255u;
    let tex_size = vec2<f32>(textureDimensions(grayscale));
    let ratio = globals.screen / tex_size;
    var dest = tex_size;
    switch ((info >> 4u) & 3u) {
        case 0u: { dest *= min(ratio.x, ratio.y); }
        case 1u: { dest *= max(ratio.x, ratio.y); }
        case 2u: { dest = globals.screen; }
        default: {}
    }
    let position = info & 15u;
    let axis = vec2<f32>(f32(position % 3u) / 2.0, f32(position / 3u) / 2.0);
    let offset = (globals.screen - dest) * axis;
    var coord = (in.position.xy - offset) * tex_size / dest;
    if ((info & 64u) != 0u) { coord = coord - floor(coord / tex_size) * tex_size; }
    let linear_blend = (globals.flags & 4u) != 0u;
    var c = vec4(0.0);
    // Explicit LOD allows sampling under a nonuniform fragment branch.
    if (all(coord >= vec2(0.0)) && all(coord <= tex_size)) {
        c = textureSampleLevel(grayscale, image_sampler, coord / tex_size, 0.0);
        if (!linear_blend) { c = vec4(gamma(c.rgb), c.a); }
        c = vec4(c.rgb * c.a, c.a);
    }
    let bg = load_color(globals.background, linear_blend);
    if (bg.a == 0.0) { return vec4(0.0); }
    c *= min(opacity, 1.0 / bg.a);
    c += max(vec4(0.0), vec4(bg.rgb / bg.a, 1.0) * (1.0 - c.a));
    return c * bg.a;
}

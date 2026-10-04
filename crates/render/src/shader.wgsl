// Draws one textured quad. The framebuffer uses `opaque = 1` (its fourth
// byte is undefined); the cursor is alpha blended.

struct Quad {
    // Left, top, right, bottom in clip space.
    rect: vec4<f32>,
    opaque: f32,
}

@group(0) @binding(0) var<uniform> quad: Quad;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VOut {
    let uv = vec2<f32>(f32(i & 1u), f32(i >> 1u));
    let xy = mix(quad.rect.xy, quad.rect.zw, uv);
    return VOut(vec4<f32>(xy, 0.0, 1.0), uv);
}

@fragment
fn fs(v: VOut) -> @location(0) vec4<f32> {
    let c = textureSample(tex, samp, v.uv);
    if quad.opaque > 0.5 {
        return vec4<f32>(c.rgb, 1.0);
    }
    return c;
}

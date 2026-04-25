// NV12 (semi-planar YCbCr 4:2:0) → RGB fragment shader.
//
// Binding layout (matches bg_layout_nv12 in renderer.rs):
//   binding 0: t_y  — R8Unorm,  width   × height     (luma)
//   binding 1: t_uv — Rg8Unorm, width/2 × height/2   (interleaved chroma, R=Cb G=Cr)
//   binding 2: s    — filtering sampler
//
// Colour standard: BT.709 limited range (Y ∈ [16/255, 235/255], Cb/Cr ∈ [16/255, 240/255]).
// The sampler handles 2× chroma upsampling automatically via bilinear filtering.

@group(0) @binding(0) var t_y:  texture_2d<f32>;
@group(0) @binding(1) var t_uv: texture_2d<f32>;
@group(0) @binding(2) var s:    sampler;

@fragment
fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let y_raw  = textureSample(t_y,  s, uv).r;
    let uv_raw = textureSample(t_uv, s, uv).rg;  // .r = Cb, .g = Cr

    // Limited-range offset + scale (BT.709)
    // Y  ∈ [16, 235] → divide by 219 after removing black offset
    // Cb/Cr ∈ [16, 240] → divide by 224 after removing grey offset
    let y  = (y_raw    - 16.0  / 255.0) * (255.0 / 219.0);
    let pb = (uv_raw.x - 128.0 / 255.0) * (255.0 / 224.0);
    let pr = (uv_raw.y - 128.0 / 255.0) * (255.0 / 224.0);

    // BT.709 YCbCr → linear RGB
    let r = y                   + 1.5748 * pr;
    let g = y - 0.1873 * pb    - 0.4681 * pr;
    let b = y + 1.8556 * pb;

    return vec4<f32>(clamp(r, 0.0, 1.0), clamp(g, 0.0, 1.0), clamp(b, 0.0, 1.0), 1.0);
}

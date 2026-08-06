// highlight_shader.wgsl
// Dibuja las líneas del contorno del bloque apuntado, en negro sólido, un
// poco delante del depth buffer del mundo (ver INSET en highlight.rs) para
// que no titile contra la cara del bloque.

struct Uniforms {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

struct VertexInput {
    @location(0) position: vec3<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> @builtin(position) vec4<f32> {
    return uniforms.view_proj * vec4<f32>(in.position, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(0.05, 0.05, 0.05, 0.9);
}

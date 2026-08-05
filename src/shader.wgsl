// shader.wgsl
// Vertex + fragment shader mínimo: transforma posiciones con view-projection
// y aplica una luz direccional simple (estilo "sol") usando la normal de
// cada cara, para que el cubo no se vea totalmente plano.

struct Uniforms {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec3<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = uniforms.view_proj * vec4<f32>(in.position, 1.0);
    out.normal = in.normal;
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let light_dir = normalize(vec3<f32>(0.5, 1.0, 0.3));
    let ambient = 0.35;
    let diffuse = max(dot(in.normal, light_dir), 0.0) * 0.65;
    let light = ambient + diffuse;
    return vec4<f32>(in.color * light, 1.0);
}

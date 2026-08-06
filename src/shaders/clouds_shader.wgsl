// clouds_shader.wgsl
// Capa de nubes procedural: un solo quad gigante horizontal (ver
// clouds.rs) a altura fija, con un patrón de manchas generado con ruido
// (value noise + fbm) en vez de una textura. Se mueve lento con
// `uniforms.time` para simular viento, y se desvanece con la misma
// niebla que el terreno para que no se note el borde del plano.

struct Uniforms {
    view_proj: mat4x4<f32>,
    camera_pos: vec3<f32>,
    fog_start: f32,
    fog_color: vec3<f32>,
    fog_end: f32,
    // Solo se usa .x (segundos desde que arrancó la app); el resto es
    // relleno para mantener la alineación de 16 bytes de un vec4 en
    // WGSL, ver el comentario sobre `Uniforms` en lib.rs.
    time: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

// Altura absoluta de mundo (en bloques) a la que flota la capa de
// nubes. Bien por encima de la altura máxima del terreno (ver
// `worldgen.rs`, tope ~60 bloques) para que no se meta entre las
// colinas.
const CLOUD_HEIGHT: f32 = 128.0;

// Tamaño de celda del ruido, en bloques: celdas más grandes = nubes más
// grandes y menos "ruidosas".
const NOISE_SCALE: f32 = 1.0 / 70.0;
// Velocidad del viento (bloques por segundo) arrastrando el patrón.
const WIND_SPEED: f32 = 3.0;
// Umbral de cobertura: más alto = cielo más despejado, menos nubes.
const COVERAGE_LOW: f32 = 0.52;
const COVERAGE_HIGH: f32 = 0.72;
// Opacidad máxima de una nube en su parte más densa.
const MAX_OPACITY: f32 = 0.8;

struct VertexInput {
    @location(0) position: vec3<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_xz: vec2<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let world_x = in.position.x + uniforms.camera_pos.x;
    let world_z = in.position.z + uniforms.camera_pos.z;
    out.clip_position = uniforms.view_proj * vec4<f32>(world_x, CLOUD_HEIGHT, world_z, 1.0);
    out.world_xz = vec2<f32>(world_x, world_z);
    return out;
}

// --- Ruido de valor (value noise) + fbm, todo hecho a mano: no hace
// falta textura de ruido, con un hash barato alcanza para el patrón de
// nubes (no necesita ser de alta calidad, se ve borroso de todos modos).
fn hash(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.x, p.y, p.x) * 0.1031);
    p3 += dot(p3, p3.yzx + vec3<f32>(33.33));
    return fract((p3.x + p3.y) * p3.z);
}

fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let a = hash(i);
    let b = hash(i + vec2<f32>(1.0, 0.0));
    let c = hash(i + vec2<f32>(0.0, 1.0));
    let d = hash(i + vec2<f32>(1.0, 1.0));
    let u = f * f * (3.0 - 2.0 * f);
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

fn fbm(p: vec2<f32>) -> f32 {
    var value = 0.0;
    var amplitude = 0.5;
    var freq = p;
    for (var i = 0; i < 4; i = i + 1) {
        value += amplitude * value_noise(freq);
        freq *= 2.0;
        amplitude *= 0.5;
    }
    return value;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let wind_offset = vec2<f32>(uniforms.time.x * WIND_SPEED, uniforms.time.x * WIND_SPEED * 0.35);
    let sample_pos = (in.world_xz + wind_offset) * NOISE_SCALE;
    let density = fbm(sample_pos);

    // Cobertura suave: por debajo de COVERAGE_LOW no hay nube (alpha 0),
    // por encima de COVERAGE_HIGH está a máxima opacidad.
    let coverage = smoothstep(COVERAGE_LOW, COVERAGE_HIGH, density);

    // Niebla horizontal: usamos solo distancia en XZ (no la diferencia
    // de altura contra la cámara, que acá sería enorme y apagaría las
    // nubes incluso mirando hacia arriba). Mismo fog_start/fog_end que
    // el terreno, así el horizonte se ve consistente.
    let dist = distance(in.world_xz, uniforms.camera_pos.xz);
    let fog_range = max(uniforms.fog_end - uniforms.fog_start, 0.001);
    let fog_factor = clamp((dist - uniforms.fog_start) / fog_range, 0.0, 1.0);

    let alpha = coverage * MAX_OPACITY * (1.0 - fog_factor);

    // Blanco ligeramente azulado, un toque más oscuro en las zonas menos
    // densas para dar volumen sin necesitar sombreado real.
    let cloud_color = mix(vec3<f32>(0.82, 0.85, 0.9), vec3<f32>(1.0, 1.0, 1.0), coverage);

    return vec4<f32>(cloud_color, alpha);
}

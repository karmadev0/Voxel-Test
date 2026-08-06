// shader.wgsl
// Vertex + fragment shader del terreno: transforma posiciones con
// view-projection, muestrea el atlas de texturas de bloques y aplica una
// luz direccional simple (estilo "sol") usando la normal de cada cara,
// para que el cubo no se vea totalmente plano.
//
// Además aplica niebla: mezcla el color final con `fog_color` (el mismo
// celeste del cielo) a medida que un fragmento está más lejos de la
// cámara, entre `fog_start` y `fog_end` (ambos en bloques, ver `update`
// en lib.rs). Así, cuando el radio de chunks cargados es corto, el borde
// donde el mundo se corta queda escondido detrás de la niebla en vez de
// verse como un límite abrupto en el horizonte.

struct Uniforms {
    view_proj: mat4x4<f32>,
    camera_pos: vec3<f32>,
    fog_start: f32,
    fog_color: vec3<f32>,
    fog_end: f32,
};

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

// Atlas de texturas de bloques (ver textures/loader.rs). Grupo aparte del
// uniform porque cambia con mucha menos frecuencia (una vez, al cargar).
@group(1) @binding(0)
var atlas_texture: texture_2d<f32>;
@group(1) @binding(1)
var atlas_sampler: sampler;

// Tiene que coincidir con ATLAS_COLS/ATLAS_ROWS en textures/atlas.rs, que
// a su vez tiene que coincidir con la cantidad de líneas de
// assets/textures/blocks.txt (build.rs arma 1 fila por bloque). Si
// agregás un bloque a blocks.txt, actualizar ahí Y acá.
const ATLAS_COLS: f32 = 6.0;
const ATLAS_ROWS: f32 = 3.0;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
    // Coordenadas LOCALES al quad fusionado (en unidades de bloque, no
    // 0..1) — ver el comentario de `Vertex` en mesher.rs. `fract()` en el
    // fragment shader las envuelve de vuelta a 0..1 por bloque.
    @location(3) uv: vec2<f32>,
    // Esquina superior-izquierda del tile de este bloque/cara en el
    // atlas, en UV normalizadas 0..1.
    @location(4) tile_origin: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec3<f32>,
    @location(2) world_position: vec3<f32>,
    @location(3) uv: vec2<f32>,
    @location(4) tile_origin: vec2<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = uniforms.view_proj * vec4<f32>(in.position, 1.0);
    out.normal = in.normal;
    out.color = in.color;
    out.world_position = in.position;
    out.uv = in.uv;
    out.tile_origin = in.tile_origin;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Envolvemos la UV local del quad a 0..1 por bloque (así un quad
    // fusionado de, digamos, 5 bloques de ancho repite la textura 5
    // veces en vez de estirarla) y la reubicamos dentro del tile que le
    // toca a este bloque/cara en el atlas.
    let tile_size = vec2<f32>(1.0 / ATLAS_COLS, 1.0 / ATLAS_ROWS);
    let wrapped_uv = fract(in.uv);
    let atlas_uv = in.tile_origin + wrapped_uv * tile_size;
    let tex_color = textureSample(atlas_texture, atlas_sampler, atlas_uv).rgb;

    let light_dir = normalize(vec3<f32>(0.5, 1.0, 0.3));
    let ambient = 0.35;
    let diffuse = max(dot(in.normal, light_dir), 0.0) * 0.65;
    let light = ambient + diffuse;
    // `color` es el tinte multiplicativo por vértice (hoy blanco, ver
    // mesher.rs) — deja el gancho para variar el tile por bioma más
    // adelante sin tener que tocar este shader.
    let lit_color = tex_color * in.color * light;

    // Niebla: mezcla lineal entre el color iluminado y el color de
    // niebla según qué tan lejos está este fragmento de la cámara,
    // saturada entre 0 (en fog_start o más cerca) y 1 (en fog_end o
    // más lejos).
    let dist = distance(in.world_position, uniforms.camera_pos);
    let fog_range = max(uniforms.fog_end - uniforms.fog_start, 0.001);
    let fog_factor = clamp((dist - uniforms.fog_start) / fog_range, 0.0, 1.0);
    let final_color = mix(lit_color, uniforms.fog_color, fog_factor);

    return vec4<f32>(final_color, 1.0);
}

/// mesher.rs
/// Greedy meshing: por cada una de las 6 direcciones, barremos el chunk capa
/// por capa y fusionamos bloques del mismo tipo en rectángulos lo más
/// grandes posible, generando UN solo quad en vez de una cara por bloque.
///
/// Esto es la optimización clave del proyecto: un chunk de 16x64x16 tiene
/// hasta 16384 bloques. Sin greedy meshing, un chunk sólido podría generar
/// decenas de miles de triángulos. Con greedy meshing, una pared plana de
/// piedra se reduce a UN solo quad, sin importar su tamaño.
///
/// Implementación basada en el algoritmo clásico de mikolalysenko
/// (https://github.com/mikolalysenko/mikolalysenko.github.com/blob/master/MinecraftMeshes2/js/greedy.js),
/// adaptado a Rust y a nuestra estructura de Chunk.

use crate::environment::chunk::{BlockType, Chunk, CHUNK_SIZE_X, CHUNK_SIZE_Y, CHUNK_SIZE_Z};
use crate::textures::atlas;
use bytemuck::{Pod, Zeroable};

/// Qué tan lleno se ve un bloque de agua según su nivel de flujo (ver
/// `BlockType::Water` y `environment/fluids.rs`): 0 = fuente (casi
/// lleno, 8/9), 7 = a punto de secarse (apenas 1/9). Mismos 9 pasos que
/// usa Minecraft, así el nivel de agua se nota a simple vista en vez de
/// que todo bloque de agua se vea igual — es lo que hace que el
/// esparcido/secado gradual del tick de `fluids.rs` se vea de verdad,
/// no solo que pase "por atrás" sin manifestarse en pantalla.
fn water_height_frac(level: u8) -> f32 {
    1.0 - (level as f32 + 1.0) / 9.0
}

/// Fase 5: antes, el mesh de un chunk solo miraba sus propios bloques —
/// en el borde con un chunk vecino, ese vecino se trataba siempre como
/// aire, así que quedaban caras de más dibujadas ahí (invisibles para el
/// jugador, pero trabajo de GPU desperdiciado). `ChunkNeighborhood` junta
/// el chunk que se está malleando con referencias a sus 4 vecinos en X/Z
/// (si están cargados) y resuelve los lookups fuera de rango consultando
/// al vecino correcto en vez de asumir aire. No hace falta un vecino
/// "diagonal": el greedy meshing sweep nunca se mueve más de un eje a la
/// vez, así que un lookup fuera de rango cambia una sola coordenada por
/// vez (nunca X y Z a la vez).
pub struct ChunkNeighborhood<'a> {
    pub center: &'a Chunk,
    pub neg_x: Option<&'a Chunk>,
    pub pos_x: Option<&'a Chunk>,
    pub neg_z: Option<&'a Chunk>,
    pub pos_z: Option<&'a Chunk>,
}

impl<'a> ChunkNeighborhood<'a> {
    /// Un chunk "solo", sin vecinos conocidos (equivalente al comportamiento
    /// de antes de la Fase 5: bordes tratados como aire). Útil para tests
    /// o para mallear un chunk aislado sin tener que armar un `World`.
    pub fn isolated(center: &'a Chunk) -> Self {
        Self {
            center,
            neg_x: None,
            pos_x: None,
            neg_z: None,
            pos_z: None,
        }
    }

    fn get(&self, x: i32, y: i32, z: i32) -> BlockType {
        if y < 0 || y >= CHUNK_SIZE_Y as i32 {
            return BlockType::Air;
        }
        if x < 0 {
            return self
                .neg_x
                .map(|c| c.get(CHUNK_SIZE_X as i32 + x, y, z))
                .unwrap_or(BlockType::Air);
        }
        if x >= CHUNK_SIZE_X as i32 {
            return self
                .pos_x
                .map(|c| c.get(x - CHUNK_SIZE_X as i32, y, z))
                .unwrap_or(BlockType::Air);
        }
        if z < 0 {
            return self
                .neg_z
                .map(|c| c.get(x, y, CHUNK_SIZE_Z as i32 + z))
                .unwrap_or(BlockType::Air);
        }
        if z >= CHUNK_SIZE_Z as i32 {
            return self
                .pos_z
                .map(|c| c.get(x, y, z - CHUNK_SIZE_Z as i32))
                .unwrap_or(BlockType::Air);
        }
        self.center.get(x, y, z)
    }
}

/// `uv` son coordenadas LOCALES al quad fusionado, en unidades de bloque
/// (van de (0,0) a (w,h), no de (0,0) a (1,1)): como el greedy meshing
/// fusiona muchos bloques en un solo quad grande, si mandáramos UV
/// normalizadas 0..1 la textura se estiraría en vez de repetirse por
/// bloque. El fragment shader hace `fract(uv)` para volver a 0..1 y
/// después lo reubica dentro del tile correcto del atlas usando
/// `tile_origin` (ver textures/atlas.rs para el tamaño de tile fijo).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub color: [f32; 3],
    pub uv: [f32; 2],
    pub tile_origin: [f32; 2],
}

impl Vertex {
    pub fn desc() -> wgpu::VertexBufferLayout<'static> {
        use std::mem;
        wgpu::VertexBufferLayout {
            array_stride: mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x3,
                },
                wgpu::VertexAttribute {
                    offset: mem::size_of::<[f32; 3]>() as wgpu::BufferAddress,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x3,
                },
                wgpu::VertexAttribute {
                    offset: (mem::size_of::<[f32; 3]>() * 2) as wgpu::BufferAddress,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Float32x3,
                },
                wgpu::VertexAttribute {
                    offset: (mem::size_of::<[f32; 3]>() * 3) as wgpu::BufferAddress,
                    shader_location: 3,
                    format: wgpu::VertexFormat::Float32x2,
                },
                wgpu::VertexAttribute {
                    offset: (mem::size_of::<[f32; 3]>() * 3 + mem::size_of::<[f32; 2]>())
                        as wgpu::BufferAddress,
                    shader_location: 4,
                    format: wgpu::VertexFormat::Float32x2,
                },
            ],
        }
    }
}

pub struct MeshData {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

/// Genera la malla completa de un chunk recorriendo los 3 ejes, en las
/// 2 direcciones cada uno (positiva y negativa) = 6 direcciones totales.
/// `neighborhood` incluye el chunk a mallear y, si están disponibles, sus
/// vecinos en X/Z — así las caras en el borde se cullean de verdad en vez
/// de asumir aire del otro lado.
pub fn generate_mesh(neighborhood: &ChunkNeighborhood) -> MeshData {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();

    for axis in 0..3 {
        for backface in [false, true] {
            greedy_sweep(neighborhood, axis, backface, &mut vertices, &mut indices);
        }
    }

    MeshData { vertices, indices }
}

fn dims() -> [i32; 3] {
    [CHUNK_SIZE_X as i32, CHUNK_SIZE_Y as i32, CHUNK_SIZE_Z as i32]
}

/// Barre el chunk a lo largo de `axis`, capa por capa, fusionando caras
/// del mismo tipo de bloque en rectángulos (greedy meshing 2D por capa).
fn greedy_sweep(
    neighborhood: &ChunkNeighborhood,
    axis: usize,
    backface: bool,
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
) {
    let d = dims();
    let u = (axis + 1) % 3;
    let v = (axis + 2) % 3;

    let mut x = [0i32; 3];
    let mut q = [0i32; 3];
    q[axis] = 1;

    // mask guarda, por cada celda de la capa 2D, el tipo de bloque visible
    // (si corresponde generar cara ahí) para poder fusionarlas después.
    let mut mask = vec![None::<BlockType>; (d[u] * d[v]) as usize];

    x[axis] = -1;
    while x[axis] < d[axis] {
        // 1. Construir la máscara de la capa actual: comparamos el bloque
        //    actual contra su vecino en la dirección del eje para saber si
        //    hay una cara visible (un bloque sólido pegado a aire).
        let mut n = 0;
        x[v] = 0;
        while x[v] < d[v] {
            x[u] = 0;
            while x[u] < d[u] {
                let a = get_block(neighborhood, x[0], x[1], x[2]);
                let mut xb = x;
                xb[axis] += 1;
                let b = get_block(neighborhood, xb[0], xb[1], xb[2]);

                // Agua con distinto nivel a los dos lados: ambas cuentan
                // como "sólidas" para `is_solid` (ninguna es Aire), así
                // que la rama de abajo no generaría ninguna cara acá —
                // pero SÍ tienen distinta altura visual (ver
                // `water_height_frac`), así que sin esto quedaba un
                // hueco real en la malla justo donde el nivel más bajo
                // no llega a tapar al más alto: el "rayos X" que se veía
                // en los bordes de cualquier cuerpo de agua no uniforme.
                let water_step = match (a, b) {
                    (BlockType::Water(la), BlockType::Water(lb)) if la != lb => {
                        // Elegir un único lado entre los dos pases
                        // (backface=false/true) para emitir un solo
                        // quad acá — si los dos pasaran la condición,
                        // saldrían dos quads coincidentes en el mismo
                        // plano (parpadeo por z-fighting). Con esta
                        // comparación, para un mismo par (la, lb) fijo,
                        // solo uno de los dos pases la cumple.
                        (!backface && la < lb) || (backface && la > lb)
                    }
                    _ => false,
                };

                mask[n] = if a.is_solid() != b.is_solid() {
                    if backface {
                        if b.is_solid() { Some(b) } else { None }
                    } else if a.is_solid() {
                        Some(a)
                    } else {
                        None
                    }
                } else if water_step {
                    Some(a)
                } else {
                    None
                };

                n += 1;
                x[u] += 1;
            }
            x[v] += 1;
        }

        x[axis] += 1;

        // 2. Fusionar rectángulos dentro de la máscara (greedy 2D).
        let mut n = 0;
        let mut j = 0;
        while j < d[v] {
            let mut i = 0;
            while i < d[u] {
                if let Some(block) = mask[n] {
                    // ancho: cuántas celdas iguales hay a la derecha
                    let mut w = 1;
                    while i + w < d[u] && mask[n + w as usize] == Some(block) {
                        w += 1;
                    }

                    // alto: cuántas filas completas de ancho `w` son iguales
                    let mut h = 1;
                    'outer: while j + h < d[v] {
                        for k in 0..w {
                            if mask[n + k as usize + (h * d[u]) as usize] != Some(block) {
                                break 'outer;
                            }
                        }
                        h += 1;
                    }

                    // Emitir el quad fusionado
                    let mut base = x;
                    base[u] = i;
                    base[v] = j;

                    let mut du = [0i32; 3];
                    du[u] = w;
                    let mut dv = [0i32; 3];
                    dv[v] = h;

                    emit_quad(base, du, dv, axis, backface, block, vertices, indices);

                    // Limpiar la zona fusionada de la máscara para no repetirla
                    for l in 0..h {
                        for k in 0..w {
                            mask[n + k as usize + (l * d[u]) as usize] = None;
                        }
                    }

                    i += w;
                    n += w as usize;
                } else {
                    i += 1;
                    n += 1;
                }
            }
            j += 1;
        }
    }
}

fn get_block(neighborhood: &ChunkNeighborhood, x: i32, y: i32, z: i32) -> BlockType {
    neighborhood.get(x, y, z)
}

#[allow(clippy::too_many_arguments)]
fn emit_quad(
    base: [i32; 3],
    du: [i32; 3],
    dv: [i32; 3],
    axis: usize,
    backface: bool,
    block: BlockType,
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
) {
    let p0 = base;
    let p1 = [base[0] + du[0], base[1] + du[1], base[2] + du[2]];
    let p2 = [
        base[0] + du[0] + dv[0],
        base[1] + du[1] + dv[1],
        base[2] + du[2] + dv[2],
    ];
    let p3 = [base[0] + dv[0], base[1] + dv[1], base[2] + dv[2]];

    let mut normal = [0.0f32; 3];
    normal[axis] = if backface { -1.0 } else { 1.0 };

    // Tinte multiplicativo sobre la textura: blanco por ahora (color
    // real viene del atlas), pero queda el gancho para variar por bioma
    // más adelante (ej. teñir el tile de pasto según el bioma) sin tener
    // que tocar el shader.
    let color = [1.0, 1.0, 1.0];

    let to_f = |p: [i32; 3]| [p[0] as f32, p[1] as f32, p[2] as f32];

    let mut corners = [to_f(p0), to_f(p1), to_f(p2), to_f(p3)];

    // Solo la cara de ARRIBA (axis=1, no backface: el agua mirando hacia
    // el aire) se achica según el nivel — ver `water_height_frac`. Las
    // caras laterales y la de abajo quedan a altura completa a
    // propósito: es mucho más simple que recortarlas también (el greedy
    // meshing fusiona rectángulos asumiendo bloques de una unidad
    // completa; tocar eso ahí rompería esa fusión) y el resultado visual
    // sigue siendo correcto — se ve como un borde/labio parado en la
    // orilla en vez de un hueco o un corte raro.
    if axis == 1 && !backface {
        if let BlockType::Water(level) = block {
            let y = corners[0][1] - (1.0 - water_height_frac(level));
            for c in &mut corners {
                c[1] = y;
            }
        }
    }

    // Ancho/alto del quad en bloques, para las UV locales (ver doc de Vertex).
    let quad_w = (du[0].abs() + du[1].abs() + du[2].abs()) as f32;
    let quad_h = (dv[0].abs() + dv[1].abs() + dv[2].abs()) as f32;

    let face = atlas::face_for(axis, backface);
    let tile_origin = atlas::uv_origin(atlas::tile_for(block, face));

    let start_index = vertices.len() as u32;

    // UV de textura (s = horizontal del atlas, t = vertical del atlas),
    // que NO es lo mismo que los ejes de barrido u/v del greedy meshing
    // de arriba (esos son sobre el mundo, elegidos genéricamente como
    // (axis+1)%3 / (axis+2)%3 para que el sweep 2D funcione en cualquier
    // dirección). Para las caras laterales (Norte/Sur/Este/Oeste) el
    // arte SÍ tiene una orientación fija (pasto arriba, tierra abajo en
    // grass_north.png etc.), así que acá forzamos que t siga siempre al
    // eje Y del mundo, sin importar si el sweep genérico lo puso en du o
    // en dv:
    //   - axis 2 (Norte/Sur): dv ya es el eje Y (v=(2+2)%3=Y), pero en
    //     t=0..h sin invertir quedaba con los pies (Y chico) apuntando
    //     al top de la textura (pasto) y la cabeza a tierra — al revés.
    //     Achicar Y (t más grande) = tierra, Y grande (t=0) = pasto.
    //   - axis 0 (Este/Oeste): acá el eje Y del mundo cae en du, no en
    //     dv (u=(0+1)%3=Y) — sin corregir, la textura quedaba rotada
    //     90°: la franja pasto/tierra corría a lo largo de Z en vez de
    //     Y. Mismo criterio de inversión que arriba, pero leyendo w (la
    //     extensión de du) en vez de h.
    // axis 1 (Arriba/Abajo) no tiene noción de "arriba" en su arte
    // (pasto-arriba y piedra son ruido sin orientación), así que se
    // queda con el mapeo genérico de siempre.
    let uvs = match axis {
        0 => [
            [0.0, quad_w],
            [0.0, 0.0],
            [quad_h, 0.0],
            [quad_h, quad_w],
        ],
        2 => [
            [0.0, quad_h],
            [quad_w, quad_h],
            [quad_w, 0.0],
            [0.0, 0.0],
        ],
        _ => [[0.0, 0.0], [quad_w, 0.0], [quad_w, quad_h], [0.0, quad_h]],
    };
    for (c, uv) in corners.into_iter().zip(uvs) {
        vertices.push(Vertex {
            position: c,
            normal,
            color,
            uv,
            tile_origin,
        });
    }

    // El orden de los índices depende de si es cara frontal o trasera,
    // para que el "winding order" (sentido horario/antihorario) sea
    // consistente y el backface culling de la GPU funcione bien.
    if backface {
        indices.extend_from_slice(&[
            start_index,
            start_index + 2,
            start_index + 1,
            start_index,
            start_index + 3,
            start_index + 2,
        ]);
    } else {
        indices.extend_from_slice(&[
            start_index,
            start_index + 1,
            start_index + 2,
            start_index,
            start_index + 2,
            start_index + 3,
        ]);
    }
}

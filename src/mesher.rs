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

use crate::chunk::{BlockType, Chunk, CHUNK_SIZE_X, CHUNK_SIZE_Y, CHUNK_SIZE_Z};
use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub color: [f32; 3],
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
pub fn generate_mesh(chunk: &Chunk) -> MeshData {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();

    for axis in 0..3 {
        for backface in [false, true] {
            greedy_sweep(chunk, axis, backface, &mut vertices, &mut indices);
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
    chunk: &Chunk,
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
                let a = get_block(chunk, x[0], x[1], x[2]);
                let mut xb = x;
                xb[axis] += 1;
                let b = get_block(chunk, xb[0], xb[1], xb[2]);

                mask[n] = if a.is_solid() != b.is_solid() {
                    if backface {
                        if b.is_solid() { Some(b) } else { None }
                    } else if a.is_solid() {
                        Some(a)
                    } else {
                        None
                    }
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

fn get_block(chunk: &Chunk, x: i32, y: i32, z: i32) -> BlockType {
    chunk.get(x, y, z)
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

    let color = block.color();
    let to_f = |p: [i32; 3]| [p[0] as f32, p[1] as f32, p[2] as f32];

    let start_index = vertices.len() as u32;

    let corners = [to_f(p0), to_f(p1), to_f(p2), to_f(p3)];
    for c in corners {
        vertices.push(Vertex {
            position: c,
            normal,
            color,
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

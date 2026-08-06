/// highlight.rs
/// Fase 5: contorno wireframe (12 aristas de un cubo) alrededor del bloque
/// que el raycast está apuntando en este momento — la señal visual de
/// "esto es lo que vas a romper con click izquierdo / al lado de esto se
/// coloca con click derecho". Comparte el mismo uniform de view-projection
/// que el mundo (`uniform_bind_group` en lib.rs), así que no hace falta
/// ningún cálculo de proyección propio acá, solo generar las líneas en
/// espacio de mundo.
use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct HighlightVertex {
    pub position: [f32; 3],
}

impl HighlightVertex {
    pub fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<HighlightVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                offset: 0,
                shader_location: 0,
                format: wgpu::VertexFormat::Float32x3,
            }],
        }
    }
}

/// Devuelve los 24 vértices (12 aristas × 2 puntos, para dibujar como
/// `PrimitiveTopology::LineList`) del contorno de un cubo unitario ubicado
/// en `block_pos`. Se infla levemente (`INSET`) hacia afuera de la
/// superficie real del bloque para evitar z-fighting con las caras del
/// mundo — si el contorno quedara exactamente pegado a la cara, a cierta
/// distancia parpadearía contra el terreno por la precisión limitada del
/// depth buffer.
const INSET: f32 = -0.0025;

pub fn build_block_outline(block_pos: (i32, i32, i32)) -> Vec<HighlightVertex> {
    let (bx, by, bz) = block_pos;
    let (x0, y0, z0) = (bx as f32 + INSET, by as f32 + INSET, bz as f32 + INSET);
    let (x1, y1, z1) = (
        bx as f32 + 1.0 - INSET,
        by as f32 + 1.0 - INSET,
        bz as f32 + 1.0 - INSET,
    );

    let corners = [
        [x0, y0, z0], // 0
        [x1, y0, z0], // 1
        [x1, y0, z1], // 2
        [x0, y0, z1], // 3
        [x0, y1, z0], // 4
        [x1, y1, z0], // 5
        [x1, y1, z1], // 6
        [x0, y1, z1], // 7
    ];

    // Las 12 aristas de un cubo, como pares de índices en `corners`.
    const EDGES: [(usize, usize); 12] = [
        (0, 1), (1, 2), (2, 3), (3, 0), // cara inferior
        (4, 5), (5, 6), (6, 7), (7, 4), // cara superior
        (0, 4), (1, 5), (2, 6), (3, 7), // verticales
    ];

    EDGES
        .iter()
        .flat_map(|&(a, b)| {
            [
                HighlightVertex { position: corners[a] },
                HighlightVertex { position: corners[b] },
            ]
        })
        .collect()
}

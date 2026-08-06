/// clouds.rs
/// Geometría de la capa de nubes: un solo quad gigante horizontal (2
/// triángulos) en coordenadas LOCALES relativas a la cámara — es decir,
/// `position` acá es un desplazamiento en X/Z desde donde esté parada la
/// cámara en ese frame, no una posición de mundo fija. El shader
/// (`clouds_shader.wgsl`) le suma `camera_pos.xz` y fija la altura en
/// `CLOUD_HEIGHT`, así que el plano "sigue" al jugador sin que haga falta
/// reconstruir este buffer nunca (se crea una sola vez en `State::new`).
///
/// El patrón de nubes en sí (manchas, forma, movimiento) es 100% procedural
/// en el fragment shader vía ruido — no hay textura ni malla real de nubes,
/// por eso este quad no necesita más que 4 esquinas.
use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct CloudVertex {
    /// (x, _, z): desplazamiento local desde la cámara. La componente Y
    /// se ignora en el vertex shader (la altura real sale de la
    /// constante `CLOUD_HEIGHT` del lado del shader), pero la dejamos en
    /// 0.0 acá para poder reusar `Float32x3` sin un layout especial.
    pub position: [f32; 3],
}

impl CloudVertex {
    pub fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<CloudVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                offset: 0,
                shader_location: 0,
                format: wgpu::VertexFormat::Float32x3,
            }],
        }
    }
}

/// Arma los 6 vértices (2 triángulos, sin index buffer porque son tan
/// pocos que no vale la pena) de un quad cuadrado de lado `2 * extent`
/// centrado en el origen local (osea, en la cámara, una vez que el
/// vertex shader aplique el offset). `extent` tiene que cubrir la
/// distancia de niebla más larga posible (ver `MAX_RENDER_RADIUS` en
/// lib.rs) para que el jugador nunca vea el borde recto del plano —
/// tiene que perderse en la niebla antes de llegar ahí.
pub fn build_cloud_plane(extent: f32) -> Vec<CloudVertex> {
    let p = |x: f32, z: f32| CloudVertex {
        position: [x, 0.0, z],
    };
    vec![
        p(-extent, -extent),
        p(extent, -extent),
        p(extent, extent),
        p(-extent, -extent),
        p(extent, extent),
        p(-extent, extent),
    ]
}

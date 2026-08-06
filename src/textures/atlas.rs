/// atlas.rs
/// Layout del atlas de texturas: una sola imagen con todos los tiles de
/// bloque en una grilla, en vez de una textura por bloque (así el mesh de
/// un chunk entero puede usar un solo bind group de textura, sin importar
/// cuántos tipos de bloque distintos tenga).
///
/// Si el atlas (assets/textures/atlas.png) cambia de tamaño de grilla,
/// actualizar ATLAS_COLS/ATLAS_ROWS acá Y las mismas constantes en
/// shaders/shader.wgsl (el shader las necesita para el `fract()` que
/// reubica la UV local del quad dentro del tile).
use crate::environment::chunk::BlockType;

pub const ATLAS_COLS: u32 = 4;
pub const ATLAS_ROWS: u32 = 4;
pub const TILE_PX: u32 = 16;

/// Cara de un quad, en términos de textura (no de eje X/Y/Z): "Top" y
/// "Bottom" son siempre el eje vertical (Y), "Side" es cualquiera de los
/// 4 costados verticales (X o Z) — la mayoría de los bloques no necesitan
/// distinguir entre sus 4 costados.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    Top,
    Bottom,
    Side,
}

/// El mesher recorre los ejes como índices 0/1/2 (X/Y/Z, ver `dims()` en
/// mesher.rs); acá lo traducimos a la semántica de textura. Axis 1 = Y.
pub fn face_for(axis: usize, backface: bool) -> Face {
    if axis == 1 {
        if backface {
            Face::Bottom
        } else {
            Face::Top
        }
    } else {
        Face::Side
    }
}

// Coordenadas (columna, fila) de cada tile dentro de la grilla del atlas.
// Fila 0 = fila superior de la imagen. Dejamos huecos (filas 1-3) para
// agregar más bloques sin tener que reacomodar los existentes.
const TILE_GRASS_TOP: (u32, u32) = (0, 0);
const TILE_GRASS_SIDE: (u32, u32) = (1, 0);
const TILE_DIRT: (u32, u32) = (2, 0);
const TILE_STONE: (u32, u32) = (3, 0);

/// Tile que le corresponde a un bloque+cara. `BlockType::Air` no debería
/// llegar acá nunca (el mesher no genera caras para aire), pero devolvemos
/// piedra como fallback inofensivo en vez de entrar en pánico.
pub fn tile_for(block: BlockType, face: Face) -> (u32, u32) {
    match (block, face) {
        (BlockType::Grass, Face::Top) => TILE_GRASS_TOP,
        (BlockType::Grass, Face::Bottom) => TILE_DIRT,
        (BlockType::Grass, Face::Side) => TILE_GRASS_SIDE,
        (BlockType::Dirt, _) => TILE_DIRT,
        (BlockType::Stone, _) => TILE_STONE,
        (BlockType::Air, _) => TILE_STONE,
    }
}

/// Esquina superior-izquierda de un tile, en coordenadas UV normalizadas
/// (0..1) dentro del atlas completo.
pub fn uv_origin(tile: (u32, u32)) -> [f32; 2] {
    [
        tile.0 as f32 / ATLAS_COLS as f32,
        tile.1 as f32 / ATLAS_ROWS as f32,
    ]
}

/// Tamaño de un tile en UV normalizadas (0..1).
pub fn tile_size_uv() -> [f32; 2] {
    [1.0 / ATLAS_COLS as f32, 1.0 / ATLAS_ROWS as f32]
}

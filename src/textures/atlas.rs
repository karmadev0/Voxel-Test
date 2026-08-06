/// atlas.rs
/// Layout del atlas de texturas: una sola imagen con todos los tiles de
/// bloque en una grilla, en vez de una textura por bloque (así el mesh de
/// un chunk entero puede usar un solo bind group de textura, sin importar
/// cuántos tipos de bloque distintos tenga).
///
/// El atlas (antes assets/textures/atlas.png a mano) ahora lo arma
/// build.rs en cada compilación, combinando los PNG sueltos por cara de
/// assets/textures/blocks/ (ver build.rs y assets/textures/blocks.txt).
/// Cada bloque de blocks.txt ocupa una fila completa de 6 columnas
/// (Top/Bottom/North/South/East/West, ver FACE_COLUMN_ORDER), en el mismo
/// orden en que aparece en blocks.txt.
///
/// Si blocks.txt gana o pierde una línea, el atlas cambia de alto:
/// actualizar ATLAS_ROWS acá Y la misma constante en shaders/shader.wgsl
/// (el shader la necesita para el `fract()` que reubica la UV local del
/// quad dentro del tile). build.rs imprime un cargo:warning con el valor
/// esperado si te olvidás.
use crate::environment::chunk::BlockType;

pub const ATLAS_COLS: u32 = 6;
// Debe coincidir con la cantidad de líneas (no vacías) de
// assets/textures/blocks.txt. Hoy: stone, dirt, grass.
pub const ATLAS_ROWS: u32 = 3;
pub const TILE_PX: u32 = 16;

/// Cara de un quad en términos de textura, ahora con soporte real para
/// las 6 caras de un cubo. Top/Bottom siguen siendo siempre el eje
/// vertical (Y). North/South/East/West son las 4 caras laterales — un
/// bloque simple (piedra, tierra) puede seguir devolviendo el mismo tile
/// para las 4, pero un bloque como horno o tronco puede diferenciarlas.
///
/// Convención de orientación (mundo, no pantalla): +X = East, -X = West,
/// +Z = South, -Z = North. Ver `face_for` para cómo se deriva del eje del
/// mesher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    Top,
    Bottom,
    North,
    South,
    East,
    West,
}

impl Face {
    /// Las 4 caras laterales, en el mismo orden de columnas que usa
    /// `cube_template_tool` al escribir una fila de 6 caras en el atlas
    /// (ver `FACE_COLUMN_ORDER`).
    pub fn is_side(self) -> bool {
        matches!(self, Face::North | Face::South | Face::East | Face::West)
    }
}

/// El mesher recorre los ejes como índices 0/1/2 (X/Y/Z, ver `dims()` en
/// mesher.rs) y una dirección (`backface`: false = positiva, true =
/// negativa). Acá lo traducimos a la semántica de textura de cada cara.
pub fn face_for(axis: usize, backface: bool) -> Face {
    match axis {
        1 => {
            if backface {
                Face::Bottom
            } else {
                Face::Top
            }
        }
        0 => {
            if backface {
                Face::West
            } else {
                Face::East
            }
        }
        _ => {
            if backface {
                Face::North
            } else {
                Face::South
            }
        }
    }
}

// Fila de cada bloque dentro del atlas, en el mismo orden que
// assets/textures/blocks.txt (build.rs arma el atlas en ese orden). Si
// agregás una línea a blocks.txt, agregá la constante correspondiente acá
// (y sumá 1 a ATLAS_ROWS arriba, y en shaders/shader.wgsl).
const ROW_STONE: u32 = 0;
const ROW_DIRT: u32 = 1;
const ROW_GRASS: u32 = 2;

/// Orden de columnas que usa build.rs al volcar los 6 PNG sueltos de un
/// bloque (<bloque>_top.png ... <bloque>_west.png) en una fila del atlas.
/// Antes lo generaba `cube_template_tool` a partir de una plantilla
/// "cruz"; ahora build.rs lee directamente los PNG sueltos de
/// assets/textures/blocks/, pero el orden de columnas es el mismo.
pub const FACE_COLUMN_ORDER: [Face; 6] = [
    Face::Top,
    Face::Bottom,
    Face::North,
    Face::South,
    Face::East,
    Face::West,
];

/// Tile de la fila de un bloque (una fila = sus 6 PNG sueltos volcados por
/// build.rs), según el orden de `FACE_COLUMN_ORDER`.
pub fn tile_for_row(row: u32, face: Face) -> (u32, u32) {
    let col = FACE_COLUMN_ORDER
        .iter()
        .position(|f| *f == face)
        .expect("FACE_COLUMN_ORDER cubre las 6 variantes de Face");
    (col as u32, row)
}

/// Tile que le corresponde a un bloque+cara. `BlockType::Air` no debería
/// llegar acá nunca (el mesher no genera caras para aire), pero devolvemos
/// piedra como fallback inofensivo en vez de entrar en pánico.
///
/// Grass tiene sus 6 PNG sueltos igual que cualquier otro bloque
/// (grass_top.png, grass_bottom.png, grass_north.png, ...) — que
/// grass_bottom.png sea una copia del contenido de dirt no lo sabe este
/// código, es una decisión de arte que vive en assets/textures/blocks/.
pub fn tile_for(block: BlockType, face: Face) -> (u32, u32) {
    match block {
        BlockType::Stone => tile_for_row(ROW_STONE, face),
        BlockType::Dirt => tile_for_row(ROW_DIRT, face),
        BlockType::Grass => tile_for_row(ROW_GRASS, face),
        BlockType::Air => tile_for_row(ROW_STONE, face),
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

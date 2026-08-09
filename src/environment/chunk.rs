/// chunk.rs
/// Representa un chunk de voxels y el algoritmo de "greedy meshing":
/// en vez de dibujar una cara por cada bloque individual (lo cual sería
/// carísimo en una CPU de 2 núcleos como la del Celeron N4000), fusionamos
/// caras adyacentes del mismo tipo en rectángulos grandes. Esto reduce
/// drásticamente el número de triángulos que la GPU tiene que procesar.

use serde::{Deserialize, Serialize};

pub const CHUNK_SIZE_X: usize = 16;
pub const CHUNK_SIZE_Y: usize = 64; // reducido respecto al original (256) para el primer entregable
pub const CHUNK_SIZE_Z: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum BlockType {
    Air,
    Grass,
    Dirt,
    Stone,
    Wood,
    Leaves,
    // A partir de acá: SIEMPRE agregar variantes nuevas al final. El
    // guardado de chunks usa bincode, que serializa el enum por el
    // índice de la variante (0, 1, 2...) — insertar algo en el medio
    // corrompería silenciosamente todos los mundos guardados con la
    // versión anterior (un `Stone` guardado podría leerse como otra
    // cosa después de la migración).
    //
    /// Agua que fluye: `0` = manantial/fuente (nunca se seca sola, solo
    /// si la rompe el jugador o la absorbe una esponja cercana).
    /// `1..=7` = agua que llegó fluyendo desde una fuente, más lejos =
    /// número más alto; si en algún momento deja de tener un vecino
    /// con nivel menor (o una fuente) alimentándola, se seca sola (ver
    /// `environment/fluids.rs`).
    Water(u8),
    /// Absorbe el agua de los alrededores de a poco (ver
    /// `environment/fluids.rs`), igual que en Minecraft. Bloque sólido
    /// normal en todo lo demás.
    Sponge,
}

impl BlockType {
    /// Color plano por tipo de bloque. Fase 2 lo reemplaza por texturas reales
    /// (atlas de texturas + UV mapping).
    pub fn color(&self) -> [f32; 3] {
        match self {
            BlockType::Air => [0.0, 0.0, 0.0],
            BlockType::Grass => [0.36, 0.62, 0.28],
            BlockType::Dirt => [0.46, 0.33, 0.20],
            BlockType::Stone => [0.5, 0.5, 0.52],
            BlockType::Wood => [0.42, 0.30, 0.18],
            BlockType::Leaves => [0.27, 0.42, 0.20],
            BlockType::Water(_) => [0.2, 0.39, 0.82],
            BlockType::Sponge => [0.78, 0.74, 0.26],
        }
    }

    /// `Leaves` queda sólida por simplicidad, igual que el resto de los
    /// bloques del engine hoy (no hay transparencia/alpha-blend en el
    /// mesher todavía). Esto es a propósito TAMBIÉN el criterio que usa
    /// el mesher para decidir si dibuja una cara (ver
    /// `mesher::greedy_sweep`): cualquier cosa que no sea `Air` se
    /// considera "opaca" a los efectos del dibujado — incluida el agua,
    /// que sí se ve aunque el jugador pueda atravesarla nadando (ver
    /// `is_collidable`, que es la que de verdad importa para colisión
    /// física).
    pub fn is_solid(&self) -> bool {
        !matches!(self, BlockType::Air)
    }

    /// Para colisión física (`physics/player.rs`) y raycasting
    /// (`environment/world.rs::raycast`): a diferencia de `is_solid`
    /// (que el mesher usa solo para decidir "aire vs. no aire" al
    /// dibujar caras), acá el agua NO cuenta como sólida — el jugador
    /// puede caminar/nadar a través suyo. La esponja sí es sólida
    /// (se puede pisar).
    pub fn is_collidable(&self) -> bool {
        !matches!(self, BlockType::Air | BlockType::Water(_))
    }

    /// Nombre en mayúsculas para mostrar en overlays de texto (panel de
    /// debug, ver logic/ui_overlay.rs) — la fuente bitmap del engine solo
    /// soporta A-Z/0-9/algunos símbolos, así que ya viene en mayúsculas.
    pub fn label(&self) -> &'static str {
        match self {
            BlockType::Air => "AIRE",
            BlockType::Grass => "PASTO",
            BlockType::Dirt => "TIERRA",
            BlockType::Stone => "PIEDRA",
            BlockType::Wood => "MADERA",
            BlockType::Leaves => "HOJAS",
            BlockType::Water(_) => "AGUA",
            BlockType::Sponge => "ESPONJA",
        }
    }

    /// Cantidad de slots de la hotbar (ver `logic/touch.rs::rect_hotbar`
    /// y `logic/ui_overlay.rs::build_hotbar`) — único lugar donde vive
    /// este número, para que ningún llamador se desincronice del resto.
    pub const HOTBAR_SLOTS: u8 = 7;

    /// Bloque que corresponde al slot `n` (1..=HOTBAR_SLOTS) de la
    /// hotbar. Único lugar donde vive este mapeo — antes estaba
    /// duplicado a mano en tres puntos distintos (dibujado, tap táctil
    /// y teclado 1-7), lo que es una receta para que se desincronicen.
    /// Colocar `Water` desde acá siempre pone una fuente (`Water(0)`):
    /// el jugador nunca coloca agua "a medio secar".
    pub fn from_hotbar_slot(n: u8) -> BlockType {
        match n {
            1 => BlockType::Grass,
            2 => BlockType::Dirt,
            3 => BlockType::Stone,
            4 => BlockType::Wood,
            5 => BlockType::Leaves,
            6 => BlockType::Water(0),
            _ => BlockType::Sponge,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub blocks: Vec<BlockType>, // indexado con index(x,y,z)
}

impl Chunk {
    pub fn empty() -> Self {
        Self {
            blocks: vec![BlockType::Air; CHUNK_SIZE_X * CHUNK_SIZE_Y * CHUNK_SIZE_Z],
        }
    }

    #[inline]
    pub fn index(x: usize, y: usize, z: usize) -> usize {
        x + y * CHUNK_SIZE_X + z * CHUNK_SIZE_X * CHUNK_SIZE_Y
    }

    #[inline]
    pub fn get(&self, x: i32, y: i32, z: i32) -> BlockType {
        if x < 0
            || y < 0
            || z < 0
            || x >= CHUNK_SIZE_X as i32
            || y >= CHUNK_SIZE_Y as i32
            || z >= CHUNK_SIZE_Z as i32
        {
            return BlockType::Air; // fuera del chunk => se trata como aire (borde visible)
        }
        self.blocks[Self::index(x as usize, y as usize, z as usize)]
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, z: usize, block: BlockType) {
        self.blocks[Self::index(x, y, z)] = block;
    }
}

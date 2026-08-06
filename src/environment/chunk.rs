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
        }
    }

    pub fn is_solid(&self) -> bool {
        !matches!(self, BlockType::Air)
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

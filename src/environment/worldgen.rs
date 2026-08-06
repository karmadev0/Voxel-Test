/// worldgen.rs
/// Genera el terreno de un chunk usando ruido Perlin. Por cada columna (x, z)
/// calculamos una altura, y rellenamos el chunk con capas: piedra abajo,
/// tierra encima, y pasto en la superficie. Esto corre en un hilo aparte
/// (ver rayon en main.rs) para no bloquear el frame de renderizado.

use crate::environment::chunk::{BlockType, Chunk, CHUNK_SIZE_X, CHUNK_SIZE_Y, CHUNK_SIZE_Z};
use noise::{NoiseFn, Perlin};

pub struct WorldGenerator {
    noise: Perlin,
    seed: u32,
}

impl WorldGenerator {
    pub fn new(seed: u32) -> Self {
        Self {
            noise: Perlin::new(seed),
            seed,
        }
    }

    /// Genera un chunk ubicado en la posición de mundo (chunk_x, chunk_z),
    /// en unidades de chunks (no de bloques).
    pub fn generate_chunk(&self, chunk_x: i32, chunk_z: i32) -> Chunk {
        let mut chunk = Chunk::empty();

        for local_x in 0..CHUNK_SIZE_X {
            for local_z in 0..CHUNK_SIZE_Z {
                let world_x = chunk_x * CHUNK_SIZE_X as i32 + local_x as i32;
                let world_z = chunk_z * CHUNK_SIZE_Z as i32 + local_z as i32;

                let height = self.height_at(world_x, world_z);

                for y in 0..height.min(CHUNK_SIZE_Y) {
                    let block = if y == height - 1 {
                        BlockType::Grass
                    } else if y >= height.saturating_sub(4) {
                        BlockType::Dirt
                    } else {
                        BlockType::Stone
                    };
                    chunk.set(local_x, y, local_z, block);
                }
            }
        }

        chunk
    }

    /// Combina varias octavas de ruido para un terreno con colinas suaves
    /// y algo de detalle fino encima.
    fn height_at(&self, x: i32, z: i32) -> usize {
        let base_freq = 0.02;
        let detail_freq = 0.08;

        let base = self.noise.get([x as f64 * base_freq, z as f64 * base_freq]);
        let detail = self
            .noise
            .get([x as f64 * detail_freq + 100.0, z as f64 * detail_freq + 100.0]);

        let combined = base * 0.8 + detail * 0.2;
        let normalized = (combined + 1.0) / 2.0; // de [-1,1] a [0,1]

        let min_height = 8.0;
        let max_height = (CHUNK_SIZE_Y as f64) - 4.0;
        let height = min_height + normalized * (max_height - min_height);

        height.round().max(1.0) as usize
    }

    pub fn seed(&self) -> u32 {
        self.seed
    }
}

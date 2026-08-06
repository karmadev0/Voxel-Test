/// world.rs
/// Fase 2: el mundo deja de ser una lista fija de meshes generados una sola
/// vez, y pasa a ser un mapa de chunks *editable* (HashMap<(chunk_x, chunk_z), Chunk>).
/// Esto permite romper/colocar bloques y regenerar solo el chunk afectado,
/// en vez de re-generar todo el mundo.

use crate::environment::chunk::{BlockType, Chunk, CHUNK_SIZE_X, CHUNK_SIZE_Y, CHUNK_SIZE_Z};
use crate::environment::mesher;
use crate::environment::worldgen::WorldGenerator;
use glam::Vec3;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

pub struct World {
    pub chunks: HashMap<(i32, i32), Chunk>,
    generator: Arc<WorldGenerator>,
    save_dir: PathBuf,
    // Chunks modificados desde la última vez que se guardaron a disco
    // (por romper/colocar bloques). Solo estos se re-escriben al guardar,
    // en vez de reescribir el mundo entero cada vez.
    dirty_for_save: HashSet<(i32, i32)>,
}

impl World {
    pub fn new(seed: u32) -> Self {
        Self {
            chunks: HashMap::new(),
            generator: Arc::new(WorldGenerator::new(seed)),
            save_dir: PathBuf::from("world_save"),
            dirty_for_save: HashSet::new(),
        }
    }

    fn chunk_file_path(&self, cx: i32, cz: i32) -> PathBuf {
        self.save_dir.join(format!("chunk_{}_{}.bin", cx, cz))
    }

    /// Genera (o carga desde disco, si ya existe un guardado previo) el
    /// área de chunks alrededor del origen.
    pub fn generate_area(&mut self, radius: i32) {
        for cx in -radius..=radius {
            for cz in -radius..=radius {
                let chunk = self.load_or_generate_chunk(cx, cz);
                self.chunks.insert((cx, cz), chunk);
            }
        }
    }

    fn load_or_generate_chunk(&self, cx: i32, cz: i32) -> Chunk {
        self.loader().load_or_generate(cx, cz)
    }

    /// Handle liviano y clonable, sin ningún préstamo de `&self`, que sabe
    /// generar o cargar desde disco un chunk puntual. Pensado para
    /// mandarse a `rayon::spawn` en un hilo de fondo (streaming
    /// asincrónico): el hilo principal sigue mutando `World.chunks`
    /// libremente mientras un hilo de fondo genera el próximo chunk, sin
    /// pisarse porque el loader no toca `chunks` para nada — el resultado
    /// se inserta después, desde el hilo principal, cuando llega.
    pub fn loader(&self) -> ChunkLoader {
        ChunkLoader {
            generator: Arc::clone(&self.generator),
            save_dir: self.save_dir.clone(),
        }
    }

    /// Inserta un chunk ya generado (típicamente por un `ChunkLoader` en
    /// un hilo de fondo) sin volver a generarlo.
    pub fn insert_loaded_chunk(&mut self, cx: i32, cz: i32, chunk: Chunk) {
        self.chunks.insert((cx, cz), chunk);
    }

    /// Guarda a disco únicamente los chunks modificados desde el último
    /// guardado (no todo el mundo), en `world_save/chunk_X_Z.bin`.
    pub fn save_dirty_chunks(&mut self) -> usize {
        if self.dirty_for_save.is_empty() {
            return 0;
        }
        if let Err(e) = fs::create_dir_all(&self.save_dir) {
            log::error!("No se pudo crear la carpeta de guardado: {:?}", e);
            return 0;
        }

        let mut saved = 0;
        for &(cx, cz) in &self.dirty_for_save {
            if let Some(chunk) = self.chunks.get(&(cx, cz)) {
                match bincode::serialize(chunk) {
                    Ok(bytes) => {
                        if let Err(e) = fs::write(self.chunk_file_path(cx, cz), bytes) {
                            log::error!("Error guardando chunk ({}, {}): {:?}", cx, cz, e);
                            continue;
                        }
                        saved += 1;
                    }
                    Err(e) => log::error!("Error serializando chunk ({}, {}): {:?}", cx, cz, e),
                }
            }
        }
        self.dirty_for_save.clear();
        saved
    }

    /// Carga (desde disco si existe guardado, si no genera) un chunk
    /// puntual y lo inserta en el mundo, de forma **síncrona** (bloquea
    /// el hilo que la llama). La usa `generate_area` para la carga
    /// inicial; el streaming dinámico usa en cambio `loader()` +
    /// `rayon::spawn` para no bloquear el frame (ver `update_chunk_streaming`
    /// en lib.rs).
    pub fn load_chunk(&mut self, cx: i32, cz: i32) {
        if self.chunks.contains_key(&(cx, cz)) {
            return;
        }
        let chunk = self.load_or_generate_chunk(cx, cz);
        self.chunks.insert((cx, cz), chunk);
    }

    /// Descarga un chunk: si tiene cambios sin guardar, los guarda primero
    /// (no perdemos ediciones por alejarnos caminando), y después lo saca
    /// de memoria.
    pub fn unload_chunk(&mut self, cx: i32, cz: i32) {
        if self.dirty_for_save.remove(&(cx, cz)) {
            if let Some(chunk) = self.chunks.get(&(cx, cz)) {
                if let Ok(bytes) = bincode::serialize(chunk) {
                    let _ = fs::create_dir_all(&self.save_dir);
                    let _ = fs::write(self.chunk_file_path(cx, cz), bytes);
                }
            }
        }
        self.chunks.remove(&(cx, cz));
    }

    /// Convierte una posición de mundo (bloques) a coordenadas de chunk.
    pub fn world_pos_to_chunk(x: f32, z: f32) -> (i32, i32) {
        (
            (x as i32).div_euclid(CHUNK_SIZE_X as i32),
            (z as i32).div_euclid(CHUNK_SIZE_Z as i32),
        )
    }

    /// Convierte coordenadas de mundo (bloque) a (chunk, coordenada local).
    fn world_to_chunk(x: i32, y: i32, z: i32) -> ((i32, i32), (i32, i32, i32)) {
        let chunk_x = x.div_euclid(CHUNK_SIZE_X as i32);
        let chunk_z = z.div_euclid(CHUNK_SIZE_Z as i32);
        let local_x = x.rem_euclid(CHUNK_SIZE_X as i32);
        let local_z = z.rem_euclid(CHUNK_SIZE_Z as i32);
        ((chunk_x, chunk_z), (local_x, y, local_z))
    }

    pub fn get_block(&self, x: i32, y: i32, z: i32) -> BlockType {
        if y < 0 || y >= CHUNK_SIZE_Y as i32 {
            return BlockType::Air;
        }
        let (chunk_pos, (lx, ly, lz)) = Self::world_to_chunk(x, y, z);
        match self.chunks.get(&chunk_pos) {
            Some(chunk) => chunk.get(lx, ly, lz),
            None => BlockType::Air, // chunk no generado todavía => se trata como aire
        }
    }

    /// Arma el `ChunkNeighborhood` de (cx, cz): el chunk pedido más
    /// referencias a los 4 vecinos en X/Z que ya estén cargados en
    /// memoria (los que no estén cargados quedan en `None`, y el mesher
    /// los trata como aire — lo mismo que pasaba antes en todos los
    /// bordes, ahora limitado solo a bordes con el mundo todavía sin
    /// cargar del todo). Devuelve `None` si el chunk pedido ni siquiera
    /// está cargado.
    pub fn chunk_neighborhood(&self, cx: i32, cz: i32) -> Option<mesher::ChunkNeighborhood<'_>> {
        let center = self.chunks.get(&(cx, cz))?;
        Some(mesher::ChunkNeighborhood {
            center,
            neg_x: self.chunks.get(&(cx - 1, cz)),
            pos_x: self.chunks.get(&(cx + 1, cz)),
            neg_z: self.chunks.get(&(cx, cz - 1)),
            pos_z: self.chunks.get(&(cx, cz + 1)),
        })
    }

    /// Genera el `MeshData` de (cx, cz) con culling consciente de sus
    /// vecinos ya cargados (ver `chunk_neighborhood`). Punto de entrada
    /// único usado por la carga inicial, el re-mallado tras romper/
    /// colocar, y el re-mallado de vecinos tras el streaming — para no
    /// repetir la misma lógica de "armar vecindario + mallear" en cada
    /// lugar que necesita un mesh actualizado.
    pub fn generate_chunk_mesh(&self, cx: i32, cz: i32) -> Option<mesher::MeshData> {
        self.chunk_neighborhood(cx, cz)
            .map(|nb| mesher::generate_mesh(&nb))
    }

    /// Coordenadas de los 4 vecinos directos (X/Z) de un chunk, sin
    /// diagonales — el greedy meshing nunca necesita vecinos diagonales
    /// (ver comentario en `ChunkNeighborhood`).
    pub fn direct_neighbors(cx: i32, cz: i32) -> [(i32, i32); 4] {
        [(cx - 1, cz), (cx + 1, cz), (cx, cz - 1), (cx, cz + 1)]
    }

    /// Coloca/rompe un bloque. Devuelve la lista de chunks que hay que
    /// re-mallear: el chunk modificado, y además cualquier chunk vecino
    /// si el bloque está justo en el borde (porque el mesh de un chunk
    /// depende de qué hay al otro lado del borde).
    pub fn set_block(&mut self, x: i32, y: i32, z: i32, block: BlockType) -> Vec<(i32, i32)> {
        if y < 0 || y >= CHUNK_SIZE_Y as i32 {
            return vec![];
        }
        let (chunk_pos, (lx, ly, lz)) = Self::world_to_chunk(x, y, z);

        let chunk = self
            .chunks
            .entry(chunk_pos)
            .or_insert_with(Chunk::empty);
        chunk.set(lx as usize, ly as usize, lz as usize, block);

        let mut dirty = vec![chunk_pos];
        self.dirty_for_save.insert(chunk_pos);
        if lx == 0 {
            dirty.push((chunk_pos.0 - 1, chunk_pos.1));
        }
        if lx == CHUNK_SIZE_X as i32 - 1 {
            dirty.push((chunk_pos.0 + 1, chunk_pos.1));
        }
        if lz == 0 {
            dirty.push((chunk_pos.0, chunk_pos.1 - 1));
        }
        if lz == CHUNK_SIZE_Z as i32 - 1 {
            dirty.push((chunk_pos.0, chunk_pos.1 + 1));
        }
        dirty
    }
}

/// Handle clonable y `Send` para generar/cargar un chunk sin acceso al
/// resto de `World`. `Arc<WorldGenerator>` hace que clonarlo sea barato
/// (un incremento de contador atómico, no una copia del ruido Perlin), así
/// que se puede clonar uno por cada tarea que se manda a `rayon::spawn`
/// sin costo real.
#[derive(Clone)]
pub struct ChunkLoader {
    generator: Arc<WorldGenerator>,
    save_dir: PathBuf,
}

impl ChunkLoader {
    pub fn load_or_generate(&self, cx: i32, cz: i32) -> Chunk {
        let path = self.save_dir.join(format!("chunk_{}_{}.bin", cx, cz));
        if let Ok(bytes) = fs::read(&path) {
            if let Ok(chunk) = bincode::deserialize::<Chunk>(&bytes) {
                return chunk;
            }
            log::warn!(
                "No se pudo leer el guardado de chunk ({}, {}), se regenera desde cero.",
                cx,
                cz
            );
        }
        self.generator.generate_chunk(cx, cz)
    }
}

pub struct RaycastHit {
    /// Posición (en coordenadas de bloque) del bloque sólido golpeado.
    pub block_pos: (i32, i32, i32),
    /// Posición del bloque de aire justo antes del impacto — ahí es donde
    /// se coloca un bloque nuevo si el jugador hace click derecho.
    pub place_pos: (i32, i32, i32),
}

/// Raycasting por DDA (Digital Differential Analyzer) sobre la grilla de
/// voxels: avanzamos el rayo celda por celda (no en pasos fijos de
/// distancia), lo cual es exacto y no se puede "saltear" un bloque fino,
/// a diferencia de un raymarch con paso constante.
pub fn raycast(world: &World, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<RaycastHit> {
    let dir = direction.normalize();

    let mut x = origin.x.floor() as i32;
    let mut y = origin.y.floor() as i32;
    let mut z = origin.z.floor() as i32;

    let step_x = if dir.x > 0.0 { 1 } else { -1 };
    let step_y = if dir.y > 0.0 { 1 } else { -1 };
    let step_z = if dir.z > 0.0 { 1 } else { -1 };

    let t_delta_x = if dir.x != 0.0 { (1.0 / dir.x).abs() } else { f32::INFINITY };
    let t_delta_y = if dir.y != 0.0 { (1.0 / dir.y).abs() } else { f32::INFINITY };
    let t_delta_z = if dir.z != 0.0 { (1.0 / dir.z).abs() } else { f32::INFINITY };

    let next_boundary = |pos: f32, step: i32| -> f32 {
        if step > 0 {
            pos.floor() + 1.0 - pos
        } else {
            pos - pos.floor()
        }
    };

    let mut t_max_x = if dir.x != 0.0 {
        next_boundary(origin.x, step_x) / dir.x.abs()
    } else {
        f32::INFINITY
    };
    let mut t_max_y = if dir.y != 0.0 {
        next_boundary(origin.y, step_y) / dir.y.abs()
    } else {
        f32::INFINITY
    };
    let mut t_max_z = if dir.z != 0.0 {
        next_boundary(origin.z, step_z) / dir.z.abs()
    } else {
        f32::INFINITY
    };

    let mut last_empty = (x, y, z);
    let mut traveled = 0.0;

    while traveled < max_distance {
        if world.get_block(x, y, z).is_solid() {
            return Some(RaycastHit {
                block_pos: (x, y, z),
                place_pos: last_empty,
            });
        }
        last_empty = (x, y, z);

        if t_max_x < t_max_y && t_max_x < t_max_z {
            x += step_x;
            traveled = t_max_x;
            t_max_x += t_delta_x;
        } else if t_max_y < t_max_z {
            y += step_y;
            traveled = t_max_y;
            t_max_y += t_delta_y;
        } else {
            z += step_z;
            traveled = t_max_z;
            t_max_z += t_delta_z;
        }
    }

    None
}

/// worldgen.rs
/// Genera el terreno de un chunk usando ruido Perlin. Por cada columna (x, z)
/// calculamos una altura, y rellenamos el chunk con capas: piedra abajo,
/// tierra encima, y pasto en la superficie. Esto corre en un hilo aparte
/// (ver rayon en main.rs) para no bloquear el frame de renderizado.
///
/// Encima del terreno, `generate_chunk` también estampa árboles (ver
/// `tree_at`/`stamp_tree` más abajo) con el mismo requisito de pureza: cada
/// chunk se genera de forma totalmente independiente, en threads de rayon,
/// sin acceso a los chunks vecinos.

use crate::environment::chunk::{BlockType, Chunk, CHUNK_SIZE_X, CHUNK_SIZE_Y, CHUNK_SIZE_Z};
use noise::{NoiseFn, Perlin};

/// Cuántos bloques de margen, más allá de sus propias 16x16 columnas,
/// recorre `generate_chunk` en busca de raíces de árbol. Un árbol
/// enraizado del otro lado del borde puede igual estampar hojas (radio
/// de copa 2) dentro de este chunk, así que el margen tiene que cubrir
/// ese radio.
const TREE_MARGIN: i32 = 2;

/// A qué altura el mundo se considera "bajo el mar": toda columna cuyo
/// terreno (`height_at`) quede por debajo de esto se inunda desde ahí
/// hasta acá con agua fuente — así aparecen lagos/mares en los valles,
/// sin tocar el resto del mapa (que queda por encima, seco). Con
/// min_height=8 y max_height≈60 (ver `height_at`), 16 inunda una
/// franja angosta de los valles más bajos, no la mitad del mapa.
const SEA_LEVEL: usize = 16;

/// Alto mínimo garantizado de una caverna "queso" (ver `banded_y`): 4
/// bloques, cómodo para caminar parado sin agacharse en ningún punto.
const CAVE_BAND_CHEESE: i32 = 4;
/// Alto mínimo de un túnel "fideo" — un poco más angosto que las
/// cavernas a propósito, para que se sientan pasillo y no salón, pero
/// nunca por debajo de lo que el jugador puede atravesar caminando.
const CAVE_BAND_TUNNEL: i32 = 3;

/// Por debajo de esta altura absoluta, cualquier hueco de cueva (ver
/// `is_cave`) se genera lleno de agua en vez de aire — lagos
/// subterráneos, como los acuíferos de Minecraft. Deliberadamente más
/// bajo que `SEA_LEVEL` para que la mayoría de las cuevas exploradas
/// sigan secas; solo las más profundas se inundan.
const CAVE_WATER_LEVEL: i32 = 10;

/// A partir de esta altura de terreno (ver `height_at`, rango
/// 8..~60) se considera "montaña" a los efectos de dónde preferir que
/// aparezcan bocas de cueva (ver `entrance_zone`) — recreando el
/// paisaje de Minecraft, donde las entradas de cueva más memorables
/// están en las laderas, no en el pasto llano.
const MOUNTAIN_HEIGHT: usize = 40;

pub struct WorldGenerator {
    noise: Perlin,
    /// Ruido 3D aparte para las cuevas (semilla derivada, no la misma
    /// instancia que `noise`): si usáramos el mismo campo que la altura,
    /// las cuevas quedarían correlacionadas con la forma del terreno de
    /// superficie (cuevas más probables donde hay colinas, por ejemplo),
    /// que no es lo que queremos.
    cave_noise: Perlin,
    seed: u32,
}

/// Parámetros de un árbol, derivados de forma 100% determinística de su
/// columna raíz (ver `tree_at`). Dos llamadas con el mismo (wx, wz)
/// siempre dan el mismo árbol, lo evalúe el chunk que lo evalúe.
struct TreeSpec {
    trunk_height: i32,
}

impl WorldGenerator {
    pub fn new(seed: u32) -> Self {
        Self {
            noise: Perlin::new(seed),
            // XOR con una constante arbitraria: misma técnica que ya usa
            // `tree_at` para el segundo hash de altura de tronco (ver
            // `h2`), para derivar un campo de ruido "independiente" del
            // principal sin tener que manejar dos semillas por separado.
            cave_noise: Perlin::new(seed ^ 0xCAFE_F00D),
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
                    // Cuevas: la mayor parte del mapa mantiene el
                    // colchón bajo pasto/tierra (ver `is_cave`), así que
                    // acá abajo solo se corta piedra. Pero en las zonas
                    // raras de "entrada" ese colchón baja a 0 a
                    // propósito, y ahí sí hace falta poder tallar
                    // también tierra/pasto para que la cueva llegue de
                    // verdad hasta el aire — por eso este chequeo va
                    // antes de decidir el tipo de bloque, sin filtrar
                    // por tipo. El chunk arranca todo `Air`
                    // (`Chunk::empty()`), así que "no poner nada" es
                    // exactamente lo que hace falta para que quede
                    // hueco.
                    if self.is_cave(world_x, y as i32, world_z, height) {
                        // Cuevas profundas: en vez de dejar el hueco
                        // vacío, lo generamos ya inundado (lago
                        // subterráneo). No hace falta simulación para
                        // esto — nace así, completo, igual que el mar
                        // de superficie más abajo.
                        if (y as i32) <= CAVE_WATER_LEVEL {
                            chunk.set(local_x, y, local_z, BlockType::Water(0));
                        }
                        continue;
                    }

                    let block = if y == height - 1 {
                        BlockType::Grass
                    } else if y >= height.saturating_sub(4) {
                        BlockType::Dirt
                    } else {
                        BlockType::Stone
                    };

                    chunk.set(local_x, y, local_z, block);
                }

                // Mar/lago de superficie: si el terreno de esta columna
                // queda por debajo del nivel del mar, se rellena con
                // agua fuente desde la superficie hasta `SEA_LEVEL`.
                if height < SEA_LEVEL {
                    for y in height..SEA_LEVEL.min(CHUNK_SIZE_Y) {
                        chunk.set(local_x, y, local_z, BlockType::Water(0));
                    }
                }
            }
        }

        // Árboles: recorremos también un margen de columnas de chunks
        // vecinos (todavía no generados quizás), porque la copa de un
        // árbol enraizado justo al otro lado del borde puede pisar nuestro
        // territorio. `tree_at` es una función pura de (seed, wx, wz) — el
        // chunk vecino, cuando le toque generarse, va a llegar exactamente
        // al mismo árbol para esa misma columna y va a estampar su propia
        // parte. Ningún chunk espera al otro ni le escribe nada.
        let origin_x = chunk_x * CHUNK_SIZE_X as i32;
        let origin_z = chunk_z * CHUNK_SIZE_Z as i32;
        for wx in (origin_x - TREE_MARGIN)..(origin_x + CHUNK_SIZE_X as i32 + TREE_MARGIN) {
            for wz in (origin_z - TREE_MARGIN)..(origin_z + CHUNK_SIZE_Z as i32 + TREE_MARGIN) {
                if let Some(tree) = self.tree_at(wx, wz) {
                    self.stamp_tree(&mut chunk, origin_x, origin_z, wx, wz, &tree);
                }
            }
        }

        chunk
    }

    /// ¿Hay un árbol enraizado en la columna de mundo (wx, wz)? Función
    /// pura y sin estado (hash, no RNG): solo depende de `self.seed` y de
    /// (wx, wz). Nunca depende de qué chunk la está evaluando ni en qué
    /// orden, que es justamente lo que permite que dos chunks vecinos se
    /// pongan de acuerdo sobre el mismo árbol sin coordinarse.
    fn tree_at(&self, wx: i32, wz: i32) -> Option<TreeSpec> {
        let height = self.height_at(wx, wz);
        if height == 0 || height >= CHUNK_SIZE_Y {
            return None; // sin superficie válida, o ya toca el techo del mundo
        }
        if height <= SEA_LEVEL {
            return None; // bajo el agua (ver SEA_LEVEL): nada de árboles ahogados
        }

        // La superficie de toda columna hoy es siempre Grass (ver el loop
        // de arriba: el bloque en y = height - 1 siempre es Grass, no hay
        // biomas todavía) — salvo que una entrada de cueva rara (ver
        // `is_cave`) se haya comido justo ese bloque, en cuyo caso no hay
        // nada sólido donde enraizar y el árbol quedaría flotando sobre
        // el agujero.
        let surface_is_grass = !self.is_cave(wx, height as i32 - 1, wz, height);
        if !surface_is_grass {
            return None;
        }

        // ~2.5% de las columnas de pasto enraízan un árbol: bosque
        // disperso, árboles sueltos y no pegados, al estilo llanura de
        // Minecraft (no bosque denso).
        const DENSITY_PER_MILLE: u64 = 25;
        if hash_xz(self.seed, wx, wz) % 1000 >= DENSITY_PER_MILLE {
            return None;
        }

        // Segundo hash (mismo seed/columna, mezclado distinto) para la
        // altura del tronco, para que no quede correlacionada con la
        // decisión de densidad de arriba.
        let h2 = hash_xz(self.seed ^ 0x5EED_1234, wx, wz);
        let trunk_height = 4 + (h2 % 3) as i32; // 4..=6, tronco recto tipo "oak"

        Some(TreeSpec { trunk_height })
    }

    /// Estampa la parte de un árbol (raíz en columna de mundo wx,wz) que
    /// cae dentro de los límites locales de *este* chunk (cuyo origen en
    /// coordenadas de mundo es origin_x/origin_z). Nunca pisa un bloque
    /// que no sea `Air` — ni terreno, ni tronco/hojas de otro árbol ya
    /// estampado.
    fn stamp_tree(
        &self,
        chunk: &mut Chunk,
        origin_x: i32,
        origin_z: i32,
        wx: i32,
        wz: i32,
        tree: &TreeSpec,
    ) {
        let base_y = self.height_at(wx, wz) as i32; // primer bloque de aire sobre el pasto
        let top_y = base_y + tree.trunk_height - 1; // último bloque de tronco

        let mut place = |dx: i32, world_y: i32, dz: i32, block: BlockType| {
            let local_x = wx + dx - origin_x;
            let local_z = wz + dz - origin_z;
            if local_x < 0
                || local_x >= CHUNK_SIZE_X as i32
                || local_z < 0
                || local_z >= CHUNK_SIZE_Z as i32
                || world_y < 0
                || world_y >= CHUNK_SIZE_Y as i32
            {
                return; // cae fuera de este chunk (le toca al vecino)
            }
            if chunk.get(local_x, world_y, local_z) == BlockType::Air {
                chunk.set(local_x as usize, world_y as usize, local_z as usize, block);
            }
        };

        // Tronco recto.
        for y in base_y..=top_y {
            place(0, y, 0, BlockType::Wood);
        }

        // Copa: blob esférico chico en capas (dos anchas con esquinas
        // recortadas, una angosta, una hoja sola arriba), mismo perfil
        // "oak" clásico chico. `dy` es relativo a `top_y`.
        for &(dx, dy, dz) in CANOPY_OFFSETS {
            place(dx, top_y + dy, dz, BlockType::Leaves);
        }
    }

    /// ¿El bloque de mundo (wx, wy, wz) cae dentro de una cueva? Función
    /// pura (mismo espíritu que `tree_at`): depende solo de wx/wy/wz y
    /// de los campos de ruido, nunca del chunk que la evalúa ni del
    /// orden en que se generan los chunks vecinos.
    ///
    /// Combina dos "familias" de cueva, como en Minecraft moderno:
    /// - "queso" (`cheese`): cavernas anchas e irregulares, salones
    ///   grandes — más anchas que altas a propósito (ver el
    ///   multiplicador de Y más abajo).
    /// - "fideo"/túnel (`noodle`): pasillos largos y serpenteantes,
    ///   también más anchos/largos que altos — la técnica clásica de
    ///   "gusanos de Perlin": dos campos de ruido 3D independientes, y
    ///   donde los DOS están cerca de cero a la vez queda un tubo
    ///   hueco.
    ///
    /// `region_bias` (ruido de zona, mucho más grande que un chunk) le
    /// da variedad región por región: en algunas zonas los umbrales
    /// bajan (cuevas grandes e interconectadas) y en otras suben
    /// (bolsones chicos y sueltos, o casi nada).
    ///
    /// Las salidas a la superficie tienen dos fuentes distintas: un
    /// barranco raro y garantizado (`ravine`), y zonas donde se le
    /// permite a esta misma cueva orgánica llegar más cerca del techo
    /// (`entrance_zone`) — sin forzar nada, la boca solo aparece si la
    /// forma de la cueva de verdad llega hasta ahí.
    fn is_cave(&self, wx: i32, wy: i32, wz: i32, surface_height: usize) -> bool {
        const FLOOR_BUFFER: i32 = 2;
        if wy < FLOOR_BUFFER {
            return false;
        }

        // Barranco: grieta abierta de verdad, de punta a punta, sin
        // techo — pero la EXCEPCIÓN, no la regla (ver `ravine`, umbral
        // alto a propósito). Antes esto mismo pasaba en casi todos
        // lados porque el pozo se tallaba entero sin importar si abajo
        // había una cueva real — literal falla de San Andrés en cada
        // entrada. Ahora es raro, como los barrancos de Minecraft.
        if self.ravine(wx, wy, wz, surface_height) {
            return true;
        }

        // Zona de "boca de cueva" natural: a diferencia del barranco de
        // arriba, esto NO fuerza nada — solo le baja el colchón a la
        // cueva normal (queso/túnel, más abajo) para que, SI de verdad
        // pasa cerca de la superficie en esta zona, pueda asomar. Si la
        // cueva no llega hasta ahí, sigue sellada igual que en el resto
        // del mapa. Así la boca queda con la forma orgánica de la cueva
        // (angosta, irregular) en vez de un agujero geométrico forzado.
        let surface_buffer = if self.entrance_zone(wx, wz, surface_height) { 1 } else { 4 };
        let ceiling = surface_height as i32 - surface_buffer;
        if wy > ceiling {
            return false;
        }

        let region_bias = self
            .noise
            .get([wx as f64 * 0.008 + 7_000.0, wz as f64 * 0.008 + 7_000.0]);

        // --- Cavernas "queso" ---
        // El multiplicador de frecuencia en Y (como estaba antes) hacía
        // el trabajo de aplanar, pero tiene un efecto secundario: en
        // los bordes de la banda que SÍ supera el umbral, esa banda
        // puede llegar a durar un solo bloque de alto antes de volver a
        // caer por debajo — resultado: cuevas de 1 bloque, injugables.
        // La solución es cuantizar Y ANTES de muestrear (`banded_y`):
        // adentro de una misma banda de `CAVE_BAND` bloques el ruido da
        // siempre el mismo valor, así que si un punto es cueva, toda la
        // banda entera (mínimo `CAVE_BAND` bloques) lo es también.
        let by = Self::banded_y(wy, CAVE_BAND_CHEESE);
        let base = self.cave_noise.get([wx as f64 * 0.045, by * 0.045, wz as f64 * 0.045]);
        let detail = self.cave_noise.get([
            wx as f64 * 0.12 + 500.0,
            by * 0.12 + 500.0,
            wz as f64 * 0.12 + 500.0,
        ]);
        let cheese_value = base * 0.75 + detail * 0.25;
        // region_bias en [-1, 1]: hasta ±0.18 de corrimiento del umbral
        // base (0.62) — suficiente para que algunas zonas tengan
        // cavernas bastante más grandes/frecuentes que otras.
        let cheese_threshold = 0.62 - region_bias * 0.18;
        let is_cheese = cheese_value > cheese_threshold;

        // --- Túneles "fideo" ---
        // Mismo criterio de bandas, banda un poco más chica que el
        // queso (`CAVE_BAND_TUNNEL` = 3, contra 4) para que se sientan
        // pasillo y no salón, pero nunca por debajo de lo caminable.
        let tunnel_freq = 0.02;
        let ty = Self::banded_y(wy, CAVE_BAND_TUNNEL);
        let t1 = self.cave_noise.get([
            wx as f64 * tunnel_freq + 9_000.0,
            ty * tunnel_freq + 9_000.0,
            wz as f64 * tunnel_freq + 9_000.0,
        ]);
        let t2 = self.cave_noise.get([
            wx as f64 * tunnel_freq - 9_000.0,
            ty * tunnel_freq - 9_000.0,
            wz as f64 * tunnel_freq - 9_000.0,
        ]);
        let tunnel_value = t1.abs() + t2.abs();
        // Umbral chico a propósito: "los dos campos cerca de cero a la
        // vez" es raro por definición, así el tubo sale angosto en vez
        // de una franja ancha. `region_bias` solo lo agranda (nunca lo
        // achica más) en las mismas zonas "grandes" que ya agranda el
        // queso, para que ahí los dos tipos de cueva se sientan parte
        // del mismo sistema conectado.
        let tunnel_threshold = 0.07 + region_bias.max(0.0) * 0.05;
        let is_tunnel = tunnel_value < tunnel_threshold;

        is_cheese || is_tunnel
    }

    /// Barranco: grieta abierta de verdad, garantizada, de punta a
    /// punta — pero rara (`RAVINE_THRESHOLD` alto), la excepción entre
    /// las cuevas normales y no la norma. Mismo espíritu que los
    /// ravines de Minecraft: se ven de tanto en tanto explorando, no en
    /// cada loma.
    fn ravine(&self, wx: i32, wy: i32, wz: i32, surface_height: usize) -> bool {
        const RAVINE_FREQ: f64 = 0.05;
        const RAVINE_THRESHOLD: f64 = 0.88;
        const RAVINE_DEPTH: i32 = 20;

        let bias = self
            .noise
            .get([wx as f64 * RAVINE_FREQ + 80_000.0, wz as f64 * RAVINE_FREQ + 80_000.0]);
        if bias <= RAVINE_THRESHOLD {
            return false;
        }

        let bottom = (surface_height as i32 - RAVINE_DEPTH).max(2);
        wy >= bottom
    }

    /// ¿(wx, wz) cae dentro de una zona rara y esparcida donde se le
    /// permite a una cueva normal (queso/túnel) llegar más cerca de la
    /// superficie que en el resto del mapa? Ojo: esto NO decide por sí
    /// solo si hay una boca de cueva — solo baja el colchón en
    /// `is_cave`. Que la boca aparezca de verdad depende de que la
    /// cueva orgánica realmente pase por ahí cerca del techo.
    ///
    /// Sesgada hacia terreno de montaña (`MOUNTAIN_HEIGHT`): igual que
    /// en Minecraft, las bocas de cueva "memorables" están en laderas,
    /// no en pasto llano. En terreno bajo el umbral requerido sube
    /// bastante, así que ahí siguen siendo mucho más raras (no
    /// imposibles del todo, para no dejar el pasto llano 100% sin
    /// ninguna).
    fn entrance_zone(&self, wx: i32, wz: i32, surface_height: usize) -> bool {
        const ENTRANCE_FREQ: f64 = 0.015;
        const ENTRANCE_THRESHOLD_MOUNTAIN: f64 = 0.55;
        const ENTRANCE_THRESHOLD_FLAT: f64 = 0.78;

        let threshold = if surface_height >= MOUNTAIN_HEIGHT {
            ENTRANCE_THRESHOLD_MOUNTAIN
        } else {
            ENTRANCE_THRESHOLD_FLAT
        };

        let bias = self
            .noise
            .get([wx as f64 * ENTRANCE_FREQ + 40_000.0, wz as f64 * ENTRANCE_FREQ + 40_000.0]);
        bias > threshold
    }

    /// Cuantiza `wy` a la banda de `band` bloques que le corresponde
    /// (ej. con `band=4`: 0..=3 -> 0.0, 4..=7 -> 4.0, ...). Usado para
    /// muestrear el ruido de cuevas en vez de `wy` directo — ver el
    /// comentario grande en `is_cave` sobre por qué, si no, aparecían
    /// cuevas de 1 solo bloque de alto.
    fn banded_y(wy: i32, band: i32) -> f64 {
        (wy.div_euclid(band) * band) as f64
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

/// Hash puro y determinístico de (seed, x, z) -> u64 (variante de
/// splitmix64/murmur-style mixing). A propósito NO es un RNG con estado:
/// no hay `next()`, no importa el orden de llamadas — mismos argumentos,
/// mismo resultado siempre, lo cual es exactamente lo que necesita
/// `tree_at` para que chunks generados en threads distintos, en
/// cualquier orden, concuerden sobre el mismo árbol en la misma columna.
fn hash_xz(seed: u32, x: i32, z: i32) -> u64 {
    let mut h = (seed as u64).wrapping_mul(0x9E3779B97F4A7C15)
        ^ (x as i64 as u64).wrapping_mul(0xBF58476D1CE4E5B9)
        ^ (z as i64 as u64).wrapping_mul(0x94D049BB133111EB);
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58476D1CE4E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D049BB133111EB);
    h ^= h >> 31;
    h
}

/// Offsets (dx, dy, dz) de la copa de hojas, relativos a (raíz X/Z, tope
/// del tronco). Dos capas anchas (5x5 sin esquinas) apenas debajo del
/// tope, una capa angosta (3x3) al nivel del tope, y una hoja sola arriba
/// — el blob "oak" chico y redondeado de siempre.
const CANOPY_OFFSETS: &[(i32, i32, i32)] = &[
    // capa top_y - 2 (5x5 sin esquinas)
    (-2, -2, -1), (-2, -2, 0), (-2, -2, 1),
    (-1, -2, -2), (-1, -2, -1), (-1, -2, 0), (-1, -2, 1), (-1, -2, 2),
    (0, -2, -2), (0, -2, -1), (0, -2, 0), (0, -2, 1), (0, -2, 2),
    (1, -2, -2), (1, -2, -1), (1, -2, 0), (1, -2, 1), (1, -2, 2),
    (2, -2, -1), (2, -2, 0), (2, -2, 1),
    // capa top_y - 1 (5x5 sin esquinas)
    (-2, -1, -1), (-2, -1, 0), (-2, -1, 1),
    (-1, -1, -2), (-1, -1, -1), (-1, -1, 0), (-1, -1, 1), (-1, -1, 2),
    (0, -1, -2), (0, -1, -1), (0, -1, 0), (0, -1, 1), (0, -1, 2),
    (1, -1, -2), (1, -1, -1), (1, -1, 0), (1, -1, 1), (1, -1, 2),
    (2, -1, -1), (2, -1, 0), (2, -1, 1),
    // capa top_y (3x3)
    (-1, 0, -1), (-1, 0, 0), (-1, 0, 1),
    (0, 0, -1), (0, 0, 0), (0, 0, 1),
    (1, 0, -1), (1, 0, 0), (1, 0, 1),
    // capa top_y + 1 (una hoja sola, en cruz)
    (0, 1, 0), (1, 1, 0), (-1, 1, 0), (0, 1, 1), (0, 1, -1),
];

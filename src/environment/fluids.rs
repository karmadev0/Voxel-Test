/// fluids.rs
/// Simulación de agua al estilo Minecraft: fuentes (`Water(0)`) que se
/// esparcen hacia afuera hasta una distancia máxima, se caen si hay aire
/// debajo, y se secan solas si en algún momento dejan de tener una
/// fuente/vecino que las alimente. La esponja absorbe el agua cercana de
/// a poco.
///
/// ## Por qué es una cola de eventos y no un scan del mundo entero
///
/// `worldgen.rs` ya deja los mares/lagos y las cuevas inundadas
/// completamente llenos al generarlos (ver `WATER_LEVEL`/agua de cueva
/// ahí) — no hace falta "simular" esa agua estática nunca, ya nació en
/// su estado final. Lo único que necesita simulación de verdad es lo que
/// cambia por el juego: el jugador coloca/rompe agua, rompe un bloque
/// que le abre paso a agua ya existente, o planta una esponja. Por eso
/// alcanza con una cola de posiciones "a revisar" que se alimenta desde
/// esos eventos (ver `notify_block_changed`) en vez de recorrer todos
/// los chunks cargados en cada tick — mucho más barato, y de paso es
/// literalmente lo que hace que el esparcido/secado se vea "de a poco":
/// cada posición que cambia solo encola a sus vecinos para el tick
/// siguiente, así que un manantial nuevo tarda varios ticks en llegar a
/// su distancia máxima, en vez de aparecer todo de una.
use crate::environment::chunk::BlockType;
use crate::environment::world::World;
use std::collections::{HashSet, VecDeque};

/// Nivel máximo de un bloque de agua que fluye antes de no poder seguir
/// esparciéndose más lejos de la fuente (mismo número que Minecraft).
const MAX_FLOW_LEVEL: u8 = 7;

/// Radio (distancia de Chebyshev, es decir un cubo, no una esfera —
/// suficiente para este propósito y mucho más barato de recorrer) en el
/// que una esponja recién colocada encola agua cercana para absorber.
const SPONGE_RADIUS: i32 = 4;

/// Cuántas posiciones como máximo se procesan por tick. Ponerle un techo
/// es lo que evita que un lago grande recién generado (si alguna vez se
/// encola de golpe, ej. al colocar una esponja al lado de un lago
/// entero) trabe un frame entero de golpe — el resto simplemente espera
/// al tick siguiente, reforzando el efecto "de a poco".
const MAX_UPDATES_PER_TICK: usize = 96;

pub struct FluidSim {
    pending: VecDeque<(i32, i32, i32)>,
    /// Para no encolar la misma celda dos veces en el mismo tick (sí
    /// puede volver a encolarse en un tick futuro, una vez que salga de
    /// acá al procesarse).
    queued: HashSet<(i32, i32, i32)>,
    sponges: HashSet<(i32, i32, i32)>,
}

impl FluidSim {
    pub fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            queued: HashSet::new(),
            sponges: HashSet::new(),
        }
    }

    fn enqueue(&mut self, pos: (i32, i32, i32)) {
        if self.queued.insert(pos) {
            self.pending.push_back(pos);
        }
    }

    fn enqueue_neighbors(&mut self, (x, y, z): (i32, i32, i32)) {
        self.enqueue((x + 1, y, z));
        self.enqueue((x - 1, y, z));
        self.enqueue((x, y + 1, z));
        self.enqueue((x, y - 1, z));
        self.enqueue((x, y, z + 1));
        self.enqueue((x, y, z - 1));
    }

    /// Avisale a la simulación que `(x, y, z)` cambió de `old` a `new`
    /// (rotura/colocación del jugador — ver los dos call sites en
    /// `handle_click`, en lib.rs). Con eso alcanza para que la
    /// simulación reaccione: encola la celda y sus vecinas para
    /// reevaluar en el próximo tick, y mantiene al día el registro de
    /// esponjas activas.
    pub fn notify_block_changed(&mut self, pos: (i32, i32, i32), old: BlockType, new: BlockType) {
        if old == BlockType::Sponge {
            self.sponges.remove(&pos);
        }
        if new == BlockType::Sponge {
            self.sponges.insert(pos);
            // Encolar el agua ya existente en el radio: no la borramos
            // acá directamente (eso sería instantáneo, no "de a poco")
            // — solo la marcamos para que el tick normal la vaya
            // absorbiendo, un puñado de bloques por vez.
            for dx in -SPONGE_RADIUS..=SPONGE_RADIUS {
                for dy in -SPONGE_RADIUS..=SPONGE_RADIUS {
                    for dz in -SPONGE_RADIUS..=SPONGE_RADIUS {
                        self.enqueue((pos.0 + dx, pos.1 + dy, pos.2 + dz));
                    }
                }
            }
        }
        self.enqueue(pos);
        self.enqueue_neighbors(pos);
    }

    /// ¿`pos` está a distancia-Chebyshev `SPONGE_RADIUS` (o menos) de
    /// alguna esponja activa?
    fn near_sponge(&self, pos: (i32, i32, i32)) -> bool {
        self.sponges.iter().any(|s| {
            (s.0 - pos.0).abs() <= SPONGE_RADIUS
                && (s.1 - pos.1).abs() <= SPONGE_RADIUS
                && (s.2 - pos.2).abs() <= SPONGE_RADIUS
        })
    }

    /// El nivel de agua "correcto" que debería tener `pos` en este
    /// instante, según sus vecinos — o `None` si no debería haber agua
    /// ahí. No mira el estado ACTUAL de `pos`, solo lo que sus vecinos
    /// dicen que debería ser (por eso sirve tanto para decidir si una
    /// celda de aire debería mojarse como para decidir si una celda de
    /// agua ya existente debería seguir estando ahí).
    fn desired_level(world: &World, (x, y, z): (i32, i32, i32)) -> Option<u8> {
        // El agua siempre prioriza caer: si arriba hay agua (fuente o
        // fluyendo, no importa el nivel), esta celda se llena a full
        // (nivel 0) sin importar qué tan lejos esté horizontalmente de
        // la fuente original — así es como una cascada larga sigue
        // siendo agua "fresca" al tocar el piso, en vez de ir perdiendo
        // fuerza en cada caída.
        if let BlockType::Water(_) = world.get_block(x, y + 1, z) {
            return Some(0);
        }

        // Si no hay nada cayendo, se esparce horizontalmente desde el
        // vecino con el nivel más bajo (más "lleno").
        let neighbors = [(x + 1, y, z), (x - 1, y, z), (x, y, z + 1), (x, y, z - 1)];
        let best = neighbors
            .iter()
            .filter_map(|&p| match world.get_block(p.0, p.1, p.2) {
                BlockType::Water(level) => Some(level),
                _ => None,
            })
            .min()?;

        if best >= MAX_FLOW_LEVEL {
            None // ya está en el límite, no puede esparcirse más lejos
        } else {
            Some(best + 1)
        }
    }

    /// Procesa hasta `MAX_UPDATES_PER_TICK` posiciones de la cola.
    /// Devuelve los chunks que quedaron sucios (para remallar).
    pub fn tick(&mut self, world: &mut World) -> Vec<(i32, i32)> {
        let mut dirty = Vec::new();
        let mut processed = 0;

        while processed < MAX_UPDATES_PER_TICK {
            let Some(pos) = self.pending.pop_front() else {
                break;
            };
            self.queued.remove(&pos);
            processed += 1;

            let current = world.get_block(pos.0, pos.1, pos.2);

            // Absorción de esponja: pisa cualquier otra regla. El agua
            // dentro del radio simplemente desaparece (y encolamos sus
            // vecinos, porque perder ese vecino puede hacer que OTRA
            // agua más lejos también se quede sin alimentación y se
            // seque en cadena).
            if matches!(current, BlockType::Water(_)) && self.near_sponge(pos) {
                let changed = world.set_block(pos.0, pos.1, pos.2, BlockType::Air);
                dirty.extend(changed);
                self.enqueue_neighbors(pos);
                continue;
            }

            match current {
                BlockType::Air => {
                    // ¿Debería mojarse esta celda de aire?
                    if let Some(level) = Self::desired_level(world, pos) {
                        let changed = world.set_block(pos.0, pos.1, pos.2, BlockType::Water(level));
                        dirty.extend(changed);
                        self.enqueue_neighbors(pos);
                    }
                }
                BlockType::Water(0) => {
                    // Fuente: nunca se seca sola (solo la rompe el
                    // jugador o la absorbe una esponja, ya cubierto
                    // arriba). Igual hay que reintentar esparcirse por
                    // si un vecino que antes bloqueaba el paso
                    // (ej. una piedra que el jugador acaba de romper)
                    // ahora es aire.
                    self.enqueue_neighbors(pos);
                }
                BlockType::Water(current_level) => {
                    match Self::desired_level(world, pos) {
                        Some(level) if level <= current_level => {
                            // Sigue alimentada (y no le sobra nivel de
                            // más, lo cual no debería pasar salvo
                            // empate): sin cambios.
                            if level != current_level {
                                let changed =
                                    world.set_block(pos.0, pos.1, pos.2, BlockType::Water(level));
                                dirty.extend(changed);
                                self.enqueue_neighbors(pos);
                            }
                        }
                        Some(level) => {
                            // Un vecino la alimenta pero con un nivel
                            // peor que el actual (ej. se cortó el
                            // camino corto y ahora solo llega por una
                            // vuelta más larga): actualiza al nuevo
                            // nivel, más débil.
                            let changed =
                                world.set_block(pos.0, pos.1, pos.2, BlockType::Water(level));
                            dirty.extend(changed);
                            self.enqueue_neighbors(pos);
                        }
                        None => {
                            // Ya nadie la alimenta: se seca. Esto es lo
                            // que hace que romper la fuente de un río
                            // largo lo vaya secando de la punta hacia
                            // atrás, un tick a la vez, en vez de
                            // desaparecer todo de golpe.
                            let changed = world.set_block(pos.0, pos.1, pos.2, BlockType::Air);
                            dirty.extend(changed);
                            self.enqueue_neighbors(pos);
                        }
                    }
                }
                _ => {} // cualquier otro bloque sólido: nada que hacer acá
            }
        }

        dirty
    }
}

impl Default for FluidSim {
    fn default() -> Self {
        Self::new()
    }
}

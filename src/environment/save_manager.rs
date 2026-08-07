/// save_manager.rs
/// Persistencia de la LISTA de mundos guardados (no de los chunks en sí,
/// eso lo sigue haciendo `World`/`ChunkLoader` en `world.rs`). Cada mundo
/// vive en su propia carpeta `saves/<nombre_sanitizado>/`, con:
///   - `meta.bin`: este módulo (nombre, semilla, fecha de creación).
///   - `chunk_X_Z.bin`: por cada chunk modificado (`World::save_dir`).
///
/// Separar "lista de mundos" de "chunks de un mundo" es lo que permite
/// que el menú principal (`GameScreen::WorldList`) muestre qué mundos
/// existen sin tener que cargar ninguno de ellos entero en memoria.
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Carpeta raíz real a usar, si fue configurada explícitamente (ver
/// `set_saves_root`). En Android, `PathBuf::from("saves")` (el default de
/// abajo) es una ruta relativa que resuelve contra el directorio de
/// trabajo del proceso — que ahí es "/", de solo lectura. Por eso hace
/// falta poder pisarlo con una carpeta privada de verdad de la app
/// (`internal_data_path()` / `external_data_path()`), la misma idea que
/// ya usa `crash_dir` en `platform/crash.rs`.
static SAVES_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Configura la carpeta raíz de guardado. Llamar una sola vez, lo antes
/// posible en `android_main` (antes de tocar cualquier mundo), con una
/// carpeta escribible de verdad. En desktop no hace falta llamarlo: el
/// default relativo ("saves", al lado del ejecutable) ya funciona.
/// Si se llama más de una vez, las llamadas siguientes no tienen efecto
/// (no debería pasar en la práctica, pero mejor no entrar en pánico por
/// eso).
pub fn set_saves_root(dir: PathBuf) {
    let _ = SAVES_ROOT.set(dir);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldMeta {
    pub name: String,
    pub seed: u32,
    /// Segundos desde epoch. Solo se usa para ordenar la lista (más
    /// reciente primero) y, a futuro, mostrar fecha de creación.
    pub created_at: u64,
    /// Última posición conocida del jugador (pies) + rotación de cámara,
    /// para reanudar exactamente donde se dejó el mundo. `None` en un
    /// mundo recién creado (todavía no se guardó ninguna partida) o en
    /// `meta.bin` de versiones anteriores a este campo — `bincode` con
    /// `Option` viejo/nuevo es compatible siempre que el campo se agregue
    /// al final del struct, así que esto no rompe saves ya existentes.
    #[serde(default)]
    pub player_state: Option<PlayerState>,
}

/// Snapshot mínimo de dónde estaba el jugador al guardar: posición de
/// los PIES (no de los ojos/cámara — mismo criterio que `Player::feet_position`)
/// más `yaw`/`pitch` de la cámara, en radianes. Con esto alcanza para
/// reconstruir tanto `Player::new` como `Camera::new` + rotación al
/// recargar el mundo.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PlayerState {
    pub feet_x: f32,
    pub feet_y: f32,
    pub feet_z: f32,
    pub yaw: f32,
    pub pitch: f32,
}

/// Carpeta raíz donde viven todas las carpetas de mundos, una por mundo.
fn saves_root() -> PathBuf {
    SAVES_ROOT
        .get()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("saves"))
}

/// Convierte el nombre elegido por el jugador en un nombre de carpeta
/// seguro (sin espacios ni caracteres que puedan confundir al
/// filesystem). El nombre "de verdad" para mostrar en pantalla sigue
/// siendo el que queda guardado en `WorldMeta::name`.
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    if cleaned.is_empty() {
        "mundo".to_string()
    } else {
        cleaned
    }
}

/// Carpeta de guardado de un mundo puntual — la que recibe `World::new`.
pub fn world_dir(name: &str) -> PathBuf {
    saves_root().join(sanitize(name))
}

fn meta_path(name: &str) -> PathBuf {
    world_dir(name).join("meta.bin")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Lista todos los mundos guardados (leyendo cada `meta.bin`), más
/// reciente primero. Si `saves/` todavía no existe (primera vez que se
/// abre la app) devuelve una lista vacía en vez de fallar.
pub fn list_worlds() -> Vec<WorldMeta> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(saves_root()) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let meta_path = path.join("meta.bin");
            if let Ok(bytes) = fs::read(&meta_path) {
                if let Ok(meta) = bincode::deserialize::<WorldMeta>(&bytes) {
                    out.push(meta);
                }
            }
        }
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    out
}

/// Primer nombre libre tipo "Mundo N". Se usa para prellenar el cuadro
/// de texto de `GameScreen::NameWorld` (el jugador puede borrarlo y
/// escribir el suyo con el teclado en pantalla — ver
/// `TouchAction::OpenNameWorld`) y como respaldo si confirma con el
/// nombre vacío.
pub fn next_free_name() -> String {
    let existing = list_worlds();
    let mut i = existing.len() + 1;
    loop {
        let candidate = format!("Mundo {}", i);
        if !existing.iter().any(|w| w.name == candidate) {
            return candidate;
        }
        i += 1;
    }
}

/// Si `name` ya está en uso (por otro mundo, o por chocar de carpeta
/// una vez sanitizado — ver `sanitize`), le agrega " (2)", " (3)", etc.
/// hasta encontrar uno libre. El jugador puede escribir cualquier
/// nombre en el teclado en pantalla, así que esto es lo que evita que
/// dos mundos con el mismo nombre (o nombres que sanitizan igual, tipo
/// "Mi Mundo" y "Mi_Mundo") terminen pisándose la carpeta.
fn unique_name(name: &str) -> String {
    let existing = list_worlds();
    let taken = |candidate: &str| {
        existing.iter().any(|w| w.name == candidate) || world_dir(candidate).exists()
    };
    if !taken(name) {
        return name.to_string();
    }
    let mut i = 2;
    loop {
        let candidate = format!("{} ({})", name, i);
        if !taken(&candidate) {
            return candidate;
        }
        i += 1;
    }
}

/// Crea la carpeta y el `meta.bin` de un mundo nuevo, con una semilla
/// aleatoria (derivada del reloj, alcanza para que dos mundos no
/// terminen siendo el mismo terreno). Si `name` ya está en uso, se
/// desambigua automáticamente (ver `unique_name`). Devuelve la
/// metadata ya guardada, con el nombre final (puede diferir del
/// pedido).
pub fn create_world(name: &str) -> WorldMeta {
    let name = unique_name(name);
    let dir = world_dir(&name);
    let _ = fs::create_dir_all(&dir);
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(1337);
    let meta = WorldMeta {
        name: name.clone(),
        seed,
        created_at: now_secs(),
        // Mundo recién creado: todavía no hay ninguna partida jugada,
        // así que no hay posición que guardar. `start_world` usa el
        // spawn default (8,40,8) cuando esto es `None` — ver `lib.rs`.
        player_state: None,
    };
    if let Ok(bytes) = bincode::serialize(&meta) {
        let _ = fs::write(meta_path(&name), bytes);
    }
    meta
}

/// Lee la metadata de un mundo ya existente por nombre.
pub fn load_meta(name: &str) -> Option<WorldMeta> {
    fs::read(meta_path(name))
        .ok()
        .and_then(|bytes| bincode::deserialize::<WorldMeta>(&bytes).ok())
}

/// Actualiza solo `player_state` dentro del `meta.bin` de `name` y lo
/// reescribe. Se llama junto con `World::save_dirty_chunks()` — mismo
/// caller, mismo momento — desde autoguardado, guardado manual (F5) y
/// "Salir" (ver `lib.rs`). Si `meta.bin` no existe o no se puede leer
/// (no debería pasar salvo carpeta corrupta/borrada a mano), no hace
/// nada: no queremos crear un `meta.bin` a medias sin `seed`/`created_at`
/// correctos.
pub fn save_player_state(name: &str, state: PlayerState) {
    let Some(mut meta) = load_meta(name) else {
        log::warn!(
            "save_player_state: no se pudo leer meta.bin de '{}', se omite guardar posición.",
            name
        );
        return;
    };
    meta.player_state = Some(state);
    if let Ok(bytes) = bincode::serialize(&meta) {
        let _ = fs::write(meta_path(name), bytes);
    }
}

/// Borra por completo la carpeta de un mundo guardado (meta + todos sus
/// chunks) — llamado desde `GameScreen::ConfirmDeleteWorld`, después de
/// que el jugador confirmó. `true` si se borró algo. No falla si la
/// carpeta ya no existía (por ejemplo, doble toque).
pub fn delete_world(name: &str) -> bool {
    let dir = world_dir(name);
    if !dir.exists() {
        return false;
    }
    fs::remove_dir_all(&dir).is_ok()
}

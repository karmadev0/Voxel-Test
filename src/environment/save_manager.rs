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
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldMeta {
    pub name: String,
    pub seed: u32,
    /// Segundos desde epoch. Solo se usa para ordenar la lista (más
    /// reciente primero) y, a futuro, mostrar fecha de creación.
    pub created_at: u64,
}

/// Carpeta raíz donde viven todas las carpetas de mundos, una por mundo.
fn saves_root() -> PathBuf {
    PathBuf::from("saves")
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

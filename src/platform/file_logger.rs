/// file_logger.rs
///
/// Logger propio que envuelve al logger normal de la plataforma
/// (`env_logger` en desktop, `android_logger` en Android — ambos se
/// siguen viendo en stderr/logcat exactamente igual que antes) y además
/// escribe cada línea a un archivo de texto en disco (`game_log.txt`),
/// en la misma carpeta que ya usa `crash.rs` para los reportes de crash.
///
/// Por qué así y no un buffer en memoria: el panel de debug (F3) puede
/// mostrar un snapshot en pantalla y copiarlo al portapapeles, pero el
/// historial completo de logs de una sesión larga puede pesar más de lo
/// razonable para tener siempre en RAM o para pegar en un portapapeles.
/// Un archivo de texto de toda la sesión es lo que un jugador reportando
/// un bug puede simplemente adjuntar o abrir con cualquier editor — en
/// Android queda visible con un explorador de archivos o `adb pull`,
/// igual que `crash_*.txt`.
///
/// `log::set_boxed_logger` solo admite UN logger global a la vez, así
/// que no podemos tener "el logger de siempre" Y "el logger de archivo"
/// coexistiendo por separado: en vez de eso, este logger es el único
/// instalado, y por dentro hace las dos cosas — delega el formateo y la
/// salida a stderr/logcat en el logger de plataforma que ya existía, y
/// además escribe la misma línea a archivo.
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// Logger de plataforma que este módulo delega para stderr/logcat.
/// Mismo tipo que ya se instalaba antes de este cambio en cada
/// plataforma, solo que ahora no se instala directamente con
/// `log::set_logger` sino que queda guardado acá adentro.
enum PlatformLogger {
    #[cfg(not(target_os = "android"))]
    Desktop(env_logger::Logger),
    #[cfg(target_os = "android")]
    Android(android_logger::AndroidLogger),
}

impl PlatformLogger {
    #[cfg(not(target_os = "android"))]
    fn new() -> Self {
        // `from_default_env` respeta `RUST_LOG` igual que antes
        // (`env_logger::init()` internamente hace lo mismo). `.build()`
        // devuelve el logger ya armado sin instalarlo con
        // `log::set_logger` — eso lo hacemos nosotros una sola vez, más
        // abajo, con `FileLogger` completo.
        PlatformLogger::Desktop(env_logger::Builder::from_default_env().build())
    }

    #[cfg(target_os = "android")]
    fn new() -> Self {
        PlatformLogger::Android(android_logger::AndroidLogger::new(
            android_logger::Config::default().with_max_level(log::LevelFilter::Info),
        ))
    }

    fn log(&self, record: &log::Record) {
        match self {
            #[cfg(not(target_os = "android"))]
            PlatformLogger::Desktop(l) => l.log(record),
            #[cfg(target_os = "android")]
            PlatformLogger::Android(l) => l.log(record),
        }
    }

    fn flush(&self) {
        match self {
            #[cfg(not(target_os = "android"))]
            PlatformLogger::Desktop(l) => l.flush(),
            #[cfg(target_os = "android")]
            PlatformLogger::Android(l) => l.flush(),
        }
    }

    fn enabled(&self, metadata: &log::Metadata) -> bool {
        match self {
            #[cfg(not(target_os = "android"))]
            PlatformLogger::Desktop(l) => l.enabled(metadata),
            #[cfg(target_os = "android")]
            PlatformLogger::Android(l) => l.enabled(metadata),
        }
    }
}

struct FileLogger {
    platform: PlatformLogger,
    file: Option<Mutex<File>>,
}

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.platform.enabled(metadata)
    }

    fn log(&self, record: &log::Record) {
        // Primero el comportamiento de siempre (stderr en desktop,
        // logcat en Android) — si escribir a archivo falla más abajo
        // (disco lleno, sin permiso, etc.), el log normal ya salió.
        self.platform.log(record);

        if let Some(file_mutex) = &self.file {
            let line = format!(
                "[{level}] {target}: {args}\n",
                level = record.level(),
                target = record.target(),
                args = record.args()
            );
            if let Ok(mut f) = file_mutex.lock() {
                // Silenciamos errores de escritura a propósito: un fallo
                // acá (por ejemplo, tarjeta SD llena) no debería tirar
                // abajo el juego ni generar un log::error! recursivo.
                let _ = f.write_all(line.as_bytes());
            }
        }
    }

    fn flush(&self) {
        self.platform.flush();
        if let Some(file_mutex) = &self.file {
            if let Ok(mut f) = file_mutex.lock() {
                let _ = f.flush();
            }
        }
    }
}

/// Ruta del archivo de log de la sesión actual, para mostrarla en el
/// panel de debug (F3) y en el mensaje que copia el botón "COPIAR" del
/// panel. `None` hasta que `install()` haya corrido (y `None` para
/// siempre si no se pudo abrir el archivo).
static LOG_FILE_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn log_file_path() -> Option<PathBuf> {
    LOG_FILE_PATH.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Instala este logger como el logger global. Llamar una sola vez, al
/// arrancar (`run_desktop()` / `android_main`), en el mismo lugar donde
/// antes se llamaba `env_logger::init()` / `android_logger::init_once()`
/// — de hecho, reemplaza esas llamadas.
///
/// `log_dir`: carpeta donde escribir `game_log.txt`.
/// - Desktop: pasar `None`, se calcula sola (al lado del ejecutable,
///   misma carpeta `crash_logs` que ya usa `crash.rs`).
/// - Android: pasar `Some(path)` con `external_data_path()` /
///   `internal_data_path()` del `AndroidApp`, igual que se le pasa a
///   `crash::install`.
pub fn install(log_dir: Option<PathBuf>) {
    let dir = log_dir.or_else(default_desktop_log_dir);

    let file = dir.as_ref().and_then(|d| {
        if let Err(e) = std::fs::create_dir_all(d) {
            // No podemos usar log::warn! todavía (el logger no está
            // instalado), así que esto va directo a stderr.
            eprintln!("No se pudo crear la carpeta de logs {:?}: {}", d, e);
            return None;
        }
        let path = d.join("game_log.txt");
        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => {
                let mut slot = LOG_FILE_PATH.lock().unwrap_or_else(|e| e.into_inner());
                *slot = Some(path);
                Some(Mutex::new(f))
            }
            Err(e) => {
                eprintln!("No se pudo abrir el archivo de log {:?}: {}", path, e);
                None
            }
        }
    });

    let logger = FileLogger {
        platform: PlatformLogger::new(),
        file,
    };

    // `set_max_level` es necesario aparte de `set_boxed_logger`: sin
    // esto, `log`'s macros filtran todo antes de siquiera preguntarle a
    // `enabled()` (mismo motivo por el que `env_logger::init()` también
    // lo hacía puertas adentro).
    log::set_max_level(log::LevelFilter::Info);
    if let Err(e) = log::set_boxed_logger(Box::new(logger)) {
        eprintln!("No se pudo instalar el logger (¿ya había uno instalado?): {}", e);
    }
}

#[cfg(not(target_os = "android"))]
fn default_desktop_log_dir() -> Option<PathBuf> {
    // Misma carpeta que `crash::default_desktop_crash_dir` (al lado del
    // ejecutable, "crash_logs") — así el jugador solo tiene que mirar en
    // un único lugar para encontrar tanto `game_log.txt` como los
    // `crash_*.txt`.
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.join("crash_logs")))
        .or_else(|| Some(PathBuf::from("crash_logs")))
}

#[cfg(target_os = "android")]
fn default_desktop_log_dir() -> Option<PathBuf> {
    None
}

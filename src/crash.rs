/// crash.rs
/// Manejo de crashes (panics) para que ni en Android ni en desktop la app
/// se cierre de golpe sin dejar rastro de qué pasó. La estrategia tiene
/// dos partes:
///
///   1. Un panic hook global (`install`), instalado apenas arranca el
///      programa, que arma un reporte de texto (timestamp, mensaje,
///      ubicación, backtrace), lo escribe a un archivo en disco — para
///      poder sacarlo con un explorador de archivos o `adb pull` — y
///      además lo manda a logcat/stderr vía `log::error!`.
///
///   2. El loop principal (en lib.rs) envuelve cada callback de winit en
///      `std::panic::catch_unwind`. Como el crate ya compila con
///      `panic = "unwind"` (ver Cargo.toml — hacía falta igual para el
///      cdylib de Android), un panic ahí adentro NO tira abajo el
///      proceso entero: solo aborta esa llamada puntual, y el control
///      vuelve normalmente al loop de eventos. Después de atrapar un
///      panic dejamos de correr la lógica normal del juego (podría haber
///      quedado en un estado a medio actualizar) y en su lugar
///      dibujamos una pantalla simple de "crasheado", para no seguir
///      operando sobre datos posiblemente corruptos.
///
/// En desktop, además, se muestra un cuadro de diálogo nativo bloqueante
/// (rfd) con el mensaje corto y la ruta del log, y el reporte completo se
/// puede volver a copiar al portapapeles (arboard) apretando C mientras
/// se ve la pantalla roja. En Android no hay diálogo (no tiene un
/// equivalente simple sin armar una Activity/layout de Java aparte), pero
/// sí se puede copiar el log completo al portapapeles del sistema tocando
/// la pantalla, vía JNI contra `android.content.ClipboardManager` — como
/// no hay pipeline de texto en el engine todavía para confirmarlo en
/// pantalla, la confirmación es un flash breve de color en la pantalla de
/// crash (ver `render_crash_screen` en lib.rs).
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Lo que queda guardado en memoria tras el último panic atrapado.
struct CrashReport {
    full_text: String,
    short_message: String,
    file_path: Option<PathBuf>,
}

static LAST_CRASH: OnceLock<Mutex<Option<CrashReport>>> = OnceLock::new();

fn last_crash_slot() -> &'static Mutex<Option<CrashReport>> {
    LAST_CRASH.get_or_init(|| Mutex::new(None))
}

/// Instala el panic hook. Llamar una sola vez, lo antes posible dentro de
/// `run_desktop()` / `android_main` (antes de crear ventana, event loop,
/// etc.), para no dejar ninguna ventana sin cubrir.
///
/// `crash_dir`: carpeta donde guardar los archivos `crash_*.txt`.
/// - Desktop: pasarle `None`, se calcula sola (al lado del ejecutable).
/// - Android: pasarle `Some(path)` con `internal_data_path()` /
///   `external_data_path()` del `AndroidApp` — ahí adentro no existe
///   `std::env::current_exe()` de forma útil, ni la app tiene permiso de
///   escribir en cualquier lado.
pub fn install(crash_dir: Option<PathBuf>) {
    let dir = crash_dir.or_else(default_desktop_crash_dir);

    if let Some(d) = &dir {
        if let Err(e) = std::fs::create_dir_all(d) {
            log::warn!("No se pudo crear la carpeta de crash logs {:?}: {}", d, e);
        }
    }

    std::panic::set_hook(Box::new(move |info| {
        let (full_text, short_message) = build_report(info);

        // Primero el log normal: aunque falle todo lo demás (por ejemplo
        // si el panic pasó justo por falta de espacio en disco), esto ya
        // llega a logcat (Android) / stderr vía env_logger (desktop).
        log::error!("PANIC:\n{}", full_text);

        let file_path = dir
            .as_ref()
            .and_then(|d| write_report_to_file(d, &full_text));

        let mut slot = last_crash_slot().lock().unwrap_or_else(|e| e.into_inner());
        *slot = Some(CrashReport {
            full_text,
            short_message,
            file_path,
        });
    }));
}

fn build_report(info: &std::panic::PanicHookInfo) -> (String, String) {
    let short_message = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(panic sin mensaje de texto)".to_string());

    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "(ubicación desconocida)".to_string());

    // force_capture() (a diferencia de capture()) ignora RUST_BACKTRACE y
    // siempre intenta capturar. En un release con símbolos recortados
    // puede salir menos detallado, pero siempre da algo mejor que nada.
    let backtrace = std::backtrace::Backtrace::force_capture();

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let full_text = format!(
        "=== Voxel Engine: reporte de crash ===\n\
         Unix time: {timestamp}\n\
         Plataforma: {os} ({arch})\n\
         Mensaje: {short_message}\n\
         Ubicación: {location}\n\
         \n\
         --- Backtrace ---\n\
         {backtrace}\n",
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
    );

    (full_text, short_message)
}

fn write_report_to_file(dir: &PathBuf, text: &str) -> Option<PathBuf> {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("crash_{}.txt", timestamp));

    match std::fs::File::create(&path).and_then(|mut f| f.write_all(text.as_bytes())) {
        Ok(()) => {
            // Además del archivo con timestamp (para no pisar crashes
            // anteriores si querés compararlos), dejamos siempre una
            // copia con nombre fijo, para no tener que buscar cuál es
            // la más nueva.
            let _ = std::fs::write(dir.join("last_crash.txt"), text);
            Some(path)
        }
        Err(e) => {
            log::warn!("No se pudo escribir el crash log en {:?}: {}", path, e);
            None
        }
    }
}

#[cfg(not(target_os = "android"))]
fn default_desktop_crash_dir() -> Option<PathBuf> {
    // Al lado del ejecutable, en una carpeta "crash_logs" — visible sin
    // tener que ir a buscar en carpetas de usuario / AppData.
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.join("crash_logs")))
        .or_else(|| Some(PathBuf::from("crash_logs")))
}

// En Android no hay "al lado del ejecutable": si `android_main` no nos
// pasó una carpeta válida (internal/external data path), directamente no
// escribimos archivo — el reporte igual queda en logcat y en memoria
// (`last_crash()`) para la pantalla de crash en pantalla.
#[cfg(target_os = "android")]
fn default_desktop_crash_dir() -> Option<PathBuf> {
    None
}

/// Último reporte de crash capturado (mensaje corto, texto completo, y
/// ruta del archivo si se pudo guardar). Se usa desde el loop principal
/// para mostrar el mensaje corto en pantalla/título y, en desktop, para
/// el diálogo nativo y el copiado al portapapeles.
pub fn last_crash() -> Option<(String, String, Option<PathBuf>)> {
    let slot = last_crash_slot().lock().unwrap_or_else(|e| e.into_inner());
    slot.as_ref()
        .map(|r| (r.short_message.clone(), r.full_text.clone(), r.file_path.clone()))
}

/// Copia el reporte completo del último crash al portapapeles del
/// sistema. Se usa cuando el usuario aprieta C en desktop, o toca la
/// pantalla en Android, mientras se ve la pantalla de crash.
#[cfg(not(target_os = "android"))]
pub fn copy_last_crash_to_clipboard() -> bool {
    let Some((_, full_text, _)) = last_crash() else {
        return false;
    };
    match arboard::Clipboard::new() {
        Ok(mut cb) => cb.set_text(full_text).is_ok(),
        Err(e) => {
            log::warn!("No se pudo abrir el portapapeles: {}", e);
            false
        }
    }
}

/// Versión Android de lo mismo, vía JNI contra
/// `android.content.ClipboardManager` (no hay crate de portapapeles
/// multiplataforma tipo `arboard` que soporte Android). `android-activity`
/// ya deja el `JavaVM`/`Activity` disponibles en `ndk_context` apenas
/// arranca la app, así que no hace falta guardar nada nuestro para
/// llegar a ellos desde acá.
#[cfg(target_os = "android")]
pub fn copy_last_crash_to_clipboard() -> bool {
    let Some((_, full_text, _)) = last_crash() else {
        return false;
    };
    match copy_to_clipboard_android(&full_text) {
        Ok(()) => true,
        Err(e) => {
            log::warn!("No se pudo copiar el log al portapapeles (Android): {}", e);
            false
        }
    }
}

#[cfg(target_os = "android")]
fn copy_to_clipboard_android(text: &str) -> Result<(), String> {
    let ctx = ndk_context::android_context();
    // Seguro de llamar desde este hilo (el que corre `android_main` y el
    // loop de eventos, no el hilo de UI de Java): `attach_current_thread`
    // se encarga de adjuntarlo a la JVM si todavía no lo estaba.
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| e.to_string())?;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    let activity = unsafe { jni::objects::JObject::from_raw(ctx.context().cast()) };

    let service_name = env.new_string("clipboard").map_err(|e| e.to_string())?;
    let clipboard = env
        .call_method(
            &activity,
            "getSystemService",
            "(Ljava/lang/String;)Ljava/lang/Object;",
            &[(&service_name).into()],
        )
        .and_then(|v| v.l())
        .map_err(|e| e.to_string())?;

    let label = env
        .new_string("voxel-engine-crash-log")
        .map_err(|e| e.to_string())?;
    let content = env.new_string(text).map_err(|e| e.to_string())?;

    let clip_data_class = env
        .find_class("android/content/ClipData")
        .map_err(|e| e.to_string())?;
    let clip = env
        .call_static_method(
            clip_data_class,
            "newPlainText",
            "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Landroid/content/ClipData;",
            &[(&label).into(), (&content).into()],
        )
        .and_then(|v| v.l())
        .map_err(|e| e.to_string())?;

    env.call_method(
        &clipboard,
        "setPrimaryClip",
        "(Landroid/content/ClipData;)V",
        &[(&clip).into()],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

/// Muestra un cuadro de diálogo nativo y bloqueante con el mensaje corto
/// del crash y dónde quedó guardado el log completo. Se llama una sola
/// vez, apenas se detecta el panic (no en cada frame — es bloqueante a
/// propósito, para que sea imposible no verlo, pero por eso mismo no se
/// puede repetir en el loop de render).
#[cfg(not(target_os = "android"))]
pub fn show_crash_dialog(short_message: &str, file_path: Option<&PathBuf>) {
    let where_saved = match file_path {
        Some(p) => format!("\n\nLog completo guardado en:\n{}", p.display()),
        None => "\n\n(No se pudo guardar el log a un archivo; revisá la consola.)".to_string(),
    };
    let description = format!(
        "El motor encontró un error interno y no pudo seguir con lo que estaba haciendo.\n\n\
         {short_message}{where_saved}\n\n\
         La app va a seguir abierta, en una pantalla roja de emergencia. \
         Apretá C ahí para volver a copiar el log completo al portapapeles."
    );
    rfd::MessageDialog::new()
        .set_title("Voxel Engine — Crash")
        .set_description(&description)
        .set_level(rfd::MessageLevel::Error)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

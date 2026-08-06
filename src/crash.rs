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

// --- Manejo de crashes NATIVOS (señales) ---
//
// Todo lo de arriba (panic hook + `catch_unwind` en lib.rs) SOLO atrapa
// panics de Rust: `panic!()`, un `.unwrap()` que falla, un índice fuera
// de rango, etc. Un crash de verdad — driver de GPU que segfaultea, una
// llamada JNI con una firma/puntero inválido, un `SIGABRT` desde código
// C/C++ de una dependencia nativa — llega como una señal del kernel
// (`SIGSEGV`, `SIGABRT`, `SIGBUS`, `SIGILL`), no como un panic de Rust,
// y ese tipo de evento se salta por completo tanto el hook como
// `catch_unwind`: el proceso muere directo, sin pasar por ninguno de
// los dos. Eso explica el síntoma de "se cierra sin generar ni archivo
// ni pantalla de crash" — no es que el crash handler esté fallando, es
// que nunca llegó a enterarse, porque no hay ningún handler de señales
// instalado.
//
// Importante: de una señal como SIGSEGV no hay forma de "recuperarse"
// (la memoria del proceso puede haber quedado en un estado indefinido;
// intentar seguir corriendo Rust normal después, incluida la pantalla
// roja de `render_crash_screen`, sería más arriesgado que dejarlo
// morir). La única meta acá es dejar CONSTANCIA de que pasó, con lo
// mínimo indispensable, antes de que el proceso termine. El
// diagnóstico real (qué línea, qué función) sale de logcat: Android ya
// genera su propio "tombstone" con backtrace nativo completo vía
// `debuggerd` para estas señales — por eso, después de anotar lo
// nuestro, reencadenamos al manejador por defecto y volvemos a lanzar
// la señal, en vez de tragárnosla.
#[cfg(target_os = "android")]
use std::os::unix::io::AsRawFd;
#[cfg(target_os = "android")]
use std::sync::atomic::{AtomicI32, Ordering};

/// Descriptor crudo (no un `std::fs::File`) del archivo
/// `native_crash.txt`, abierto por adelantado en
/// `install_native_signal_handlers`. No se puede abrir un archivo
/// nuevo DENTRO del manejador de señal (`open()` no es
/// async-signal-safe: puede alocar/lockear internamente), así que
/// dejamos el fd listo de antemano y ahí adentro solo hacemos el
/// syscall crudo `write()`, que sí lo es.
#[cfg(target_os = "android")]
static CRASH_FD: AtomicI32 = AtomicI32::new(-1);

/// Instala los manejadores de señal. Llamar junto con `install()`
/// (panic hook), lo antes posible en `android_main`, con la misma
/// carpeta de crash logs.
#[cfg(target_os = "android")]
pub fn install_native_signal_handlers(crash_dir: Option<&std::path::Path>) {
    if let Some(dir) = crash_dir {
        if let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("native_crash.txt"))
        {
            CRASH_FD.store(file.as_raw_fd(), Ordering::SeqCst);
            // A propósito NO cerramos `file` (lo "leakeamos" con
            // `forget`): el fd tiene que seguir válido durante toda la
            // vida del proceso, porque no hay forma de saber de
            // antemano cuándo (ni si) va a llegar una señal.
            std::mem::forget(file);
        } else {
            log::warn!("No se pudo abrir native_crash.txt para el manejador de señales.");
        }
    }

    unsafe {
        for &sig in &[
            libc::SIGSEGV,
            libc::SIGABRT,
            libc::SIGBUS,
            libc::SIGILL,
            libc::SIGFPE,
        ] {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = handle_fatal_signal as usize;
            action.sa_flags = libc::SA_SIGINFO;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(sig, &action, std::ptr::null_mut());
        }
    }
}

#[cfg(not(target_os = "android"))]
pub fn install_native_signal_handlers(_crash_dir: Option<&std::path::Path>) {}

/// El manejador de señal en sí. Tiene que ser `extern "C"` (lo llama el
/// kernel vía libc, no hay ABI de Rust acá) y evitar por completo
/// cualquier cosa que pueda alocar memoria, tomar un lock, o correr
/// código de Rust "normal" (`log::error!`, `format!`, `String`, un
/// panic) — el estado del proceso en este punto puede estar corrupto
/// (por ejemplo, si el heap fue lo que se corrompió), y usar ese
/// mismo estado roto adentro del manejador es la forma clásica de
/// convertir un crash simple en un deadlock o un segundo crash dentro
/// del primero. Por eso arma el mensaje a mano en un buffer de stack
/// de tamaño fijo (nada de `format!`) y escribe con el syscall crudo
/// `libc::write` (nada de `std::fs::File::write`, que sí podría
/// alocar/lockear internamente).
#[cfg(target_os = "android")]
extern "C" fn handle_fatal_signal(
    sig: libc::c_int,
    _info: *mut libc::siginfo_t,
    _ctx: *mut libc::c_void,
) {
    let fd = CRASH_FD.load(Ordering::SeqCst);
    if fd >= 0 {
        // "=== CRASH NATIVO: senal N ===\n" armado a mano, sin format!.
        let mut buf = [0u8; 64];
        let mut pos = 0;
        for &b in b"=== CRASH NATIVO: senal " {
            buf[pos] = b;
            pos += 1;
        }
        let mut digits = [0u8; 8];
        let mut n = sig;
        let mut ndig = 0;
        if n == 0 {
            digits[0] = b'0';
            ndig = 1;
        } else {
            while n > 0 && ndig < digits.len() {
                digits[ndig] = b'0' + (n % 10) as u8;
                n /= 10;
                ndig += 1;
            }
        }
        for i in (0..ndig).rev() {
            if pos < buf.len() {
                buf[pos] = digits[i];
                pos += 1;
            }
        }
        for &b in b" ===\n" {
            if pos < buf.len() {
                buf[pos] = b;
                pos += 1;
            }
        }
        unsafe {
            libc::write(fd, buf.as_ptr() as *const libc::c_void, pos);
        }
    }

    // Reencadenamos: restauramos el manejador por defecto de esta señal
    // y la volvemos a lanzar, para que el sistema (debuggerd en
    // Android) siga su camino normal y genere el tombstone con
    // backtrace nativo completo en logcat — bastante más útil que lo
    // que podemos armar acá a mano. Si no hiciéramos esto, la señal
    // quedaría "sin resolver" y el proceso podría terminar en un estado
    // más raro que si la dejamos seguir su curso normal.
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

// --- Detección de crash de la corrida ANTERIOR, al arrancar ---
//
// Todo lo de arriba (panic hook, catch_unwind, manejador de señal) sirve
// para reaccionar a un crash DENTRO del mismo proceso en el que pasó.
// Pero un crash nativo de verdad mata el proceso: no hay "pantalla roja"
// posible en esa misma corrida, porque no queda proceso vivo para
// dibujarla. Lo único que se puede hacer es, en el PRÓXIMO arranque,
// preguntar "¿la corrida anterior terminó mal?" y si la respuesta es sí,
// arrancar directo en modo crasheado (ver `run()` en lib.rs) reusando la
// misma pantalla/mecanismo de copiar al portapapeles de siempre.
//
// Se usan dos fuentes, en orden de preferencia:
//
//   1. `ApplicationExitInfo` (Android 11 / API 30+): se lo preguntamos
//      directamente al sistema operativo vía `ActivityManager`. Es la
//      fuente más confiable — Android sabe con certeza si el proceso
//      murió por una señal, un ANR, etc. — y para crash nativo/ANR trae
//      su propio trace completo (el "tombstone"), sin depender de
//      logcat/adb para nada. No existe en API < 30.
//
//   2. El archivo `native_crash.txt` que ya escribe el manejador de
//      señal de arriba: sirve de respaldo universal para API 26-29 (el
//      mínimo de este proyecto) y para el caso en que
//      `ApplicationExitInfo` todavía no se terminó de poblar.
#[cfg(target_os = "android")]
pub fn check_previous_run_crash(crash_dir: Option<&std::path::Path>) -> Option<String> {
    if let Some(short) = check_application_exit_info() {
        return Some(short);
    }
    check_crash_file(crash_dir)
}

#[cfg(not(target_os = "android"))]
pub fn check_previous_run_crash(_crash_dir: Option<&std::path::Path>) -> Option<String> {
    // En desktop no hace falta: si el proceso muere de verdad (por
    // ejemplo un SIGSEGV real) no hay "próximo arranque" que vigilar de
    // la misma forma, y los crashes normales ya se cubren con
    // `catch_unwind` + el diálogo nativo dentro de la misma corrida.
    None
}

#[cfg(target_os = "android")]
fn check_application_exit_info() -> Option<String> {
    let ctx = ndk_context::android_context();
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }.ok()?;
    let mut env = vm.attach_current_thread().ok()?;
    let activity = unsafe { jni::objects::JObject::from_raw(ctx.context().cast()) };

    // ApplicationExitInfo no existe antes de API 30: chequeamos
    // Build.VERSION.SDK_INT antes de tocar nada de esa clase.
    let sdk_int = env
        .find_class("android/os/Build$VERSION")
        .and_then(|c| env.get_static_field(c, "SDK_INT", "I"))
        .and_then(|v| v.i())
        .ok()?;
    if sdk_int < 30 {
        return None;
    }

    let package_name = env
        .call_method(&activity, "getPackageName", "()Ljava/lang/String;", &[])
        .and_then(|v| v.l())
        .ok()?;

    let service_name = env.new_string("activity").ok()?;
    let activity_manager = env
        .call_method(
            &activity,
            "getSystemService",
            "(Ljava/lang/String;)Ljava/lang/Object;",
            &[(&service_name).into()],
        )
        .and_then(|v| v.l())
        .ok()?;

    // getHistoricalProcessExitReasons(packageName, pid=0 -> cualquiera de
    // este paquete, maxNum=1 -> solo la más reciente; vienen ordenadas de
    // más nueva a más vieja).
    let exit_infos = env
        .call_method(
            &activity_manager,
            "getHistoricalProcessExitReasons",
            "(Ljava/lang/String;II)Ljava/util/List;",
            &[(&package_name).into(), 0i32.into(), 1i32.into()],
        )
        .and_then(|v| v.l())
        .ok()?;

    let size = env
        .call_method(&exit_infos, "size", "()I", &[])
        .and_then(|v| v.i())
        .ok()?;
    if size == 0 {
        return None;
    }

    let info = env
        .call_method(&exit_infos, "get", "(I)Ljava/lang/Object;", &[0i32.into()])
        .and_then(|v| v.l())
        .ok()?;

    let reason = env
        .call_method(&info, "getReason", "()I", &[])
        .and_then(|v| v.i())
        .ok()?;

    // Leemos los valores de las constantes REASON_* desde la propia
    // clase en vez de copiarlos a mano: distintas fuentes (documentación
    // oficial vs. artículos de terceros) dan números DISTINTOS para
    // REASON_CRASH/REASON_CRASH_NATIVE según la versión consultada, así
    // que hardcodear un entero acá sería jugársela. Pedirle el valor a
    // la clase siempre da el correcto, sea cual sea la versión de
    // Android que esté corriendo.
    let reason_crash = env
        .find_class("android/app/ApplicationExitInfo")
        .and_then(|c| env.get_static_field(c, "REASON_CRASH", "I"))
        .and_then(|v| v.i())
        .ok()?;
    let reason_crash_native = env
        .find_class("android/app/ApplicationExitInfo")
        .and_then(|c| env.get_static_field(c, "REASON_CRASH_NATIVE", "I"))
        .and_then(|v| v.i())
        .ok()?;
    let reason_anr = env
        .find_class("android/app/ApplicationExitInfo")
        .and_then(|c| env.get_static_field(c, "REASON_ANR", "I"))
        .and_then(|v| v.i())
        .ok()?;

    // Cualquier otro motivo (el usuario cerró la app, el sistema la mató
    // por poca memoria, una actualización de la app...) no es un crash
    // real: no queremos mostrar la pantalla roja por un cierre normal.
    if reason != reason_crash && reason != reason_crash_native && reason != reason_anr {
        return None;
    }

    let description = env
        .call_method(&info, "getDescription", "()Ljava/lang/String;", &[])
        .ok()
        .and_then(|v| v.l().ok())
        .and_then(|obj| env.get_string((&obj).into()).ok())
        .map(|s| s.into())
        .unwrap_or_else(|| "(sin descripción)".to_string());

    let reason_name = if reason == reason_crash_native {
        "CRASH_NATIVO"
    } else if reason == reason_anr {
        "ANR"
    } else {
        "CRASH"
    };

    let timestamp = env
        .call_method(&info, "getTimestamp", "()J", &[])
        .and_then(|v| v.j())
        .unwrap_or(0);

    // El trace (tombstone completo para crash nativo/ANR) viene como
    // InputStream, no como String directo: hay que leerlo a mano.
    let trace = read_trace_input_stream(&mut env, &info).unwrap_or_default();

    let full_text = format!(
        "=== Voxel Engine: reporte de ApplicationExitInfo (corrida anterior) ===\n\
         Motivo: {reason_name} (código {reason})\n\
         Timestamp (ms): {timestamp}\n\
         Descripción: {description}\n\
         \n\
         --- Trace ---\n\
         {trace}\n"
    );
    let short_message = format!("{reason_name}: {description}");

    let mut slot = last_crash_slot().lock().unwrap_or_else(|e| e.into_inner());
    *slot = Some(CrashReport {
        full_text,
        short_message: short_message.clone(),
        file_path: None,
    });
    drop(slot);

    Some(short_message)
}

/// Lee un `java.io.InputStream` completo a un `String`, de a bloques de
/// 4KB por `read(byte[])`. No hay atajo más corto en JNI: no existe un
/// `InputStream` -> `String` directo del lado nativo.
#[cfg(target_os = "android")]
fn read_trace_input_stream(env: &mut jni::JNIEnv, info: &jni::objects::JObject) -> Option<String> {
    let stream = env
        .call_method(info, "getTraceInputStream", "()Ljava/io/InputStream;", &[])
        .and_then(|v| v.l())
        .ok()?;
    if stream.is_null() {
        // Es normal: REASON_CRASH (Java) no siempre trae trace, solo
        // REASON_CRASH_NATIVE / REASON_ANR lo traen consistentemente.
        return None;
    }

    let mut out: Vec<u8> = Vec::new();
    let byte_array = env.new_byte_array(4096).ok()?;
    loop {
        let read = env
            .call_method(&stream, "read", "([B)I", &[(&byte_array).into()])
            .and_then(|v| v.i())
            .ok()?;
        if read <= 0 {
            break;
        }
        let mut chunk = vec![0i8; read as usize];
        env.get_byte_array_region(&byte_array, 0, &mut chunk).ok()?;
        out.extend(chunk.iter().map(|&b| b as u8));
    }
    let _ = env.call_method(&stream, "close", "()V", &[]);

    Some(String::from_utf8_lossy(&out).into_owned())
}

/// Fallback para cuando `ApplicationExitInfo` no está disponible (API <
/// 30) o todavía no tiene nada útil: revisa si el manejador de señal de
/// la corrida anterior dejó un `native_crash.txt` (prioridad, porque ese
/// es justo el caso — un crash que mató el proceso sin pasar por el
/// panic hook) o, si no, un `last_crash.txt` (panic de Rust normal que
/// también mató el proceso antes de que la app llegara a mostrar su
/// propia pantalla roja, por ejemplo si pasó muy al principio del
/// arranque). El archivo se renombra después de leerlo, para no volver a
/// mostrar el mismo crash en cada arranque futuro.
#[cfg(target_os = "android")]
fn check_crash_file(crash_dir: Option<&std::path::Path>) -> Option<String> {
    let dir = crash_dir?;
    for name in ["native_crash.txt", "last_crash.txt"] {
        let path = dir.join(name);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        let _ = std::fs::rename(&path, dir.join(format!("{name}.handled")));

        let short_message = text
            .lines()
            .find(|l| l.starts_with("Mensaje:") || l.starts_with("=== CRASH NATIVO"))
            .unwrap_or("(crash detectado en la corrida anterior)")
            .to_string();

        let mut slot = last_crash_slot().lock().unwrap_or_else(|e| e.into_inner());
        *slot = Some(CrashReport {
            full_text: text,
            short_message: short_message.clone(),
            file_path: Some(path),
        });
        drop(slot);

        return Some(short_message);
    }
    None
}

/// immersive.rs
/// Pantalla completa "inmersiva" en Android: oculta la barra de estado y
/// la barra de navegación del sistema, igual que cualquier juego, para
/// que no se coman parte de la pantalla que el overlay táctil (ver
/// `touch.rs`/`ui_overlay.rs`) ya está usando para sus controles.
///
/// `NativeActivity` (la Activity que usa `winit` con el feature
/// `android-native-activity`) no expone esto por sí sola, así que hay
/// que pedírselo por JNI directamente, el mismo mecanismo (y el mismo
/// patrón de JNI) que ya usa `crash::copy_to_clipboard_android` para el
/// portapapeles del sistema.
///
/// Hay dos APIs de Android en juego acá, según la versión del teléfono
/// (leemos `Build.VERSION.SDK_INT` en runtime, ver `sdk_int()`):
///   - Android 11 / API 30 en adelante: `WindowInsetsController`
///     (clase del framework — no hace falta AndroidX, que este proyecto
///     no usa por ser `NativeActivity` puro). Es la API vigente.
///   - Android 8-9 / API 26-29 (nuestro `min_sdk_version`, ver
///     Cargo.toml): `WindowInsetsController` no existe todavía, así que
///     ahí sí hace falta la API vieja `View.setSystemUiVisibility`.
///
/// REDISEÑO DEFENSIVO (tras un crash nativo real en un HONOR con
/// Android 14/SDK 34): tener SDK_INT >= 30 NO garantiza que el
/// fabricante haya dejado `WindowInsetsController` intacto — algunas
/// capas de personalización (MagicOS, EMUI, MIUI, etc.) modifican
/// `Window`/`View` lo suficiente como para que `GetMethodID` falle en
/// tiempo de ejecución con `NoSuchMethodError`, algo que ningún chequeo
/// de versión en el manifiesto puede predecir.
///
/// Ese `NoSuchMethodError` es una excepción *Java* que queda pendiente
/// sobre el `JNIEnv`. Si no se limpia explícitamente antes de la
/// siguiente llamada JNI, el comportamiento es indefinido a nivel de
/// ART y en la práctica se manifiesta como un abort nativo (SIGSEGV) —
/// que ni siquiera pasa por nuestro `catch_unwind` en `lib.rs`, porque
/// no es un panic de Rust. Por eso acá:
///
///   1. Toda llamada JNI pasa por `jni_call`, que SIEMPRE chequea y
///      limpia una excepción pendiente después de la llamada, sin
///      confiar en que el crate `jni` lo haga por nosotros en todas
///      sus versiones.
///   2. Los pasos "lindo tenerlo" (setDecorFitsSystemWindows,
///      setSystemBarsBehavior, el tipo exacto de barra) pueden fallar
///      individualmente sin abortar el camino moderno completo.
///   3. Si el camino moderno falla en un paso esencial (no hay
///      `InsetsController`, o `hide()` no está), se cae automáticamente
///      a la API vieja `setSystemUiVisibility` — pase lo que pase con
///      `SDK_INT`. En el peor caso (ninguna de las dos anda) la función
///      simplemente loguea y sigue: las barras del sistema quedan
///      visibles, pero el juego no crashea.
///
/// Se llama desde `resumed()` en lib.rs cada vez que se (re)crea la
/// ventana, y también desde `WindowEvent::Focused(true)`: Android
/// resetea el modo inmersivo cada vez que la ventana recupera foco (por
/// ejemplo, al volver de segundo plano, o al abrir el cajón de
/// notificaciones y cerrarlo), así que no alcanza con pedirlo una sola
/// vez al arrancar.
#[cfg(target_os = "android")]
pub fn apply_immersive_fullscreen() {
    if let Err(e) = apply_immersive_fullscreen_impl() {
        log::warn!("No se pudo activar pantalla completa inmersiva: {}", e);
    }
}

#[cfg(not(target_os = "android"))]
pub fn apply_immersive_fullscreen() {}

#[cfg(target_os = "android")]
fn apply_immersive_fullscreen_impl() -> Result<(), String> {
    let ctx = ndk_context::android_context();
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| e.to_string())?;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    let activity = unsafe { jni::objects::JObject::from_raw(ctx.context().cast()) };

    let Some(window) = jni_call(&mut env, "Activity.getWindow", |env| {
        env.call_method(&activity, "getWindow", "()Landroid/view/Window;", &[])
            .and_then(|v| v.l())
    }) else {
        return Err("no se pudo obtener Window en este dispositivo".to_string());
    };

    // Si ni siquiera se puede leer SDK_INT, asumimos lo peor (0) y
    // vamos directo al camino viejo, que es el mínimo común denominador.
    let sdk = sdk_int(&mut env).unwrap_or(0);

    if sdk >= 30 {
        if apply_via_insets_controller(&mut env, &window) {
            return Ok(());
        }
        log::warn!(
            "WindowInsetsController no disponible o falló pese a SDK_INT={} \
             (probable personalización del fabricante) — se prueba la API vieja",
            sdk
        );
    }

    if apply_via_legacy_flags(&mut env, &window) {
        Ok(())
    } else {
        Err("ninguna de las dos APIs de pantalla completa inmersiva funcionó en este dispositivo".to_string())
    }
}

/// Ejecuta una llamada JNI fallible y, pase lo que pase (éxito o
/// error), chequea si quedó una excepción Java pendiente sobre el
/// `JNIEnv` y la limpia. No confiamos en que el crate `jni` la limpie
/// por nosotros en todos los casos/versiones — hacerlo acá, en un solo
/// lugar, evita que una excepción de una llamada se filtre y corrompa
/// la siguiente (la causa real del crash nativo original).
///
/// Devuelve `None` tanto si la llamada dio `Err` como si dejó una
/// excepción pendiente; en ambos casos ya quedó logueado el motivo.
#[cfg(target_os = "android")]
fn jni_call<T>(
    env: &mut jni::JNIEnv,
    what: &str,
    f: impl FnOnce(&mut jni::JNIEnv) -> Result<T, jni::errors::Error>,
) -> Option<T> {
    let result = f(env);

    if env.exception_check() {
        log::warn!(
            "JNI: excepción pendiente tras \"{}\" (método/clase probablemente ausente \
             en este dispositivo) — se limpia para no corromper la siguiente llamada",
            what
        );
        let _ = env.exception_describe(); // deja el detalle completo en logcat
        let _ = env.exception_clear();
        return None;
    }

    match result {
        Ok(v) => Some(v),
        Err(e) => {
            log::warn!("JNI: \"{}\" falló: {}", what, e);
            None
        }
    }
}

/// Lee `Build.VERSION.SDK_INT` (el nivel de API real del teléfono en el
/// que corre la app, no el `target_sdk_version`/`min_sdk_version` de
/// compilación) para elegir qué API de pantalla completa probar primero.
#[cfg(target_os = "android")]
fn sdk_int(env: &mut jni::JNIEnv) -> Option<i32> {
    jni_call(env, "Build.VERSION.SDK_INT", |env| {
        env.get_static_field("android/os/Build$VERSION", "SDK_INT", "I")
            .and_then(|v| v.i())
    })
}

/// Camino nuevo (Android 11 / API 30+ nominalmente): `WindowInsetsController`.
/// Devuelve `true` solo si se llegó a ocultar las barras (lo esencial);
/// los extras (edge-to-edge, comportamiento sticky) se intentan pero no
/// son condición para el éxito.
#[cfg(target_os = "android")]
fn apply_via_insets_controller(env: &mut jni::JNIEnv, window: &jni::objects::JObject) -> bool {
    use jni::objects::JValue;

    // No esencial: si falla, en el peor caso queda una franja donde
    // estaban las barras, pero no vale la pena abortar el camino
    // moderno completo por esto.
    jni_call(env, "Window.setDecorFitsSystemWindows", |env| {
        env.call_method(
            window,
            "setDecorFitsSystemWindows",
            "(Z)V",
            &[JValue::Bool(false as u8)],
        )
    });

    let Some(controller) = jni_call(env, "Window.getInsetsController", |env| {
        env.call_method(
            window,
            "getInsetsController",
            "()Landroid/view/WindowInsetsController;",
            &[],
        )
        .and_then(|v| v.l())
    }) else {
        return false; // esencial: sin esto no hay camino moderno
    };

    if controller.is_null() {
        log::warn!("Window.getInsetsController() devolvió null en este dispositivo");
        return false;
    }

    // Si alguno de los dos tipos de barra no se puede resolver, seguimos
    // igual con el otro (mejor ocultar una barra que ninguna).
    let status_bars = jni_call(env, "WindowInsets.Type.statusBars", |env| {
        env.call_static_method("android/view/WindowInsets$Type", "statusBars", "()I", &[])
            .and_then(|v| v.i())
    })
    .unwrap_or(0);
    let nav_bars = jni_call(env, "WindowInsets.Type.navigationBars", |env| {
        env.call_static_method(
            "android/view/WindowInsets$Type",
            "navigationBars",
            "()I",
            &[],
        )
        .and_then(|v| v.i())
    })
    .unwrap_or(0);

    if status_bars == 0 && nav_bars == 0 {
        log::warn!("No se pudo resolver ningún WindowInsets.Type en este dispositivo");
        return false;
    }

    let hidden = jni_call(env, "WindowInsetsController.hide", |env| {
        env.call_method(
            &controller,
            "hide",
            "(I)V",
            &[JValue::Int(status_bars | nav_bars)],
        )
    })
    .is_some();

    if !hidden {
        return false; // esencial
    }

    // No esencial: el modo "sticky" al arrastrar desde el borde es un
    // extra, no lo necesario para estar en pantalla completa.
    if let Some(behavior) = jni_call(
        env,
        "WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE",
        |env| {
            env.get_static_field(
                "android/view/WindowInsetsController",
                "BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE",
                "I",
            )
            .and_then(|v| v.i())
        },
    ) {
        jni_call(env, "WindowInsetsController.setSystemBarsBehavior", |env| {
            env.call_method(
                &controller,
                "setSystemBarsBehavior",
                "(I)V",
                &[JValue::Int(behavior)],
            )
        });
    }

    true
}

/// Camino viejo: `View.setSystemUiVisibility`, deprecado desde API 30
/// pero es el único que existe en API 26-29, y ahora también el
/// fallback para cualquier dispositivo donde el camino moderno falle
/// por la razón que sea.
#[cfg(target_os = "android")]
fn apply_via_legacy_flags(env: &mut jni::JNIEnv, window: &jni::objects::JObject) -> bool {
    // View.SYSTEM_UI_FLAG_LAYOUT_STABLE (0x100) | LAYOUT_HIDE_NAVIGATION
    // (0x200) | LAYOUT_FULLSCREEN (0x400) | HIDE_NAVIGATION (0x2) |
    // FULLSCREEN (0x4) | IMMERSIVE_STICKY (0x1000): la combinación
    // estándar de "modo inmersivo pegajoso".
    const IMMERSIVE_FLAGS: i32 = 0x100 | 0x200 | 0x400 | 0x2 | 0x4 | 0x1000;

    let Some(decor_view) = jni_call(env, "Window.getDecorView", |env| {
        env.call_method(window, "getDecorView", "()Landroid/view/View;", &[])
            .and_then(|v| v.l())
    }) else {
        return false;
    };

    jni_call(env, "View.setSystemUiVisibility", |env| {
        env.call_method(
            &decor_view,
            "setSystemUiVisibility",
            "(I)V",
            &[jni::objects::JValue::Int(IMMERSIVE_FLAGS)],
        )
    })
    .is_some()
}

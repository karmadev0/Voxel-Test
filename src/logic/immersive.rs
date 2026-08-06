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
/// `View.setSystemUiVisibility` está deprecated desde API 30 y, más
/// grave, en Android 15 / API 35 con edge-to-edge forzado esas flags se
/// ignoran directamente y las barras no se ocultan — por eso no la
/// usamos como camino principal aunque el `target_sdk_version` actual
/// (34, Android 14) todavía la respetaría.
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

    let window = env
        .call_method(&activity, "getWindow", "()Landroid/view/Window;", &[])
        .and_then(|v| v.l())
        .map_err(|e| e.to_string())?;

    if sdk_int(&mut env)? >= 30 {
        apply_via_insets_controller(&mut env, &window)
    } else {
        apply_via_legacy_flags(&mut env, &window)
    }
}

/// Lee `Build.VERSION.SDK_INT` (el nivel de API real del teléfono en el
/// que corre la app, no el `target_sdk_version`/`min_sdk_version` de
/// compilación) para elegir qué API de pantalla completa usar.
#[cfg(target_os = "android")]
fn sdk_int(env: &mut jni::JNIEnv) -> Result<i32, String> {
    env.get_static_field("android/os/Build$VERSION", "SDK_INT", "I")
        .and_then(|v| v.i())
        .map_err(|e| e.to_string())
}

/// Camino nuevo (Android 11 / API 30+): `WindowInsetsController`.
/// Es una clase del framework (`android.view.*`), no de AndroidX, así
/// que no hace falta ninguna dependencia extra para llamarla por JNI.
#[cfg(target_os = "android")]
fn apply_via_insets_controller(
    env: &mut jni::JNIEnv,
    window: &jni::objects::JObject,
) -> Result<(), String> {
    use jni::objects::JValue;

    // Window.setDecorFitsSystemWindows(false): el contenido pasa a
    // dibujarse por debajo de donde estarían las barras, en vez de que
    // el sistema le achique el área disponible. Sin esto, ocultar las
    // barras con el controller de más abajo puede dejar una franja
    // negra donde estaban.
    env.call_method(
        window,
        "setDecorFitsSystemWindows",
        "(Z)V",
        &[JValue::Bool(false as u8)],
    )
    .map_err(|e| e.to_string())?;

    let controller = env
        .call_method(
            window,
            "getInsetsController",
            "()Landroid/view/WindowInsetsController;",
            &[],
        )
        .and_then(|v| v.l())
        .map_err(|e| e.to_string())?;

    // WindowInsets.Type.statusBars() | WindowInsets.Type.navigationBars():
    // qué barras ocultar. Son métodos estáticos, no constantes, porque
    // el valor real puede variar de dispositivo a dispositivo (por eso
    // se piden por JNI en vez de hardcodear un bitmask como en la API
    // vieja).
    let status_bars = env
        .call_static_method("android/view/WindowInsets$Type", "statusBars", "()I", &[])
        .and_then(|v| v.i())
        .map_err(|e| e.to_string())?;
    let nav_bars = env
        .call_static_method(
            "android/view/WindowInsets$Type",
            "navigationBars",
            "()I",
            &[],
        )
        .and_then(|v| v.i())
        .map_err(|e| e.to_string())?;

    env.call_method(
        &controller,
        "hide",
        "(I)V",
        &[JValue::Int(status_bars | nav_bars)],
    )
    .map_err(|e| e.to_string())?;

    // BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE: si el jugador arrastra
    // desde el borde, las barras aparecen un momento en modo
    // semitransparente y se esconden solas, igual que el viejo
    // IMMERSIVE_STICKY. Se lee como campo estático en vez de
    // hardcodear el número por la misma razón que los tipos de barra
    // de arriba.
    let behavior = env
        .get_static_field(
            "android/view/WindowInsetsController",
            "BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE",
            "I",
        )
        .and_then(|v| v.i())
        .map_err(|e| e.to_string())?;

    env.call_method(
        &controller,
        "setSystemBarsBehavior",
        "(I)V",
        &[JValue::Int(behavior)],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

/// Camino viejo, solo para Android 8-9 / API 26-29 (`min_sdk_version`
/// en Cargo.toml): ahí `WindowInsetsController` todavía no existe, así
/// que no queda otra que la API deprecada.
#[cfg(target_os = "android")]
fn apply_via_legacy_flags(
    env: &mut jni::JNIEnv,
    window: &jni::objects::JObject,
) -> Result<(), String> {
    let decor_view = env
        .call_method(window, "getDecorView", "()Landroid/view/View;", &[])
        .and_then(|v| v.l())
        .map_err(|e| e.to_string())?;

    // View.SYSTEM_UI_FLAG_LAYOUT_STABLE (0x100) | LAYOUT_HIDE_NAVIGATION
    // (0x200) | LAYOUT_FULLSCREEN (0x400) | HIDE_NAVIGATION (0x2) |
    // FULLSCREEN (0x4) | IMMERSIVE_STICKY (0x1000): la combinación
    // estándar de "modo inmersivo pegajoso" en las versiones donde
    // `WindowInsetsController` no existe todavía.
    const IMMERSIVE_FLAGS: i32 = 0x100 | 0x200 | 0x400 | 0x2 | 0x4 | 0x1000;

    env.call_method(
        &decor_view,
        "setSystemUiVisibility",
        "(I)V",
        &[jni::objects::JValue::Int(IMMERSIVE_FLAGS)],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

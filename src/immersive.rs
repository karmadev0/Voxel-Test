/// immersive.rs
/// Pantalla completa "inmersiva" en Android: oculta la barra de estado y
/// la barra de navegación del sistema, igual que cualquier juego, para
/// que no se coman parte de la pantalla que el overlay táctil (ver
/// `touch.rs`/`ui_overlay.rs`) ya está usando para sus controles.
///
/// `NativeActivity` (la Activity que usa `winit` con el feature
/// `android-native-activity`) no expone esto por sí sola, así que hay
/// que pedírselo por JNI directamente a `View.setSystemUiVisibility`, el
/// mismo mecanismo (y el mismo patrón de JNI) que ya usa
/// `crash::copy_to_clipboard_android` para el portapapeles del sistema.
///
/// Se llama desde `resumed()` en lib.rs cada vez que se (re)crea la
/// ventana: Android resetea estas flags cada vez que la Activity vuelve
/// a primer plano (por ejemplo, al volver de segundo plano, o al abrir
/// el cajón de notificaciones y cerrarlo), así que no alcanza con
/// pedirlo una sola vez al arrancar.
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

    let decor_view = env
        .call_method(&window, "getDecorView", "()Landroid/view/View;", &[])
        .and_then(|v| v.l())
        .map_err(|e| e.to_string())?;

    // View.SYSTEM_UI_FLAG_LAYOUT_STABLE (0x100) | LAYOUT_HIDE_NAVIGATION
    // (0x200) | LAYOUT_FULLSCREEN (0x400) | HIDE_NAVIGATION (0x2) |
    // FULLSCREEN (0x4) | IMMERSIVE_STICKY (0x1000). La combinación
    // estándar para "modo inmersivo pegajoso": las barras se ocultan y,
    // si el jugador las hace aparecer arrastrando desde el borde, vuelven
    // a esconderse solas después de un momento sin tocar el borde de
    // nuevo. `setSystemUiVisibility` está marcado deprecated desde
    // Android 11 (a favor de `WindowInsetsController`), pero sigue
    // andando en todas las versiones que este proyecto soporta
    // (min_sdk_version 26 hasta target_sdk_version 34, ver Cargo.toml) y
    // evita depender de AndroidX, que no está disponible en este
    // proyecto (usa `NativeActivity` puro, no una Activity de Kotlin).
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

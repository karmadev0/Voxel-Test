/// main.rs
/// Punto de entrada del binario de escritorio. Toda la lógica del engine
/// vive en lib.rs (compartida con el punto de entrada de Android,
/// `android_main`, generado por cargo-apk).
fn main() {
    voxel_engine::run_desktop();
}

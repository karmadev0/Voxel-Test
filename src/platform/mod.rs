/// platform/mod.rs
/// Bindings a tecnología específica de plataforma (reporte de crash con
/// clipboard nativo en desktop/Android vía JNI, y logger que además de
/// stderr/logcat escribe a un archivo de texto — ver file_logger.rs).
pub mod crash;
pub mod file_logger;

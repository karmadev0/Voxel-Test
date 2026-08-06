//! build.rs
//!
//! Reemplaza a `cube_template_tool` en el flujo de trabajo: en vez de armar
//! el atlas a mano con una plantilla "cruz", cada bloque tiene sus 6 PNG
//! sueltos (uno por cara) en `assets/textures/blocks/`, y este script los
//! combina automáticamente en un solo atlas cada vez que se compila.
//!
//! Por qué build.rs y no runtime: el mesher arma el mesh de un chunk
//! entero asumiendo un solo bind group de textura (ver comentario en
//! textures/atlas.rs). Subir cada PNG suelto a la GPU por separado
//! rompería eso. build.rs deja el atlas combinado listo *antes* de que
//! el binario exista, así que en runtime (`textures/loader.rs`) seguimos
//! embebiendo un solo PNG con `include_bytes!`, sin tocar filesystem en
//! Android.
//!
//! Convención de archivos (ver assets/textures/blocks.txt para el orden
//! de bloques -> filas del atlas):
//!   assets/textures/blocks/<bloque>_<cara>.png
//! con <cara> en {top, bottom, north, south, east, west} — siempre las 6,
//! aunque el bloque sea uniforme (mismo contenido repetido en las 6).
//!
//! Cada bloque ocupa una fila completa de 6 columnas, mismo layout que ya
//! generaba `cube_template_tool` (ver `atlas::FACE_COLUMN_ORDER`). Si se
//! agrega o quita un bloque de blocks.txt, ATLAS_ROWS en textures/atlas.rs
//! y en shaders/shader.wgsl tienen que actualizarse a mano para que
//! coincidan con la cantidad de líneas de blocks.txt — este script no
//! puede tocar el shader WGSL, así que lo avisamos con cargo:warning.

use image::{imageops::FilterType, GenericImageView, RgbaImage};
use std::path::Path;

const TILE_PX: u32 = 16;

// Mismo orden que `atlas::FACE_COLUMN_ORDER` en textures/atlas.rs.
const FACES: [&str; 6] = ["top", "bottom", "north", "south", "east", "west"];

fn main() {
    // --- Etiqueta de build para el overlay en pantalla ---
    // Se puede fijar al compilar con `BUILD_TAG=voxel-engine-build-35-06082026
    // cargo build`. Si no se define, usamos un default que igual sirve para
    // distinguir builds locales de desarrollo. `rustc-env` deja la variable
    // disponible en tiempo de compilación vía `env!("VOXEL_BUILD_TAG")` en
    // el código Rust (a diferencia de `std::env::var` en runtime, que no
    // vería nada porque la env var del build no persiste al ejecutar el
    // binario final).
    println!("cargo:rerun-if-env-changed=BUILD_TAG");
    let build_tag = std::env::var("BUILD_TAG").unwrap_or_else(|_| "voxel-engine-dev".to_string());
    println!("cargo:rustc-env=VOXEL_BUILD_TAG={}", build_tag);

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR no está seteado (¿corriendo fuera de cargo?)");
    let textures_dir = Path::new(&manifest_dir).join("assets/textures");
    let blocks_dir = textures_dir.join("blocks");
    let blocks_list_path = textures_dir.join("blocks.txt");

    // Le dice a cargo que re-corra este script si cambia la lista de
    // bloques o cualquier PNG suelto (si no, un `cargo build` incremental
    // podría no notar que agregaste/cambiaste una textura).
    println!("cargo:rerun-if-changed={}", blocks_list_path.display());
    println!("cargo:rerun-if-changed={}", blocks_dir.display());

    let blocks_txt = std::fs::read_to_string(&blocks_list_path).unwrap_or_else(|e| {
        panic!(
            "no pude leer '{}': {} — este archivo lista los bloques, uno por línea, \
             en el orden en que van a ocupar filas del atlas",
            blocks_list_path.display(),
            e
        )
    });
    let block_names: Vec<&str> = blocks_txt
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();

    if block_names.is_empty() {
        panic!(
            "'{}' está vacío: necesito al menos un bloque listado",
            blocks_list_path.display()
        );
    }

    let cols = FACES.len() as u32;
    let rows = block_names.len() as u32;
    let mut atlas = RgbaImage::new(cols * TILE_PX, rows * TILE_PX);

    for (row, block) in block_names.iter().enumerate() {
        for (col, face) in FACES.iter().enumerate() {
            let path = blocks_dir.join(format!("{}_{}.png", block, face));
            println!("cargo:rerun-if-changed={}", path.display());

            let img = image::open(&path).unwrap_or_else(|e| {
                panic!(
                    "no pude abrir '{}': {}\n\
                     Todo bloque en blocks.txt necesita sus 6 caras en \
                     assets/textures/blocks/: {}_top.png, {}_bottom.png, \
                     {}_north.png, {}_south.png, {}_east.png, {}_west.png \
                     (aunque el contenido sea igual en todas).",
                    path.display(),
                    e,
                    block,
                    block,
                    block,
                    block,
                    block,
                    block
                )
            });

            let (w, h) = img.dimensions();
            let tile = if w != TILE_PX || h != TILE_PX {
                println!(
                    "cargo:warning={} mide {}x{}, lo reescalo a {}x{} (Nearest)",
                    path.display(),
                    w,
                    h,
                    TILE_PX,
                    TILE_PX
                );
                image::imageops::resize(&img.to_rgba8(), TILE_PX, TILE_PX, FilterType::Nearest)
            } else {
                img.to_rgba8()
            };

            image::imageops::overlay(
                &mut atlas,
                &tile,
                (col as u32 * TILE_PX) as i64,
                (row as u32 * TILE_PX) as i64,
            );
        }
    }

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR no está seteado por cargo");
    let out_path = Path::new(&out_dir).join("atlas.png");
    atlas
        .save(&out_path)
        .unwrap_or_else(|e| panic!("no pude guardar '{}': {}", out_path.display(), e));

    println!(
        "cargo:warning=atlas.png generado: {} bloques -> {} filas x {} columnas. \
         Verificá que ATLAS_ROWS = {} en textures/atlas.rs Y en shaders/shader.wgsl.",
        rows, rows, cols, rows
    );
}

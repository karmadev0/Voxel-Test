//! cube_template_tool
//!
//! Convierte una plantilla de cubo (imagen "cruz" de 3 columnas x 4 filas
//! de celdas iguales) en los 6 tiles de una fila del atlas de texturas
//! (assets/textures/atlas.png), en el orden que espera
//! `atlas::FACE_COLUMN_ORDER` (Top, Bottom, North, South, East, West).
//!
//! Layout esperado de la plantilla (cada celda del mismo tamaño,
//! el resto de las celdas de la grilla 3x4 se ignoran):
//!
//!     .     Top    .
//!     Left  Front  Right
//!     .     Bottom .
//!     .     Back   .
//!
//! Es el mismo layout de "cruz" que ya veníamos usando a mano (ver
//! historial: "grilla de 3×4 celdas, cruz de cubo"), ahora automatizado.
//! Front/Back/Left/Right de la plantilla se mapean a South/North/West/East
//! del motor (ver la convención de orientación en textures/atlas.rs).
//!
//! Uso:
//!   cargo run --bin cube_template_tool -- <plantilla.png> <fila_destino> [atlas.png]
//!
//! Ejemplo (bloque nuevo en la fila 1 del atlas, ej. horno):
//!   cargo run --bin cube_template_tool -- horno_template.png 1
//!
//! No hace falta el filesystem de Android acá: esta herramienta corre en
//! desktop/Termux, edita assets/textures/atlas.png en disco, y el
//! `include_bytes!` de loader.rs lo vuelve a embeber en el próximo build
//! del engine.
//!
//! No depende del crate `voxel_engine` (no usa wgpu/winit), así que
//! compila rápido y corre bien en Termux/ARM64.

use image::{imageops::FilterType, GenericImageView, RgbaImage};
use std::env;
use std::path::Path;

const TILE_PX: u32 = 16;
const ATLAS_COLS: u32 = 6;

/// Debe coincidir con el orden de `atlas::FACE_COLUMN_ORDER` en
/// textures/atlas.rs: Top, Bottom, North, South, East, West.
/// `(col_en_plantilla, fila_en_plantilla)` de la celda que le corresponde
/// a cada columna del atlas, en la grilla 3x4 de la plantilla.
const TEMPLATE_CELLS: [(u32, u32); 6] = [
    (1, 0), // Top
    (1, 2), // Bottom
    (1, 3), // North  <- "Back" de la plantilla
    (1, 1), // South  <- "Front" de la plantilla
    (2, 1), // East   <- "Right" de la plantilla
    (0, 1), // West   <- "Left" de la plantilla
];

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "Uso: cube_template_tool <plantilla.png> <fila_destino> [ruta_atlas.png]\n\n\
             <plantilla.png>   imagen cruz de cubo, grilla 3 columnas x 4 filas de celdas iguales\n\
             <fila_destino>    fila del atlas (0-indexed) donde escribir las 6 caras\n\
             [ruta_atlas.png]  por defecto: assets/textures/atlas.png"
        );
        std::process::exit(1);
    }

    let template_path = &args[1];
    let row: u32 = args[2]
        .parse()
        .unwrap_or_else(|_| panic!("<fila_destino> debe ser un número entero, recibí '{}'", args[2]));
    let atlas_path = args
        .get(3)
        .cloned()
        .unwrap_or_else(|| "assets/textures/atlas.png".to_string());

    let template = image::open(template_path)
        .unwrap_or_else(|e| panic!("no pude abrir la plantilla '{}': {}", template_path, e));
    let (tw, th) = template.dimensions();
    if tw % 3 != 0 || th % 4 != 0 {
        eprintln!(
            "Advertencia: la plantilla mide {}x{}, no es divisible exacto en una grilla 3x4. \
             Las celdas se van a redondear, revisá el resultado.",
            tw, th
        );
    }
    let cell_w = tw / 3;
    let cell_h = th / 4;
    if cell_w == 0 || cell_h == 0 {
        panic!("la plantilla es demasiado chica para una grilla 3x4 ({}x{})", tw, th);
    }

    // Extraer y reescalar las 6 celdas a TILE_PX x TILE_PX con filtro
    // Nearest (no Lanczos/Linear): mantiene el look pixel-art nítido en
    // vez de emborronar los bordes, igual que hace el sampler del shader.
    let tiles: Vec<RgbaImage> = TEMPLATE_CELLS
        .iter()
        .map(|&(cx, cy)| {
            let cropped = template
                .view(cx * cell_w, cy * cell_h, cell_w, cell_h)
                .to_image();
            image::imageops::resize(&cropped, TILE_PX, TILE_PX, FilterType::Nearest)
        })
        .collect();

    // Abrir el atlas existente (o crear uno transparente nuevo si no
    // existe todavía) y agrandar el canvas si `row` no entra en el
    // tamaño actual, preservando todos los tiles ya dibujados.
    let needed_rows = row + 1;
    let mut atlas: RgbaImage = if Path::new(&atlas_path).exists() {
        let existing = image::open(&atlas_path)
            .unwrap_or_else(|e| panic!("no pude abrir el atlas '{}': {}", atlas_path, e))
            .to_rgba8();
        let (ew, eh) = existing.dimensions();
        let cur_rows = eh / TILE_PX;
        let target_h = cur_rows.max(needed_rows) * TILE_PX;
        let target_w = ew.max(ATLAS_COLS * TILE_PX);
        if target_w == ew && target_h == eh {
            existing
        } else {
            let mut grown = RgbaImage::new(target_w, target_h);
            image::imageops::overlay(&mut grown, &existing, 0, 0);
            grown
        }
    } else {
        RgbaImage::new(ATLAS_COLS * TILE_PX, needed_rows * TILE_PX)
    };

    for (col, tile) in tiles.iter().enumerate() {
        image::imageops::overlay(&mut atlas, tile, (col as u32 * TILE_PX) as i64, (row * TILE_PX) as i64);
    }

    // Backup antes de pisar el atlas actual, por las dudas.
    if Path::new(&atlas_path).exists() {
        let backup = format!("{}.bak", atlas_path);
        std::fs::copy(&atlas_path, &backup)
            .unwrap_or_else(|e| panic!("no pude hacer backup en '{}': {}", backup, e));
    }
    atlas
        .save(&atlas_path)
        .unwrap_or_else(|e| panic!("no pude guardar '{}': {}", atlas_path, e));

    println!("OK: fila {} de '{}' actualizada con las 6 caras de '{}'.", row, atlas_path, template_path);
    println!(
        "Atlas final: {}x{} px ({} cols x {} filas de {}px).",
        atlas.width(),
        atlas.height(),
        atlas.width() / TILE_PX,
        atlas.height() / TILE_PX,
        TILE_PX
    );
    println!(
        "\nPara usar este bloque en el motor, en textures/atlas.rs agregá algo como:\n\
         \n    BlockType::TuBloqueNuevo => atlas::tile_for_row({row}, face),\n\
         \ndentro del match de `tile_for` (delega a `tile_for_row` para las 6 caras reales),\n\
         y agregá la variante correspondiente a `BlockType` en environment/chunk.rs."
    );
}

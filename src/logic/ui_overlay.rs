/// ui_overlay.rs
/// Geometría 2D del overlay táctil de Android: joystick, botones,
/// hotbar, mira, contador FPS y la nueva pantalla fullscreen de
/// configuración (que pausa el juego).
///
/// Novedad: sistema de fuentes de bitmap para texto legible en pantalla.
/// Cada letra se dibuja como una serie de quads con una resolución de
/// 5×7 píxeles de "celda". Soporta mayúsculas A-Z, dígitos 0-9 y algunos
/// símbolos. Esto permite mostrar "JUEGO PAUSADO", etiquetas de opciones,
/// etc., sin necesidad de cargar un atlas de textura externo.
use crate::environment::chunk::BlockType;
use crate::logic::touch::TouchController;
use bytemuck::{Pod, Zeroable};
use winit::dpi::PhysicalSize;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct UiVertex {
    pub position: [f32; 2],
    pub color: [f32; 4],
}

impl UiVertex {
    pub fn desc() -> wgpu::VertexBufferLayout<'static> {
        use std::mem;
        wgpu::VertexBufferLayout {
            array_stride: mem::size_of::<UiVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x2,
                },
                wgpu::VertexAttribute {
                    offset: mem::size_of::<[f32; 2]>() as wgpu::BufferAddress,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x4,
                },
            ],
        }
    }
}

fn to_ndc(px: f64, py: f64, size: PhysicalSize<u32>) -> [f32; 2] {
    let x = (px / size.width.max(1) as f64) * 2.0 - 1.0;
    let y = 1.0 - (py / size.height.max(1) as f64) * 2.0;
    [x as f32, y as f32]
}

fn push_quad(
    out: &mut Vec<UiVertex>,
    size: PhysicalSize<u32>,
    rect: (f64, f64, f64, f64),
    color: [f32; 4],
) {
    let (x, y, w, h) = rect;
    let p00 = to_ndc(x, y, size);
    let p10 = to_ndc(x + w, y, size);
    let p01 = to_ndc(x, y + h, size);
    let p11 = to_ndc(x + w, y + h, size);
    out.push(UiVertex { position: p00, color });
    out.push(UiVertex { position: p10, color });
    out.push(UiVertex { position: p11, color });
    out.push(UiVertex { position: p00, color });
    out.push(UiVertex { position: p11, color });
    out.push(UiVertex { position: p01, color });
}

fn push_circle(
    out: &mut Vec<UiVertex>,
    size: PhysicalSize<u32>,
    center: (f64, f64),
    radius: f64,
    color: [f32; 4],
) {
    const SEGMENTS: usize = 28;
    let center_ndc = to_ndc(center.0, center.1, size);
    let center_v = UiVertex { position: center_ndc, color };
    let mut prev = to_ndc(center.0 + radius, center.1, size);
    for i in 1..=SEGMENTS {
        let angle = (i as f64 / SEGMENTS as f64) * std::f64::consts::TAU;
        let p = to_ndc(
            center.0 + radius * angle.cos(),
            center.1 + radius * angle.sin(),
            size,
        );
        out.push(center_v);
        out.push(UiVertex { position: prev, color });
        out.push(UiVertex { position: p, color });
        prev = p;
    }
}

fn push_ring(
    out: &mut Vec<UiVertex>,
    size: PhysicalSize<u32>,
    center: (f64, f64),
    outer_radius: f64,
    inner_radius: f64,
    color: [f32; 4],
) {
    const SEGMENTS: usize = 28;
    let mut prev_outer = to_ndc(center.0 + outer_radius, center.1, size);
    let mut prev_inner = to_ndc(center.0 + inner_radius, center.1, size);
    for i in 1..=SEGMENTS {
        let angle = (i as f64 / SEGMENTS as f64) * std::f64::consts::TAU;
        let (s, c) = angle.sin_cos();
        let outer = to_ndc(center.0 + outer_radius * c, center.1 + outer_radius * s, size);
        let inner = to_ndc(center.0 + inner_radius * c, center.1 + inner_radius * s, size);
        out.push(UiVertex { position: prev_outer, color });
        out.push(UiVertex { position: prev_inner, color });
        out.push(UiVertex { position: outer, color });
        out.push(UiVertex { position: prev_inner, color });
        out.push(UiVertex { position: inner, color });
        out.push(UiVertex { position: outer, color });
        prev_outer = outer;
        prev_inner = inner;
    }
}

fn push_gear_icon(
    out: &mut Vec<UiVertex>,
    size: PhysicalSize<u32>,
    center: (f64, f64),
    radius: f64,
    color: [f32; 4],
) {
    push_ring(out, size, center, radius, radius * 0.55, color);
    let tooth = radius * 0.55;
    let half = tooth / 2.0;
    push_quad(out, size, (center.0 - half, center.1 - radius - half * 0.5, tooth, tooth), color);
    push_quad(out, size, (center.0 - half, center.1 + radius - half * 0.5, tooth, tooth), color);
    push_quad(out, size, (center.0 - radius - half * 0.5, center.1 - half, tooth, tooth), color);
    push_quad(out, size, (center.0 + radius - half * 0.5, center.1 - half, tooth, tooth), color);
}

fn push_toggle_switch(
    out: &mut Vec<UiVertex>,
    size: PhysicalSize<u32>,
    rect: (f64, f64, f64, f64),
    is_on: bool,
) {
    let (x, y, w, h) = rect;
    let track_color = if is_on { [0.25, 0.75, 0.35, 0.9] } else { [0.4, 0.4, 0.4, 0.75] };
    push_quad(out, size, rect, track_color);
    let knob_radius = h * 0.4;
    let knob_cx = if is_on { x + w - knob_radius - 4.0 } else { x + knob_radius + 4.0 };
    let knob_cy = y + h * 0.5;
    push_circle(out, size, (knob_cx, knob_cy), knob_radius, [1.0, 1.0, 1.0, 0.95]);
}

// ============================================================
//  SISTEMA DE FUENTES BITMAP 5×7
// ============================================================
// Cada glifo es una máscara de bits de 5 columnas × 7 filas.
// Bit 0 del u32 = columna 0, fila 0 (arriba-izquierda).
// Codificado por filas de arriba a abajo, bits de izquierda a derecha.

fn char_bitmap(c: char) -> [u8; 7] {
    match c.to_ascii_uppercase() {
        'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'B' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110],
        'C' => [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110],
        'D' => [0b11100, 0b10010, 0b10001, 0b10001, 0b10001, 0b10010, 0b11100],
        'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
        'F' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000],
        'G' => [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111],
        'H' => [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'I' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b11111],
        'J' => [0b00111, 0b00010, 0b00010, 0b00010, 0b10010, 0b10010, 0b01100],
        'K' => [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001],
        'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        'M' => [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001],
        'N' => [0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001],
        'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
        'Q' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101],
        'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
        'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
        'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'V' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001],
        'X' => [0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b01010, 0b10001],
        'Y' => [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100],
        'Z' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111],
        '0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        '2' => [0b01110, 0b10001, 0b00001, 0b00110, 0b01000, 0b10000, 0b11111],
        '3' => [0b11111, 0b00001, 0b00010, 0b00110, 0b00001, 0b10001, 0b01110],
        '4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        '6' => [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        '7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b00100, 0b00100, 0b00100],
        '8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        '9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
        ':' => [0b00000, 0b00100, 0b00000, 0b00000, 0b00000, 0b00100, 0b00000],
        '.' => [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00100],
        ' ' => [0b00000; 7],
        '-' => [0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000],
        '+' => [0b00000, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0b00000],
        '/' => [0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000],
        '<' => [0b00010, 0b00100, 0b01000, 0b10000, 0b01000, 0b00100, 0b00010],
        '>' => [0b01000, 0b00100, 0b00010, 0b00001, 0b00010, 0b00100, 0b01000],
        _ =>   [0b11111, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11111], // caja para desconocidos
    }
}

/// Escala en píxeles de cada "pixel" del bitmap de la fuente.
const FONT_SCALE: f64 = 3.0;
/// Ancho de cada celda (5 cols × escala).
const FONT_CELL_W: f64 = 5.0 * FONT_SCALE;
/// Alto de cada celda (7 filas × escala).
const FONT_CELL_H: f64 = 7.0 * FONT_SCALE;
/// Separación horizontal entre caracteres.
const FONT_CHAR_GAP: f64 = 2.0 * FONT_SCALE;

/// Dibuja un string en la posición (x_left, y_top) — la esquina
/// superior-izquierda del primer carácter.
pub fn push_text(
    out: &mut Vec<UiVertex>,
    size: PhysicalSize<u32>,
    text: &str,
    x_left: f64,
    y_top: f64,
    color: [f32; 4],
) {
    let mut cur_x = x_left;
    for c in text.chars() {
        let bitmap = char_bitmap(c);
        for row in 0..7usize {
            let bits = bitmap[row];
            for col in 0..5usize {
                if bits & (0b10000 >> col) != 0 {
                    let px = cur_x + col as f64 * FONT_SCALE;
                    let py = y_top + row as f64 * FONT_SCALE;
                    push_quad(out, size, (px, py, FONT_SCALE, FONT_SCALE), color);
                }
            }
        }
        cur_x += FONT_CELL_W + FONT_CHAR_GAP;
    }
}

/// Devuelve el ancho en píxeles de un string con la fuente bitmap.
pub fn text_width(text: &str) -> f64 {
    let n = text.chars().count();
    if n == 0 { return 0.0; }
    n as f64 * FONT_CELL_W + (n - 1) as f64 * FONT_CHAR_GAP
}

/// Dibuja texto centrado horizontalmente en `cx`.
fn push_text_centered(
    out: &mut Vec<UiVertex>,
    size: PhysicalSize<u32>,
    text: &str,
    cx: f64,
    y_top: f64,
    color: [f32; 4],
) {
    let w = text_width(text);
    push_text(out, size, text, cx - w * 0.5, y_top, color);
}

// ============================================================
//  FUENTE GRANDE (escala ×5) para el título
// ============================================================
const FONT_LARGE_SCALE: f64 = 5.0;
const FONT_LARGE_CELL_W: f64 = 5.0 * FONT_LARGE_SCALE;
const FONT_LARGE_CELL_H: f64 = 7.0 * FONT_LARGE_SCALE;
const FONT_LARGE_CHAR_GAP: f64 = 3.0 * FONT_LARGE_SCALE;

fn push_text_large(
    out: &mut Vec<UiVertex>,
    size: PhysicalSize<u32>,
    text: &str,
    x_left: f64,
    y_top: f64,
    color: [f32; 4],
) {
    let mut cur_x = x_left;
    for c in text.chars() {
        let bitmap = char_bitmap(c);
        for row in 0..7usize {
            let bits = bitmap[row];
            for col in 0..5usize {
                if bits & (0b10000 >> col) != 0 {
                    let px = cur_x + col as f64 * FONT_LARGE_SCALE;
                    let py = y_top + row as f64 * FONT_LARGE_SCALE;
                    push_quad(out, size, (px, py, FONT_LARGE_SCALE, FONT_LARGE_SCALE), color);
                }
            }
        }
        cur_x += FONT_LARGE_CELL_W + FONT_LARGE_CHAR_GAP;
    }
}

fn text_width_large(text: &str) -> f64 {
    let n = text.chars().count();
    if n == 0 { return 0.0; }
    n as f64 * FONT_LARGE_CELL_W + (n - 1) as f64 * FONT_LARGE_CHAR_GAP
}

fn push_text_large_centered(
    out: &mut Vec<UiVertex>,
    size: PhysicalSize<u32>,
    text: &str,
    cx: f64,
    y_top: f64,
    color: [f32; 4],
) {
    let w = text_width_large(text);
    push_text_large(out, size, text, cx - w * 0.5, y_top, color);
}

// ============================================================
//  CONTADOR FPS (display 7 segmentos — igual que antes)
// ============================================================
const FPS_DIGIT_W: f64 = 16.0;
const FPS_DIGIT_H: f64 = 26.0;
const FPS_DIGIT_THICKNESS: f64 = 4.0;
const FPS_DIGIT_GAP: f64 = 6.0;
const FPS_PANEL_PADDING: f64 = 10.0;
const FPS_MARGIN: f64 = 16.0;

fn push_digit(
    out: &mut Vec<UiVertex>,
    size: PhysicalSize<u32>,
    top_left: (f64, f64),
    w: f64,
    h: f64,
    t: f64,
    digit: u8,
    color: [f32; 4],
) {
    const SEGMENTS_BY_DIGIT: [[bool; 7]; 10] = [
        [true, true, true, false, true, true, true],
        [false, false, true, false, false, true, false],
        [true, false, true, true, true, false, true],
        [true, false, true, true, false, true, true],
        [false, true, true, true, false, true, false],
        [true, true, false, true, false, true, true],
        [true, true, false, true, true, true, true],
        [true, false, true, false, false, true, false],
        [true, true, true, true, true, true, true],
        [true, true, true, true, false, true, true],
    ];
    let segs = SEGMENTS_BY_DIGIT[(digit.min(9)) as usize];
    let (x, y) = top_left;
    let half_h = h / 2.0;

    if segs[0] { push_quad(out, size, (x + t, y, w - 2.0 * t, t), color); }
    if segs[1] { push_quad(out, size, (x, y, t, half_h), color); }
    if segs[2] { push_quad(out, size, (x + w - t, y, t, half_h), color); }
    if segs[3] { push_quad(out, size, (x + t, y + half_h - t / 2.0, w - 2.0 * t, t), color); }
    if segs[4] { push_quad(out, size, (x, y + half_h, t, half_h), color); }
    if segs[5] { push_quad(out, size, (x + w - t, y + half_h, t, half_h), color); }
    if segs[6] { push_quad(out, size, (x + t, y + h - t, w - 2.0 * t, t), color); }
}

pub fn build_fps_counter(fps: f32, size: PhysicalSize<u32>) -> Vec<UiVertex> {
    let mut verts = Vec::with_capacity(64);
    let value = fps.round().clamp(0.0, 999.0) as i32;
    let text = value.to_string();
    let num_digits = text.len();
    let digits_w = num_digits as f64 * FPS_DIGIT_W
        + (num_digits.saturating_sub(1)) as f64 * FPS_DIGIT_GAP;
    let panel_w = digits_w + FPS_PANEL_PADDING * 2.0;
    let panel_h = FPS_DIGIT_H + FPS_PANEL_PADDING * 2.0;
    let panel_x = size.width as f64 - FPS_MARGIN - panel_w;
    let panel_y = FPS_MARGIN;
    push_quad(&mut verts, size, (panel_x, panel_y, panel_w, panel_h), [0.0, 0.0, 0.0, 0.45]);
    let digit_color = [0.25, 1.0, 0.35, 0.95];
    let start_x = panel_x + FPS_PANEL_PADDING;
    let start_y = panel_y + FPS_PANEL_PADDING;
    for (i, ch) in text.chars().enumerate() {
        let digit = ch.to_digit(10).unwrap_or(0) as u8;
        let x = start_x + i as f64 * (FPS_DIGIT_W + FPS_DIGIT_GAP);
        push_digit(&mut verts, size, (x, start_y), FPS_DIGIT_W, FPS_DIGIT_H, FPS_DIGIT_THICKNESS, digit, digit_color);
    }
    verts
}

// ============================================================
//  MIRA CENTRAL
// ============================================================
const CROSSHAIR_LENGTH: f64 = 10.0;
const CROSSHAIR_THICKNESS: f64 = 2.0;
const CROSSHAIR_GAP: f64 = 4.0;

pub fn build_crosshair(size: PhysicalSize<u32>) -> Vec<UiVertex> {
    let mut verts = Vec::with_capacity(12);
    let cx = size.width as f64 / 2.0;
    let cy = size.height as f64 / 2.0;
    let color = [1.0, 1.0, 1.0, 0.85];
    let half_t = CROSSHAIR_THICKNESS / 2.0;
    push_quad(&mut verts, size, (cx - CROSSHAIR_GAP - CROSSHAIR_LENGTH, cy - half_t, CROSSHAIR_LENGTH, CROSSHAIR_THICKNESS), color);
    push_quad(&mut verts, size, (cx + CROSSHAIR_GAP, cy - half_t, CROSSHAIR_LENGTH, CROSSHAIR_THICKNESS), color);
    push_quad(&mut verts, size, (cx - half_t, cy - CROSSHAIR_GAP - CROSSHAIR_LENGTH, CROSSHAIR_THICKNESS, CROSSHAIR_LENGTH), color);
    push_quad(&mut verts, size, (cx - half_t, cy + CROSSHAIR_GAP, CROSSHAIR_THICKNESS, CROSSHAIR_LENGTH), color);
    verts
}

// ============================================================
//  OVERLAY DE INFO DE BUILD (esquina superior izquierda)
// ============================================================
// Muestra la etiqueta de build (fijada en compilación vía
// `BUILD_TAG=... cargo build`, ver build.rs) junto con la plataforma
// actual (WINDOWS/LINUX/ANDROID/MACOS). Toggleable desde el panel de
// configuración, fila "INFO DE BUILD".
const BUILD_INFO_MARGIN: f64 = 16.0;
const BUILD_INFO_PADDING: f64 = 8.0;
const BUILD_INFO_LINE_GAP: f64 = 4.0;

/// Nombre de la plataforma actual, en mayúsculas, listo para dibujar
/// con la fuente bitmap (que solo soporta A-Z/0-9/algunos símbolos).
fn platform_label() -> &'static str {
    match std::env::consts::OS {
        "windows" => "WINDOWS",
        "linux" => "LINUX",
        "android" => "ANDROID",
        "macos" => "MACOS",
        "ios" => "IOS",
        other => {
            // Fallback genérico para plataformas no listadas arriba;
            // no debería pasar en los targets que soporta el proyecto,
            // pero evita un panic si `main()` corre en algo inesperado.
            if other.is_empty() { "DESCONOCIDO" } else { "OTRO" }
        }
    }
}

/// Construye el texto de build+plataforma para la esquina superior
/// izquierda. `build_tag` viene de `env!("VOXEL_BUILD_TAG")` en
/// lib.rs (fijado en compilación, ver build.rs).
pub fn build_build_info_overlay(size: PhysicalSize<u32>, build_tag: &str) -> Vec<UiVertex> {
    let mut v = Vec::with_capacity(128);

    let tag_upper = build_tag.to_ascii_uppercase();
    let platform = platform_label();

    let line1_w = text_width(&tag_upper);
    let line2_w = text_width(platform);
    let panel_w = line1_w.max(line2_w) + BUILD_INFO_PADDING * 2.0;
    let panel_h = FONT_CELL_H * 2.0 + BUILD_INFO_LINE_GAP + BUILD_INFO_PADDING * 2.0;

    let panel_x = BUILD_INFO_MARGIN;
    let panel_y = BUILD_INFO_MARGIN;

    // Fondo semitransparente para que el texto se lea sobre cualquier
    // paisaje (cielo claro, follaje, etc.).
    push_quad(&mut v, size, (panel_x, panel_y, panel_w, panel_h), [0.0, 0.0, 0.0, 0.45]);

    let text_x = panel_x + BUILD_INFO_PADDING;
    let line1_y = panel_y + BUILD_INFO_PADDING;
    let line2_y = line1_y + FONT_CELL_H + BUILD_INFO_LINE_GAP;

    push_text(&mut v, size, &tag_upper, text_x, line1_y, [0.85, 0.9, 1.0, 0.95]);
    push_text(&mut v, size, platform, text_x, line2_y, [0.6, 0.75, 0.95, 0.85]);

    v
}

// ============================================================
//  OVERLAY DE JUEGO (joystick, botones, hotbar, botón config)
// ============================================================
const JOYSTICK_VISUAL_RADIUS: f64 = 70.0;
const NUB_VISUAL_RADIUS: f64 = 26.0;

pub fn build_touch_overlay(
    touch: &TouchController,
    size: PhysicalSize<u32>,
    selected_block: BlockType,
    show_fps: bool,
) -> Vec<UiVertex> {
    let mut verts = Vec::with_capacity(320);

    // Joystick de movimiento.
    let (base_center, nub_center) = touch.joystick_visual(size);
    push_ring(&mut verts, size, base_center, JOYSTICK_VISUAL_RADIUS, JOYSTICK_VISUAL_RADIUS - 8.0, [1.0, 1.0, 1.0, 0.35]);
    push_circle(&mut verts, size, nub_center, NUB_VISUAL_RADIUS, [1.0, 1.0, 1.0, 0.55]);

    // Salto: único botón de acción visible (romper/colocar ahora viven en
    // la zona de mirar, ver touch.rs), por eso es bastante más grande.
    let jump_rect = TouchController::rect_jump(size);
    let jump_center = (jump_rect.0 + jump_rect.2 * 0.5, jump_rect.1 + jump_rect.3 * 0.5);
    let jump_alpha = if touch.jump_held() { 0.65 } else { 0.35 };
    push_circle(&mut verts, size, jump_center, jump_rect.2 * 0.5, [1.0, 1.0, 1.0, jump_alpha]);

    // Hotbar.
    for i in 1..=3u8 {
        let block = match i {
            1 => BlockType::Grass,
            2 => BlockType::Dirt,
            _ => BlockType::Stone,
        };
        let [r, g, b] = block.color();
        let rect = TouchController::rect_hotbar(size, i);
        let is_selected = block == selected_block;
        if is_selected {
            let pad = 6.0;
            push_quad(&mut verts, size, (rect.0 - pad, rect.1 - pad, rect.2 + pad * 2.0, rect.3 + pad * 2.0), [1.0, 1.0, 1.0, 0.9]);
        }
        let alpha = if is_selected { 1.0 } else { 0.6 };
        push_quad(&mut verts, size, rect, [r, g, b, alpha]);
    }

    // Botón de configuración (engranaje): arriba a la derecha.
    let settings_rect = TouchController::rect_settings(size);
    let settings_center = (settings_rect.0 + settings_rect.2 * 0.5, settings_rect.1 + settings_rect.3 * 0.5);
    push_circle(&mut verts, size, settings_center, settings_rect.2 * 0.5, [1.0, 1.0, 1.0, 0.35]);
    push_gear_icon(&mut verts, size, settings_center, settings_rect.2 * 0.28, [0.1, 0.1, 0.1, 0.9]);

    verts
}

// ============================================================
//  PANTALLA FULLSCREEN DE CONFIGURACIÓN (JUEGO PAUSADO)
// ============================================================

/// Construye toda la geometría de la pantalla de configuración.
/// Se dibuja encima del frame del juego (que queda congelado).
pub fn build_settings_screen(
    size: PhysicalSize<u32>,
    show_fps: bool,
    walk_mode: bool,
    render_radius: i32,
    show_clouds: bool,
    show_fog: bool,
    show_build_info: bool,
) -> Vec<UiVertex> {
    let mut v = Vec::with_capacity(512);
    let sw = size.width as f64;
    let sh = size.height as f64;
    let cx = sw * 0.5;

    // --- Fondo semitransparente oscuro sobre el mundo pausado ---
    push_quad(&mut v, size, (0.0, 0.0, sw, sh), [0.0, 0.0, 0.0, 0.72]);

    // --- Panel central ---
    let panel_w = 520.0_f64.min(sw * 0.85);
    let panel_h = sh * 0.78;
    let panel_x = cx - panel_w * 0.5;
    let panel_y = sh * 0.14;
    // Fondo del panel con borde sutil.
    push_quad(&mut v, size, (panel_x - 3.0, panel_y - 3.0, panel_w + 6.0, panel_h + 6.0), [0.4, 0.55, 0.9, 0.35]);
    push_quad(&mut v, size, (panel_x, panel_y, panel_w, panel_h), [0.05, 0.06, 0.10, 0.93]);

    // --- Título: "CONFIGURACION" en fuente grande ---
    let title = "CONFIGURACION";
    let title_y = panel_y + 22.0;
    push_text_large_centered(&mut v, size, title, cx, title_y, [0.9, 0.95, 1.0, 1.0]);

    // Línea separadora bajo el título.
    let sep_y = title_y + FONT_LARGE_CELL_H + 16.0;
    push_quad(&mut v, size, (panel_x + 20.0, sep_y, panel_w - 40.0, 2.0), [0.4, 0.55, 0.9, 0.5]);

    // --- Fila 1: "MOSTRAR FPS" con toggle ---
    let row1 = TouchController::rect_settings_fps_row(size);
    let label1_y = row1.1 + (row1.3 - FONT_CELL_H) * 0.5;
    push_text(&mut v, size, "MOSTRAR FPS", row1.0, label1_y, [0.85, 0.88, 0.95, 1.0]);
    let switch1 = TouchController::rect_row_switch(row1);
    push_toggle_switch(&mut v, size, switch1, show_fps);
    // Etiqueta de estado ON/OFF.
    let state1 = if show_fps { "ON" } else { "OFF" };
    let state_color1: [f32; 4] = if show_fps { [0.3, 0.9, 0.4, 1.0] } else { [0.6, 0.6, 0.6, 1.0] };
    let state1_x = switch1.0 - text_width(state1) - 12.0;
    push_text(&mut v, size, state1, state1_x, label1_y, state_color1);

    // Separador entre filas.
    push_quad(&mut v, size, (row1.0, row1.1 + row1.3 + 4.0, row1.2, 1.0), [0.3, 0.3, 0.4, 0.4]);

    // --- Fila 2: "MODO CAMINAR" con toggle ---
    let row2 = TouchController::rect_settings_walk_row(size);
    let label2_y = row2.1 + (row2.3 - FONT_CELL_H) * 0.5;
    push_text(&mut v, size, "MODO CAMINAR", row2.0, label2_y, [0.85, 0.88, 0.95, 1.0]);
    let switch2 = TouchController::rect_row_switch(row2);
    push_toggle_switch(&mut v, size, switch2, walk_mode);
    let state2 = if walk_mode { "ON" } else { "OFF" };
    let state_color2: [f32; 4] = if walk_mode { [0.3, 0.9, 0.4, 1.0] } else { [0.6, 0.6, 0.6, 1.0] };
    let state2_x = switch2.0 - text_width(state2) - 12.0;
    push_text(&mut v, size, state2, state2_x, label2_y, state_color2);

    push_quad(&mut v, size, (row2.0, row2.1 + row2.3 + 4.0, row2.2, 1.0), [0.3, 0.3, 0.4, 0.4]);

    // --- Fila 3: "DISTANCIA DE CHUNKS" con stepper [-] valor [+] ---
    let row3 = TouchController::rect_settings_render_distance_row(size);
    let label3_y = row3.1 + (row3.3 - FONT_CELL_H) * 0.5;
    push_text(&mut v, size, "RADIO DE CHUNKS", row3.0, label3_y, [0.85, 0.88, 0.95, 1.0]);

    let minus_rect = TouchController::rect_stepper_minus(row3);
    let plus_rect = TouchController::rect_stepper_plus(row3);
    push_quad(&mut v, size, minus_rect, [0.18, 0.22, 0.32, 0.9]);
    push_quad(&mut v, size, plus_rect, [0.18, 0.22, 0.32, 0.9]);
    let minus_label = "-";
    let minus_lx = minus_rect.0 + (minus_rect.2 - text_width(minus_label)) * 0.5;
    let minus_ly = minus_rect.1 + (minus_rect.3 - FONT_CELL_H) * 0.5;
    push_text(&mut v, size, minus_label, minus_lx, minus_ly, [0.85, 0.9, 1.0, 1.0]);
    let plus_label = "+";
    let plus_lx = plus_rect.0 + (plus_rect.2 - text_width(plus_label)) * 0.5;
    let plus_ly = plus_rect.1 + (plus_rect.3 - FONT_CELL_H) * 0.5;
    push_text(&mut v, size, plus_label, plus_lx, plus_ly, [0.85, 0.9, 1.0, 1.0]);

    // Valor numérico, centrado entre los dos botones.
    let value_text = render_radius.to_string();
    let value_cx = (minus_rect.0 + minus_rect.2 + plus_rect.0) * 0.5;
    let value_lx = value_cx - text_width(&value_text) * 0.5;
    push_text(&mut v, size, &value_text, value_lx, label3_y, [1.0, 0.85, 0.4, 1.0]);

    push_quad(&mut v, size, (row3.0, row3.1 + row3.3 + 4.0, row3.2, 1.0), [0.3, 0.3, 0.4, 0.4]);

    // --- Fila 4: "NUBES" con toggle ---
    let row4 = TouchController::rect_settings_clouds_row(size);
    let label4_y = row4.1 + (row4.3 - FONT_CELL_H) * 0.5;
    push_text(&mut v, size, "NUBES", row4.0, label4_y, [0.85, 0.88, 0.95, 1.0]);
    let switch4 = TouchController::rect_row_switch(row4);
    push_toggle_switch(&mut v, size, switch4, show_clouds);
    let state4 = if show_clouds { "ON" } else { "OFF" };
    let state_color4: [f32; 4] = if show_clouds { [0.3, 0.9, 0.4, 1.0] } else { [0.6, 0.6, 0.6, 1.0] };
    let state4_x = switch4.0 - text_width(state4) - 12.0;
    push_text(&mut v, size, state4, state4_x, label4_y, state_color4);

    push_quad(&mut v, size, (row4.0, row4.1 + row4.3 + 4.0, row4.2, 1.0), [0.3, 0.3, 0.4, 0.4]);

    // --- Fila 5: "NIEBLA" con toggle ---
    let row5 = TouchController::rect_settings_fog_row(size);
    let label5_y = row5.1 + (row5.3 - FONT_CELL_H) * 0.5;
    push_text(&mut v, size, "NIEBLA", row5.0, label5_y, [0.85, 0.88, 0.95, 1.0]);
    let switch5 = TouchController::rect_row_switch(row5);
    push_toggle_switch(&mut v, size, switch5, show_fog);
    let state5 = if show_fog { "ON" } else { "OFF" };
    let state_color5: [f32; 4] = if show_fog { [0.3, 0.9, 0.4, 1.0] } else { [0.6, 0.6, 0.6, 1.0] };
    let state5_x = switch5.0 - text_width(state5) - 12.0;
    push_text(&mut v, size, state5, state5_x, label5_y, state_color5);

    push_quad(&mut v, size, (row5.0, row5.1 + row5.3 + 4.0, row5.2, 1.0), [0.3, 0.3, 0.4, 0.4]);

    // --- Fila 6: "INFO DE BUILD" con toggle ---
    let row6 = TouchController::rect_settings_build_info_row(size);
    let label6_y = row6.1 + (row6.3 - FONT_CELL_H) * 0.5;
    push_text(&mut v, size, "INFO DE BUILD", row6.0, label6_y, [0.85, 0.88, 0.95, 1.0]);
    let switch6 = TouchController::rect_row_switch(row6);
    push_toggle_switch(&mut v, size, switch6, show_build_info);
    let state6 = if show_build_info { "ON" } else { "OFF" };
    let state_color6: [f32; 4] = if show_build_info { [0.3, 0.9, 0.4, 1.0] } else { [0.6, 0.6, 0.6, 1.0] };
    let state6_x = switch6.0 - text_width(state6) - 12.0;
    push_text(&mut v, size, state6, state6_x, label6_y, state_color6);

    push_quad(&mut v, size, (row6.0, row6.1 + row6.3 + 4.0, row6.2, 1.0), [0.3, 0.3, 0.4, 0.4]);

    // --- Botón "< VOLVER" arriba izquierda ---
    let back_rect = TouchController::rect_back_button(size);
    // Fondo del botón con hover glow.
    push_quad(&mut v, size, (back_rect.0 - 2.0, back_rect.1 - 2.0, back_rect.2 + 4.0, back_rect.3 + 4.0), [0.3, 0.5, 0.85, 0.4]);
    push_quad(&mut v, size, back_rect, [0.12, 0.18, 0.32, 0.90]);
    // Texto "< VOLVER" centrado en el botón.
    let back_label = "< VOLVER";
    let back_lx = back_rect.0 + (back_rect.2 - text_width(back_label)) * 0.5;
    let back_ly = back_rect.1 + (back_rect.3 - FONT_CELL_H) * 0.5;
    push_text(&mut v, size, back_label, back_lx, back_ly, [0.8, 0.88, 1.0, 1.0]);

    // --- Nota de pausa en la esquina inferior del panel ---
    let pause_text = "JUEGO PAUSADO";
    let pause_y = panel_y + panel_h - FONT_CELL_H - 18.0;
    push_text_centered(&mut v, size, pause_text, cx, pause_y, [0.5, 0.55, 0.65, 0.7]);

    v
}

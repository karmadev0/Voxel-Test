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

/// Glifos en minúscula (rows 0-1 en blanco salvo ascendentes/puntos):
/// como la celda tiene 7 filas y no hay lugar para un verdadero
/// descendente por debajo del renglón, las letras con descendente
/// (g, j, p, q, y) lo insinúan doblando el trazo en la fila 6 en vez de
/// bajar más — es un truco de fuente bitmap chica, no un error.
fn char_bitmap(c: char) -> [u8; 7] {
    match c {
        // --- Mayúsculas A-Z ---
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

        // --- Minúsculas a-z: altura-x en filas 2-6, ascendentes
        // (b,d,f,h,k,l,t) usan también las filas 0-1. ---
        'a' => [0b00000, 0b00000, 0b01110, 0b00001, 0b01111, 0b10001, 0b01111],
        'b' => [0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b10001, 0b11110],
        'c' => [0b00000, 0b00000, 0b01111, 0b10000, 0b10000, 0b10000, 0b01111],
        'd' => [0b00001, 0b00001, 0b01111, 0b10001, 0b10001, 0b10001, 0b01111],
        'e' => [0b00000, 0b00000, 0b01110, 0b10001, 0b11111, 0b10000, 0b01111],
        'f' => [0b00110, 0b01001, 0b01000, 0b11100, 0b01000, 0b01000, 0b01000],
        'g' => [0b00000, 0b00000, 0b01111, 0b10001, 0b01111, 0b00001, 0b01110],
        'h' => [0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b10001, 0b10001],
        'i' => [0b00100, 0b00000, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        'j' => [0b00010, 0b00000, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100],
        'k' => [0b10000, 0b10000, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010],
        'l' => [0b01000, 0b01000, 0b01000, 0b01000, 0b01000, 0b01000, 0b00111],
        'm' => [0b00000, 0b00000, 0b11011, 0b10101, 0b10101, 0b10101, 0b10101],
        'n' => [0b00000, 0b00000, 0b10110, 0b11001, 0b10001, 0b10001, 0b10001],
        'o' => [0b00000, 0b00000, 0b01110, 0b10001, 0b10001, 0b10001, 0b01110],
        'p' => [0b00000, 0b00000, 0b11110, 0b10001, 0b10001, 0b11110, 0b10000],
        'q' => [0b00000, 0b00000, 0b01111, 0b10001, 0b10001, 0b01111, 0b00001],
        'r' => [0b00000, 0b00000, 0b10110, 0b11001, 0b10000, 0b10000, 0b10000],
        's' => [0b00000, 0b00000, 0b01111, 0b10000, 0b01110, 0b00001, 0b11110],
        't' => [0b01000, 0b11111, 0b01000, 0b01000, 0b01000, 0b01001, 0b00110],
        'u' => [0b00000, 0b00000, 0b10001, 0b10001, 0b10001, 0b10011, 0b01101],
        'v' => [0b00000, 0b00000, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        'w' => [0b00000, 0b00000, 0b10001, 0b10001, 0b10101, 0b10101, 0b01010],
        'x' => [0b00000, 0b00000, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001],
        'y' => [0b00000, 0b00000, 0b10001, 0b10001, 0b01111, 0b00001, 0b01110],
        'z' => [0b00000, 0b00000, 0b11111, 0b00010, 0b00100, 0b01000, 0b11111],

        // --- Acentos y ñ/ü. Solo hay una fila libre arriba de la letra
        // para el signo diacrítico: en minúsculas ya sobraba (fila 1);
        // en mayúsculas se "roba" la fila 0 del glifo original (que
        // para A/E/I/O/U repite el trazo de la fila 1, así que no se
        // pierde forma reconocible). ---
        'á' => [0b00000, 0b00010, 0b01110, 0b00001, 0b01111, 0b10001, 0b01111],
        'é' => [0b00000, 0b00010, 0b01110, 0b10001, 0b11111, 0b10000, 0b01111],
        'í' => [0b00010, 0b00000, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        'ó' => [0b00000, 0b00010, 0b01110, 0b10001, 0b10001, 0b10001, 0b01110],
        'ú' => [0b00000, 0b00010, 0b10001, 0b10001, 0b10001, 0b10011, 0b01101],
        'ü' => [0b00000, 0b01010, 0b10001, 0b10001, 0b10001, 0b10011, 0b01101],
        'ñ' => [0b01010, 0b00000, 0b10110, 0b11001, 0b10001, 0b10001, 0b10001],
        'Á' => [0b00010, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'É' => [0b00010, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
        'Í' => [0b00010, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b11111],
        'Ó' => [0b00010, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'Ú' => [0b00010, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'Ü' => [0b01010, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'Ñ' => [0b01010, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001],

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

/// Alto total (con margen) del overlay de info de build, para que quien
/// dibuje el panel de debug (F3) sepa cuánto bajarlo si ambos están
/// prendidos a la vez, y no se superpongan.
pub fn build_info_overlay_height() -> f64 {
    BUILD_INFO_MARGIN + FONT_CELL_H * 2.0 + BUILD_INFO_LINE_GAP + BUILD_INFO_PADDING * 2.0
}

// ============================================================
//  PANEL DE DEBUG (F3) — posición, chunk, bloque apuntado, modo
// ============================================================
// Panel de texto en la esquina superior izquierda (debajo del overlay de
// build info si también está prendido) con datos para diagnosticar
// problemas: posición exacta del jugador, chunk en el que está parado,
// qué bloque está mirando la cámara (o "NADA" si no hay ninguno al
// alcance), modo de movimiento actual, y la ruta del archivo de log en
// disco (ver platform/file_logger.rs). Se arma en `lib.rs` a partir del
// estado del juego y se le pasa ya formado a esta función; acá solo se
// encarga del layout/dibujado, para no acoplar ui_overlay.rs a los tipos
// internos del motor (Player, Camera, World, etc.).
pub struct DebugPanelData {
    pub player_pos: (f32, f32, f32),
    pub chunk_pos: (i32, i32),
    pub looking_at: Option<(String, (i32, i32, i32))>,
    pub fps: f32,
    pub game_mode_label: &'static str,
    pub log_file_hint: String,
}

const DEBUG_PANEL_MARGIN: f64 = 16.0;
const DEBUG_PANEL_PADDING: f64 = 10.0;
const DEBUG_PANEL_LINE_GAP: f64 = 6.0;

/// Botón "COPIAR" dentro del panel de debug: copia un snapshot en texto
/// de todos estos datos al portapapeles (ver `handle_click`/tecla en
/// lib.rs, que usa el mismo `arboard`/JNI que ya usaba crash.rs).
pub fn rect_debug_panel_copy_button(size: PhysicalSize<u32>, y_offset: f64) -> (f64, f64, f64, f64) {
    let panel_x = DEBUG_PANEL_MARGIN;
    let panel_y = DEBUG_PANEL_MARGIN + y_offset;
    // Mismo ancho que el panel (ver build_debug_panel) para que el botón
    // quede pegado al borde inferior, ancho completo.
    let panel_w = DEBUG_PANEL_WIDTH;
    let lines = DEBUG_PANEL_LINE_COUNT as f64;
    let text_block_h = lines * FONT_CELL_H + (lines - 1.0) * DEBUG_PANEL_LINE_GAP;
    let button_y = panel_y + DEBUG_PANEL_PADDING * 2.0 + text_block_h;
    (panel_x, button_y, panel_w, DEBUG_PANEL_BUTTON_H)
}

const DEBUG_PANEL_WIDTH: f64 = 460.0;
const DEBUG_PANEL_BUTTON_H: f64 = 40.0;
/// Cantidad fija de líneas de texto del panel: posición, chunk, mirando,
/// modo, fps, archivo de log. Si se agrega/saca una línea en
/// `build_debug_panel`, actualizar este número también (se usa para
/// calcular la altura del panel y la posición del botón "COPIAR" sin
/// tener que dibujar dos veces).
const DEBUG_PANEL_LINE_COUNT: usize = 6;

pub fn build_debug_panel(
    size: PhysicalSize<u32>,
    data: &DebugPanelData,
    y_offset: f64,
    copy_flash: bool,
) -> Vec<UiVertex> {
    let mut v = Vec::with_capacity(512);

    let panel_x = DEBUG_PANEL_MARGIN;
    let panel_y = DEBUG_PANEL_MARGIN + y_offset;
    let panel_w = DEBUG_PANEL_WIDTH;

    let lines_text = [
        format!(
            "POS: {:.1} {:.1} {:.1}",
            data.player_pos.0, data.player_pos.1, data.player_pos.2
        ),
        format!("CHUNK: {} {}", data.chunk_pos.0, data.chunk_pos.1),
        match &data.looking_at {
            Some((label, (x, y, z))) => format!("MIRANDO: {} {} {} {}", label, x, y, z),
            None => "MIRANDO: NADA".to_string(),
        },
        format!("MODO: {}", data.game_mode_label),
        format!("FPS: {}", data.fps.round() as i32),
        format!("LOG: {}", data.log_file_hint),
    ];
    debug_assert_eq!(lines_text.len(), DEBUG_PANEL_LINE_COUNT);

    let text_block_h = DEBUG_PANEL_LINE_COUNT as f64 * FONT_CELL_H
        + (DEBUG_PANEL_LINE_COUNT as f64 - 1.0) * DEBUG_PANEL_LINE_GAP;
    let panel_h = DEBUG_PANEL_PADDING * 2.0
        + text_block_h
        + DEBUG_PANEL_PADDING
        + DEBUG_PANEL_BUTTON_H
        + DEBUG_PANEL_PADDING;

    // Fondo del panel.
    push_quad(&mut v, size, (panel_x, panel_y, panel_w, panel_h), [0.0, 0.0, 0.0, 0.55]);

    let text_x = panel_x + DEBUG_PANEL_PADDING;
    let mut cur_y = panel_y + DEBUG_PANEL_PADDING;
    for line in &lines_text {
        // Truncamos líneas que no entrarían en el ancho del panel (por
        // ejemplo "MIRANDO: PIEDRA 123 45 -678" con coordenadas muy
        // largas) en vez de dejar que el texto se salga del panel.
        let max_chars = ((panel_w - DEBUG_PANEL_PADDING * 2.0) / (FONT_CELL_W + FONT_CHAR_GAP))
            .floor()
            .max(1.0) as usize;
        let shown: String = if line.chars().count() > max_chars {
            line.chars().take(max_chars.saturating_sub(1)).collect::<String>() + ">"
        } else {
            line.clone()
        };
        push_text(&mut v, size, &shown.to_ascii_uppercase(), text_x, cur_y, [0.85, 0.92, 0.95, 0.95]);
        cur_y += FONT_CELL_H + DEBUG_PANEL_LINE_GAP;
    }

    // Botón "COPIAR".
    let button_rect = rect_debug_panel_copy_button(size, y_offset);
    let button_color = if copy_flash { [0.25, 0.75, 0.35, 0.9] } else { [0.2, 0.28, 0.4, 0.9] };
    push_quad(&mut v, size, button_rect, button_color);
    let button_label = if copy_flash { "COPIADO!" } else { "COPIAR" };
    let (bx, by, bw, bh) = button_rect;
    let label_x = bx + (bw - text_width(button_label)) * 0.5;
    let label_y = by + (bh - FONT_CELL_H) * 0.5;
    push_text(&mut v, size, button_label, label_x, label_y, [0.9, 0.95, 1.0, 1.0]);

    v
}

// ============================================================
//  OVERLAY DE JUEGO (joystick, botones, hotbar, botón config)
// ============================================================
const JOYSTICK_VISUAL_RADIUS: f64 = 70.0;
const NUB_VISUAL_RADIUS: f64 = 26.0;

/// Nombre del material seleccionado, mostrado un momento arriba de la
/// hotbar cada vez que cambia la selección — igual que en Minecraft.
/// `alpha` ya viene calculado por el llamador (ver `render()` en
/// `lib.rs`: 1.0 mientras está "sostenido", después baja a 0.0 en un
/// fundido) — acá solo se aplica a cada vértice.
pub fn build_selected_block_popup(size: PhysicalSize<u32>, name: &str, alpha: f32) -> Vec<UiVertex> {
    let mut v = Vec::with_capacity(64);
    let cx = size.width as f64 * 0.5;
    let hotbar_top = size.height as f64 - 24.0 - 64.0; // MARGIN + HOTBAR_SIZE, ver touch.rs
    let y = hotbar_top - 34.0;

    // Fondito oscuro detrás del texto, para que se lea encima de
    // cualquier fondo (cielo claro, nieve, etc.) — mismo criterio que
    // el resto del HUD.
    let text_w = text_width(name);
    let pad_x = 14.0;
    let bg_w = text_w + pad_x * 2.0;
    let bg_h = FONT_CELL_H + 10.0;
    push_quad(&mut v, size, (cx - bg_w * 0.5, y - 5.0, bg_w, bg_h), [0.05, 0.05, 0.08, 0.55 * alpha]);

    push_text(&mut v, size, name, cx - text_w * 0.5, y, [1.0, 1.0, 1.0, alpha]);
    v
}

/// Dibuja la hotbar de bloques (`BlockType::HOTBAR_SLOTS` slots,
/// resaltando el seleccionado con un borde blanco). Independiente de
/// touch: solo depende del tamaño de pantalla y del bloque seleccionado,
/// así que la usan tanto el overlay táctil de Android
/// (`build_touch_overlay`) como el HUD de escritorio (ver `lib.rs`, rama
/// `not(target_os = "android")` de `GameScreen::Playing`).
pub fn build_hotbar(size: PhysicalSize<u32>, selected_block: BlockType) -> Vec<UiVertex> {
    let mut verts = Vec::with_capacity(BlockType::HOTBAR_SLOTS as usize * 12);
    for i in 1..=BlockType::HOTBAR_SLOTS {
        let rect = TouchController::rect_hotbar(size, i);
        let Some(block) = BlockType::from_hotbar_slot(i) else {
            // Slot vacío reservado (ver `BlockType::MATERIAL_COUNT`):
            // solo el marco, nada seleccionable.
            push_quad(&mut verts, size, rect, [0.12, 0.13, 0.16, 0.5]);
            continue;
        };
        let [r, g, b] = block.color();
        let is_selected = block == selected_block;
        if is_selected {
            let pad = 6.0;
            push_quad(&mut verts, size, (rect.0 - pad, rect.1 - pad, rect.2 + pad * 2.0, rect.3 + pad * 2.0), [1.0, 1.0, 1.0, 0.9]);
        }
        let alpha = if is_selected { 1.0 } else { 0.6 };
        push_quad(&mut verts, size, rect, [r, g, b, alpha]);
    }
    verts
}

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

    // Salto: el más grande de los dos botones de acción visibles
    // (romper/colocar viven en la zona de mirar, ver touch.rs), porque es
    // el que se usa con más frecuencia.
    let jump_rect = TouchController::rect_jump(size);
    let jump_center = (jump_rect.0 + jump_rect.2 * 0.5, jump_rect.1 + jump_rect.3 * 0.5);
    let jump_alpha = if touch.jump_held() { 0.65 } else { 0.35 };
    push_circle(&mut verts, size, jump_center, jump_rect.2 * 0.5, [1.0, 1.0, 1.0, jump_alpha]);

    // Agachar/bajar: segundo botón de acción, a la izquierda de salto y
    // más chico. Agacha en Supervivencia, baja en Creativo/Espectador
    // (ver Camera::wants_crouch / set_touch_down en camera.rs).
    let crouch_rect = TouchController::rect_crouch(size);
    let crouch_center = (crouch_rect.0 + crouch_rect.2 * 0.5, crouch_rect.1 + crouch_rect.3 * 0.5);
    let crouch_alpha = if touch.crouch_held() { 0.65 } else { 0.35 };
    push_circle(&mut verts, size, crouch_center, crouch_rect.2 * 0.5, [1.0, 1.0, 1.0, crouch_alpha]);

    // Hotbar.
    verts.extend(build_hotbar(size, selected_block));

    // Botón "..." (abre el inventario completo, ver GameScreen::Inventory):
    // un slot más pegado a la derecha de la hotbar.
    let inv_rect = TouchController::rect_inventory_button(size);
    push_quad(&mut verts, size, inv_rect, [0.16, 0.17, 0.22, 0.85]);
    let dots = "...";
    let dx = inv_rect.0 + (inv_rect.2 - text_width(dots)) * 0.5;
    let dy = inv_rect.1 + (inv_rect.3 - FONT_CELL_H) * 0.5;
    push_text(&mut verts, size, dots, dx, dy, [0.85, 0.88, 0.95, 1.0]);

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
/// Fondo semitransparente oscuro + panel central con borde, compartido
/// por las 4 pantallas de menú (Pause, GameMode, Settings,
/// SettingsMore) para que todas tengan el mismo look. Devuelve
/// `(panel_x, panel_y, panel_w, panel_h)` para que el llamador siga
/// dibujando contenido adentro.
fn push_menu_panel_background(
    v: &mut Vec<UiVertex>,
    size: PhysicalSize<u32>,
) -> (f64, f64, f64, f64) {
    let sw = size.width as f64;
    let sh = size.height as f64;
    let cx = sw * 0.5;

    push_quad(v, size, (0.0, 0.0, sw, sh), [0.0, 0.0, 0.0, 0.72]);

    let panel_w = 520.0_f64.min(sw * 0.85);
    let panel_h = sh * 0.78;
    let panel_x = cx - panel_w * 0.5;
    let panel_y = sh * 0.14;
    push_quad(v, size, (panel_x - 3.0, panel_y - 3.0, panel_w + 6.0, panel_h + 6.0), [0.4, 0.55, 0.9, 0.35]);
    push_quad(v, size, (panel_x, panel_y, panel_w, panel_h), [0.05, 0.06, 0.10, 0.93]);

    (panel_x, panel_y, panel_w, panel_h)
}

/// Título grande centrado + línea separadora debajo, común a las 4
/// pantallas de menú. Devuelve la Y donde termina el separador, para
/// que el contenido que sigue no se le superponga.
fn push_menu_title(
    v: &mut Vec<UiVertex>,
    size: PhysicalSize<u32>,
    panel: (f64, f64, f64, f64),
    title: &str,
) -> f64 {
    let (panel_x, panel_y, panel_w, _) = panel;
    let cx = size.width as f64 * 0.5;
    let title_y = panel_y + 22.0;
    push_text_large_centered(v, size, title, cx, title_y, [0.9, 0.95, 1.0, 1.0]);
    let sep_y = title_y + FONT_LARGE_CELL_H + 16.0;
    push_quad(v, size, (panel_x + 20.0, sep_y, panel_w - 40.0, 2.0), [0.4, 0.55, 0.9, 0.5]);
    sep_y
}

/// Botón "< VOLVER" arriba a la izquierda, común a las 4 pantallas de
/// menú (sube un nivel en la jerarquía, ver `TouchAction::Back`).
fn push_back_button(v: &mut Vec<UiVertex>, size: PhysicalSize<u32>) {
    let back_rect = TouchController::rect_back_button(size);
    push_quad(v, size, (back_rect.0 - 2.0, back_rect.1 - 2.0, back_rect.2 + 4.0, back_rect.3 + 4.0), [0.3, 0.5, 0.85, 0.4]);
    push_quad(v, size, back_rect, [0.12, 0.18, 0.32, 0.90]);
    let back_label = "< VOLVER";
    let back_lx = back_rect.0 + (back_rect.2 - text_width(back_label)) * 0.5;
    let back_ly = back_rect.1 + (back_rect.3 - FONT_CELL_H) * 0.5;
    push_text(v, size, back_label, back_lx, back_ly, [0.8, 0.88, 1.0, 1.0]);
}

/// Nota "JUEGO PAUSADO" en la esquina inferior del panel, común a las 4
/// pantallas de menú (todas pausan el juego, ver `GameScreen`/`update`).
fn push_pause_note(v: &mut Vec<UiVertex>, size: PhysicalSize<u32>, panel: (f64, f64, f64, f64)) {
    let (_, panel_y, _, panel_h) = panel;
    let cx = size.width as f64 * 0.5;
    let pause_text = "JUEGO PAUSADO";
    let pause_y = panel_y + panel_h - FONT_CELL_H - 18.0;
    push_text_centered(v, size, pause_text, cx, pause_y, [0.5, 0.55, 0.65, 0.7]);
}

/// Dibuja un botón grande de una sola línea (usado por el menú de pausa
/// y por "AJUSTES ADICIONALES"), con su fondo y etiqueta centrada.
fn push_big_button(
    v: &mut Vec<UiVertex>,
    size: PhysicalSize<u32>,
    rect: (f64, f64, f64, f64),
    label: &str,
    bg: [f32; 4],
    text_color: [f32; 4],
) {
    push_quad(v, size, rect, bg);
    let (rx, ry, rw, rh) = rect;
    let lx = rx + (rw - text_width(label)) * 0.5;
    let ly = ry + (rh - FONT_CELL_H) * 0.5;
    push_text(v, size, label, lx, ly, text_color);
}

/// Dibuja una fila de ajuste ON/OFF completa: etiqueta a la izquierda +
/// switch visual + texto de estado a la derecha, más el separador
/// debajo. Reutilizado por las filas "MOSTRAR FPS", "NUBES", "NIEBLA",
/// "INFO DE BUILD" y "PANEL DE DEBUG (F3)".
fn push_toggle_row(
    v: &mut Vec<UiVertex>,
    size: PhysicalSize<u32>,
    row: (f64, f64, f64, f64),
    label: &str,
    is_on: bool,
) {
    let label_y = row.1 + (row.3 - FONT_CELL_H) * 0.5;
    push_text(v, size, label, row.0, label_y, [0.85, 0.88, 0.95, 1.0]);
    let switch = TouchController::rect_row_switch(row);
    push_toggle_switch(v, size, switch, is_on);
    let state = if is_on { "ON" } else { "OFF" };
    let state_color: [f32; 4] = if is_on { [0.3, 0.9, 0.4, 1.0] } else { [0.6, 0.6, 0.6, 1.0] };
    let state_x = switch.0 - text_width(state) - 12.0;
    push_text(v, size, state, state_x, label_y, state_color);
    push_quad(v, size, (row.0, row.1 + row.3 + 4.0, row.2, 1.0), [0.3, 0.3, 0.4, 0.4]);
}

/// Menú principal: primera pantalla que ve el jugador al abrir la app
/// (ver `GameScreen::MainMenu` en lib.rs). Título placeholder
/// "VOXEL-ENGINE" + 3 botones ("JUGAR" / "CONFIGURACIÓN" / "SALIR"). A
/// diferencia de las otras 4 pantallas de menú, no tiene botón
/// "< VOLVER" (es la raíz de todo) ni la nota "JUEGO PAUSADO" (todavía
/// no hay una partida en curso para pausar).
pub fn build_main_menu_screen(size: PhysicalSize<u32>) -> Vec<UiVertex> {
    let mut v = Vec::with_capacity(256);
    let panel = push_menu_panel_background(&mut v, size);
    push_menu_title(&mut v, size, panel, "VOXEL-ENGINE");

    const LABELS: [&str; 3] = ["JUGAR", "CONFIGURACION", "SALIR"];
    const COLORS: [[f32; 4]; 3] = [
        [0.16, 0.32, 0.18, 0.9],
        [0.15, 0.2, 0.32, 0.9],
        [0.32, 0.14, 0.16, 0.9],
    ];
    const TEXT_COLORS: [[f32; 4]; 3] = [
        [0.78, 1.0, 0.82, 1.0],
        [0.85, 0.9, 1.0, 1.0],
        [1.0, 0.75, 0.72, 1.0],
    ];
    for i in 0..3 {
        let rect = TouchController::rect_main_menu_button(size, i);
        push_big_button(&mut v, size, rect, LABELS[i], COLORS[i], TEXT_COLORS[i]);
    }

    v
}

/// Pantalla de pausa (Playing -> Pause): el menú raíz al que se llega
/// desde el botón de engranaje / tecla Esc. 3 botones grandes: "MODO DE
/// JUEGO", "AJUSTES" y "SALIR". Ya no muestra ningún control en sí —
/// esos viven en las pantallas a las que llevan estos botones (ver
/// `build_gamemode_screen`/`build_settings_screen`).
pub fn build_pause_screen(size: PhysicalSize<u32>) -> Vec<UiVertex> {
    let mut v = Vec::with_capacity(256);
    let panel = push_menu_panel_background(&mut v, size);
    push_menu_title(&mut v, size, panel, "PAUSA");

    const LABELS: [&str; 3] = ["MODO DE JUEGO", "AJUSTES", "SALIR"];
    const COLORS: [[f32; 4]; 3] = [
        [0.15, 0.2, 0.32, 0.9],
        [0.15, 0.2, 0.32, 0.9],
        [0.32, 0.14, 0.16, 0.9],
    ];
    const TEXT_COLORS: [[f32; 4]; 3] = [
        [0.85, 0.9, 1.0, 1.0],
        [0.85, 0.9, 1.0, 1.0],
        [1.0, 0.75, 0.72, 1.0],
    ];
    for i in 0..3 {
        let rect = TouchController::rect_pause_button(size, i);
        push_big_button(&mut v, size, rect, LABELS[i], COLORS[i], TEXT_COLORS[i]);
    }

    push_back_button(&mut v, size);
    push_pause_note(&mut v, size, panel);
    v
}

/// Pantalla de "GUARDANDO...", intercalada entre Playing/Pause y
/// MainMenu al tocar "SALIR" (ver `GameScreen::Saving` en lib.rs). A
/// diferencia de las otras pantallas de menú, no tiene botones ni
/// "< VOLVER" ni la nota "JUEGO PAUSADO" — no es interactiva, solo
/// feedback de que el guardado (síncrono, puede tardar) está en curso.
pub fn build_saving_screen(size: PhysicalSize<u32>) -> Vec<UiVertex> {
    let mut v = Vec::with_capacity(64);
    let panel = push_menu_panel_background(&mut v, size);
    let (_panel_x, panel_y, _panel_w, panel_h) = panel;
    push_menu_title(&mut v, size, panel, "GUARDANDO...");

    let cx = size.width as f64 * 0.5;
    let note_y = panel_y + panel_h * 0.5;
    push_text_centered(
        &mut v,
        size,
        "No cierres la app",
        cx,
        note_y,
        [0.6, 0.65, 0.75, 0.85],
    );
    v
}

/// Pantalla de selector de modo de juego (Pause -> GameMode).
pub fn build_gamemode_screen(size: PhysicalSize<u32>, game_mode_index: usize) -> Vec<UiVertex> {
    let mut v = Vec::with_capacity(256);
    let panel = push_menu_panel_background(&mut v, size);
    push_menu_title(&mut v, size, panel, "MODO DE JUEGO");

    let row = TouchController::rect_gamemode_row(size);
    let label_y = row.1 - FONT_CELL_H - 10.0;
    push_text_centered(&mut v, size, "ELEGI COMO QUERES JUGAR", size.width as f64 * 0.5, label_y, [0.7, 0.75, 0.85, 1.0]);

    const MODE_LABELS: [&str; 3] = ["SUPERVIV.", "CREATIVO", "ESPECT."];
    for i in 0..3 {
        let opt_rect = TouchController::rect_mode_option(size, i);
        let selected = i == game_mode_index;
        let bg = if selected { [0.25, 0.55, 0.35, 0.9] } else { [0.15, 0.17, 0.22, 0.85] };
        push_quad(&mut v, size, opt_rect, bg);
        let label = MODE_LABELS[i];
        let (ox, oy, ow, oh) = opt_rect;
        let text_color: [f32; 4] = if selected { [0.75, 1.0, 0.8, 1.0] } else { [0.65, 0.68, 0.75, 1.0] };
        let lx = ox + (ow - text_width(label)) * 0.5;
        let ly = oy + (oh - FONT_CELL_H) * 0.5;
        push_text(&mut v, size, label, lx, ly, text_color);
    }

    push_back_button(&mut v, size);
    push_pause_note(&mut v, size, panel);
    v
}

/// Pantalla de ajustes principales (Pause -> Settings): los controles
/// que se tocan más seguido — FPS, radio de chunks, nubes, niebla —
/// más el botón "AJUSTES ADICIONALES" que lleva al resto (ver
/// `build_settings_more_screen`).
pub fn build_settings_screen(
    size: PhysicalSize<u32>,
    show_fps: bool,
    render_radius: i32,
    show_clouds: bool,
    show_fog: bool,
) -> Vec<UiVertex> {
    let mut v = Vec::with_capacity(512);
    let panel = push_menu_panel_background(&mut v, size);
    push_menu_title(&mut v, size, panel, "AJUSTES");

    // --- "MOSTRAR FPS" ---
    let row1 = TouchController::rect_settings_fps_row(size);
    push_toggle_row(&mut v, size, row1, "MOSTRAR FPS", show_fps);

    // --- "RADIO DE CHUNKS" con stepper [-] valor [+] ---
    let row2 = TouchController::rect_settings_render_distance_row(size);
    let label2_y = row2.1 + (row2.3 - FONT_CELL_H) * 0.5;
    push_text(&mut v, size, "RADIO DE CHUNKS", row2.0, label2_y, [0.85, 0.88, 0.95, 1.0]);

    let minus_rect = TouchController::rect_stepper_minus(row2);
    let plus_rect = TouchController::rect_stepper_plus(row2);
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

    let value_text = render_radius.to_string();
    let value_cx = (minus_rect.0 + minus_rect.2 + plus_rect.0) * 0.5;
    let value_lx = value_cx - text_width(&value_text) * 0.5;
    push_text(&mut v, size, &value_text, value_lx, label2_y, [1.0, 0.85, 0.4, 1.0]);

    push_quad(&mut v, size, (row2.0, row2.1 + row2.3 + 4.0, row2.2, 1.0), [0.3, 0.3, 0.4, 0.4]);

    // --- "NUBES" ---
    let row3 = TouchController::rect_settings_clouds_row(size);
    push_toggle_row(&mut v, size, row3, "NUBES", show_clouds);

    // --- "NIEBLA" ---
    let row4 = TouchController::rect_settings_fog_row(size);
    push_toggle_row(&mut v, size, row4, "NIEBLA", show_fog);

    // --- Botón "AJUSTES ADICIONALES" ---
    let more_rect = TouchController::rect_settings_more_button(size);
    push_big_button(&mut v, size, more_rect, "AJUSTES ADICIONALES", [0.16, 0.22, 0.34, 0.9], [0.8, 0.88, 1.0, 1.0]);

    push_back_button(&mut v, size);
    push_pause_note(&mut v, size, panel);
    v
}

/// Inventario (Playing -> Inventory), abierto con "E" en desktop o el
/// botón "..." en Android (ver `TouchAction::OpenInventory`). Grilla 3x3:
/// a diferencia de la hotbar, acá se ve el NOMBRE de cada material (ver
/// `BlockType::label`), no solo su color/textura — es la idea completa
/// del pedido: poder elegir sabiendo qué es cada cosa. Los slots que
/// sobran (si `BlockType::HOTBAR_SLOTS` < 9) quedan vacíos y atenuados,
/// como espacio reservado para más materiales el día de mañana.
pub fn build_inventory_screen(size: PhysicalSize<u32>, selected_block: BlockType) -> Vec<UiVertex> {
    let mut v = Vec::with_capacity(512);
    let panel = push_menu_panel_background(&mut v, size);
    push_menu_title(&mut v, size, panel, "INVENTARIO");

    for i in 1..=9u8 {
        let rect = TouchController::rect_inventory_slot(size, i);

        if let Some(block) = BlockType::from_hotbar_slot(i) {
            let [r, g, b] = block.color();
            let is_selected = block == selected_block;
            if is_selected {
                let pad = 5.0;
                push_quad(&mut v, size, (rect.0 - pad, rect.1 - pad, rect.2 + pad * 2.0, rect.3 + pad * 2.0), [1.0, 1.0, 1.0, 0.9]);
            }
            push_quad(&mut v, size, rect, [r, g, b, 1.0]);

            let label = block.label();
            let lx = rect.0 + (rect.2 - text_width(label)) * 0.5;
            let ly = rect.1 + rect.3 + 8.0;
            let text_color = if is_selected { [1.0, 1.0, 0.75, 1.0] } else { [0.82, 0.85, 0.92, 1.0] };
            push_text(&mut v, size, label, lx, ly, text_color);
        } else {
            // Slot vacío: solo el marco, sin nombre ni relleno de color.
            push_quad(&mut v, size, rect, [0.14, 0.15, 0.19, 0.55]);
        }
    }

    push_back_button(&mut v, size);
    v
}

/// Lista de mundos guardados (MainMenu -> WorldList), alcanzable desde
/// "JUGAR". "+ CREAR MUNDO NUEVO" arriba de todo, después una fila por
/// cada mundo guardado de la página actual (más reciente primero, ver
/// `save_manager::list_worlds`), y controles de página abajo si hay más
/// de una (ver `TouchController::worldlist_rows_per_page`, que decide
/// cuántas entran por página según el alto real de pantalla). Sin nota
/// "JUEGO PAUSADO": todavía no hay ninguna partida corriendo acá, igual
/// que en `MainMenu`.
pub fn build_worldlist_screen(size: PhysicalSize<u32>, world_names: &[String], page: usize) -> Vec<UiVertex> {
    let mut v = Vec::with_capacity(512);
    let panel = push_menu_panel_background(&mut v, size);
    push_menu_title(&mut v, size, panel, "MUNDOS");

    let create_rect = TouchController::rect_worldlist_create_button(size);
    push_big_button(&mut v, size, create_rect, "+ CREAR MUNDO NUEVO", [0.16, 0.32, 0.18, 0.9], [0.78, 1.0, 0.82, 1.0]);

    if world_names.is_empty() {
        let cx = size.width as f64 * 0.5;
        let empty_y = create_rect.1 + create_rect.3 + 40.0;
        push_text_centered(&mut v, size, "TODAVIA NO HAY MUNDOS GUARDADOS", cx, empty_y, [0.6, 0.63, 0.7, 1.0]);
        push_back_button(&mut v, size);
        return v;
    }

    let rows_per_page = TouchController::worldlist_rows_per_page(size).max(1);
    let page_count = (world_names.len() + rows_per_page - 1) / rows_per_page;
    let page_count = page_count.max(1);
    let page = page.min(page_count - 1);
    let page_start = page * rows_per_page;
    let page_names = &world_names[page_start..(page_start + rows_per_page).min(world_names.len())];

    for (i, name) in page_names.iter().enumerate() {
        let row = TouchController::rect_worldlist_row(size, i);
        push_quad(&mut v, size, row, [0.14, 0.16, 0.22, 0.9]);
        let lx = row.0 + 18.0;
        let ly = row.1 + (row.3 - FONT_CELL_H) * 0.5;
        push_text(&mut v, size, name, lx, ly, [0.85, 0.9, 1.0, 1.0]);

        // Ícono de borrar ("X"), separado de la fila para que no se
        // pueda tocar por error al elegir el mundo (ver
        // `TouchController::rect_worldlist_delete_button`).
        let del = TouchController::rect_worldlist_delete_button(size, i);
        push_quad(&mut v, size, del, [0.30, 0.12, 0.14, 0.9]);
        let x_label = "X";
        let xlx = del.0 + (del.2 - text_width(x_label)) * 0.5;
        let xly = del.1 + (del.3 - FONT_CELL_H) * 0.5;
        push_text(&mut v, size, x_label, xlx, xly, [1.0, 0.6, 0.6, 1.0]);
    }

    if page_count > 1 {
        let cx = size.width as f64 * 0.5;
        let prev = TouchController::rect_worldlist_page_prev(size);
        let next = TouchController::rect_worldlist_page_next(size);
        let label_y = prev.1 + (prev.3 - FONT_CELL_H) * 0.5;

        // Botón anterior: solo se dibuja (y solo es clickeable, ver
        // `hit_worldlist`) si no estamos ya en la primera página. Misma
        // idea para el siguiente con la última.
        if page > 0 {
            push_quad(&mut v, size, prev, [0.14, 0.18, 0.26, 0.9]);
            let label = "< ANTERIOR";
            let lx = prev.0 + (prev.2 - text_width(label)) * 0.5;
            push_text(&mut v, size, label, lx, label_y, [0.8, 0.88, 1.0, 1.0]);
        }
        if page + 1 < page_count {
            push_quad(&mut v, size, next, [0.14, 0.18, 0.26, 0.9]);
            let label = "SIGUIENTE >";
            let lx = next.0 + (next.2 - text_width(label)) * 0.5;
            push_text(&mut v, size, label, lx, label_y, [0.8, 0.88, 1.0, 1.0]);
        }

        let page_label = format!("PAGINA {}/{}", page + 1, page_count);
        push_text_centered(&mut v, size, &page_label, cx, label_y, [0.6, 0.68, 0.8, 1.0]);
    }

    push_back_button(&mut v, size);
    v
}

/// Pantalla para escribir el nombre de un mundo nuevo (WorldList ->
/// NameWorld, ver `TouchAction::OpenNameWorld`). El texto lo escribe el
/// IME nativo del sistema (teclado de Android/PC de toda la vida, ver
/// `set_ime_allowed`/`WindowEvent::Ime` en lib.rs) — acá solo se dibuja
/// el cuadro con lo ya escrito, "BORRAR" y el botón grande "CREAR
/// MUNDO" que confirma. "< VOLVER" (arriba a la izquierda, como en toda
/// pantalla de menú) cancela sin crear nada.
pub fn build_nameworld_screen(size: PhysicalSize<u32>, name_input: &str, name_preedit: &str) -> Vec<UiVertex> {
    let mut v = Vec::with_capacity(512);
    let panel = push_menu_panel_background(&mut v, size);
    push_menu_title(&mut v, size, panel, "NOMBRE DEL MUNDO");

    // Cuadro de texto con el nombre ya confirmado + lo que el IME esté
    // componiendo en este momento (`name_preedit`, en un color más
    // apagado y con una línea debajo — el mismo lenguaje visual que usa
    // cualquier IME de escritorio para distinguir "todavía escribiendo"
    // de "ya confirmado") + cursor fijo al final.
    let field = TouchController::rect_nameworld_textfield(size);
    push_quad(&mut v, size, (field.0 - 2.0, field.1 - 2.0, field.2 + 4.0, field.3 + 4.0), [0.4, 0.55, 0.9, 0.35]);
    push_quad(&mut v, size, field, [0.08, 0.09, 0.14, 0.95]);
    let tx = field.0 + 14.0;
    let ty = field.1 + (field.3 - FONT_CELL_H) * 0.5;
    push_text(&mut v, size, name_input, tx, ty, [0.9, 0.95, 1.0, 1.0]);
    let mut cur_x = tx + text_width(name_input);
    if !name_preedit.is_empty() {
        if !name_input.is_empty() {
            cur_x += FONT_CHAR_GAP;
        }
        push_text(&mut v, size, name_preedit, cur_x, ty, [0.7, 0.78, 0.95, 0.85]);
        // Línea de subrayado bajo el texto en composición, para que se
        // note de un vistazo que todavía no está "confirmado" aunque ya
        // se vea escrito.
        let underline_y = ty + FONT_CELL_H + 2.0;
        push_quad(&mut v, size, (cur_x, underline_y, text_width(name_preedit), 2.0), [0.5, 0.6, 0.85, 0.7]);
        cur_x += text_width(name_preedit) + FONT_CHAR_GAP;
    }
    push_text(&mut v, size, "_", cur_x, ty, [0.9, 0.95, 1.0, 1.0]);

    // "BORRAR".
    let back_key = TouchController::rect_nameworld_backspace(size);
    push_big_button(&mut v, size, back_key, "BORRAR", [0.30, 0.12, 0.14, 0.9], [1.0, 0.75, 0.75, 1.0]);

    // Confirmar.
    let confirm = TouchController::rect_nameworld_confirm(size);
    push_big_button(&mut v, size, confirm, "CREAR MUNDO", [0.16, 0.32, 0.18, 0.9], [0.78, 1.0, 0.82, 1.0]);

    push_back_button(&mut v, size);
    v
}

/// Confirmación antes de borrar un mundo (WorldList -> ConfirmDeleteWorld,
/// ver `TouchAction::RequestDeleteWorld`). "< VOLVER" y "CANCELAR" hacen
/// lo mismo (`TouchAction::Back`, vuelve a la lista sin tocar nada);
/// solo "BORRAR" dispara `TouchAction::ConfirmDeleteWorld`.
pub fn build_confirm_delete_screen(size: PhysicalSize<u32>, world_name: &str) -> Vec<UiVertex> {
    let mut v = Vec::with_capacity(256);
    let panel = push_menu_panel_background(&mut v, size);
    let sep_y = push_menu_title(&mut v, size, panel, "BORRAR MUNDO");

    let cx = size.width as f64 * 0.5;
    push_text_centered(&mut v, size, "SE VA A BORRAR PARA SIEMPRE:", cx, sep_y + 30.0, [0.8, 0.85, 0.95, 1.0]);
    push_text_large_centered(&mut v, size, world_name, cx, sep_y + 66.0, [1.0, 0.8, 0.8, 1.0]);
    push_text_centered(&mut v, size, "ESTA ACCION NO SE PUEDE DESHACER", cx, sep_y + 112.0, [0.6, 0.63, 0.7, 1.0]);

    let cancel = TouchController::rect_confirmdelete_cancel_button(size);
    push_big_button(&mut v, size, cancel, "CANCELAR", [0.14, 0.16, 0.22, 0.9], [0.85, 0.9, 1.0, 1.0]);
    let confirm = TouchController::rect_confirmdelete_confirm_button(size);
    push_big_button(&mut v, size, confirm, "BORRAR", [0.36, 0.12, 0.14, 0.95], [1.0, 0.7, 0.7, 1.0]);

    push_back_button(&mut v, size);
    v
}

/// Pantalla de ajustes adicionales (Settings -> SettingsMore): controles
/// que se tocan con menos frecuencia — info de build, panel de debug e
/// intervalo de autoguardado.
pub fn build_settings_more_screen(
    size: PhysicalSize<u32>,
    show_build_info: bool,
    show_debug_panel: bool,
    autosave_interval_secs: u32,
) -> Vec<UiVertex> {
    let mut v = Vec::with_capacity(512);
    let panel = push_menu_panel_background(&mut v, size);
    push_menu_title(&mut v, size, panel, "AJUSTES ADICIONALES");

    let row1 = TouchController::rect_settings_build_info_row(size);
    push_toggle_row(&mut v, size, row1, "INFO DE BUILD", show_build_info);

    let row2 = TouchController::rect_settings_debug_panel_row(size);
    push_toggle_row(&mut v, size, row2, "PANEL DE DEBUG (F3)", show_debug_panel);

    // --- "AUTOGUARDADO" (cíclico [-]/[+] implícito: un tap avanza al
    // siguiente valor de `AUTOSAVE_OPTIONS_SECS`, ver
    // `TouchAction::CycleAutosaveInterval`) ---
    let row3 = TouchController::rect_settings_autosave_row(size);
    let label3_y = row3.1 + (row3.3 - FONT_CELL_H) * 0.5;
    push_text(&mut v, size, "AUTOGUARDADO", row3.0, label3_y, [0.85, 0.88, 0.95, 1.0]);
    let value_text = if autosave_interval_secs >= 60 {
        format!("{} MIN", autosave_interval_secs / 60)
    } else {
        format!("{} S", autosave_interval_secs)
    };
    push_quad(&mut v, size, TouchController::rect_row_switch(row3), [0.18, 0.22, 0.32, 0.9]);
    let switch = TouchController::rect_row_switch(row3);
    let value_lx = switch.0 + (switch.2 - text_width(&value_text)) * 0.5;
    push_text(&mut v, size, &value_text, value_lx, label3_y, [1.0, 0.85, 0.4, 1.0]);
    push_quad(&mut v, size, (row3.0, row3.1 + row3.3 + 4.0, row3.2, 1.0), [0.3, 0.3, 0.4, 0.4]);

    push_back_button(&mut v, size);
    push_pause_note(&mut v, size, panel);
    v
}

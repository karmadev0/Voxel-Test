/// ui_overlay.rs
/// Geometría 2D del overlay táctil de Android: el joystick de movimiento,
/// los botones de romper/colocar/saltar y la hotbar, dibujados como
/// círculos/cuadrados semitransparentes directamente encima de la escena
/// 3D. No hay texto ni iconos (este engine no tiene un pase de texto
/// todavía) — la hotbar usa el mismo color que el bloque que selecciona
/// (`BlockType::color`), así que sigue siendo legible sin glifos.
///
/// Todo se calcula en espacio de píxeles físicos y se convierte a NDC acá
/// mismo (no hace falta ninguna matriz ni uniform: es la única razón por
/// la que el pipeline de UI en lib.rs no necesita bind group).
use crate::chunk::BlockType;
use crate::touch::TouchController;
use bytemuck::{Pod, Zeroable};
use winit::dpi::PhysicalSize;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct UiVertex {
    pub position: [f32; 2], // ya en NDC (-1..1)
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

/// Convierte una posición en píxeles físicos (origen arriba-izquierda,
/// como los eventos táctiles de winit) a NDC (origen al centro, Y hacia
/// arriba).
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

/// Aro (círculo hueco, dibujado como un anillo de triángulos) — se usa
/// para la base del joystick, así el nub se distingue de la base incluso
/// siendo del mismo tono.
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

/// Ícono simple de "engranaje" para el botón de configuración: un aro
/// (cuerpo) más 4 dientes cuadrados en las posiciones cardinales. No es
/// un gear geométricamente exacto (los dientes son cuadrados, no
/// trapezoides rotados — `push_quad` solo hace rectángulos alineados a
/// los ejes), pero a tamaño de ícono de HUD se lee bien como "ajustes".
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

/// Interruptor on/off: una "pastilla" (acá, un rectángulo simple — no
/// hay forma de hacer esquinas redondeadas sin un shader de distancia
/// aparte) que cambia de color según el estado, con un círculo ("nub")
/// que se desliza a la izquierda (apagado) o la derecha (encendido).
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

/// Tres barritas de altura creciente, a modo de ícono decorativo para la
/// fila de "FPS" del panel de configuración (no hay pase de texto para
/// escribir la palabra, ver comentario al principio del archivo).
fn push_stat_bars_icon(
    out: &mut Vec<UiVertex>,
    size: PhysicalSize<u32>,
    bottom_left: (f64, f64),
    bar_w: f64,
    gap: f64,
    max_h: f64,
    color: [f32; 4],
) {
    for i in 0..3u32 {
        let h = max_h * (0.4 + 0.3 * i as f64);
        let x = bottom_left.0 + i as f64 * (bar_w + gap);
        let y = bottom_left.1 + (max_h - h);
        push_quad(out, size, (x, y, bar_w, h), color);
    }
}

const JOYSTICK_VISUAL_RADIUS: f64 = 70.0;
const NUB_VISUAL_RADIUS: f64 = 26.0;

/// Mira central: una pequeña cruz en el medio de la pantalla, para saber
/// hacia dónde apunta la cámara incluso cuando el bloque apuntado está
/// fuera de alcance (`REACH`) y no hay contorno 3D dibujado todavía (ver
/// `highlight.rs`). Se dibuja en las dos plataformas, no solo Android.
const CROSSHAIR_LENGTH: f64 = 10.0;
const CROSSHAIR_THICKNESS: f64 = 2.0;
const CROSSHAIR_GAP: f64 = 4.0; // hueco en el centro, para no tapar el bloque apuntado

pub fn build_crosshair(size: PhysicalSize<u32>) -> Vec<UiVertex> {
    let mut verts = Vec::with_capacity(12);
    let cx = size.width as f64 / 2.0;
    let cy = size.height as f64 / 2.0;
    let color = [1.0, 1.0, 1.0, 0.85];
    let half_t = CROSSHAIR_THICKNESS / 2.0;

    // Barra horizontal: dos segmentos (izquierda y derecha del hueco central).
    push_quad(
        &mut verts,
        size,
        (cx - CROSSHAIR_GAP - CROSSHAIR_LENGTH, cy - half_t, CROSSHAIR_LENGTH, CROSSHAIR_THICKNESS),
        color,
    );
    push_quad(
        &mut verts,
        size,
        (cx + CROSSHAIR_GAP, cy - half_t, CROSSHAIR_LENGTH, CROSSHAIR_THICKNESS),
        color,
    );
    // Barra vertical: arriba y abajo del hueco central.
    push_quad(
        &mut verts,
        size,
        (cx - half_t, cy - CROSSHAIR_GAP - CROSSHAIR_LENGTH, CROSSHAIR_THICKNESS, CROSSHAIR_LENGTH),
        color,
    );
    push_quad(
        &mut verts,
        size,
        (cx - half_t, cy + CROSSHAIR_GAP, CROSSHAIR_THICKNESS, CROSSHAIR_LENGTH),
        color,
    );

    verts
}

/// Ancho/alto/grosor de trazo de cada dígito del contador de FPS, y
/// separación entre dígitos consecutivos. En píxeles físicos.
const FPS_DIGIT_W: f64 = 16.0;
const FPS_DIGIT_H: f64 = 26.0;
const FPS_DIGIT_THICKNESS: f64 = 4.0;
const FPS_DIGIT_GAP: f64 = 6.0;
const FPS_PANEL_PADDING: f64 = 10.0;
const FPS_MARGIN: f64 = 16.0; // separación del panel respecto al borde de la pantalla

/// Dibuja un dígito (0-9) como display de 7 segmentos dentro del
/// rectángulo `(x, y, w, h)` (esquina superior-izquierda + tamaño).
/// Segmentos, en orden: arriba, arriba-izq, arriba-der, medio,
/// abajo-izq, abajo-der, abajo.
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
        [true, true, true, false, true, true, true],    // 0
        [false, false, true, false, false, true, false], // 1
        [true, false, true, true, true, false, true],    // 2
        [true, false, true, true, false, true, true],    // 3
        [false, true, true, true, false, true, false],   // 4
        [true, true, false, true, false, true, true],    // 5
        [true, true, false, true, true, true, true],     // 6
        [true, false, true, false, false, true, false],  // 7
        [true, true, true, true, true, true, true],      // 8
        [true, true, true, true, false, true, true],     // 9
    ];
    let segs = SEGMENTS_BY_DIGIT[(digit.min(9)) as usize];
    let (x, y) = top_left;
    let half_h = h / 2.0;

    if segs[0] {
        push_quad(out, size, (x + t, y, w - 2.0 * t, t), color); // arriba
    }
    if segs[1] {
        push_quad(out, size, (x, y, t, half_h), color); // arriba-izquierda
    }
    if segs[2] {
        push_quad(out, size, (x + w - t, y, t, half_h), color); // arriba-derecha
    }
    if segs[3] {
        push_quad(out, size, (x + t, y + half_h - t / 2.0, w - 2.0 * t, t), color); // medio
    }
    if segs[4] {
        push_quad(out, size, (x, y + half_h, t, half_h), color); // abajo-izquierda
    }
    if segs[5] {
        push_quad(out, size, (x + w - t, y + half_h, t, half_h), color); // abajo-derecha
    }
    if segs[6] {
        push_quad(out, size, (x + t, y + h - t, w - 2.0 * t, t), color); // abajo
    }
}

/// Contador de FPS: un panel semitransparente pegado a la esquina
/// superior derecha con el número redondeado dibujado en dígitos de 7
/// segmentos (no hay pase de texto/fuentes en este engine todavía, ver
/// comentario al principio del archivo). Soporta 1 a 3 dígitos
/// (0-999); valores fuera de ese rango se recortan.
pub fn build_fps_counter(fps: f32, size: PhysicalSize<u32>) -> Vec<UiVertex> {
    let mut verts = Vec::with_capacity(64);

    let value = fps.round().clamp(0.0, 999.0) as i32;
    let text = value.to_string();
    let num_digits = text.len();

    let digits_w = num_digits as f64 * FPS_DIGIT_W
        + (num_digits.saturating_sub(1)) as f64 * FPS_DIGIT_GAP;
    let panel_w = digits_w + FPS_PANEL_PADDING * 2.0;
    let panel_h = FPS_DIGIT_H + FPS_PANEL_PADDING * 2.0;

    // Esquina superior derecha, con margen respecto a ambos bordes.
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

/// Arma toda la geometría del overlay táctil para este frame. Los
/// controles siempre se dibujan (no solo mientras se tocan), para que el
/// jugador vea dónde están antes de tocarlos.
///
/// `show_fps` refleja el estado actual del interruptor del panel de
/// configuración (se dibuja distinto según esté encendido o apagado);
/// no decide acá si el contador de FPS en sí se dibuja — eso lo hace
/// `lib.rs` por separado.
pub fn build_touch_overlay(
    touch: &TouchController,
    size: PhysicalSize<u32>,
    selected_block: BlockType,
    show_fps: bool,
) -> Vec<UiVertex> {
    let mut verts = Vec::with_capacity(320);

    // Joystick de movimiento: aro base + nub relleno.
    let (base_center, nub_center) = touch.joystick_visual(size);
    push_ring(
        &mut verts,
        size,
        base_center,
        JOYSTICK_VISUAL_RADIUS,
        JOYSTICK_VISUAL_RADIUS - 8.0,
        [1.0, 1.0, 1.0, 0.35],
    );
    push_circle(
        &mut verts,
        size,
        nub_center,
        NUB_VISUAL_RADIUS,
        [1.0, 1.0, 1.0, 0.55],
    );

    // Columna de acciones, lado derecho: colocar / romper / salto (de
    // arriba hacia abajo — ver el diagrama en touch.rs).
    let jump_rect = TouchController::rect_jump(size);
    let jump_center = (
        jump_rect.0 + jump_rect.2 * 0.5,
        jump_rect.1 + jump_rect.3 * 0.5,
    );
    let jump_alpha = if touch.jump_held() { 0.65 } else { 0.35 };
    push_circle(
        &mut verts,
        size,
        jump_center,
        jump_rect.2 * 0.5,
        [1.0, 1.0, 1.0, jump_alpha],
    );

    let break_rect = TouchController::rect_break(size);
    let break_center = (
        break_rect.0 + break_rect.2 * 0.5,
        break_rect.1 + break_rect.3 * 0.5,
    );
    push_circle(&mut verts, size, break_center, break_rect.2 * 0.5, [0.8, 0.15, 0.1, 0.5]);

    let place_rect = TouchController::rect_place(size);
    let place_center = (
        place_rect.0 + place_rect.2 * 0.5,
        place_rect.1 + place_rect.3 * 0.5,
    );
    push_circle(&mut verts, size, place_center, place_rect.2 * 0.5, [0.15, 0.65, 0.2, 0.5]);

    // Hotbar: abajo, centrada — un cuadrado por bloque, con el color del
    // bloque que selecciona (mismo que ve pintado en el mundo). El
    // seleccionado actualmente se dibuja más grande y opaco a modo de
    // "borde".
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
            // "Borde" de selección: un cuadrado blanco un poco más grande
            // detrás, para que se note cuál está activo sin necesitar texto.
            let pad = 6.0;
            push_quad(
                &mut verts,
                size,
                (rect.0 - pad, rect.1 - pad, rect.2 + pad * 2.0, rect.3 + pad * 2.0),
                [1.0, 1.0, 1.0, 0.9],
            );
        }
        let alpha = if is_selected { 1.0 } else { 0.6 };
        push_quad(&mut verts, size, rect, [r, g, b, alpha]);
    }

    // Botón de configuración: arriba a la derecha. Más opaco mientras el
    // panel está abierto, para que quede claro que sigue siendo el botón
    // que lo cierra.
    let settings_rect = TouchController::rect_settings(size);
    let settings_center = (
        settings_rect.0 + settings_rect.2 * 0.5,
        settings_rect.1 + settings_rect.3 * 0.5,
    );
    let settings_bg_alpha = if touch.settings_open() { 0.55 } else { 0.35 };
    push_circle(&mut verts, size, settings_center, settings_rect.2 * 0.5, [1.0, 1.0, 1.0, settings_bg_alpha]);
    push_gear_icon(
        &mut verts,
        size,
        settings_center,
        settings_rect.2 * 0.28,
        [0.1, 0.1, 0.1, 0.9],
    );

    // Panel de configuración: solo cuando está abierto, dibujado al
    // final para que quede por encima de todo lo demás.
    if touch.settings_open() {
        let panel_rect = TouchController::rect_settings_panel(size);
        push_quad(&mut verts, size, panel_rect, [0.05, 0.05, 0.05, 0.85]);

        let row_rect = TouchController::rect_fps_toggle_row(size);
        let bars_h = row_rect.3 * 0.55;
        push_stat_bars_icon(
            &mut verts,
            size,
            (row_rect.0, row_rect.1 + (row_rect.3 - bars_h)),
            8.0,
            5.0,
            bars_h,
            [0.85, 0.85, 0.85, 0.9],
        );

        let switch_rect = TouchController::rect_fps_toggle_switch(size);
        push_toggle_switch(&mut verts, size, switch_rect, show_fps);
    }

    verts
}

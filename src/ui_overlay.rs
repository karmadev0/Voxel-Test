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

const JOYSTICK_VISUAL_RADIUS: f64 = 70.0;
const NUB_VISUAL_RADIUS: f64 = 26.0;

/// Arma toda la geometría del overlay táctil para este frame. Los
/// controles siempre se dibujan (no solo mientras se tocan), para que el
/// jugador vea dónde están antes de tocarlos.
pub fn build_touch_overlay(
    touch: &TouchController,
    size: PhysicalSize<u32>,
    selected_block: BlockType,
) -> Vec<UiVertex> {
    let mut verts = Vec::with_capacity(256);

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

    // Botón de salto: círculo inscripto en su zona cuadrada de hit-test,
    // más brillante mientras se mantiene apretado.
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

    // Romper (rojo) / Colocar (verde), como círculos traslúcidos.
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

    // Hotbar: un cuadrado por bloque, con el color del bloque que
    // selecciona (mismo que ve pintado en el mundo). El seleccionado
    // actualmente se dibuja más grande y opaco a modo de "borde".
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

    verts
}

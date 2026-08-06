/// touch.rs
/// Controles táctiles para Android: hit-testing de las zonas fijas del
/// joystick de movimiento, mirar arrastrando, salto, romper/colocar,
/// hotbar y el botón de configuración. El dibujo del overlay (lo que el
/// jugador ve) vive en `ui_overlay.rs`, que reutiliza los mismos
/// `rect_*`/`joystick_visual` de acá para que el hit-test y el dibujo
/// nunca se desincronicen — son la misma fuente de verdad.
///
/// Cuando se toca el engranaje, el juego pasa a `GameScreen::Settings`:
/// se pausa toda la lógica de juego y se muestra una pantalla fullscreen
/// de configuración. Tocar "Volver" (o el botón X) regresa al juego.
///
///   ┌───────────────────────────────────────┬───────┐
///   │                                        │ ⚙ (cfg)│  <- config (arriba, dcha)
///   │                                        └───────┘
///   │                                        ┌───────┐
///   │            mirar (drag)                │ COLOCAR│
///   │           (mitad derecha)               ├───────┤
///   ├────────────────┐                        │ ROMPER │
///   │   moverse       │                       ├───────┤
///   │  (joystick,     │                       │ SALTO  │  <- acciones (dcha)
///   │ mitad izq.)     │                       └───────┘
///   │                 └───── hotbar (1 2 3) ──┘
///   └─────────────────────────────────────────────────┘
use std::collections::HashMap;
use winit::dpi::PhysicalSize;
use winit::event::{Touch, TouchPhase};

/// Radio, en píxeles físicos, del joystick de movimiento.
const JOYSTICK_RADIUS: f64 = 70.0;

/// Tamaño de los botones cuadrados (romper/colocar/salto).
const BUTTON_SIZE: f64 = 90.0;
const BUTTON_GAP: f64 = 16.0;
const HOTBAR_SIZE: f64 = 64.0;
const HOTBAR_GAP: f64 = 12.0;
const MARGIN: f64 = 24.0;

pub const SETTINGS_BUTTON_SIZE: f64 = 64.0;

/// Acciones de un solo disparo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchAction {
    Break,
    Place,
    SelectBlock(u8), // 1, 2 o 3
    /// El jugador tocó el botón de engranaje → ir a pantalla de config.
    OpenSettings,
    /// El jugador tocó "Volver" en la pantalla de config → volver al juego.
    CloseSettings,
    ToggleFps,
    ToggleWalkMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Zone {
    Movement,
    Look,
    Jump,
}

struct ActiveDrag {
    zone: Zone,
    start: (f64, f64),
    last: (f64, f64),
}

pub struct TouchController {
    drags: HashMap<u64, ActiveDrag>,
    pending_look_dx: f32,
    pending_look_dy: f32,
    jump_held: bool,
}

impl TouchController {
    pub fn new() -> Self {
        Self {
            drags: HashMap::new(),
            pending_look_dx: 0.0,
            pending_look_dy: 0.0,
            jump_held: false,
        }
    }

    // --- Rectangulos de botones del juego ---

    pub(crate) fn rect_jump(size: PhysicalSize<u32>) -> (f64, f64, f64, f64) {
        let x = size.width as f64 - MARGIN - BUTTON_SIZE;
        let y = size.height as f64 - MARGIN - BUTTON_SIZE;
        (x, y, BUTTON_SIZE, BUTTON_SIZE)
    }

    pub(crate) fn rect_break(size: PhysicalSize<u32>) -> (f64, f64, f64, f64) {
        let (x, jump_y, ..) = Self::rect_jump(size);
        let y = jump_y - BUTTON_GAP - BUTTON_SIZE;
        (x, y, BUTTON_SIZE, BUTTON_SIZE)
    }

    pub(crate) fn rect_place(size: PhysicalSize<u32>) -> (f64, f64, f64, f64) {
        let (x, break_y, ..) = Self::rect_break(size);
        let y = break_y - BUTTON_GAP - BUTTON_SIZE;
        (x, y, BUTTON_SIZE, BUTTON_SIZE)
    }

    pub(crate) fn rect_hotbar(size: PhysicalSize<u32>, index: u8) -> (f64, f64, f64, f64) {
        let total_w = HOTBAR_SIZE * 3.0 + HOTBAR_GAP * 2.0;
        let start_x = size.width as f64 * 0.5 - total_w * 0.5;
        let x = start_x + (index - 1) as f64 * (HOTBAR_SIZE + HOTBAR_GAP);
        let y = size.height as f64 - MARGIN - HOTBAR_SIZE;
        (x, y, HOTBAR_SIZE, HOTBAR_SIZE)
    }

    /// Botón de configuración: arriba a la derecha.
    pub(crate) fn rect_settings(size: PhysicalSize<u32>) -> (f64, f64, f64, f64) {
        let x = size.width as f64 - MARGIN - SETTINGS_BUTTON_SIZE;
        let y = MARGIN;
        (x, y, SETTINGS_BUTTON_SIZE, SETTINGS_BUTTON_SIZE)
    }

    // --- Rectangulos de la pantalla de configuración fullscreen ---

    /// Botón "Volver" en la pantalla de configuración: esquina superior izquierda.
    pub(crate) fn rect_back_button(size: PhysicalSize<u32>) -> (f64, f64, f64, f64) {
        (MARGIN, MARGIN, 160.0, 56.0)
    }

    /// Fila del toggle FPS en la pantalla de config.
    pub(crate) fn rect_settings_fps_row(size: PhysicalSize<u32>) -> (f64, f64, f64, f64) {
        let cx = size.width as f64 * 0.5;
        let row_w = 480.0_f64.min(size.width as f64 * 0.8);
        let row_x = cx - row_w * 0.5;
        let row_y = size.height as f64 * 0.35;
        (row_x, row_y, row_w, 64.0)
    }

    /// Fila del toggle modo Caminar en la pantalla de config.
    pub(crate) fn rect_settings_walk_row(size: PhysicalSize<u32>) -> (f64, f64, f64, f64) {
        let (x, y, w, h) = Self::rect_settings_fps_row(size);
        (x, y + h + 24.0, w, h)
    }

    /// Sub-rectángulo del switch visual dentro de una fila (pegado al borde derecho).
    pub(crate) fn rect_row_switch(row: (f64, f64, f64, f64)) -> (f64, f64, f64, f64) {
        let (rx, ry, rw, rh) = row;
        let w = 72.0;
        let h = 36.0;
        let x = rx + rw - w;
        let y = ry + (rh - h) * 0.5;
        (x, y, w, h)
    }

    pub(crate) fn joystick_rest_center(size: PhysicalSize<u32>) -> (f64, f64) {
        let cx = MARGIN + JOYSTICK_RADIUS + 12.0;
        let cy = size.height as f64 - MARGIN - JOYSTICK_RADIUS - 12.0;
        (cx, cy)
    }

    pub(crate) fn joystick_visual(&self, size: PhysicalSize<u32>) -> ((f64, f64), (f64, f64)) {
        for drag in self.drags.values() {
            if drag.zone == Zone::Movement {
                let dx = drag.last.0 - drag.start.0;
                let dy = drag.last.1 - drag.start.1;
                let len = (dx * dx + dy * dy).sqrt();
                let nub = if len > JOYSTICK_RADIUS {
                    let scale = JOYSTICK_RADIUS / len;
                    (drag.start.0 + dx * scale, drag.start.1 + dy * scale)
                } else {
                    (drag.last.0, drag.last.1)
                };
                return (drag.start, nub);
            }
        }
        let rest = Self::joystick_rest_center(size);
        (rest, rest)
    }

    fn point_in_rect(p: (f64, f64), r: (f64, f64, f64, f64)) -> bool {
        p.0 >= r.0 && p.0 <= r.0 + r.2 && p.1 >= r.1 && p.1 <= r.1 + r.3
    }

    fn zone_for(pos: (f64, f64), size: PhysicalSize<u32>) -> Option<Zone> {
        if Self::point_in_rect(pos, Self::rect_jump(size)) {
            return Some(Zone::Jump);
        }
        if pos.0 < size.width as f64 * 0.5 {
            Some(Zone::Movement)
        } else {
            Some(Zone::Look)
        }
    }

    /// Procesa un evento táctil durante el juego (pantalla de juego activa).
    pub fn on_touch_game(&mut self, touch: Touch, size: PhysicalSize<u32>) -> Option<TouchAction> {
        let pos = (touch.location.x, touch.location.y);

        match touch.phase {
            TouchPhase::Started => {
                if Self::point_in_rect(pos, Self::rect_settings(size)) {
                    // Soltar drags activos para no dejar el joystick pegado.
                    self.drags.clear();
                    self.jump_held = false;
                    return Some(TouchAction::OpenSettings);
                }
                if Self::point_in_rect(pos, Self::rect_break(size)) {
                    return Some(TouchAction::Break);
                }
                if Self::point_in_rect(pos, Self::rect_place(size)) {
                    return Some(TouchAction::Place);
                }
                for i in 1..=3u8 {
                    if Self::point_in_rect(pos, Self::rect_hotbar(size, i)) {
                        return Some(TouchAction::SelectBlock(i));
                    }
                }
                if let Some(zone) = Self::zone_for(pos, size) {
                    if zone == Zone::Jump {
                        self.jump_held = true;
                    }
                    self.drags.insert(
                        touch.id,
                        ActiveDrag { zone, start: pos, last: pos },
                    );
                }
                None
            }
            TouchPhase::Moved => {
                if let Some(drag) = self.drags.get_mut(&touch.id) {
                    if drag.zone == Zone::Look {
                        self.pending_look_dx += (pos.0 - drag.last.0) as f32;
                        self.pending_look_dy += (pos.1 - drag.last.1) as f32;
                    }
                    drag.last = pos;
                }
                None
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                if let Some(drag) = self.drags.remove(&touch.id) {
                    if drag.zone == Zone::Jump {
                        self.jump_held = false;
                    }
                }
                None
            }
        }
    }

    /// Procesa un evento táctil durante la pantalla de configuración.
    pub fn on_touch_settings(
        &self,
        touch: Touch,
        size: PhysicalSize<u32>,
        show_fps: bool,
        walk_mode: bool,
    ) -> Option<TouchAction> {
        if touch.phase != TouchPhase::Started {
            return None;
        }
        let pos = (touch.location.x, touch.location.y);

        if Self::point_in_rect(pos, Self::rect_back_button(size)) {
            return Some(TouchAction::CloseSettings);
        }

        let fps_row = Self::rect_settings_fps_row(size);
        if Self::point_in_rect(pos, fps_row) {
            return Some(TouchAction::ToggleFps);
        }

        let walk_row = Self::rect_settings_walk_row(size);
        if Self::point_in_rect(pos, walk_row) {
            return Some(TouchAction::ToggleWalkMode);
        }

        None
    }

    pub fn move_axis(&self) -> (f32, f32) {
        for drag in self.drags.values() {
            if drag.zone == Zone::Movement {
                let dx = (drag.last.0 - drag.start.0) / JOYSTICK_RADIUS;
                let dy = (drag.last.1 - drag.start.1) / JOYSTICK_RADIUS;
                let len = (dx * dx + dy * dy).sqrt();
                let scale = if len > 1.0 { 1.0 / len } else { 1.0 };
                return ((dx * scale) as f32, (-dy * scale) as f32);
            }
        }
        (0.0, 0.0)
    }

    pub fn take_look_delta(&mut self) -> (f32, f32) {
        let d = (self.pending_look_dx, self.pending_look_dy);
        self.pending_look_dx = 0.0;
        self.pending_look_dy = 0.0;
        d
    }

    pub fn jump_held(&self) -> bool {
        self.jump_held
    }
}

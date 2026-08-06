/// touch.rs
/// Controles táctiles para Android: hit-testing de las zonas fijas del
/// joystick de movimiento, mirar arrastrando, salto y los botones de
/// romper/colocar/hotbar. El dibujo del overlay (lo que el jugador ve)
/// vive en `ui_overlay.rs`, que reutiliza los mismos `rect_*`/
/// `joystick_visual` de acá para que el hit-test y el dibujo nunca se
/// desincronicen — son la misma fuente de verdad:
///
///   ┌───────────────────────────────┬───────────┐
///   │                                │  1  2  3  │  <- hotbar (arriba, dcha)
///   │                                │           │
///   │         mirar (drag)          │           │
///   │        (mitad derecha)         │           │
///   │                                │ ┌───────┐ │
///   │                                │ │ COLOCAR│ │  <- botones (abajo, dcha)
///   ├───────────────┐                │ ├───────┤ │
///   │   moverse      │   (salto)     │ │ ROMPER │ │
///   │  (joystick,    │               │ └───────┘ │
///   │ mitad izq.)    │               │           │
///   └────────────────┴───────────────┴───────────┘
/// touch.rs
/// Controles táctiles para Android: hit-testing de las zonas fijas del
/// joystick de movimiento, mirar arrastrando, salto, romper/colocar,
/// hotbar y el botón de configuración. El dibujo del overlay (lo que el
/// jugador ve) vive en `ui_overlay.rs`, que reutiliza los mismos
/// `rect_*`/`joystick_visual` de acá para que el hit-test y el dibujo
/// nunca se desincronicen — son la misma fuente de verdad:
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
///
/// El panel de configuración (`settings_open`) es modal: mientras está
/// abierto, el resto de los controles de juego queda congelado (no se
/// registran drags de moverse/mirar/salto ni toques de romper/colocar/
/// hotbar) para que ajustar una opción no dispare una acción del juego
/// por accidente. Por ahora solo tiene el interruptor de FPS, pero el
/// panel queda armado para sumar más filas de opciones más adelante.
use std::collections::HashMap;
use winit::dpi::PhysicalSize;
use winit::event::{Touch, TouchPhase};

/// Radio, en píxeles físicos, del joystick de movimiento: cuánto hay que
/// arrastrar el dedo para llegar a velocidad máxima en esa dirección.
const JOYSTICK_RADIUS: f64 = 70.0;

/// Tamaño de los botones cuadrados (romper/colocar/salto), en píxeles
/// físicos, y separación entre ellos.
const BUTTON_SIZE: f64 = 90.0;
const BUTTON_GAP: f64 = 16.0;
const HOTBAR_SIZE: f64 = 64.0;
const HOTBAR_GAP: f64 = 12.0;
const MARGIN: f64 = 24.0;

const SETTINGS_BUTTON_SIZE: f64 = 64.0;
const SETTINGS_PANEL_W: f64 = 240.0;
const SETTINGS_ROW_H: f64 = 56.0;
const SETTINGS_PANEL_PADDING: f64 = 16.0;

/// Acciones de un solo disparo (se generan al apretar un botón, no al
/// mantenerlo).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchAction {
    Break,
    Place,
    SelectBlock(u8), // 1, 2 o 3
    ToggleSettings,
    ToggleFps,
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
    settings_open: bool,
}

impl TouchController {
    pub fn new() -> Self {
        Self {
            drags: HashMap::new(),
            pending_look_dx: 0.0,
            pending_look_dy: 0.0,
            jump_held: false,
            settings_open: false,
        }
    }

    /// Si el panel de configuración está abierto. `ui_overlay` lo usa
    /// para saber si tiene que dibujar el panel encima de todo lo demás.
    pub fn settings_open(&self) -> bool {
        self.settings_open
    }

    // --- Columna de acciones, lado derecho: colocar (arriba), romper
    // (medio), salto (abajo) — el orden pone el botón que más se toca
    // (salto) más cerca de donde suele descansar el pulgar. ---

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

    /// Hotbar: abajo, centrada horizontalmente (los 3 únicos bloques).
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

    /// Panel de configuración: cuelga del botón de configuración, mismo
    /// borde derecho. La altura de acá crece si en algún momento se
    /// suman más filas de opciones debajo de la de FPS.
    pub(crate) fn rect_settings_panel(size: PhysicalSize<u32>) -> (f64, f64, f64, f64) {
        let (settings_x, settings_y, _, settings_h) = Self::rect_settings(size);
        let w = SETTINGS_PANEL_W;
        let h = SETTINGS_ROW_H + SETTINGS_PANEL_PADDING * 2.0;
        let x = settings_x + SETTINGS_BUTTON_SIZE - w;
        let y = settings_y + settings_h + 12.0;
        (x, y, w, h)
    }

    /// Fila completa del interruptor de FPS dentro del panel — el hit
    /// area cubre toda la fila (no solo el interruptor visual) para que
    /// sea más fácil de tocar.
    pub(crate) fn rect_fps_toggle_row(size: PhysicalSize<u32>) -> (f64, f64, f64, f64) {
        let (px, py, pw, _) = Self::rect_settings_panel(size);
        (
            px + SETTINGS_PANEL_PADDING,
            py + SETTINGS_PANEL_PADDING,
            pw - SETTINGS_PANEL_PADDING * 2.0,
            SETTINGS_ROW_H,
        )
    }

    /// Sub-rectángulo del interruptor visual (el "switch") dentro de la
    /// fila de FPS, pegado al borde derecho de la fila.
    pub(crate) fn rect_fps_toggle_switch(size: PhysicalSize<u32>) -> (f64, f64, f64, f64) {
        let (rx, ry, rw, rh) = Self::rect_fps_toggle_row(size);
        let w = 56.0;
        let h = 28.0;
        let x = rx + rw - w;
        let y = ry + (rh - h) * 0.5;
        (x, y, w, h)
    }

    /// Centro por defecto del joystick de movimiento cuando no hay ningún
    /// dedo tocándolo — sirve para dibujar un aro "guía" en esa posición
    /// aunque nadie lo esté usando, así el jugador sabe dónde tocar sin
    /// tener que adivinar (las zonas son invisibles si no).
    pub(crate) fn joystick_rest_center(size: PhysicalSize<u32>) -> (f64, f64) {
        let cx = MARGIN + JOYSTICK_RADIUS + 12.0;
        let cy = size.height as f64 - MARGIN - JOYSTICK_RADIUS - 12.0;
        (cx, cy)
    }

    /// (centro de la base, centro del "nub") del joystick de movimiento
    /// para dibujarlo. Es un joystick "flotante": en reposo la base se
    /// dibuja en `joystick_rest_center`; en cuanto un dedo lo toca, la
    /// base salta a donde tocó (igual que el hit-testing en `on_touch`,
    /// que arranca el drag desde `drag.start`) y el nub sigue al dedo,
    /// siempre clampeado a `JOYSTICK_RADIUS` para que nunca se dibuje
    /// "suelto" del aro.
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

    /// Procesa un evento táctil crudo de winit. Devuelve `Some(acción)`
    /// si el toque correspondía a un botón de un solo disparo
    /// (romper/colocar/hotbar/configuración); el resto de los toques
    /// (joystick, mirar, salto) se consultan aparte con `move_axis`,
    /// `take_look_delta` y `jump_held`.
    pub fn on_touch(&mut self, touch: Touch, size: PhysicalSize<u32>) -> Option<TouchAction> {
        let pos = (touch.location.x, touch.location.y);

        match touch.phase {
            TouchPhase::Started => {
                // El botón de configuración siempre responde, incluso
                // con el panel ya abierto (para poder cerrarlo tocándolo
                // de nuevo).
                if Self::point_in_rect(pos, Self::rect_settings(size)) {
                    self.settings_open = !self.settings_open;
                    if self.settings_open {
                        // Soltamos cualquier drag de juego que hubiera
                        // quedado a medias (por ejemplo, otro dedo ya
                        // movía el joystick) para no dejarlo "pegado".
                        self.drags.clear();
                        self.jump_held = false;
                    }
                    return Some(TouchAction::ToggleSettings);
                }

                if self.settings_open {
                    // Con el panel abierto, la única otra zona activa es
                    // el interruptor de FPS: el resto del juego queda
                    // congelado para no tocar nada por accidente
                    // mientras se está configurando.
                    if Self::point_in_rect(pos, Self::rect_fps_toggle_row(size)) {
                        return Some(TouchAction::ToggleFps);
                    }
                    return None;
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
                        ActiveDrag {
                            zone,
                            start: pos,
                            last: pos,
                        },
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

    /// Vector analógico de movimiento (x = strafe, y = adelante), cada
    /// componente en [-1, 1], según el arrastre del joystick de
    /// movimiento desde su punto de origen.
    pub fn move_axis(&self) -> (f32, f32) {
        for drag in self.drags.values() {
            if drag.zone == Zone::Movement {
                let dx = (drag.last.0 - drag.start.0) / JOYSTICK_RADIUS;
                let dy = (drag.last.1 - drag.start.1) / JOYSTICK_RADIUS;
                let len = (dx * dx + dy * dy).sqrt();
                let scale = if len > 1.0 { 1.0 / len } else { 1.0 };
                // dy de pantalla crece hacia abajo; lo invertimos para que
                // "arrastrar hacia arriba" sea "avanzar".
                return ((dx * scale) as f32, (-dy * scale) as f32);
            }
        }
        (0.0, 0.0)
    }

    /// Delta acumulado de mirada desde la última llamada (equivalente al
    /// delta de mouse). Lo vacía al leerlo.
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

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
use std::collections::HashMap;
use winit::dpi::PhysicalSize;
use winit::event::{Touch, TouchPhase};

/// Radio, en píxeles físicos, del joystick de movimiento: cuánto hay que
/// arrastrar el dedo para llegar a velocidad máxima en esa dirección.
const JOYSTICK_RADIUS: f64 = 70.0;

/// Tamaño de los botones cuadrados (romper/colocar/salto/hotbar), en
/// píxeles físicos.
const BUTTON_SIZE: f64 = 90.0;
const HOTBAR_SIZE: f64 = 64.0;
const MARGIN: f64 = 24.0;

/// Acciones de un solo disparo (se generan al apretar un botón, no al
/// mantenerlo).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchAction {
    Break,
    Place,
    SelectBlock(u8), // 1, 2 o 3
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

    pub(crate) fn rect_break(size: PhysicalSize<u32>) -> (f64, f64, f64, f64) {
        let x = size.width as f64 - MARGIN - BUTTON_SIZE;
        let y = size.height as f64 - MARGIN - BUTTON_SIZE;
        (x, y, BUTTON_SIZE, BUTTON_SIZE)
    }

    pub(crate) fn rect_place(size: PhysicalSize<u32>) -> (f64, f64, f64, f64) {
        let x = size.width as f64 - MARGIN - BUTTON_SIZE;
        let y = size.height as f64 - MARGIN - BUTTON_SIZE * 2.0 - 16.0;
        (x, y, BUTTON_SIZE, BUTTON_SIZE)
    }

    pub(crate) fn rect_jump(size: PhysicalSize<u32>) -> (f64, f64, f64, f64) {
        let x = size.width as f64 * 0.5 - MARGIN - BUTTON_SIZE;
        let y = size.height as f64 - MARGIN - BUTTON_SIZE;
        (x, y, BUTTON_SIZE, BUTTON_SIZE)
    }

    pub(crate) fn rect_hotbar(size: PhysicalSize<u32>, index: u8) -> (f64, f64, f64, f64) {
        let x = size.width as f64
            - MARGIN
            - (HOTBAR_SIZE + 12.0) * (3 - index) as f64
            - HOTBAR_SIZE;
        let y = MARGIN;
        (x, y, HOTBAR_SIZE, HOTBAR_SIZE)
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
    /// (romper/colocar/hotbar); el resto de los toques (joystick, mirar,
    /// salto) se consultan aparte con `move_axis`, `take_look_delta` y
    /// `jump_held`.
    pub fn on_touch(&mut self, touch: Touch, size: PhysicalSize<u32>) -> Option<TouchAction> {
        let pos = (touch.location.x, touch.location.y);

        match touch.phase {
            TouchPhase::Started => {
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

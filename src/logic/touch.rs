/// touch.rs
/// Controles táctiles para Android, estilo Minecraft (Bedrock): hit-testing
/// de las zonas fijas del joystick de movimiento, la zona de mirar/romper/
/// colocar, salto, hotbar y el botón de configuración. El dibujo del
/// overlay (lo que el jugador ve) vive en `ui_overlay.rs`, que reutiliza
/// los mismos `rect_*`/`joystick_visual` de acá para que el hit-test y el
/// dibujo nunca se desincronicen — son la misma fuente de verdad.
///
/// La mitad derecha de la pantalla (la misma zona `Look` que ya rota la
/// cámara al arrastrar) hace doble función, igual que en Minecraft:
///   - Mantener presionado (sin soltar rápido) → romper el bloque
///     apuntado, en repetición mientras el dedo siga apoyado.
///   - Toque rápido y sin arrastrar demasiado → colocar un bloque.
/// Así no hacen falta botones separados de ROMPER/COLOCAR: los dos
/// botones "de acción" que quedan a la vista son SALTO (subir en modo
/// vuelo) y AGACHARSE (bajar en modo vuelo, ver `Zone::Crouch` y
/// `Camera::wants_crouch`/`set_touch_down`). SALTO es más grande porque
/// es el que se usa con más frecuencia y hay que poder tocarlo sin
/// apuntar con precisión.
///
/// Cuando se toca el engranaje, el juego pasa a `GameScreen::Settings`:
/// se pausa toda la lógica de juego y se muestra una pantalla fullscreen
/// de configuración. Tocar "Volver" (o el botón X) regresa al juego.
///
///   ┌───────────────────────────────────────┬───────┐
///   │                                        │ ⚙ (cfg)│  <- config (arriba, dcha)
///   │                                        └───────┘
///   │            mirar (drag) /
///   │     mantener = romper, toque = colocar
///   │           (mitad derecha)          ┌──────┐┌─────┐
///   ├────────────────┐                   │AGACH.││SALTO│  <- esquina inf. dcha
///   │   moverse       │                  └──────┘└─────┘
///   │  (joystick,     │
///   │ mitad izq.)     │
///   │                 └───── hotbar (1 2 3) ──┘
///   └─────────────────────────────────────────────────┘
use std::collections::HashMap;
use std::time::Instant;
use winit::dpi::PhysicalSize;
use winit::event::{Touch, TouchPhase};

/// Radio, en píxeles físicos, del joystick de movimiento.
const JOYSTICK_RADIUS: f64 = 70.0;

/// Tamaño del botón de salto: el único botón "de acción" que queda
/// visible, así que se agranda bastante respecto al viejo tamaño
/// compartido con romper/colocar (90px) para que sea fácil de tocar sin
/// mirar el dedo.
const JUMP_BUTTON_SIZE: f64 = 140.0;

/// Tamaño del botón de agachar/bajar: segundo botón de acción, más chico
/// que salto porque se usa con menos frecuencia (ver `rect_crouch`).
const CROUCH_BUTTON_SIZE: f64 = 100.0;
/// Espacio, en píxeles físicos, entre el botón de salto y el de agachar.
const ACTION_BUTTON_GAP: f64 = 16.0;
const HOTBAR_SIZE: f64 = 64.0;
const HOTBAR_GAP: f64 = 12.0;
const MARGIN: f64 = 24.0;

/// Ventana de tiempo, en milisegundos, por debajo de la cual un toque en
/// la zona de mirar cuenta como "toque rápido" → colocar. Por encima de
/// esto (y mientras el dedo siga apoyado) se considera "mantener
/// presionado" → romper, igual que el long-press de Minecraft.
const TAP_MAX_MS: u128 = 220;

/// Distancia máxima, en píxeles físicos, que el dedo puede haberse
/// movido desde que tocó la pantalla para que el toque siga contando
/// como "rápido" (colocar) al soltarlo. Por encima de esto se interpreta
/// como que el jugador estaba mirando alrededor (arrastrando), no
/// colocando, así que soltar no hace nada.
const TAP_MAX_MOVE: f64 = 18.0;

/// Cada cuánto, en milisegundos, se repite la rotura mientras el dedo
/// sigue apoyado en la zona de mirar (evita romper varios bloques de
/// un tirón en un solo frame apenas se cumple `TAP_MAX_MS`).
const HOLD_BREAK_REPEAT_MS: u128 = 200;

pub const SETTINGS_BUTTON_SIZE: f64 = 64.0;

/// Acciones de un solo disparo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchAction {
    /// Toque rápido (sin arrastrar) en la zona de mirar. Romper ya no es
    /// un `TouchAction`: se dispara directamente desde `App::update` vía
    /// `TouchController::poll_hold_break`, porque tiene que poder repetirse
    /// mientras el dedo sigue apoyado sin que llegue un evento táctil
    /// nuevo cada vez (ver el comentario al principio de este archivo).
    Place,
    SelectBlock(u8), // 1, 2 o 3
    /// El jugador tocó el botón de engranaje → ir al menú de pausa.
    OpenPause,
    /// El jugador tocó "MODO DE JUEGO" en el menú de pausa → ir al
    /// selector de modo de juego.
    OpenGameModeScreen,
    /// El jugador tocó "AJUSTES" en el menú de pausa → ir a la pantalla
    /// de ajustes.
    OpenSettingsScreen,
    /// El jugador tocó "AJUSTES ADICIONALES" en la pantalla de ajustes
    /// → ir a la pantalla de ajustes adicionales.
    OpenSettingsMore,
    /// El jugador tocó "SALIR" en el menú de pausa → cierra el juego.
    ExitGame,
    /// El jugador tocó "< VOLVER" en cualquier pantalla de menú → sube
    /// un nivel en la jerarquía (ver el comentario de `GameScreen` en
    /// lib.rs, que decide a qué pantalla exacta se vuelve).
    Back,
    ToggleFps,
    /// Selección de modo de juego desde la pantalla de selector: 0 =
    /// Supervivencia, 1 = Creativo, 2 = Espectador (mismo índice que
    /// `TouchController::rect_mode_option`). Solo se dispara desde
    /// `on_touch_gamemode` — no hay forma de cambiar de modo durante el
    /// juego en sí, a propósito (ver `State::set_game_mode` en lib.rs).
    SetGameMode(usize),
    /// Prender/apagar el dibujado de la capa de nubes (fila "NUBES" en
    /// la pantalla de config). No afecta el streaming de chunks ni nada
    /// más, solo si el draw call de `clouds_pipeline` se ejecuta o no.
    ToggleClouds,
    /// Prender/apagar la niebla de distancia (fila "NIEBLA"). Al
    /// apagarla, `fog_start`/`fog_end` se mandan con un valor centinela
    /// enorme (ver `update` en lib.rs) para que la mezcla hacia el color
    /// de cielo nunca llegue a activarse — el terreno se corta en seco
    /// en el borde del radio de chunks, y la capa de nubes (si está
    /// prendida) deja de perder su borde recto contra el horizonte.
    ToggleFog,
    /// Bajar/subir la distancia de renderizado (radio de chunks) un
    /// paso, con los botones [-]/[+] de la fila "Distancia de chunks"
    /// en la pantalla de config. El clamp a MIN/MAX vive en lib.rs,
    /// junto con el resto del estado de `render_radius`.
    DecreaseRenderRadius,
    IncreaseRenderRadius,
    /// Prender/apagar el overlay de información de build (etiqueta de
    /// build + plataforma, esquina superior izquierda). Fila "INFO DE
    /// BUILD" en la pantalla de config.
    ToggleBuildInfo,
    /// Prender/apagar el panel de debug (posición, chunk, bloque
    /// apuntado, modo, fps, ruta de log). Fila "PANEL DE DEBUG (F3)" en
    /// la pantalla de config; también togglea con la tecla F3 en
    /// desktop (ver lib.rs), de ahí el nombre entre paréntesis en la
    /// etiqueta de la fila.
    ToggleDebugPanel,
    /// El jugador tocó el botón "COPIAR" dentro del panel de debug (solo
    /// llega desde `on_touch_game`, no desde `on_touch_settings`: el
    /// panel de debug se dibuja encima del juego, no de la pantalla de
    /// ajustes).
    CopyDebugSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Zone {
    Movement,
    Look,
    Jump,
    /// Segundo botón de acción: agacharse en Supervivencia, bajar en
    /// Creativo/Espectador (ver `Camera::wants_crouch`/`set_touch_down`).
    Crouch,
}

struct ActiveDrag {
    zone: Zone,
    start: (f64, f64),
    last: (f64, f64),
    /// Cuándo empezó este toque. Solo se usa para la zona `Look`, para
    /// distinguir toque rápido (colocar) de mantener presionado (romper).
    started_at: Instant,
    /// Última vez que este toque disparó una rotura mientras estaba en
    /// modo "mantener presionado". `None` hasta que se cumple
    /// `TAP_MAX_MS` por primera vez.
    last_break_at: Option<Instant>,
}

pub struct TouchController {
    drags: HashMap<u64, ActiveDrag>,
    pending_look_dx: f32,
    pending_look_dy: f32,
    jump_held: bool,
    crouch_held: bool,
}

impl TouchController {
    pub fn new() -> Self {
        Self {
            drags: HashMap::new(),
            pending_look_dx: 0.0,
            pending_look_dy: 0.0,
            jump_held: false,
            crouch_held: false,
        }
    }

    // --- Rectangulos de botones del juego ---

    pub(crate) fn rect_jump(size: PhysicalSize<u32>) -> (f64, f64, f64, f64) {
        let x = size.width as f64 - MARGIN - JUMP_BUTTON_SIZE;
        let y = size.height as f64 - MARGIN - JUMP_BUTTON_SIZE;
        (x, y, JUMP_BUTTON_SIZE, JUMP_BUTTON_SIZE)
    }

    /// Botón de agachar/bajar: a la izquierda del de salto, más chico y
    /// alineado por abajo con él (mismo borde inferior).
    pub(crate) fn rect_crouch(size: PhysicalSize<u32>) -> (f64, f64, f64, f64) {
        let jump = Self::rect_jump(size);
        let x = jump.0 - ACTION_BUTTON_GAP - CROUCH_BUTTON_SIZE;
        let y = jump.1 + jump.3 - CROUCH_BUTTON_SIZE;
        (x, y, CROUCH_BUTTON_SIZE, CROUCH_BUTTON_SIZE)
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

    // --- Menú de pausa (Playing -> Pause): 3 botones grandes apilados ---

    /// Uno de los 3 botones del menú de pausa: 0 = "MODO DE JUEGO",
    /// 1 = "AJUSTES", 2 = "SALIR" (mismo orden en `build_pause_screen`
    /// y en `on_touch_pause`).
    pub(crate) fn rect_pause_button(size: PhysicalSize<u32>, index: usize) -> (f64, f64, f64, f64) {
        let cx = size.width as f64 * 0.5;
        let btn_w = 420.0_f64.min(size.width as f64 * 0.8);
        let btn_h = 68.0;
        let gap = 22.0;
        let total_h = btn_h * 3.0 + gap * 2.0;
        let start_y = size.height as f64 * 0.5 - total_h * 0.5;
        let y = start_y + index as f64 * (btn_h + gap);
        (cx - btn_w * 0.5, y, btn_w, btn_h)
    }

    // --- Selector de modo de juego (Pause -> GameMode) ---

    /// Fila base del selector de modo de juego en su propia pantalla
    /// (ya no comparte panel con el resto de ajustes).
    pub(crate) fn rect_gamemode_row(size: PhysicalSize<u32>) -> (f64, f64, f64, f64) {
        let cx = size.width as f64 * 0.5;
        let row_w = 480.0_f64.min(size.width as f64 * 0.8);
        let row_h = 68.0;
        let row_y = size.height as f64 * 0.46;
        (cx - row_w * 0.5, row_y, row_w, row_h)
    }

    /// Sub-rectángulo de una de las 3 opciones del selector de modo.
    /// `index` es 0=Supervivencia, 1=Creativo, 2=Espectador — mismo
    /// orden en el que se dibujan (ver `build_gamemode_screen`) y en el
    /// que `on_touch_gamemode` hace el hit-test.
    pub(crate) fn rect_mode_option(size: PhysicalSize<u32>, index: usize) -> (f64, f64, f64, f64) {
        let (rx, ry, rw, rh) = Self::rect_gamemode_row(size);
        let gap = 8.0;
        let option_w = (rw - gap * 2.0) / 3.0;
        let x = rx + index as f64 * (option_w + gap);
        (x, ry, option_w, rh)
    }

    // --- Pantalla de ajustes principales (Pause -> Settings) ---
    // FPS, radio de chunks, nubes, niebla — los que se tocan más
    // seguido — más el botón "AJUSTES ADICIONALES" al final. El resto
    // (info de build, panel de debug) vive en su propia pantalla, ver
    // más abajo.

    /// Fila del toggle FPS en la pantalla de ajustes.
    pub(crate) fn rect_settings_fps_row(size: PhysicalSize<u32>) -> (f64, f64, f64, f64) {
        let cx = size.width as f64 * 0.5;
        let row_w = 480.0_f64.min(size.width as f64 * 0.8);
        let row_x = cx - row_w * 0.5;
        let row_y = size.height as f64 * 0.27;
        (row_x, row_y, row_w, 52.0)
    }

    /// Fila del stepper de distancia de chunks, justo debajo de FPS.
    pub(crate) fn rect_settings_render_distance_row(size: PhysicalSize<u32>) -> (f64, f64, f64, f64) {
        let (x, y, w, h) = Self::rect_settings_fps_row(size);
        (x, y + h + 14.0, w, h)
    }

    /// Fila del toggle de nubes, justo debajo de la de distancia de chunks.
    pub(crate) fn rect_settings_clouds_row(size: PhysicalSize<u32>) -> (f64, f64, f64, f64) {
        let (x, y, w, h) = Self::rect_settings_render_distance_row(size);
        (x, y + h + 14.0, w, h)
    }

    /// Fila del toggle de niebla, justo debajo de la de nubes.
    pub(crate) fn rect_settings_fog_row(size: PhysicalSize<u32>) -> (f64, f64, f64, f64) {
        let (x, y, w, h) = Self::rect_settings_clouds_row(size);
        (x, y + h + 14.0, w, h)
    }

    /// Botón "AJUSTES ADICIONALES", al final de la pantalla de ajustes
    /// principales — lleva a `GameScreen::SettingsMore`.
    pub(crate) fn rect_settings_more_button(size: PhysicalSize<u32>) -> (f64, f64, f64, f64) {
        let (x, y, w, h) = Self::rect_settings_fog_row(size);
        (x, y + h + 28.0, w, 56.0)
    }

    // --- Pantalla de ajustes adicionales (Settings -> SettingsMore) ---

    /// Fila del toggle de info de build, primera de esta pantalla.
    pub(crate) fn rect_settings_build_info_row(size: PhysicalSize<u32>) -> (f64, f64, f64, f64) {
        Self::rect_settings_fps_row(size)
    }

    /// Fila del toggle del panel de debug (F3), justo debajo de la de
    /// info de build.
    pub(crate) fn rect_settings_debug_panel_row(size: PhysicalSize<u32>) -> (f64, f64, f64, f64) {
        let (x, y, w, h) = Self::rect_settings_build_info_row(size);
        (x, y + h + 14.0, w, h)
    }

    /// Botón [-] del stepper de distancia de chunks: pegado al borde
    /// derecho de la fila, con el [+] al lado (ver `rect_stepper_plus`).
    pub(crate) fn rect_stepper_minus(row: (f64, f64, f64, f64)) -> (f64, f64, f64, f64) {
        let (rx, ry, rw, rh) = row;
        let w = 48.0;
        let h = 48.0;
        let x = rx + rw - w * 2.0 - 12.0;
        let y = ry + (rh - h) * 0.5;
        (x, y, w, h)
    }

    /// Botón [+] del stepper de distancia de chunks.
    pub(crate) fn rect_stepper_plus(row: (f64, f64, f64, f64)) -> (f64, f64, f64, f64) {
        let (rx, ry, rw, rh) = row;
        let w = 48.0;
        let h = 48.0;
        let x = rx + rw - w;
        let y = ry + (rh - h) * 0.5;
        (x, y, w, h)
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
        if Self::point_in_rect(pos, Self::rect_crouch(size)) {
            return Some(Zone::Crouch);
        }
        if pos.0 < size.width as f64 * 0.5 {
            Some(Zone::Movement)
        } else {
            Some(Zone::Look)
        }
    }

    /// Procesa un evento táctil durante el juego (pantalla de juego activa).
    pub fn on_touch_game(
        &mut self,
        touch: Touch,
        size: PhysicalSize<u32>,
        debug_panel_copy_rect: Option<(f64, f64, f64, f64)>,
    ) -> Option<TouchAction> {
        let pos = (touch.location.x, touch.location.y);

        match touch.phase {
            TouchPhase::Started => {
                if let Some(rect) = debug_panel_copy_rect {
                    if Self::point_in_rect(pos, rect) {
                        return Some(TouchAction::CopyDebugSnapshot);
                    }
                }
                if Self::point_in_rect(pos, Self::rect_settings(size)) {
                    // Soltar drags activos para no dejar el joystick pegado.
                    self.drags.clear();
                    self.jump_held = false;
                    self.crouch_held = false;
                    return Some(TouchAction::OpenPause);
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
                    if zone == Zone::Crouch {
                        self.crouch_held = true;
                    }
                    self.drags.insert(
                        touch.id,
                        ActiveDrag {
                            zone,
                            start: pos,
                            last: pos,
                            started_at: Instant::now(),
                            last_break_at: None,
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
                    if drag.zone == Zone::Crouch {
                        self.crouch_held = false;
                    }
                    if drag.zone == Zone::Look && touch.phase == TouchPhase::Ended {
                        let dx = drag.last.0 - drag.start.0;
                        let dy = drag.last.1 - drag.start.1;
                        let moved = (dx * dx + dy * dy).sqrt();
                        let held_ms = drag.started_at.elapsed().as_millis();
                        // Toque corto y sin arrastrar apenas: era un
                        // "tap" → colocar. Si ya se mantuvo presionado lo
                        // suficiente como para haber roto bloques (ver
                        // `poll_hold_break`), soltar no hace nada más.
                        if held_ms <= TAP_MAX_MS && moved <= TAP_MAX_MOVE {
                            return Some(TouchAction::Place);
                        }
                    }
                }
                None
            }
        }
    }

    /// Se llama una vez por frame (no depende de eventos táctiles nuevos)
    /// para sostener la rotura mientras el jugador mantiene el dedo
    /// apoyado en la zona de mirar sin soltarlo, igual que el long-press
    /// de Minecraft. Devuelve `true` si toca romper el bloque apuntado
    /// en este frame.
    pub fn poll_hold_break(&mut self) -> bool {
        let mut should_break = false;
        for drag in self.drags.values_mut() {
            if drag.zone != Zone::Look {
                continue;
            }
            if drag.started_at.elapsed().as_millis() < TAP_MAX_MS {
                continue;
            }
            let ready = match drag.last_break_at {
                None => true,
                Some(t) => t.elapsed().as_millis() >= HOLD_BREAK_REPEAT_MS,
            };
            if ready {
                drag.last_break_at = Some(Instant::now());
                should_break = true;
            }
        }
        should_break
    }

    /// Procesa un evento táctil durante el menú de pausa (Playing ->
    /// Pause): sus 3 botones ("MODO DE JUEGO", "AJUSTES", "SALIR") y
    /// "< VOLVER" para reanudar el juego directamente.
    pub fn on_touch_pause(&self, touch: Touch, size: PhysicalSize<u32>) -> Option<TouchAction> {
        if touch.phase != TouchPhase::Started {
            return None;
        }
        let pos = (touch.location.x, touch.location.y);

        if Self::point_in_rect(pos, Self::rect_back_button(size)) {
            return Some(TouchAction::Back);
        }
        if Self::point_in_rect(pos, Self::rect_pause_button(size, 0)) {
            return Some(TouchAction::OpenGameModeScreen);
        }
        if Self::point_in_rect(pos, Self::rect_pause_button(size, 1)) {
            return Some(TouchAction::OpenSettingsScreen);
        }
        if Self::point_in_rect(pos, Self::rect_pause_button(size, 2)) {
            return Some(TouchAction::ExitGame);
        }
        None
    }

    /// Procesa un evento táctil durante el selector de modo de juego
    /// (Pause -> GameMode).
    pub fn on_touch_gamemode(&self, touch: Touch, size: PhysicalSize<u32>) -> Option<TouchAction> {
        if touch.phase != TouchPhase::Started {
            return None;
        }
        let pos = (touch.location.x, touch.location.y);

        if Self::point_in_rect(pos, Self::rect_back_button(size)) {
            return Some(TouchAction::Back);
        }
        for i in 0..3 {
            if Self::point_in_rect(pos, Self::rect_mode_option(size, i)) {
                return Some(TouchAction::SetGameMode(i));
            }
        }
        None
    }

    /// Procesa un evento táctil durante la pantalla de ajustes
    /// principales (Pause -> Settings): FPS, radio de chunks, nubes,
    /// niebla, y el botón "AJUSTES ADICIONALES".
    pub fn on_touch_settings(
        &self,
        touch: Touch,
        size: PhysicalSize<u32>,
        show_fps: bool,
        show_clouds: bool,
        show_fog: bool,
    ) -> Option<TouchAction> {
        if touch.phase != TouchPhase::Started {
            return None;
        }
        let pos = (touch.location.x, touch.location.y);

        if Self::point_in_rect(pos, Self::rect_back_button(size)) {
            return Some(TouchAction::Back);
        }

        let fps_row = Self::rect_settings_fps_row(size);
        if Self::point_in_rect(pos, fps_row) {
            return Some(TouchAction::ToggleFps);
        }

        let render_distance_row = Self::rect_settings_render_distance_row(size);
        if Self::point_in_rect(pos, Self::rect_stepper_minus(render_distance_row)) {
            return Some(TouchAction::DecreaseRenderRadius);
        }
        if Self::point_in_rect(pos, Self::rect_stepper_plus(render_distance_row)) {
            return Some(TouchAction::IncreaseRenderRadius);
        }

        let clouds_row = Self::rect_settings_clouds_row(size);
        if Self::point_in_rect(pos, clouds_row) {
            return Some(TouchAction::ToggleClouds);
        }

        let fog_row = Self::rect_settings_fog_row(size);
        if Self::point_in_rect(pos, fog_row) {
            return Some(TouchAction::ToggleFog);
        }

        if Self::point_in_rect(pos, Self::rect_settings_more_button(size)) {
            return Some(TouchAction::OpenSettingsMore);
        }

        // `show_fps`/`show_clouds`/`show_fog` no hacen falta para el
        // hit-testing en sí (las filas están en posiciones fijas sin
        // importar el estado actual de cada control) — se reciben acá
        // solo para mantener la firma simétrica con
        // `build_settings_screen` en ui_overlay.rs, que sí los necesita
        // para dibujar el estado actual de cada control.
        let _ = (show_fps, show_clouds, show_fog);

        None
    }

    /// Procesa un evento táctil durante la pantalla de ajustes
    /// adicionales (Settings -> SettingsMore): info de build y panel de
    /// debug.
    pub fn on_touch_settings_more(
        &self,
        touch: Touch,
        size: PhysicalSize<u32>,
        show_build_info: bool,
        show_debug_panel: bool,
    ) -> Option<TouchAction> {
        if touch.phase != TouchPhase::Started {
            return None;
        }
        let pos = (touch.location.x, touch.location.y);

        if Self::point_in_rect(pos, Self::rect_back_button(size)) {
            return Some(TouchAction::Back);
        }

        let build_info_row = Self::rect_settings_build_info_row(size);
        if Self::point_in_rect(pos, build_info_row) {
            return Some(TouchAction::ToggleBuildInfo);
        }

        let debug_panel_row = Self::rect_settings_debug_panel_row(size);
        if Self::point_in_rect(pos, debug_panel_row) {
            return Some(TouchAction::ToggleDebugPanel);
        }

        let _ = (show_build_info, show_debug_panel);

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

    pub fn crouch_held(&self) -> bool {
        self.crouch_held
    }
}

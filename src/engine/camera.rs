/// camera.rs
/// Cámara de vuelo libre (fly camera) en primera persona: WASD para mover,
/// mouse para rotar. Fase 1 no tiene colisión con el terreno todavía —
/// eso se suma en la Fase 1.1 junto con gravedad, una vez que el mundo
/// navegable esté validado en tu máquina.

use glam::{Mat4, Vec3};
use winit::event::ElementState;
use winit::keyboard::KeyCode;

/// Se deriva `Clone` para poder tomar una copia de la cámara al armar
/// `SavedSession` (ver lib.rs) cuando Android destruye la superficie en
/// segundo plano — así `resumed()` la restaura tal cual estaba, sin
/// resetear posición/rotación.
#[derive(Clone)]
pub struct Camera {
    pub position: Vec3,
    pub yaw: f32,   // rotación horizontal, en radianes
    pub pitch: f32, // rotación vertical, en radianes

    pub move_speed: f32,
    pub sensitivity: f32,

    forward: bool,
    backward: bool,
    left: bool,
    right: bool,
    up: bool,
    down: bool,

    // Entrada analógica del joystick táctil (Android): strafe en x,
    // adelante/atrás en y, cada uno en [-1, 1]. Se combina con las teclas
    // WASD (que en desktop siguen mandando 0.0 acá) sumando ambos
    // vectores y luego clampeando la magnitud a 1, así que un jugador con
    // teclado+mouse conectado a una tablet Android, por ejemplo, puede
    // usar ambos a la vez sin que se pisen.
    touch_axis: (f32, f32),
    touch_jump: bool,
    /// Botón táctil de agachar/bajar (segundo botón de acción, aparte de
    /// salto). Igual que `touch_jump` con `up`, se combina por OR con la
    /// tecla de escritorio (`down`, ShiftLeft) — ver `set_touch_down`.
    touch_down: bool,
}

impl Camera {
    pub fn new(position: Vec3) -> Self {
        Self {
            position,
            yaw: -90f32.to_radians(),
            pitch: 0.0,
            move_speed: 12.0,
            sensitivity: 0.0025,
            forward: false,
            backward: false,
            left: false,
            right: false,
            up: false,
            down: false,
            touch_axis: (0.0, 0.0),
            touch_jump: false,
            touch_down: false,
        }
    }

    /// Actualiza el vector analógico de movimiento del joystick táctil.
    /// `axis.0` es strafe (+derecha), `axis.1` es adelante/atrás (+adelante).
    pub fn set_touch_move_axis(&mut self, axis: (f32, f32)) {
        self.touch_axis = axis;
    }

    /// Mantiene o suelta el botón táctil de salto/subir.
    pub fn set_touch_jump(&mut self, held: bool) {
        self.touch_jump = held;
    }

    /// Mantiene o suelta el segundo botón táctil (agachar en Supervivencia,
    /// bajar en Creativo/Espectador — ver `wants_crouch` y
    /// `free_move_vector`/`update`).
    pub fn set_touch_down(&mut self, held: bool) {
        self.touch_down = held;
    }

    /// Aplica un delta de "mirada" venido de un drag táctil, con la misma
    /// semántica que `process_mouse` (mismo signo, misma sensibilidad).
    pub fn process_touch_look(&mut self, dx: f32, dy: f32) {
        self.process_mouse(dx as f64, dy as f64);
    }

    pub fn process_key(&mut self, key: KeyCode, state: ElementState) {
        let pressed = state == ElementState::Pressed;
        match key {
            KeyCode::KeyW => self.forward = pressed,
            KeyCode::KeyS => self.backward = pressed,
            KeyCode::KeyA => self.left = pressed,
            KeyCode::KeyD => self.right = pressed,
            KeyCode::Space => self.up = pressed,
            KeyCode::ShiftLeft => self.down = pressed,
            _ => {}
        }
    }

    pub fn process_mouse(&mut self, dx: f64, dy: f64) {
        self.yaw += dx as f32 * self.sensitivity;
        self.pitch -= dy as f32 * self.sensitivity;
        self.pitch = self.pitch.clamp(-89f32.to_radians(), 89f32.to_radians());
    }

    fn front(&self) -> Vec3 {
        Vec3::new(
            self.yaw.cos() * self.pitch.cos(),
            self.pitch.sin(),
            self.yaw.sin() * self.pitch.cos(),
        )
        .normalize()
    }

    /// Dirección hacia donde mira la cámara, usada para el raycasting de
    /// romper/colocar bloques.
    pub fn view_direction(&self) -> Vec3 {
        self.front()
    }

    /// Vector de movimiento horizontal (sin componente Y) según las teclas
    /// WASD activas, relativo a hacia dónde mira la cámara. Usado por el
    /// modo caminar (con gravedad/colisión), a diferencia de `update()`
    /// que es el movimiento libre en 3D del modo vuelo.
    pub fn horizontal_move_vector(&self, speed: f32) -> Vec3 {
        let flat_front = Vec3::new(self.yaw.cos(), 0.0, self.yaw.sin()).normalize();
        let right = flat_front.cross(Vec3::Y).normalize();

        let mut dir = Vec3::ZERO;
        if self.forward {
            dir += flat_front;
        }
        if self.backward {
            dir -= flat_front;
        }
        if self.right {
            dir += right;
        }
        if self.left {
            dir -= right;
        }
        dir += flat_front * self.touch_axis.1 + right * self.touch_axis.0;

        if dir.length_squared() > 1.0 {
            dir = dir.normalize();
        }

        if dir.length_squared() > 0.0 {
            dir * speed
        } else {
            Vec3::ZERO
        }
    }

    /// En modo caminar, la tecla Espacio (mapeada como "up" para el modo
    /// vuelo) o el botón táctil de salto se reinterpretan como salto.
    pub fn wants_jump(&self) -> bool {
        self.up || self.touch_jump
    }

    /// En modo Supervivencia, el segundo botón táctil (o Shift izquierdo
    /// en escritorio — mismo campo `down` que en modo vuelo significa
    /// "bajar") se reinterpreta como agacharse: `State::update` (lib.rs)
    /// usa esto para reducir la velocidad de movimiento y activar la
    /// protección de borde (`Player::update`/`is_grounded_at`), igual
    /// que `wants_jump` reinterpreta "subir" como salto.
    pub fn wants_crouch(&self) -> bool {
        self.down || self.touch_down
    }

    /// Vector de movimiento libre en 3D (horizontal + vertical) según las
    /// teclas activas, para el modo Creativo (vuelo CON colisión, ver
    /// `Player::fly_update`). A diferencia de `update()` (vuelo de
    /// Espectador, sin colisión, que mueve `self.position` directo) esta
    /// función solo devuelve la dirección/velocidad deseada; quien la
    /// llama es responsable de resolver colisiones con ella.
    pub fn free_move_vector(&self, speed: f32) -> Vec3 {
        let mut dir = self.horizontal_move_vector(1.0);
        if self.up || self.touch_jump {
            dir += Vec3::Y;
        }
        if self.down || self.touch_down {
            dir -= Vec3::Y;
        }
        if dir.length_squared() > 1.0 {
            dir = dir.normalize();
        }
        if dir.length_squared() > 0.0 {
            dir * speed
        } else {
            Vec3::ZERO
        }
    }

    pub fn update(&mut self, dt: f32) {
        let front = self.front();
        let right = front.cross(Vec3::Y).normalize();
        let world_up = Vec3::Y;

        let speed = self.move_speed * dt;

        if self.forward {
            self.position += front * speed;
        }
        if self.backward {
            self.position -= front * speed;
        }
        if self.right {
            self.position += right * speed;
        }
        if self.left {
            self.position -= right * speed;
        }
        if self.up || self.touch_jump {
            self.position += world_up * speed;
        }
        if self.down || self.touch_down {
            self.position -= world_up * speed;
        }

        // Joystick táctil: mismo tratamiento que WASD pero analógico
        // (magnitud variable en vez de todo-o-nada).
        self.position += front * self.touch_axis.1 * speed + right * self.touch_axis.0 * speed;
    }

    pub fn view_matrix(&self) -> Mat4 {
        let front = self.front();
        Mat4::look_at_rh(self.position, self.position + front, Vec3::Y)
    }
}

pub fn projection_matrix(aspect: f32) -> Mat4 {
    Mat4::perspective_rh(70f32.to_radians(), aspect, 0.1, 500.0)
}

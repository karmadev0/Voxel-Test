/// camera.rs
/// Cámara de vuelo libre (fly camera) en primera persona: WASD para mover,
/// mouse para rotar. Fase 1 no tiene colisión con el terreno todavía —
/// eso se suma en la Fase 1.1 junto con gravedad, una vez que el mundo
/// navegable esté validado en tu máquina.

use glam::{Mat4, Vec3};
use winit::event::ElementState;
use winit::keyboard::KeyCode;

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
        }
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
        if self.up {
            self.position += world_up * speed;
        }
        if self.down {
            self.position -= world_up * speed;
        }
    }

    pub fn view_matrix(&self) -> Mat4 {
        let front = self.front();
        Mat4::look_at_rh(self.position, self.position + front, Vec3::Y)
    }
}

pub fn projection_matrix(aspect: f32) -> Mat4 {
    Mat4::perspective_rh(70f32.to_radians(), aspect, 0.1, 500.0)
}

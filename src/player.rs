/// player.rs
/// Física simple del jugador: gravedad + colisión AABB contra el mundo.
/// Reemplaza el modo "vuelo libre" de la cámara cuando el modo caminar
/// está activo (tecla F para togglear, ver main.rs).
///
/// La colisión se resuelve eje por eje (mover en X, corregir si choca;
/// mover en Y, corregir; mover en Z, corregir). Es más simple que un
/// solver continuo, pero para velocidades moderadas (caminar/caer) es
/// suficientemente robusto y barato en CPU — importante en el Celeron N4000.

use crate::world::World;
use glam::Vec3;

const GRAVITY: f32 = -22.0;
const JUMP_VELOCITY: f32 = 8.0;
const TERMINAL_VELOCITY: f32 = -50.0;

// Medio-ancho y alto del jugador (caja de colisión centrada en X/Z,
// desde los pies hasta la cabeza en Y).
const HALF_WIDTH: f32 = 0.3;
const HEIGHT: f32 = 1.8;
const EYE_HEIGHT: f32 = 1.6;

pub struct Player {
    /// Posición de los PIES del jugador (no de la cámara/ojos).
    pub feet_position: Vec3,
    pub velocity: Vec3,
    pub on_ground: bool,
}

impl Player {
    pub fn new(feet_position: Vec3) -> Self {
        Self {
            feet_position,
            velocity: Vec3::ZERO,
            on_ground: false,
        }
    }

    pub fn eye_position(&self) -> Vec3 {
        self.feet_position + Vec3::new(0.0, EYE_HEIGHT, 0.0)
    }

    pub fn jump(&mut self) {
        if self.on_ground {
            self.velocity.y = JUMP_VELOCITY;
            self.on_ground = false;
        }
    }

    /// Avanza la física un paso: aplica gravedad, mueve por
    /// `horizontal_move` (dirección ya calculada en main.rs a partir del
    /// input WASD relativo a la cámara) y resuelve colisiones contra el
    /// mundo, eje por eje.
    pub fn update(&mut self, world: &World, horizontal_move: Vec3, dt: f32) {
        self.velocity.x = horizontal_move.x;
        self.velocity.z = horizontal_move.z;

        self.velocity.y = (self.velocity.y + GRAVITY * dt).max(TERMINAL_VELOCITY);

        let delta = self.velocity * dt;

        self.move_axis(world, Vec3::new(delta.x, 0.0, 0.0));
        self.on_ground = false;
        self.move_axis(world, Vec3::new(0.0, delta.y, 0.0));
        self.move_axis(world, Vec3::new(0.0, 0.0, delta.z));
    }

    fn move_axis(&mut self, world: &World, delta: Vec3) {
        let mut new_pos = self.feet_position + delta;

        if self.collides(world, new_pos) {
            // Si colisiona, no nos movemos en ese eje y frenamos la
            // velocidad correspondiente. Si el eje era Y hacia abajo,
            // significa que tocamos el suelo.
            if delta.y < 0.0 {
                self.on_ground = true;
            }
            if delta.y != 0.0 {
                self.velocity.y = 0.0;
            }
            new_pos = self.feet_position;
        }

        self.feet_position = new_pos;
    }

    /// Chequea si la caja de colisión del jugador, parada en `pos`, se
    /// superpone con algún bloque sólido del mundo.
    fn collides(&self, world: &World, pos: Vec3) -> bool {
        let min_x = (pos.x - HALF_WIDTH).floor() as i32;
        let max_x = (pos.x + HALF_WIDTH).floor() as i32;
        let min_y = pos.y.floor() as i32;
        let max_y = (pos.y + HEIGHT).floor() as i32;
        let min_z = (pos.z - HALF_WIDTH).floor() as i32;
        let max_z = (pos.z + HALF_WIDTH).floor() as i32;

        for x in min_x..=max_x {
            for y in min_y..=max_y {
                for z in min_z..=max_z {
                    if world.get_block(x, y, z).is_solid() {
                        return true;
                    }
                }
            }
        }
        false
    }
}

/// player.rs
/// Física simple del jugador: gravedad + colisión AABB contra el mundo.
/// Reemplaza el modo "vuelo libre" de la cámara cuando el modo caminar
/// está activo (tecla F para togglear, ver main.rs).
///
/// La colisión se resuelve eje por eje (mover en X, corregir si choca;
/// mover en Y, corregir; mover en Z, corregir). Es más simple que un
/// solver continuo, pero para velocidades moderadas (caminar/caer) es
/// suficientemente robusto y barato en CPU — importante en el Celeron N4000.

use crate::environment::world::World;
use glam::Vec3;

const GRAVITY: f32 = -22.0;
const JUMP_VELOCITY: f32 = 8.0;
const TERMINAL_VELOCITY: f32 = -50.0;

// Medio-ancho y alto del jugador (caja de colisión centrada en X/Z,
// desde los pies hasta la cabeza en Y).
const HALF_WIDTH: f32 = 0.3;
const HEIGHT: f32 = 1.8;
const EYE_HEIGHT: f32 = 1.6;

/// `Clone` para `SavedSession` (ver lib.rs), mismo motivo que en `Camera`.
#[derive(Clone)]
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

    /// Inverso de `eye_position`: a partir de una posición de cámara/ojos
    /// (por ejemplo `Camera::position` en modo Espectador, que no tiene
    /// noción de "pies"), calcula dónde quedarían los pies del jugador si
    /// se parara ahí. Usa la misma constante `EYE_HEIGHT` que
    /// `eye_position`, para que ambas conversiones queden siempre
    /// sincronizadas — antes de este método, `lib.rs` reconstruía la
    /// resta a mano con el número `1.6` repetido, así que si `EYE_HEIGHT`
    /// cambiaba acá, ese otro lugar quedaba desincronizado en silencio.
    pub fn feet_from_eye_position(eye: Vec3) -> Vec3 {
        eye - Vec3::new(0.0, EYE_HEIGHT, 0.0)
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
    /// mundo, eje por eje. Usado en modo Supervivencia (caminar).
    ///
    /// `crouching` activa la protección de borde de Minecraft: si el
    /// jugador está agachado y parado en el suelo, un eje horizontal que
    /// lo dejaría sin piso debajo se cancela en vez de dejarlo caminar
    /// hacia el vacío (ver `is_grounded_at`, más abajo).
    pub fn update(&mut self, world: &World, horizontal_move: Vec3, dt: f32, crouching: bool) {
        self.velocity.x = horizontal_move.x;
        self.velocity.z = horizontal_move.z;

        self.velocity.y = (self.velocity.y + GRAVITY * dt).max(TERMINAL_VELOCITY);

        let delta = self.velocity * dt;
        let mut delta_x = delta.x;
        let mut delta_z = delta.z;

        // Protección de borde: solo tiene sentido si ya estamos parados
        // en el suelo (si veníamos cayendo o saltando, dejar que la
        // gravedad siga su curso normal — no hay "borde" del que
        // protegerse en el aire). Cada eje se chequea por separado y
        // contra la posición actual (no la ya desplazada por el otro
        // eje), igual de simple que el resto de la resolución de
        // colisiones acá — no cubre perfectamente el caso diagonal, pero
        // alcanza para no caerse caminando en línea recta hacia un borde.
        if crouching && self.on_ground {
            if delta_x != 0.0 {
                let candidate = self.feet_position + Vec3::new(delta_x, 0.0, 0.0);
                if !self.is_grounded_at(world, candidate) {
                    delta_x = 0.0;
                }
            }
            if delta_z != 0.0 {
                let candidate = self.feet_position + Vec3::new(0.0, 0.0, delta_z);
                if !self.is_grounded_at(world, candidate) {
                    delta_z = 0.0;
                }
            }
        }

        self.move_axis(world, Vec3::new(delta_x, 0.0, 0.0));
        self.on_ground = false;
        self.move_axis(world, Vec3::new(0.0, delta.y, 0.0));
        self.move_axis(world, Vec3::new(0.0, 0.0, delta_z));
    }

    /// Vuelo con colisión: modo Creativo. A diferencia de `update()`, no
    /// hay gravedad ni salto — `free_move` (de
    /// `Camera::free_move_vector`) ya trae la componente vertical
    /// deseada directamente (subir/bajar con Espacio/Shift). Se sigue
    /// resolviendo eje por eje contra el mundo con `move_axis`, así que
    /// el jugador no atraviesa bloques mientras vuela — a diferencia del
    /// modo Espectador, que no pasa por `Player` en absoluto (ver
    /// `Camera::update` en lib.rs, usado solo para Espectador).
    pub fn fly_update(&mut self, world: &World, free_move: Vec3, dt: f32) {
        self.velocity = free_move;
        self.on_ground = false;

        let delta = self.velocity * dt;

        self.move_axis(world, Vec3::new(delta.x, 0.0, 0.0));
        self.move_axis(world, Vec3::new(0.0, delta.y, 0.0));
        self.move_axis(world, Vec3::new(0.0, 0.0, delta.z));
    }

    fn move_axis(&mut self, world: &World, delta: Vec3) {
        let mut new_pos = self.feet_position + delta;

        if self.collides_at(world, new_pos) {
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
    /// superpone con algún bloque sólido del mundo. Pública porque el
    /// auto-rescate de Creativo (ver `find_nearest_free_position` acá
    /// mismo, y `State::maybe_rescue_player` en lib.rs) necesita probar
    /// muchas posiciones candidatas sin mover al jugador real.
    pub fn collides_at(&self, world: &World, pos: Vec3) -> bool {
        let min_x = (pos.x - HALF_WIDTH).floor() as i32;
        let max_x = (pos.x + HALF_WIDTH).floor() as i32;
        let min_y = pos.y.floor() as i32;
        let max_y = (pos.y + HEIGHT).floor() as i32;
        let min_z = (pos.z - HALF_WIDTH).floor() as i32;
        let max_z = (pos.z + HALF_WIDTH).floor() as i32;

        for x in min_x..=max_x {
            for y in min_y..=max_y {
                for z in min_z..=max_z {
                    if world.get_block(x, y, z).is_collidable() {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Si parado en `pos`, hay algún bloque sólido justo debajo de la
    /// huella en XZ del jugador (la capa de bloques a `floor(pos.y) - 1`,
    /// bajo toda su caja de colisión en X/Z). Usado por `update` para la
    /// protección de borde al agacharse: a diferencia de `collides_at`
    /// (¿choco acá?), esto responde "¿me sostiene el piso acá?" — basta
    /// con que una sola celda de la huella tenga piso para considerar la
    /// posición sostenida, así que el jugador puede seguir parado con
    /// medio pie sobre el borde de un bloque sin que esto lo frene de
    /// más, igual que en Minecraft.
    fn is_grounded_at(&self, world: &World, pos: Vec3) -> bool {
        let min_x = (pos.x - HALF_WIDTH).floor() as i32;
        let max_x = (pos.x + HALF_WIDTH).floor() as i32;
        let min_z = (pos.z - HALF_WIDTH).floor() as i32;
        let max_z = (pos.z + HALF_WIDTH).floor() as i32;
        let y = pos.y.floor() as i32 - 1;

        for x in min_x..=max_x {
            for z in min_z..=max_z {
                if world.get_block(x, y, z).is_collidable() {
                    return true;
                }
            }
        }
        false
    }

    /// Si el jugador está "atrapado" ahora mismo: su caja de colisión,
    /// parada exactamente donde está, ya se superpone con un sólido. Es
    /// el caso que dispara el auto-rescate en modo Creativo (ver
    /// `State::maybe_rescue_player` en lib.rs) — por ejemplo si quedó
    /// encerrado por streaming de chunks, por un bug, o al cargar un
    /// guardado viejo de antes de que existiera el bloqueo de
    /// autoconstrucción en Creativo (ver `handle_click`).
    pub fn is_trapped(&self, world: &World) -> bool {
        self.collides_at(world, self.feet_position)
    }

    /// Busca la celda de aire libre más cercana a `feet_position` donde
    /// el jugador pueda pararse sin colisionar — búsqueda en capas por
    /// distancia Chebyshev (primero radio 1, después 2, etc.) hasta
    /// `max_radius`. Dentro de cada capa, se prefiere una celda que
    /// tenga un bloque sólido justo debajo (parado en "suelo firme", no
    /// flotando en el aire) — si ninguna celda de esa capa tiene piso
    /// debajo, se acepta la primera celda libre igual, total la
    /// gravedad normal del jugador ya se encarga de hacerlo caer hasta
    /// el siguiente sólido, pero preferir un piso evita el caso más
    /// llamativo de "me rescataron flotando en medio de la nada".
    ///
    /// No es necesariamente la distancia euclidiana más corta (podría
    /// haber una celda en diagonal más cerca en línea recta que una
    /// "ortogonal" de la misma capa), pero es suficientemente buena para
    /// un rescate de emergencia y mucho más simple/barata que un BFS
    /// real con cola — importante porque esto corre en el hilo
    /// principal, en medio del loop de juego.
    ///
    /// Devuelve `None` si no encontró nada suelto en `max_radius`
    /// bloques (por ejemplo, enterrado en el centro de una montaña
    /// sólida enorme) — en ese caso `State::maybe_rescue_player` decide
    /// qué hacer (hoy: nada, y queda registrado en el log).
    pub fn find_nearest_free_position(&self, world: &World, max_radius: i32) -> Option<Vec3> {
        let origin = self.feet_position;
        let ox = origin.x.floor() as i32;
        let oy = origin.y.floor() as i32;
        let oz = origin.z.floor() as i32;

        for radius in 0..=max_radius {
            // Primera pasada de esta capa: solo candidatos con piso
            // sólido debajo. Si encontramos uno, listo — es el mejor
            // resultado posible a esta distancia.
            if let Some(pos) = self.find_free_in_shell(world, ox, oy, oz, radius, true) {
                return Some(pos);
            }
            // Segunda pasada: cualquier celda libre de esta capa, aunque
            // esté "flotando" sin piso debajo (por ejemplo, en medio de
            // una caverna grande). Solo se llega acá si la primera
            // pasada no encontró nada con piso a esta distancia.
            if let Some(pos) = self.find_free_in_shell(world, ox, oy, oz, radius, false) {
                return Some(pos);
            }
        }
        None
    }

    /// Recorre la "cáscara" (shell) del cubo de radio `radius` centrado
    /// en `(ox, oy, oz)` buscando una celda donde el jugador no
    /// colisione. Si `require_floor` es true, además exige que la celda
    /// justo debajo (`by - 1`) sea sólida — usado por
    /// `find_nearest_free_position` para preferir puntos con suelo antes
    /// de aceptar cualquier hueco de aire.
    fn find_free_in_shell(
        &self,
        world: &World,
        ox: i32,
        oy: i32,
        oz: i32,
        radius: i32,
        require_floor: bool,
    ) -> Option<Vec3> {
        for dx in -radius..=radius {
            for dy in -radius..=radius {
                for dz in -radius..=radius {
                    let on_shell = dx.abs() == radius || dy.abs() == radius || dz.abs() == radius;
                    if !on_shell {
                        continue;
                    }
                    let bx = ox + dx;
                    let by = oy + dy;
                    let bz = oz + dz;
                    // Probamos parado justo en el borde inferior de esa
                    // celda de bloque (coordenada entera = pies apoyados
                    // en el piso de esa celda), que es como
                    // `feet_position` se interpreta en el resto de la
                    // física del jugador.
                    let candidate = Vec3::new(bx as f32, by as f32, bz as f32);
                    if self.collides_at(world, candidate) {
                        continue;
                    }
                    if require_floor && !world.get_block(bx, by - 1, bz).is_collidable() {
                        continue;
                    }
                    return Some(candidate);
                }
            }
        }
        None
    }

    /// Si la celda de bloque `(bx, by, bz)` se solapa con la caja de
    /// colisión actual del jugador (parado donde está ahora, sin
    /// moverse). Se usa para impedir construir "encima" o "adentro" de
    /// uno mismo — el bug que esto arregla: antes se podía colocar un
    /// bloque justo en la celda donde estabas parado (por ejemplo
    /// mirando hacia abajo bajo tus pies), y como `collides_at()` ya te
    /// encuentra ahí después, quedabas atrapado sin poder moverte en
    /// ningún eje (ver `move_axis`, que al colisionar simplemente
    /// cancela el movimiento en vez de "empujarte" afuera).
    ///
    /// A diferencia de `collides_at`, esta función solo chequea una
    /// celda puntual contra la posición actual — pensada para el caso
    /// de uso de "¿puedo colocar acá?", no para probar posiciones
    /// candidatas del jugador.
    pub fn occupies_block(&self, bx: i32, by: i32, bz: i32) -> bool {
        let min_x = (self.feet_position.x - HALF_WIDTH).floor() as i32;
        let max_x = (self.feet_position.x + HALF_WIDTH).floor() as i32;
        let min_y = self.feet_position.y.floor() as i32;
        let max_y = (self.feet_position.y + HEIGHT).floor() as i32;
        let min_z = (self.feet_position.z - HALF_WIDTH).floor() as i32;
        let max_z = (self.feet_position.z + HALF_WIDTH).floor() as i32;

        (min_x..=max_x).contains(&bx) && (min_y..=max_y).contains(&by) && (min_z..=max_z).contains(&bz)
    }
}

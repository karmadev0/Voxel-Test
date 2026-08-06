/// lib.rs
/// Núcleo del engine, compartido entre el binario de escritorio (main.rs,
/// que solo llama a `run()`) y la entrada nativa de Android
/// (`android_main`, más abajo). Arma la ventana (winit), inicializa wgpu
/// (OpenGL en desktop por la GPU integrada del Celeron N4000, Vulkan en
/// Android), genera un área de chunks en paralelo con rayon, construye sus
/// mallas con greedy meshing, y corre el loop de render con una cámara de
/// vuelo libre. En Android, además de teclado/mouse, el loop reacciona a
/// eventos táctiles (ver `touch.rs`) para mover, mirar, romper/colocar y
/// elegir bloque de la hotbar.

mod engine;
mod environment;
mod logic;
mod physics;
mod platform;
mod textures;

use engine::camera::{projection_matrix, Camera};
use engine::highlight;
use engine::render_state::Uniforms;
use environment::chunk::{BlockType, Chunk, CHUNK_SIZE_X, CHUNK_SIZE_Z};
use environment::mesher::Vertex;
use environment::sky::{FOG_START_FRACTION, SKY_COLOR};
use environment::world::{raycast, World};
use logic::immersive;
use logic::touch::{TouchAction, TouchController};
use logic::ui_overlay;
use physics::player::Player;
use platform::crash;
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use wgpu::util::DeviceExt;
use winit::{
    application::ApplicationHandler,
    event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::PhysicalKey,
    window::{CursorGrabMode, Window, WindowId},
};

#[cfg(target_os = "android")]
use winit::platform::android::activity::AndroidApp;

/// Pantalla activa del juego. Controla qué se dibuja y si la lógica
/// del juego (física, cámara) se actualiza o se pausa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GameScreen {
    /// Juego corriendo normalmente.
    Playing,
    /// Configuración fullscreen: el juego queda pausado.
    Settings,
}

/// Lo que un hilo de fondo manda de vuelta al terminar de generar/cargar
/// un chunk y mallearlo: coordenadas, el chunk (para insertarlo en
/// `World`) y su malla ya calculada en CPU (`MeshData`) — lo único que
/// falta es subirla a la GPU, y eso se hace en el hilo principal porque
/// `wgpu::Device` se usa ahí.
type ChunkResult = ((i32, i32), environment::chunk::Chunk, environment::mesher::MeshData);

// Radio de chunks a generar alrededor del origen (bajo por defecto: en
// el Celeron N4000 preferimos ver menos mundo a buen framerate antes
// que un mundo enorme que trabe el frame). Ahora es ajustable en vivo
// desde la pantalla de configuración (ver `State::render_radius`); esta
// constante es solo el valor inicial.
const DEFAULT_RENDER_RADIUS: i32 = 4;

// Límites del slider de distancia de chunks en la pantalla de config.
// El mínimo (1) es el radio más chico posible: el jugador solo ve el
// chunk en el que está parado más el anillo que lo rodea. Con radios
// tan cortos la niebla (ver `fog_start`/`fog_end` en `update`) es la que
// evita que el borde del mundo cargado se vea como un corte abrupto.
// El máximo (128) ya son 257x257 = 66049 chunks si se llegara a cargar
// todo a la vez — en el hardware objetivo de este proyecto (Celeron
// N4000) eso no da un framerate jugable, así que aunque la UI lo
// permita, es responsabilidad de quien juega no dejarlo tan alto ahí.
const MIN_RENDER_RADIUS: i32 = 1;
const MAX_RENDER_RADIUS: i32 = 128;

/// Cuántos chunks recién llegados del hilo de fondo se convierten a
/// buffers de GPU por frame como máximo. Crear un `wgpu::Buffer` no es
/// gratis (aunque sea rápido comparado con generar+mallear el chunk), así
/// que ponerle un tope evita un frame largo si de golpe terminan muchos
/// chunks a la vez (por ejemplo al aparecer, o si el jugador corre rápido
/// en modo Vuelo y cruza varios chunks de un saque).
const MAX_FINALIZED_CHUNKS_PER_FRAME: usize = 2;

// Distancia máxima (en bloques) a la que se puede romper/colocar.
const REACH: f32 = 6.0;

struct ChunkMesh {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    num_indices: u32,
}

struct State {
    // Device/queue/surface, pipelines y buffers de wgpu (ver
    // engine/render_state.rs). Todo lo puramente gráfico vive ahí.
    render: engine::render_state::RenderState,

    // Instante en el que arrancó la app: `time.x` en los uniforms (ver
    // arriba) se calcula como el tiempo transcurrido desde acá, para
    // animar el desplazamiento de las nubes con el viento.
    start_time: Instant,

    chunk_meshes: HashMap<(i32, i32), ChunkMesh>,
    world: World,
    camera: Camera,
    player: Player,
    walk_mode: bool,
    selected_block: BlockType,

    mouse_captured: bool,
    last_frame: Instant,

    // Controles táctiles (Android). En desktop queda inicializado pero
    // sin uso, ya que ahí no llegan WindowEvent::Touch.
    touch: TouchController,

    // Streaming de chunks: recordamos en qué chunk está parado el jugador
    // para no recalcular qué cargar/descargar en cada frame — solo cuando
    // realmente cruza a un chunk distinto (o cambia `render_radius` desde
    // la pantalla de config, ver `last_streamed_render_radius`).
    current_player_chunk: (i32, i32),

    // Radio de chunks actual (ajustable en vivo, ver `MIN_RENDER_RADIUS`/
    // `MAX_RENDER_RADIUS` y la fila "Distancia de chunks" en la pantalla
    // de configuración). `last_streamed_render_radius` es el valor que
    // tenía la última vez que `update_chunk_streaming` cargó/descargó
    // chunks; cuando difiere de `render_radius`, forzamos un recálculo
    // aunque el jugador siga parado en el mismo chunk.
    render_radius: i32,
    last_streamed_render_radius: i32,

    // --- Streaming asincrónico de chunks ---
    // `chunk_loader` es el handle liviano que se clona y se manda a
    // `rayon::spawn` para generar/cargar un chunk en un hilo de fondo.
    // `pending_chunks` evita pedir dos veces el mismo chunk si todavía no
    // volvió del hilo de fondo. El resultado (chunk + su malla ya
    // calculada, todo trabajo de CPU) vuelve por `chunk_result_rx`; recién
    // ahí, en el hilo principal, se sube a la GPU (`finalize_ready_chunks`).
    chunk_loader: environment::world::ChunkLoader,
    pending_chunks: std::collections::HashSet<(i32, i32)>,
    chunk_result_tx: std::sync::mpsc::Sender<ChunkResult>,
    chunk_result_rx: std::sync::mpsc::Receiver<ChunkResult>,

    // Contador de FPS: contamos frames y cada 1 segundo calculamos el
    // promedio, en vez de medir frame a frame (eso saltaría demasiado
    // para ser legible).
    fps_frame_count: u32,
    fps_timer: Instant,
    pub current_fps: f32,
    // Si se dibuja o no el contador de FPS en pantalla. Se puede
    // prender/apagar desde el panel de configuración.
    show_fps: bool,
    // Si se dibuja o no la capa de nubes (draw call de `clouds_pipeline`
    // en `render`). Prendido/apagado desde el panel de configuración,
    // fila "NUBES".
    show_clouds: bool,
    // Si la niebla de distancia está activa. Cuando es `false`, `update`
    // manda un `fog_start`/`fog_end` centinela enorme en vez del
    // calculado a partir de `render_radius`, así el mix hacia
    // `SKY_COLOR` nunca llega a activarse, ni en el terreno ni en el
    // borde de la capa de nubes. Prendido/apagado desde el panel de
    // configuración, fila "NIEBLA".
    show_fog: bool,

    /// Pantalla activa: Playing o Settings (pausa).
    game_screen: GameScreen,
}

impl State {
    async fn new(window: Arc<winit::window::Window>) -> Self {
        let render = engine::render_state::RenderState::new(window).await;

        // --- Generación de mundo (paralela con rayon) ---
        log::info!("Generando terreno...");
        let mut world = World::new(1337);
        world.generate_area(DEFAULT_RENDER_RADIUS);

        // Malleamos (greedy meshing) todos los chunks generados, en
        // paralelo, aprovechando los 2 núcleos / 2 hilos del Celeron N4000.
        // Como para la carga inicial TODOS los chunks del radio ya están
        // en `world.chunks` antes de mallear ninguno, cada chunk ve a sus
        // vecinos reales (Fase 5: culling consciente de vecinos) excepto
        // en el borde exterior del radio, donde no hay más remedio que
        // tratar "fuera del mundo cargado" como aire.
        let coords: Vec<(i32, i32)> = world.chunks.keys().copied().collect();
        let mesh_data: Vec<((i32, i32), environment::mesher::MeshData)> = coords
            .par_iter()
            .map(|&(cx, cz)| {
                let mesh = world.generate_chunk_mesh(cx, cz).unwrap_or(environment::mesher::MeshData {
                    vertices: Vec::new(),
                    indices: Vec::new(),
                });
                ((cx, cz), mesh)
            })
            .collect();

        log::info!(
            "Terreno generado: {} chunks, {} vértices totales",
            mesh_data.len(),
            mesh_data.iter().map(|(_, m)| m.vertices.len()).sum::<usize>()
        );

        let mut chunk_meshes: HashMap<(i32, i32), ChunkMesh> = HashMap::new();
        for ((cx, cz), mesh) in mesh_data {
            if mesh.indices.is_empty() {
                continue;
            }
            chunk_meshes.insert((cx, cz), build_chunk_mesh(&render.device, cx, cz, &mesh));
        }

        let camera = Camera::new(glam::Vec3::new(8.0, 40.0, 8.0));
        let player = Player::new(glam::Vec3::new(8.0, 40.0, 8.0));

        let chunk_loader = world.loader();
        let (chunk_result_tx, chunk_result_rx) = std::sync::mpsc::channel();

        Self {
            render,
            start_time: Instant::now(),
            chunk_meshes,
            world,
            camera,
            player,
            walk_mode: true,
            selected_block: BlockType::Stone,
            mouse_captured: false,
            last_frame: Instant::now(),
            touch: TouchController::new(),
            fps_frame_count: 0,
            fps_timer: Instant::now(),
            current_fps: 0.0,
            show_fps: true,
            show_clouds: true,
            show_fog: true,
            game_screen: GameScreen::Playing,
            current_player_chunk: (0, 0),
            render_radius: DEFAULT_RENDER_RADIUS,
            last_streamed_render_radius: DEFAULT_RENDER_RADIUS,
            chunk_loader,
            pending_chunks: std::collections::HashSet::new(),
            chunk_result_tx,
            chunk_result_rx,
        }
    }

    /// Cuenta un frame renderizado y, si pasó 1 segundo, recalcula el FPS
    /// promedio y devuelve Some(fps) para que el caller actualice el título
    /// de la ventana. Devuelve None si todavía no pasó el segundo.
    fn tick_fps(&mut self) -> Option<f32> {
        self.fps_frame_count += 1;
        let elapsed = self.fps_timer.elapsed().as_secs_f32();
        if elapsed >= 1.0 {
            self.current_fps = self.fps_frame_count as f32 / elapsed;
            self.fps_frame_count = 0;
            self.fps_timer = Instant::now();
            Some(self.current_fps)
        } else {
            None
        }
    }

    /// Re-genera el mesh de un chunk (o lo borra del mapa si quedó vacío)
    /// y actualiza `chunk_meshes`. Se llama sobre los chunks que
    /// realmente cambiaron tras romper/colocar un bloque, y también sobre
    /// vecinos que necesitan actualizar su culling de borde tras el
    /// streaming (ver `finalize_ready_chunks`) — no todo el mundo.
    fn remesh_chunk(&mut self, cx: i32, cz: i32) {
        match self.world.generate_chunk_mesh(cx, cz) {
            Some(mesh) if !mesh.indices.is_empty() => {
                let gpu_mesh = build_chunk_mesh(&self.render.device, cx, cz, &mesh);
                self.chunk_meshes.insert((cx, cz), gpu_mesh);
            }
            _ => {
                self.chunk_meshes.remove(&(cx, cz));
            }
        }
    }

    /// Lanza un rayo desde la cámara y rompe (click izquierdo) o coloca
    /// (click derecho) un bloque, usando DDA para encontrar el bloque
    /// exacto apuntado (ver environment::world::raycast).
    fn handle_click(&mut self, button: MouseButton) {
        let origin = self.camera.position;
        let direction = self.camera.view_direction();

        let Some(hit) = raycast(&self.world, origin, direction, REACH) else {
            return;
        };

        let dirty = match button {
            MouseButton::Left => {
                let (x, y, z) = hit.block_pos;
                self.world.set_block(x, y, z, BlockType::Air)
            }
            MouseButton::Right => {
                let (x, y, z) = hit.place_pos;
                self.world.set_block(x, y, z, self.selected_block)
            }
            _ => return,
        };

        for (cx, cz) in dirty {
            self.remesh_chunk(cx, cz);
        }
    }

    /// Chequea si el jugador cruzó a un chunk distinto desde la última vez
    /// y, si es así, descarga los chunks que quedaron fuera del radio y
    /// **dispara** (no espera) la carga de los que entraron nuevos: cada
    /// uno se manda a `rayon::spawn` para generarse/mallearse en un hilo
    /// de fondo, y su resultado se recoge más tarde, de a poco, en
    /// `finalize_ready_chunks`. Esta función en sí misma es barata y
    /// nunca bloquea esperando a que un chunk termine.
    fn update_chunk_streaming(&mut self) {
        // Usamos la posición de la cámara (no la del jugador) porque en
        // modo Vuelo el jugador físico no se actualiza, y igual queremos
        // que el streaming siga a la cámara en ambos modos.
        let pos = self.camera.position;
        let player_chunk = World::world_pos_to_chunk(pos.x, pos.z);

        let radius_changed = self.render_radius != self.last_streamed_render_radius;
        if player_chunk == self.current_player_chunk && !radius_changed && !self.chunk_meshes.is_empty() {
            return;
        }
        self.current_player_chunk = player_chunk;
        self.last_streamed_render_radius = self.render_radius;

        let (pcx, pcz) = player_chunk;
        let radius = self.render_radius;
        let wanted: std::collections::HashSet<(i32, i32)> = (-radius..=radius)
            .flat_map(|dx| (-radius..=radius).map(move |dz| (pcx + dx, pcz + dz)))
            .collect();

        // Descargar los que quedaron fuera del radio. Esto es barato (solo
        // sacar del HashMap / opcionalmente escribir a disco si tenían
        // cambios sin guardar) así que se queda síncrono.
        let to_unload: Vec<(i32, i32)> = self
            .world
            .chunks
            .keys()
            .copied()
            .filter(|c| !wanted.contains(c))
            .collect();
        for (cx, cz) in to_unload {
            self.world.unload_chunk(cx, cz);
            self.chunk_meshes.remove(&(cx, cz));
        }
        // Si un chunk que quedó fuera del radio todavía estaba en vuelo
        // (pedido pero no recibido), lo dejamos que termine igual: cuando
        // llegue por el channel, `finalize_ready_chunks` lo va a insertar
        // aunque ya no se necesite. Se descarga solo en el próximo cruce
        // de chunk. Es más simple que cancelar la tarea, y el costo es
        // insignificante (un chunk de más, generado una vez).

        // Pedir los que entraron nuevos al radio y todavía no están ni
        // cargados ni pedidos.
        let to_request: Vec<(i32, i32)> = wanted
            .into_iter()
            .filter(|c| !self.world.chunks.contains_key(c) && !self.pending_chunks.contains(c))
            .collect();

        for (cx, cz) in to_request {
            self.pending_chunks.insert((cx, cz));
            let loader = self.chunk_loader.clone();
            let tx = self.chunk_result_tx.clone();

            // Fase 5: le mandamos al hilo de fondo una foto de los vecinos
            // que YA están cargados en este instante (clonar un Chunk es
            // barato: ~16 KB, nada comparado con generar ruido Perlin o
            // leer un archivo). Así el mesh que vuelve por el channel ya
            // viene con culling correcto contra esos vecinos, en vez de
            // tratarlos como aire y tener que corregir todo después. Si
            // un vecino llega a cargarse recién DESPUÉS de esta foto, el
            // paso de abajo en `finalize_ready_chunks` se encarga de
            // corregirlo re-mallando en el momento en que ese vecino
            // exista de verdad.
            let neighbor_snapshots: Vec<((i32, i32), Chunk)> = World::direct_neighbors(cx, cz)
                .into_iter()
                .filter_map(|coord| self.world.chunks.get(&coord).map(|c| (coord, c.clone())))
                .collect();

            rayon::spawn(move || {
                let chunk = loader.load_or_generate(cx, cz);

                let find = |dx: i32, dz: i32| {
                    neighbor_snapshots
                        .iter()
                        .find(|(coord, _)| *coord == (cx + dx, cz + dz))
                        .map(|(_, c)| c)
                };
                let neighborhood = environment::mesher::ChunkNeighborhood {
                    center: &chunk,
                    neg_x: find(-1, 0),
                    pos_x: find(1, 0),
                    neg_z: find(0, -1),
                    pos_z: find(0, 1),
                };
                let mesh = environment::mesher::generate_mesh(&neighborhood);

                // Si el receiver ya no existe (la ventana se cerró y State
                // se destruyó) el send simplemente falla; no hay nada que
                // limpiar del lado del hilo de fondo.
                let _ = tx.send(((cx, cz), chunk, mesh));
            });
        }
    }

    /// Recoge, como máximo `MAX_FINALIZED_CHUNKS_PER_FRAME` por llamada,
    /// los chunks que un hilo de fondo ya terminó de generar y mallear, y
    /// recién acá —en el hilo principal, único lugar donde es válido
    /// usar `self.render.device`— sube esa malla a la GPU. Se llama todos los
    /// frames desde `update()`, no solo cuando el jugador cruza de chunk,
    /// para ir vaciando el channel de a poco en vez de todo de golpe.
    fn finalize_ready_chunks(&mut self) {
        for _ in 0..MAX_FINALIZED_CHUNKS_PER_FRAME {
            let Ok((coord, chunk, mesh)) = self.chunk_result_rx.try_recv() else {
                break;
            };
            self.pending_chunks.remove(&coord);

            if !mesh.indices.is_empty() {
                let (cx, cz) = coord;
                let gpu_mesh = build_chunk_mesh(&self.render.device, cx, cz, &mesh);
                self.chunk_meshes.insert(coord, gpu_mesh);
            }
            self.world.insert_loaded_chunk(coord.0, coord.1, chunk);

            // Fase 5: el mesh que acabamos de subir ya tiene culling
            // correcto contra los vecinos que existían en el momento en
            // que se pidió (ver `update_chunk_streaming`). Pero si algún
            // vecino YA estaba cargado y mallado ANTES de eso, ese vecino
            // todavía tiene su cara del borde dibujada de más (la mandó a
            // mallear cuando este chunk nuevo todavía no existía). Ahora
            // que los dos están en `world.chunks`, lo corregimos
            // re-mallando esos vecinos ya cargados — síncrono, pero
            // barato: como mucho 4 chunks, cada uno un greedy meshing
            // normal, nada distinto en costo a un romper/colocar bloque.
            for (ncx, ncz) in World::direct_neighbors(coord.0, coord.1) {
                let already_loaded = self.world.chunks.contains_key(&(ncx, ncz));
                let in_flight = self.pending_chunks.contains(&(ncx, ncz));
                if already_loaded && !in_flight {
                    // Ese vecino ya estaba en memoria antes de que este
                    // chunk llegara, así que su mesh puede tener una cara
                    // de más en el borde compartido. Si en cambio está en
                    // vuelo (in_flight), cuando llegue va a usar una foto
                    // de vecinos que ya incluye a este chunk, así que no
                    // hace falta tocarlo acá.
                    self.remesh_chunk(ncx, ncz);
                }
            }
        }
    }

    /// Pantalla mínima que se muestra después de atrapar un panic: solo
    /// limpia la superficie a rojo oscuro, sin tocar el mundo, las
    /// mallas de chunks, ni ningún otro estado que pueda haber quedado a
    /// medio actualizar cuando pasó el panic. A propósito no dibuja nada
    /// del juego — la idea es no volver a ejecutar la lógica que
    /// crasheó.
    ///
    /// `flash`: true durante un instante corto justo después de copiar
    /// el log al portapapeles (tecla C en desktop, toque en Android).
    /// Como el engine no tiene pipeline de texto, es la única forma que
    /// tenemos hoy de confirmar "sí, se copió" sin depender de un log
    /// que el usuario no está mirando en ese momento.
    fn render_crash_screen(&mut self, flash: bool) -> Result<(), wgpu::SurfaceError> {
        let output = self.render.surface.get_current_texture()?;
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .render
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("crash_screen_encoder"),
            });

        let color = if flash {
            // Verde: "listo, se copió". Nada más que un color distinto
            // por un rato corto — ver comentario de `flash` arriba.
            wgpu::Color {
                r: 0.05,
                g: 0.45,
                b: 0.10,
                a: 1.0,
            }
        } else {
            wgpu::Color {
                r: 0.45,
                g: 0.02,
                b: 0.02,
                a: 1.0,
            }
        };

        {
            let _render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("crash_screen_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }

        self.render.queue.submit(std::iter::once(encoder.finish()));
        output.present();

        Ok(())
    }

    fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        self.render.resize(new_size);
    }

    fn update(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32();
        self.last_frame = now;

        // En pantalla de configuración: el juego queda pausado.
        // Solo actualizamos el tiempo para no acumular un dt enorme
        // al volver al juego.
        if self.game_screen == GameScreen::Settings {
            return;
        }

        // Volcamos el estado del joystick/botones táctiles a la cámara
        // antes de moverla, con la misma interfaz que usan las teclas.
        self.camera.set_touch_move_axis(self.touch.move_axis());
        self.camera.set_touch_jump(self.touch.jump_held());
        let (look_dx, look_dy) = self.touch.take_look_delta();
        if look_dx != 0.0 || look_dy != 0.0 {
            self.camera.process_touch_look(look_dx, look_dy);
        }
        // Estilo Minecraft: mantener el dedo apoyado en la zona de mirar
        // (sin soltarlo) rompe el bloque apuntado en repetición, sin
        // necesidad de un botón ROMPER separado. Ver `TouchController::
        // poll_hold_break` y el comentario al principio de touch.rs.
        if self.touch.poll_hold_break() {
            self.handle_click(MouseButton::Left);
        }

        if self.walk_mode {
            if self.camera.wants_jump() {
                self.player.jump();
            }
            let horizontal = self.camera.horizontal_move_vector(4.5);
            self.player.update(&self.world, horizontal, dt);
            self.camera.position = self.player.eye_position();
        } else {
            self.camera.update(dt);
        }

        let aspect = self.render.config.width as f32 / self.render.config.height.max(1) as f32;
        let view_proj = projection_matrix(aspect) * self.camera.view_matrix();

        // Distancia de niebla en bloques (no en chunks): `render_radius`
        // son chunks, así que la pasamos a bloques con CHUNK_SIZE_X para
        // que `fog_end` caiga justo en (o un poco antes de) el borde
        // donde los chunks dejan de cargarse. `fog_start` es un 65% de
        // eso, para que la transición sea gradual y no un muro de niebla.
        // Como es un cálculo relativo al radio actual, funciona igual de
        // bien con radio 1 (niebla muy cerca, tapa el borde del mundo
        // cargado) que con radio 128 (niebla lejos, solo un detalle de
        // atmósfera en el horizonte).
        //
        // Si la niebla está apagada (fila "NIEBLA" en configuración),
        // mandamos un umbral centinela bien por encima de cualquier
        // distancia real del mundo, para que `fog_factor` en los
        // shaders (mezcla hacia SKY_COLOR) quede siempre en 0 sin tener
        // que tocar shader.wgsl/clouds_shader.wgsl con un flag aparte.
        // Ojo: esto también apaga el desvanecido del borde de la capa de
        // nubes (ver clouds_shader.wgsl), que reusa el mismo cálculo —
        // es la contrapartida esperada de apagar la niebla "de verdad".
        let (fog_start, fog_end) = if self.show_fog {
            let fog_end = (self.render_radius * CHUNK_SIZE_X as i32) as f32;
            (fog_end * FOG_START_FRACTION, fog_end)
        } else {
            const FOG_DISABLED_SENTINEL: f32 = 1.0e6;
            (FOG_DISABLED_SENTINEL, FOG_DISABLED_SENTINEL)
        };

        let uniforms = Uniforms {
            view_proj: view_proj.to_cols_array_2d(),
            camera_pos: self.camera.position.to_array(),
            fog_start,
            fog_color: SKY_COLOR,
            fog_end,
            time: [self.start_time.elapsed().as_secs_f32(), 0.0, 0.0, 0.0],
        };
        self.render.queue
            .write_buffer(&self.render.uniform_buffer, 0, bytemuck::cast_slice(&[uniforms]));

        self.update_chunk_streaming();
        self.finalize_ready_chunks();
    }

    fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
        let output = self.render.surface.get_current_texture()?;
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .render
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("render_encoder"),
            });

        // Geometría del overlay 2D de este frame.
        // En pantalla de configuración: mostramos la pantalla fullscreen de
        // ajustes encima del mundo congelado. En juego: mira + controles.
        let ui_vertices = if self.game_screen == GameScreen::Settings {
            ui_overlay::build_settings_screen(
                self.render.size,
                self.show_fps,
                self.walk_mode,
                self.render_radius,
                self.show_clouds,
                self.show_fog,
            )
        } else {
            let mut verts = ui_overlay::build_crosshair(self.render.size);
            #[cfg(target_os = "android")]
            verts.extend(ui_overlay::build_touch_overlay(
                &self.touch,
                self.render.size,
                self.selected_block,
                self.show_fps,
            ));
            // Contador de FPS en la esquina superior derecha.
            if self.show_fps {
                verts.extend(ui_overlay::build_fps_counter(self.current_fps, self.render.size));
            }
            verts
        };
        let ui_vertex_buffer = self
            .render
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("ui_overlay_vertex_buffer"),
                contents: bytemuck::cast_slice(&ui_vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });

        // Contorno del bloque apuntado (Fase 5): mismo raycast DDA que
        // usa `handle_click`, pero solo para saber qué dibujar, sin tocar
        // el mundo. Si no hay nada al alcance (`REACH`), el buffer queda
        // vacío y el draw call de abajo no dibuja nada (0 vértices).
        let highlight_vertices: Vec<highlight::HighlightVertex> = raycast(
            &self.world,
            self.camera.position,
            self.camera.view_direction(),
            REACH,
        )
        .map(|hit| highlight::build_block_outline(hit.block_pos))
        .unwrap_or_default();
        // Ojo: un buffer de 0 bytes no es válido en todos los backends de
        // wgpu (algunos lo rechazan en tiempo de validación). Si no hay
        // nada al alcance, directamente no creamos el buffer ni el draw
        // call de abajo, en vez de mandar un buffer vacío.
        let highlight_vertex_buffer = if highlight_vertices.is_empty() {
            None
        } else {
            Some(
                self.render.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("highlight_vertex_buffer"),
                        contents: bytemuck::cast_slice(&highlight_vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    }),
            )
        };

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("render_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: SKY_COLOR[0] as f64,
                            g: SKY_COLOR[1] as f64,
                            b: SKY_COLOR[2] as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.render.depth_texture_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_pass.set_pipeline(&self.render.render_pipeline);
            render_pass.set_bind_group(0, &self.render.uniform_bind_group, &[]);
            render_pass.set_bind_group(1, &self.render.texture_atlas.bind_group, &[]);

            for mesh in self.chunk_meshes.values() {
                render_pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                render_pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                render_pass.draw_indexed(0..mesh.num_indices, 0, 0..1);
            }

            // Contorno del bloque apuntado: mismo bind group (view_proj)
            // que el terreno, porque dibuja en espacio de mundo real.
            if let Some(buffer) = &highlight_vertex_buffer {
                render_pass.set_pipeline(&self.render.highlight_pipeline);
                render_pass.set_bind_group(0, &self.render.uniform_bind_group, &[]);
                render_pass.set_vertex_buffer(0, buffer.slice(..));
                render_pass.draw(0..highlight_vertices.len() as u32, 0..1);
            }

            // Capa de nubes: se dibuja después del terreno (para que el
            // depth test pueda ocultarlas detrás de una montaña alta) y
            // con alpha blending sobre lo ya pintado. Mismo bind group
            // que el terreno; el shader recentra el quad en la cámara
            // usando `camera_pos` de ese mismo uniform. Si el jugador
            // apagó la fila "NUBES" en configuración, directamente no
            // hacemos el draw call.
            if self.show_clouds {
                render_pass.set_pipeline(&self.render.clouds_pipeline);
                render_pass.set_bind_group(0, &self.render.uniform_bind_group, &[]);
                render_pass.set_vertex_buffer(0, self.render.cloud_vertex_buffer.slice(..));
                render_pass.draw(0..self.render.cloud_num_vertices, 0..1);
            }

            // Overlay 2D (mira + controles táctiles) encima de todo (mismo
            // render pass, mismo formato de depth attachment que
            // render_pipeline por requisito de wgpu, pero con
            // depth_compare: Always y depth_write_enabled: false, así que
            // no testea ni pisa el depth buffer y siempre queda visible).
            render_pass.set_pipeline(&self.render.ui_pipeline);
            render_pass.set_vertex_buffer(0, ui_vertex_buffer.slice(..));
            render_pass.draw(0..ui_vertices.len() as u32, 0..1);
        }

        self.render.queue.submit(std::iter::once(encoder.finish()));
        output.present();

        Ok(())
    }
}


/// Construye los buffers de GPU (vértices + índices) para un chunk,
/// desplazando cada vértice a su posición de mundo según (cx, cz).
/// Se usa tanto en la generación inicial como al re-mallear un chunk
/// modificado por romper/colocar bloques.
fn build_chunk_mesh(
    device: &wgpu::Device,
    cx: i32,
    cz: i32,
    mesh: &environment::mesher::MeshData,
) -> ChunkMesh {
    let offset_x = (cx * CHUNK_SIZE_X as i32) as f32;
    let offset_z = (cz * CHUNK_SIZE_Z as i32) as f32;
    let vertices: Vec<Vertex> = mesh
        .vertices
        .iter()
        .map(|v| Vertex {
            position: [
                v.position[0] + offset_x,
                v.position[1],
                v.position[2] + offset_z,
            ],
            normal: v.normal,
            color: v.color,
            uv: v.uv,
            tile_origin: v.tile_origin,
        })
        .collect();

    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("chunk_vertex_buffer"),
        contents: bytemuck::cast_slice(&vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("chunk_index_buffer"),
        contents: bytemuck::cast_slice(&mesh.indices),
        usage: wgpu::BufferUsages::INDEX,
    });

    ChunkMesh {
        vertex_buffer,
        index_buffer,
        num_indices: mesh.indices.len() as u32,
    }
}

/// Punto de entrada nativo en Android. `cargo-apk` genera el glue que
/// invoca esta función (buscada por su símbolo `android_main`) cuando la
/// Activity arranca, pasándole el `AndroidApp` con el que se construye el
/// event loop específico de Android.
#[cfg(target_os = "android")]
#[no_mangle]
fn android_main(app: AndroidApp) {
    use winit::platform::android::EventLoopBuilderExtAndroid;

    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );

    // Antes que nada: instalar el panic hook. `external_data_path()` cae
    // bajo Android/data/<paquete>/files — visible sin root con un
    // explorador de archivos o `adb pull`, a diferencia de la carpeta
    // interna. Si por lo que sea no está disponible (algunos
    // emuladores/ROMs raras) probamos con la interna; si ninguna está,
    // `crash::install` igual sigue funcionando, solo que sin poder
    // escribir el archivo (el reporte queda en logcat).
    let crash_dir = app.external_data_path().or_else(|| app.internal_data_path());
    crash::install(crash_dir.clone());
    // Además del panic hook (que solo ve panics de Rust): instalamos
    // manejadores de señal para crashes nativos de verdad (SIGSEGV,
    // SIGABRT, etc. — ver la explicación larga en platform/crash.rs).
    // Sin esto, un crash nativo (el sospechoso más probable de "se
    // cierra sin dejar rastro") no deja ni archivo ni pantalla roja,
    // porque nunca pasa por ninguno de los dos mecanismos de arriba.
    // Antes de instalar los manejadores de señal de ESTA corrida (para
    // no pisar/leer un native_crash.txt a medio escribir por ellos),
    // preguntamos si la corrida ANTERIOR del proceso murió mal: primero
    // vía ApplicationExitInfo (API 30+, lo pregunta el propio Android),
    // si no vía el archivo que dejó nuestro manejador de señal (ver
    // crash.rs). Si hay algo, arrancamos directo en pantalla de crash —
    // sin esto, un crash nativo que mató el proceso quedaba sin ninguna
    // forma de verlo/copiarlo en el dispositivo (el proceso ya no existe
    // para dibujar nada), que es justo el síntoma original.
    let startup_crash = crash::check_previous_run_crash(crash_dir.as_deref());

    crash::install_native_signal_handlers(crash_dir.as_deref());

    let event_loop = match winit::event_loop::EventLoopBuilder::new()
        .with_android_app(app)
        .build()
    {
        Ok(el) => el,
        Err(e) => {
            // Android reutiliza el proceso entre relanzamientos rápidos (es
            // normal del sistema, para abrir apps más rápido sin recargar
            // todo desde cero) pero `winit` solo permite construir un
            // EventLoop una vez por proceso: la segunda vez, en vez de
            // crashear, devuelve este error (típicamente
            // `RecreationAttempt`; ver
            // https://github.com/rust-windowing/winit/issues/3325). No hay
            // forma de "recrear" el loop dentro del mismo proceso, así que
            // matamos el proceso entero a propósito: el próximo toque en el
            // ícono va a arrancar uno completamente nuevo, con la bandera
            // interna de winit ya reseteada.
            log::warn!(
                "No se pudo (re)crear el EventLoop ({:?}); reiniciando el proceso.",
                e
            );
            // OJO: nada de `std::process::exit()` acá. `android_main` corre
            // en un pthread que generó el glue de NativeActivity, no en el
            // hilo principal del proceso Linux. `std::process::exit()` llama
            // a `libc::exit()`, que antes de matar el proceso corre
            // `__cxa_finalize` (destructores globales/TLS de todo lo
            // enlazado, incluido el runtime de Android/JNI que está atado a
            // *este* hilo en particular). Ejecutar esa limpieza desde un
            // hilo que no es el dueño de ese estado es lo que producía el
            // SIGSEGV en __cxa_finalize del log. `libc::_exit()` es el
            // syscall crudo: mata el proceso ya, sin correr un solo
            // destructor, así que es seguro llamarlo desde cualquier hilo.
            unsafe { libc::_exit(0) };
        }
    };

    run(event_loop, startup_crash);

    // Mismo motivo que arriba: usamos _exit en vez de std::process::exit
    // para no disparar la limpieza global desde este hilo.
    unsafe { libc::_exit(0) };
}

/// Punto de entrada en desktop: lo llama `main.rs`. Separado de `run()`
/// solo para poder construir el `EventLoop` de forma distinta según la
/// plataforma (en Android lo arma `android_main` con `with_android_app`).
pub fn run_desktop() {
    env_logger::init();
    // Instalado después de env_logger::init() para que el log::error!
    // del hook realmente se imprima (antes de inicializar el logger,
    // los logs se descartan en silencio).
    crash::install(None);
    let event_loop = EventLoop::new().unwrap();
    // En desktop no hay chequeo de "corrida anterior" (ver
    // crash::check_previous_run_crash): siempre arranca en modo normal.
    run(event_loop, None);
}

/// Estado de la app para el nuevo modelo de `winit` 0.30+ (`ApplicationHandler`,
/// en reemplazo del closure único que usaba `event_loop.run(...)` en 0.29).
/// Contiene lo mismo que antes vivía como variables capturadas por el closure.
#[derive(Default)]
struct App {
    window: Option<Arc<Window>>,
    state: Option<State>,

    // --- Manejo de crashes (ver crash.rs) ---
    // Una vez que `catch_unwind` atrapa un panic en cualquiera de los
    // callbacks de abajo, `crashed` queda en true para siempre (no hay
    // forma segura de "seguir jugando" después de un panic a mitad de
    // frame, así que no lo intentamos) y dejamos de correr toda la
    // lógica normal del juego. `crash_short_message` es el mensaje corto
    // para mostrar en el título de la ventana sin tener que ir a leer el
    // archivo.
    crashed: bool,
    // Se pone en true si la propia pantalla de crash vuelve a panickear
    // (ver el fix en window_event: esa rama ahora también está envuelta
    // en catch_unwind). En ese punto ya no intentamos redibujar nada más
    // — el log del primer crash, que es el que importa, ya se escribió a
    // disco/logcat antes de llegar hasta acá.
    double_crashed: bool,
    crash_short_message: Option<String>,
    // Instante hasta el cual `render_crash_screen` debe mostrar el color
    // de "copiado" en vez del rojo normal (ver comentario de `flash` en
    // `State::render_crash_screen`). `None` = sin flash activo.
    crash_copy_flash_until: Option<Instant>,
}

impl App {
    /// Se llama la primera vez que `catch_unwind` atrapa un panic en
    /// alguno de los callbacks de `ApplicationHandler`. Es idempotente
    /// (no repite el diálogo si ya estábamos en pantalla de crash).
    fn mark_crashed(&mut self) {
        if self.crashed {
            return;
        }
        self.crashed = true;

        match crash::last_crash() {
            Some((short, _full, file_path)) => {
                self.crash_short_message = Some(short.clone());
                // Diálogo nativo bloqueante: solo desktop (ver crash.rs).
                // Se llama acá y no en el loop de render porque tiene que
                // pasar una sola vez, no todos los frames.
                #[cfg(not(target_os = "android"))]
                crash::show_crash_dialog(&short, file_path.as_ref());
                #[cfg(target_os = "android")]
                let _ = file_path;
            }
            None => {
                self.crash_short_message =
                    Some("(no se pudo leer el mensaje del panic)".to_string());
            }
        }
    }
}

impl ApplicationHandler for App {
    // En Android, la ventana no se crea hasta el primer `resumed()` (la
    // superficie nativa todavía no existe al arrancar la Activity), así
    // que el `State` de wgpu se inicializa recién ahí — y se reconstruye
    // si Android la destruye y recrea al volver de segundo plano.
    //
    // Todo el cuerpo va envuelto en `catch_unwind`: si `State::new()`
    // panickea (por ejemplo, un adaptador GPU que no cumple algún límite
    // en un dispositivo raro), lo atrapamos acá en vez de dejar que tire
    // abajo el proceso. En ese caso puntual no llegamos a tener ni
    // ventana ni superficie donde dibujar nada, así que la única señal
    // que le queda al usuario es el diálogo nativo (desktop) y el
    // archivo de log — pero al menos la app no se cierra de golpe sin
    // dejar rastro.
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if self.window.is_none() {
                let attrs = Window::default_attributes()
                    .with_title("Voxel Engine - Fase 4")
                    .with_inner_size(winit::dpi::LogicalSize::new(1280, 720));
                let win = Arc::new(event_loop.create_window(attrs).unwrap());
                let new_state = pollster::block_on(State::new(win.clone()));
                self.window = Some(win);
                self.state = Some(new_state);
                #[cfg(target_os = "android")]
                immersive::apply_immersive_fullscreen();
            } else if self.state.is_none() {
                // Android destruyó la superficie al pasar a segundo plano
                // (ver `suspended()` más abajo) y ahora, al volver, nos avisa
                // que hay una superficie nueva.
                let win = self.window.as_ref().unwrap();
                self.state = Some(pollster::block_on(State::new(win.clone())));
                #[cfg(target_os = "android")]
                immersive::apply_immersive_fullscreen();
            }
        }));

        if panic_result.is_err() {
            self.mark_crashed();
        }
    }

    // Solo llega en Android: la Activity puede pasar a segundo plano y el
    // sistema operativo destruir la superficie nativa en cualquier
    // momento. Soltamos el `State` (que retiene el `wgpu::Surface`
    // apuntando a esa superficie) para no quedar con un handle inválido;
    // se reconstruye en el próximo `resumed()`.
    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        self.state = None;
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let (Some(window), Some(state)) = (self.window.as_ref(), self.state.as_mut()) else {
            return;
        };
        if window_id != window.id() {
            return;
        }

        if self.crashed {
            // Ya atrapamos un panic antes: no volvemos a correr la
            // lógica normal del juego (mundo, input, cámara...), que
            // puede haber quedado en un estado a medio actualizar cuando
            // pasó. Solo dejamos: cerrar la ventana, redibujar la
            // pantalla roja, cambiarle el tamaño si hace falta, y copiar
            // el log (tecla C en desktop, tocar la pantalla en Android).
            //
            // TODO arreglado: esta rama entera va envuelta en catch_unwind
            // igual que la rama normal de abajo. Antes, si la propia
            // pantalla de crash volvía a panickear (por ejemplo porque lo
            // que rompió el primer panic dejó al Device/Surface en un
            // estado inválido), ese segundo panic no tenía red — se
            // propagaba sin filtro y tiraba abajo el proceso entero,
            // anulando todo el propósito de este sistema. Si vuelve a
            // panickear acá adentro, dejamos de intentar redibujar nada
            // (double_crashed) en vez de arriesgar un tercer intento.
            if self.double_crashed {
                return;
            }

            let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                match event {
                    WindowEvent::CloseRequested => event_loop.exit(),
                    WindowEvent::Resized(new_size) => state.resize(new_size),
                    WindowEvent::KeyboardInput {
                        event: key_event, ..
                    } => {
                        if key_event.state == ElementState::Pressed {
                            if let PhysicalKey::Code(winit::keyboard::KeyCode::KeyC) =
                                key_event.physical_key
                            {
                                if crash::copy_last_crash_to_clipboard() {
                                    log::info!("Log de crash copiado al portapapeles.");
                                    self.crash_copy_flash_until =
                                        Some(Instant::now() + Duration::from_millis(700));
                                }
                            }
                        }
                    }
                    // Solo llega en Android (desktop no genera eventos de
                    // touch): tocar en cualquier parte de la pantalla de
                    // crash copia el log. Se dispara en `Started` y no en
                    // cada `Moved`, para que no haga falta un tap perfecto
                    // ni se repita todo el rato mientras el dedo sigue
                    // apoyado.
                    WindowEvent::Touch(touch)
                        if touch.phase == winit::event::TouchPhase::Started =>
                    {
                        if crash::copy_last_crash_to_clipboard() {
                            log::info!("Log de crash copiado al portapapeles.");
                            self.crash_copy_flash_until =
                                Some(Instant::now() + Duration::from_millis(700));
                        }
                    }
                    WindowEvent::RedrawRequested => {
                        let flashing = self
                            .crash_copy_flash_until
                            .map(|t| Instant::now() < t)
                            .unwrap_or(false);
                        match state.render_crash_screen(flashing) {
                            Ok(_) => {}
                            Err(wgpu::SurfaceError::Lost) => state.resize(state.render.size),
                            Err(wgpu::SurfaceError::OutOfMemory) => event_loop.exit(),
                            Err(e) => log::warn!("Error de render (pantalla de crash): {:?}", e),
                        }
                        if let Some(msg) = &self.crash_short_message {
                            window.set_title(&format!(
                                "Voxel Engine — CRASHEADO: {} (C / tocar pantalla: copiar log)",
                                msg
                            ));
                        }
                    }
                    _ => {}
                }
            }));

            if panic_result.is_err() {
                self.double_crashed = true;
                log::error!(
                    "Panic DENTRO de la pantalla de crash — dejamos de intentar redibujar. \
                     El log del primer crash ya debería estar en disco/logcat."
                );
            }
            return;
        }

        // Todo lo que sigue (el juego en sí) va envuelto en
        // `catch_unwind`. Gracias a `panic = "unwind"` (Cargo.toml) un
        // panic acá adentro no mata el proceso: solo aborta esta llamada
        // puntual a `window_event`, y volvemos con `mark_crashed()` en
        // vez de dejar que se propague hacia winit.
        let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            match event {
                WindowEvent::Focused(true) => {
                    // Abrir el cajón de notificaciones y cerrarlo (o
                    // cualquier diálogo/overlay del sistema) NO pasa por
                    // `resumed()`: la Activity nunca llega a pausarse del
                    // todo, solo pierde y recupera el foco de la ventana.
                    // Android además resetea los flags de inmersivo cada
                    // vez que la ventana recupera el foco, así que hay
                    // que volver a pedirlos acá, no solo en `resumed()`.
                    #[cfg(target_os = "android")]
                    immersive::apply_immersive_fullscreen();
                }
                WindowEvent::CloseRequested => {
                    let saved = state.world.save_dirty_chunks();
                    if saved > 0 {
                        log::info!("Guardados {} chunks modificados antes de salir.", saved);
                    }
                    event_loop.exit();
                }
                WindowEvent::Resized(physical_size) => state.resize(physical_size),
                WindowEvent::MouseInput {
                    state: btn_state,
                    button,
                    ..
                } => {
                    if !state.mouse_captured {
                        // El primer click solo captura el mouse (como en
                        // cualquier juego 3D en navegador/PC), no rompe ni
                        // coloca nada todavía.
                        if btn_state == ElementState::Pressed {
                            state.mouse_captured = true;
                            let _ = window
                                .set_cursor_grab(CursorGrabMode::Confined)
                                .or_else(|_| window.set_cursor_grab(CursorGrabMode::Locked));
                            window.set_cursor_visible(false);
                        }
                    } else if btn_state == ElementState::Pressed {
                        state.handle_click(button);
                    }
                }
                WindowEvent::Touch(touch) => {
                    let action = if state.game_screen == GameScreen::Settings {
                        // En pantalla de configuración: procesamos toques
                        // del menú de ajustes solamente.
                        state.touch.on_touch_settings(
                            touch,
                            state.render.size,
                            state.show_fps,
                            state.walk_mode,
                            state.show_clouds,
                            state.show_fog,
                        )
                    } else {
                        // En juego: procesamos controles táctiles normales.
                        state.touch.on_touch_game(touch, state.render.size)
                    };
                    if let Some(action) = action {
                        match action {
                            TouchAction::Place => state.handle_click(MouseButton::Right),
                            TouchAction::SelectBlock(n) => {
                                state.selected_block = match n {
                                    1 => BlockType::Grass,
                                    2 => BlockType::Dirt,
                                    _ => BlockType::Stone,
                                };
                            }
                            TouchAction::OpenSettings => {
                                state.game_screen = GameScreen::Settings;
                            }
                            TouchAction::CloseSettings => {
                                state.game_screen = GameScreen::Playing;
                                // Reiniciamos last_frame para no acumular
                                // un dt gigante después de la pausa.
                                state.last_frame = std::time::Instant::now();
                            }
                            TouchAction::ToggleFps => {
                                state.show_fps = !state.show_fps;
                            }
                            TouchAction::ToggleWalkMode => {
                                state.walk_mode = !state.walk_mode;
                                if state.walk_mode {
                                    state.player.feet_position =
                                        state.camera.position - glam::Vec3::new(0.0, 1.6, 0.0);
                                    state.player.velocity = glam::Vec3::ZERO;
                                }
                            }
                            TouchAction::DecreaseRenderRadius => {
                                state.render_radius =
                                    (state.render_radius - 1).max(MIN_RENDER_RADIUS);
                            }
                            TouchAction::IncreaseRenderRadius => {
                                state.render_radius =
                                    (state.render_radius + 1).min(MAX_RENDER_RADIUS);
                            }
                            TouchAction::ToggleClouds => {
                                state.show_clouds = !state.show_clouds;
                            }
                            TouchAction::ToggleFog => {
                                state.show_fog = !state.show_fog;
                            }
                        }
                    }
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        if code == winit::keyboard::KeyCode::Escape
                            && event.state == ElementState::Pressed
                        {
                            if state.game_screen == GameScreen::Settings {
                                // Cerrar configuración y volver al juego.
                                state.game_screen = GameScreen::Playing;
                                state.last_frame = std::time::Instant::now();
                                state.mouse_captured = true;
                                let _ = window.set_cursor_grab(CursorGrabMode::Confined)
                                    .or_else(|_| window.set_cursor_grab(CursorGrabMode::Locked));
                                window.set_cursor_visible(false);
                            } else {
                                // Abrir configuración (pausa).
                                state.game_screen = GameScreen::Settings;
                                state.mouse_captured = false;
                                let _ = window.set_cursor_grab(CursorGrabMode::None);
                                window.set_cursor_visible(true);
                            }
                        } else if event.state == ElementState::Pressed
                            && code == winit::keyboard::KeyCode::KeyF
                        {
                            // Toggle entre modo caminar (gravedad + colisión)
                            // y modo vuelo libre (útil para construir rápido
                            // o inspeccionar el mundo desde arriba).
                            state.walk_mode = !state.walk_mode;
                            if state.walk_mode {
                                // Sincronizamos al jugador con la posición
                                // actual de la cámara para no teletransportar
                                // ni hacer que caiga desde donde volaba.
                                state.player.feet_position =
                                    state.camera.position - glam::Vec3::new(0.0, 1.6, 0.0);
                                state.player.velocity = glam::Vec3::ZERO;
                            }
                        } else if event.state == ElementState::Pressed
                            && code == winit::keyboard::KeyCode::F5
                        {
                            let saved = state.world.save_dirty_chunks();
                            log::info!("Guardado manual: {} chunks escritos a disco.", saved);
                        } else if event.state == ElementState::Pressed
                            && matches!(
                                code,
                                winit::keyboard::KeyCode::Digit1
                                    | winit::keyboard::KeyCode::Digit2
                                    | winit::keyboard::KeyCode::Digit3
                            )
                        {
                            // Selección de bloque para colocar (hotbar simple).
                            state.selected_block = match code {
                                winit::keyboard::KeyCode::Digit1 => BlockType::Grass,
                                winit::keyboard::KeyCode::Digit2 => BlockType::Dirt,
                                _ => BlockType::Stone,
                            };
                        } else {
                            state.camera.process_key(code, event.state);
                        }
                    }
                }
                WindowEvent::RedrawRequested => {
                    state.update();
                    match state.render() {
                        Ok(_) => {}
                        Err(wgpu::SurfaceError::Lost) => state.resize(state.render.size),
                        Err(wgpu::SurfaceError::OutOfMemory) => event_loop.exit(),
                        Err(e) => log::warn!("Error de render: {:?}", e),
                    }

                    // Actualizamos el título de la ventana con el FPS una vez
                    // por segundo, para no gastar tiempo de CPU formateando
                    // strings en cada frame.
                    if let Some(fps) = state.tick_fps() {
                        let mode = if state.walk_mode { "Caminar" } else { "Vuelo" };
                        window.set_title(&format!(
                            "Voxel Engine - Fase 4 | {:.0} FPS | {} chunks | {} | Bloque: {:?} (1/2/3) | F: modo, F5: guardar",
                            fps,
                            state.chunk_meshes.len(),
                            mode,
                            state.selected_block
                        ));
                    }
                }
                _ => {}
            }
        }));

        if panic_result.is_err() {
            self.mark_crashed();
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        if self.crashed {
            return;
        }
        let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let DeviceEvent::MouseMotion { delta } = event {
                if let Some(state) = self.state.as_mut() {
                    if state.mouse_captured {
                        state.camera.process_mouse(delta.0, delta.1);
                    }
                }
            }
        }));
        if panic_result.is_err() {
            self.mark_crashed();
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Sigue pidiendo redraws aunque estemos crasheados: es lo que
        // mantiene a `render_crash_screen()` dibujando (y, en Android,
        // lo único que hace falta para que la pantalla roja quede
        // visible en vez de congelada en el último frame del juego).
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

/// Loop principal, común a desktop y Android. Arranca el `ApplicationHandler`
/// de arriba, que reemplaza al closure único que usaba `event_loop.run(...)`
/// en winit 0.29 (API vieja, deprecada y quitada en 0.30+).
///
/// `startup_crash`: si viene `Some(mensaje_corto)` (solo puede pasar en
/// Android — ver `crash::check_previous_run_crash`), el `App` arranca YA
/// marcado como crasheado, mostrando la pantalla roja desde el primer
/// frame en vez del juego. `resumed()` igual crea la ventana/`State`
/// normalmente (hace falta una superficie donde dibujar la pantalla de
/// crash); es `window_event` el que, al ver `self.crashed`, se desvía a
/// `render_crash_screen()` en vez de la lógica normal del juego — el
/// mismo mecanismo que ya se usa para un panic atrapado en esta misma
/// corrida, reutilizado acá para uno de la corrida anterior.
pub fn run(event_loop: EventLoop<()>, startup_crash: Option<String>) {
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::default();
    if let Some(short_message) = startup_crash {
        app.crashed = true;
        app.crash_short_message = Some(short_message);
    }
    event_loop.run_app(&mut app).unwrap();
}


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

mod camera;
mod chunk;
mod mesher;
mod player;
mod touch;
mod ui_overlay;
mod world;
mod worldgen;

use camera::{projection_matrix, Camera};
use chunk::{BlockType, CHUNK_SIZE_X, CHUNK_SIZE_Z};
use glam::Mat4;
use mesher::Vertex;
use player::Player;
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use touch::{TouchAction, TouchController};
use wgpu::util::DeviceExt;
use winit::{
    application::ApplicationHandler,
    event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::PhysicalKey,
    window::{CursorGrabMode, Window, WindowId},
};
use world::{raycast, World};

#[cfg(target_os = "android")]
use winit::platform::android::activity::AndroidApp;

/// Lo que un hilo de fondo manda de vuelta al terminar de generar/cargar
/// un chunk y mallearlo: coordenadas, el chunk (para insertarlo en
/// `World`) y su malla ya calculada en CPU (`MeshData`) — lo único que
/// falta es subirla a la GPU, y eso se hace en el hilo principal porque
/// `wgpu::Device` se usa ahí.
type ChunkResult = ((i32, i32), chunk::Chunk, mesher::MeshData);

// Radio de chunks a generar alrededor del origen (bajo a propósito:
// en el Celeron N4000 preferimos ver menos mundo a buen framerate antes
// que un mundo enorme que trabe el frame).
const RENDER_RADIUS: i32 = 4;

/// Cuántos chunks recién llegados del hilo de fondo se convierten a
/// buffers de GPU por frame como máximo. Crear un `wgpu::Buffer` no es
/// gratis (aunque sea rápido comparado con generar+mallear el chunk), así
/// que ponerle un tope evita un frame largo si de golpe terminan muchos
/// chunks a la vez (por ejemplo al aparecer, o si el jugador corre rápido
/// en modo Vuelo y cruza varios chunks de un saque).
const MAX_FINALIZED_CHUNKS_PER_FRAME: usize = 2;

// Distancia máxima (en bloques) a la que se puede romper/colocar.
const REACH: f32 = 6.0;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    view_proj: [[f32; 4]; 4],
}

struct ChunkMesh {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    num_indices: u32,
}

struct State {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    size: winit::dpi::PhysicalSize<u32>,

    render_pipeline: wgpu::RenderPipeline,
    // Pipeline aparte para el overlay táctil (ver ui_overlay.rs): sin
    // depth test (siempre se dibuja encima de todo) y con alpha blending
    // (traslúcido). Solo existe en Android — en desktop no hay
    // WindowEvent::Touch, así que ni el pipeline ni el buffer de vértices
    // del overlay tendrían para qué existir.
    #[cfg(target_os = "android")]
    ui_pipeline: wgpu::RenderPipeline,
    depth_texture_view: wgpu::TextureView,

    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,

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
    // realmente cruza a un chunk distinto.
    current_player_chunk: (i32, i32),

    // --- Streaming asincrónico de chunks ---
    // `chunk_loader` es el handle liviano que se clona y se manda a
    // `rayon::spawn` para generar/cargar un chunk en un hilo de fondo.
    // `pending_chunks` evita pedir dos veces el mismo chunk si todavía no
    // volvió del hilo de fondo. El resultado (chunk + su malla ya
    // calculada, todo trabajo de CPU) vuelve por `chunk_result_rx`; recién
    // ahí, en el hilo principal, se sube a la GPU (`finalize_ready_chunks`).
    chunk_loader: world::ChunkLoader,
    pending_chunks: std::collections::HashSet<(i32, i32)>,
    chunk_result_tx: std::sync::mpsc::Sender<ChunkResult>,
    chunk_result_rx: std::sync::mpsc::Receiver<ChunkResult>,

    // Contador de FPS: contamos frames y cada 1 segundo calculamos el
    // promedio, en vez de medir frame a frame (eso saltaría demasiado
    // para ser legible).
    fps_frame_count: u32,
    fps_timer: Instant,
    pub current_fps: f32,
}

impl State {
    async fn new(window: Arc<winit::window::Window>) -> Self {
        let size = window.inner_size();

        // Forzamos el backend GL explícitamente: en el hardware objetivo
        // (Intel UHD 600, Gemini Lake) OpenGL 4.5 tiene menos overhead de
        // driver que Vulkan para escenas simples, y es la ruta más estable
        // en Mesa para esta generación de GPU integrada.
        // En desktop forzamos GL (Intel UHD 600 / Gemini Lake: menos
        // overhead de driver que Vulkan para escenas simples en Mesa).
        // En Android forzamos Vulkan: el ANGLE/GLES de la mayoría de
        // los dispositivos da mucha peor latencia con wgpu que su
        // Vulkan nativo, que además es obligatorio desde Android 7+
        // en casi todo el hardware con soporte real de GPU.
        #[cfg(not(target_os = "android"))]
        let backends = wgpu::Backends::GL;
        #[cfg(target_os = "android")]
        let backends = wgpu::Backends::VULKAN;

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..Default::default()
        });

        let surface = instance.create_surface(window.clone()).unwrap();

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("No se encontró un adaptador GPU compatible con el backend GL. Verificá los drivers Mesa.");

        log::info!("Adapter: {:?}", adapter.get_info());

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("device"),
                    required_features: wgpu::Features::empty(),
                    // Límites "downlevel" porque el backend GL en hardware
                    // integrado no soporta todos los límites de wgpu por defecto.
                    required_limits: wgpu::Limits::downlevel_webgl2_defaults()
                        .using_resolution(adapter.limits()),
                },
                None,
            )
            .await
            .expect("No se pudo crear el device wgpu");

        let surface_caps = surface.get_capabilities(&adapter);
        let surface_format = surface_caps
            .formats
            .iter()
            .find(|f| f.is_srgb())
            .copied()
            .unwrap_or(surface_caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: surface_caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let depth_texture_view = create_depth_texture(&device, &config);

        // --- Uniform buffer (matriz view-projection) ---
        let uniforms = Uniforms {
            view_proj: Mat4::IDENTITY.to_cols_array_2d(),
        };
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("uniform_buffer"),
            contents: bytemuck::cast_slice(&[uniforms]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let uniform_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("uniform_bind_group_layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("uniform_bind_group"),
            layout: &uniform_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        // --- Pipeline ---
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pipeline_layout"),
            bind_group_layouts: &[&uniform_bind_group_layout],
            push_constant_ranges: &[],
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("render_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[Vertex::desc()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        // --- Pipeline del overlay táctil (solo Android) ---
        #[cfg(target_os = "android")]
        let ui_pipeline = {
            let ui_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("ui_shader"),
                source: wgpu::ShaderSource::Wgsl(include_str!("ui_shader.wgsl").into()),
            });
            let ui_pipeline_layout =
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("ui_pipeline_layout"),
                    // Sin bind groups: las posiciones ya vienen en NDC
                    // calculadas en CPU (ui_overlay.rs), no hace falta
                    // ninguna matriz ni uniform acá.
                    bind_group_layouts: &[],
                    push_constant_ranges: &[],
                });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("ui_pipeline"),
                layout: Some(&ui_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &ui_shader,
                    entry_point: "vs_main",
                    buffers: &[ui_overlay::UiVertex::desc()],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &ui_shader,
                    entry_point: "fs_main",
                    targets: &[Some(wgpu::ColorTargetState {
                        format: config.format,
                        // Alpha blending normal: el overlay es
                        // semitransparente y tiene que mezclarse con la
                        // escena 3D ya dibujada, no reemplazarla.
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    // Los triángulos del overlay se arman sin cuidar el
                    // winding (son formas 2D, no un objeto 3D con "atrás"),
                    // así que no conviene cullear ninguna cara.
                    cull_mode: None,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    unclipped_depth: false,
                    conservative: false,
                },
                // Sin depth_stencil: el overlay siempre se dibuja encima
                // de la escena 3D, sin testear ni escribir profundidad.
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            })
        };

        // --- Generación de mundo (paralela con rayon) ---
        log::info!("Generando terreno...");
        let mut world = World::new(1337);
        world.generate_area(RENDER_RADIUS);

        // Malleamos (greedy meshing) todos los chunks generados, en
        // paralelo, aprovechando los 2 núcleos / 2 hilos del Celeron N4000.
        let coords: Vec<(i32, i32)> = world.chunks.keys().copied().collect();
        let mesh_data: Vec<((i32, i32), mesher::MeshData)> = coords
            .par_iter()
            .map(|&(cx, cz)| {
                let chunk = world.chunks.get(&(cx, cz)).unwrap();
                (( cx, cz), mesher::generate_mesh(chunk))
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
            chunk_meshes.insert((cx, cz), build_chunk_mesh(&device, cx, cz, &mesh));
        }

        let camera = Camera::new(glam::Vec3::new(8.0, 40.0, 8.0));
        let player = Player::new(glam::Vec3::new(8.0, 40.0, 8.0));

        let chunk_loader = world.loader();
        let (chunk_result_tx, chunk_result_rx) = std::sync::mpsc::channel();

        Self {
            surface,
            device,
            queue,
            config,
            size,
            render_pipeline,
            #[cfg(target_os = "android")]
            ui_pipeline,
            depth_texture_view,
            uniform_buffer,
            uniform_bind_group,
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
            current_player_chunk: (0, 0),
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
    /// y actualiza `chunk_meshes`. Se llama solo sobre los chunks que
    /// realmente cambiaron tras romper/colocar un bloque — no todo el mundo.
    fn remesh_chunk(&mut self, cx: i32, cz: i32) {
        match self.world.chunks.get(&(cx, cz)) {
            Some(chunk) => {
                let mesh = mesher::generate_mesh(chunk);
                if mesh.indices.is_empty() {
                    self.chunk_meshes.remove(&(cx, cz));
                } else {
                    let gpu_mesh = build_chunk_mesh(&self.device, cx, cz, &mesh);
                    self.chunk_meshes.insert((cx, cz), gpu_mesh);
                }
            }
            None => {
                self.chunk_meshes.remove(&(cx, cz));
            }
        }
    }

    /// Lanza un rayo desde la cámara y rompe (click izquierdo) o coloca
    /// (click derecho) un bloque, usando DDA para encontrar el bloque
    /// exacto apuntado (ver world::raycast).
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

        if player_chunk == self.current_player_chunk && !self.chunk_meshes.is_empty() {
            return;
        }
        self.current_player_chunk = player_chunk;

        let (pcx, pcz) = player_chunk;
        let wanted: std::collections::HashSet<(i32, i32)> = (-RENDER_RADIUS..=RENDER_RADIUS)
            .flat_map(|dx| (-RENDER_RADIUS..=RENDER_RADIUS).map(move |dz| (pcx + dx, pcz + dz)))
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
            rayon::spawn(move || {
                let chunk = loader.load_or_generate(cx, cz);
                let mesh = mesher::generate_mesh(&chunk);
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
    /// usar `self.device`— sube esa malla a la GPU. Se llama todos los
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
                let gpu_mesh = build_chunk_mesh(&self.device, cx, cz, &mesh);
                self.chunk_meshes.insert(coord, gpu_mesh);
            }
            self.world.insert_loaded_chunk(coord.0, coord.1, chunk);
        }
    }

    fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width > 0 && new_size.height > 0 {
            self.size = new_size;
            self.config.width = new_size.width;
            self.config.height = new_size.height;
            self.surface.configure(&self.device, &self.config);
            self.depth_texture_view = create_depth_texture(&self.device, &self.config);
        }
    }

    fn update(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32();
        self.last_frame = now;

        // Volcamos el estado del joystick/botones táctiles a la cámara
        // antes de moverla, con la misma interfaz que usan las teclas.
        self.camera.set_touch_move_axis(self.touch.move_axis());
        self.camera.set_touch_jump(self.touch.jump_held());
        let (look_dx, look_dy) = self.touch.take_look_delta();
        if look_dx != 0.0 || look_dy != 0.0 {
            self.camera.process_touch_look(look_dx, look_dy);
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

        let aspect = self.config.width as f32 / self.config.height.max(1) as f32;
        let view_proj = projection_matrix(aspect) * self.camera.view_matrix();
        let uniforms = Uniforms {
            view_proj: view_proj.to_cols_array_2d(),
        };
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::cast_slice(&[uniforms]));

        self.update_chunk_streaming();
        self.finalize_ready_chunks();
    }

    fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
        let output = self.surface.get_current_texture()?;
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("render_encoder"),
            });

        // El buffer del overlay táctil se crea acá afuera (no dentro del
        // bloque del render_pass) porque tiene que vivir al menos tanto
        // como el `render_pass`, que le va a tomar un préstamo prestado
        // más abajo con `set_vertex_buffer`.
        #[cfg(target_os = "android")]
        let ui_vertices = ui_overlay::build_touch_overlay(&self.touch, self.size, self.selected_block);
        #[cfg(target_os = "android")]
        let ui_vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("ui_overlay_vertex_buffer"),
                contents: bytemuck::cast_slice(&ui_vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("render_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.53,
                            g: 0.81,
                            b: 0.92,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_texture_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_pass.set_pipeline(&self.render_pipeline);
            render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);

            for mesh in self.chunk_meshes.values() {
                render_pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                render_pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                render_pass.draw_indexed(0..mesh.num_indices, 0, 0..1);
            }

            // Overlay táctil encima de todo (mismo render pass: al no
            // tener `depth_stencil`, el pipeline de UI no testea ni pisa
            // el depth buffer del terreno, así que siempre queda visible).
            #[cfg(target_os = "android")]
            {
                render_pass.set_pipeline(&self.ui_pipeline);
                render_pass.set_vertex_buffer(0, ui_vertex_buffer.slice(..));
                render_pass.draw(0..ui_vertices.len() as u32, 0..1);
            }
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();

        Ok(())
    }
}

fn create_depth_texture(
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
) -> wgpu::TextureView {
    let size = wgpu::Extent3d {
        width: config.width.max(1),
        height: config.height.max(1),
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth_texture"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// Construye los buffers de GPU (vértices + índices) para un chunk,
/// desplazando cada vértice a su posición de mundo según (cx, cz).
/// Se usa tanto en la generación inicial como al re-mallear un chunk
/// modificado por romper/colocar bloques.
fn build_chunk_mesh(
    device: &wgpu::Device,
    cx: i32,
    cz: i32,
    mesh: &mesher::MeshData,
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

    let event_loop = winit::event_loop::EventLoopBuilder::new()
        .with_android_app(app)
        .build()
        .unwrap();

    run(event_loop);
}

/// Punto de entrada en desktop: lo llama `main.rs`. Separado de `run()`
/// solo para poder construir el `EventLoop` de forma distinta según la
/// plataforma (en Android lo arma `android_main` con `with_android_app`).
pub fn run_desktop() {
    env_logger::init();
    let event_loop = EventLoop::new().unwrap();
    run(event_loop);
}

/// Estado de la app para el nuevo modelo de `winit` 0.30+ (`ApplicationHandler`,
/// en reemplazo del closure único que usaba `event_loop.run(...)` en 0.29).
/// Contiene lo mismo que antes vivía como variables capturadas por el closure.
#[derive(Default)]
struct App {
    window: Option<Arc<Window>>,
    state: Option<State>,
}

impl ApplicationHandler for App {
    // En Android, la ventana no se crea hasta el primer `resumed()` (la
    // superficie nativa todavía no existe al arrancar la Activity), así
    // que el `State` de wgpu se inicializa recién ahí — y se reconstruye
    // si Android la destruye y recrea al volver de segundo plano.
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            let attrs = Window::default_attributes()
                .with_title("Voxel Engine - Fase 4")
                .with_inner_size(winit::dpi::LogicalSize::new(1280, 720));
            let win = Arc::new(event_loop.create_window(attrs).unwrap());
            let new_state = pollster::block_on(State::new(win.clone()));
            self.window = Some(win);
            self.state = Some(new_state);
        } else if self.state.is_none() {
            // Android destruyó la superficie al pasar a segundo plano
            // (ver `suspended()` más abajo) y ahora, al volver, nos avisa
            // que hay una superficie nueva.
            let win = self.window.as_ref().unwrap();
            self.state = Some(pollster::block_on(State::new(win.clone())));
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
        match event {
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
                if let Some(action) = state.touch.on_touch(touch, state.size) {
                    match action {
                        TouchAction::Break => state.handle_click(MouseButton::Left),
                        TouchAction::Place => state.handle_click(MouseButton::Right),
                        TouchAction::SelectBlock(n) => {
                            state.selected_block = match n {
                                1 => BlockType::Grass,
                                2 => BlockType::Dirt,
                                _ => BlockType::Stone,
                            };
                        }
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    if code == winit::keyboard::KeyCode::Escape
                        && event.state == ElementState::Pressed
                    {
                        state.mouse_captured = false;
                        let _ = window.set_cursor_grab(CursorGrabMode::None);
                        window.set_cursor_visible(true);
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
                    Err(wgpu::SurfaceError::Lost) => state.resize(state.size),
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
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        if let DeviceEvent::MouseMotion { delta } = event {
            if let Some(state) = self.state.as_mut() {
                if state.mouse_captured {
                    state.camera.process_mouse(delta.0, delta.1);
                }
            }
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

/// Loop principal, común a desktop y Android. Arranca el `ApplicationHandler`
/// de arriba, que reemplaza al closure único que usaba `event_loop.run(...)`
/// en winit 0.29 (API vieja, deprecada y quitada en 0.30+).
pub fn run(event_loop: EventLoop<()>) {
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::default();
    event_loop.run_app(&mut app).unwrap();
}

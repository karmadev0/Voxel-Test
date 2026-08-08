/// engine/render_state.rs
/// Todo lo que hace falta para inicializar wgpu (device, queue, surface,
/// pipelines, buffers de uniforms/nubes, atlas de texturas) y volver a
/// crear la depth texture al resize. Separado de `State` (lib.rs) porque
/// es la parte puramente gráfica: no sabe nada de mundo, cámara, jugador
/// ni streaming de chunks — solo cómo dibujar.
use crate::environment::clouds;
use crate::environment::clouds::CloudVertex;
use crate::environment::mesher::Vertex;
use crate::environment::sky::SKY_COLOR;
use crate::engine::highlight;
use crate::logic::ui_overlay;
use crate::textures::loader::TextureAtlas;
use glam::Mat4;
use std::sync::Arc;
use wgpu::util::DeviceExt;

// Medio lado del quad gigante de nubes (ver clouds.rs), en bloques.
// Tiene que ser mayor que la niebla más larga posible (`MAX_RENDER_RADIUS`
// chunks -> MAX_RENDER_RADIUS * CHUNK_SIZE_X bloques, hoy 128*16=2048)
// más margen para las esquinas del quad (que quedan más lejos que los
// lados a igual "radio"), si no en las esquinas del plano se vería el
// borde recto en vez de perderse en la niebla.
const CLOUD_PLANE_EXTENT: f32 = 3200.0;

// El layout de este struct tiene que coincidir byte a byte con el bloque
// `Uniforms` de shader.wgsl/clouds_shader.wgsl (reglas de alineación
// std140: cada `vec3<f32>` ocupa 16 bytes, así que lo hacemos coincidir
// con un `f32` suelto justo después para no dejar padding sin usar).
// `camera_pos` es la posición de la cámara en espacio mundo, para
// calcular la distancia de niebla en el fragment shader; `fog_color` es
// el mismo celeste del clear color del cielo (ver `render` en lib.rs),
// así que la niebla se funde con el fondo en vez de notarse como un
// muro. `time` es un vec4 (solo se usa `.x`, el resto es relleno) con
// los segundos desde que arrancó la app, para animar el desplazamiento
// de las nubes en `clouds_shader.wgsl`; shader.wgsl (terreno) y
// highlight_shader.wgsl no lo usan, pero como leen un prefijo más corto
// del mismo buffer no hace falta que lo declaren.
/// Reemplaza a `wgpu::SurfaceError`, que dejó de existir cuando
/// `Surface::get_current_texture` pasó a devolver el enum
/// `CurrentSurfaceTexture` en vez de un `Result`. Solo diferenciamos los
/// casos que a los callers (`render`/`render_crash_screen` en lib.rs) les
/// interesa distinguir; el resto de los casos de `CurrentSurfaceTexture`
/// (timeout, ventana oculta, surface desactualizada) se resuelven solos
/// adentro de `RenderState::acquire_frame` sin llegar hasta acá.
#[derive(Debug)]
pub enum FrameError {
    /// El dispositivo GPU se perdió (desconexión, reset de driver, etc.).
    /// Antes se resolvía re-creando la surface; el caller mantiene esa
    /// reacción.
    Lost,
    /// Cualquier otro error de validación al pedir la textura.
    Other(String),
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Uniforms {
    pub view_proj: [[f32; 4]; 4],
    pub camera_pos: [f32; 3],
    pub fog_start: f32,
    pub fog_color: [f32; 3],
    pub fog_end: f32,
    pub time: [f32; 4],
}

/// Todo el estado "gráfico" de la app: device/queue/surface de wgpu,
/// los cuatro pipelines (terreno, UI, highlight, nubes), sus buffers
/// asociados y el atlas de texturas. `State` (lib.rs) tiene un campo
/// `render: RenderState` en vez de tener estos 15 campos sueltos.
pub struct RenderState {
    pub surface: wgpu::Surface<'static>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub config: wgpu::SurfaceConfiguration,
    pub size: winit::dpi::PhysicalSize<u32>,

    pub render_pipeline: wgpu::RenderPipeline,
    // Pipeline del overlay 2D: sin depth test contra el mundo (siempre se
    // dibuja encima), con alpha blending. En Android dibuja los controles
    // táctiles; en las dos plataformas dibuja la mira central (Fase 5).
    pub ui_pipeline: wgpu::RenderPipeline,
    // Pipeline del contorno wireframe que marca el bloque apuntado
    // (Fase 5, ver highlight.rs). A diferencia de ui_pipeline, este SÍ
    // testea profundidad contra el mundo (para quedar oculto si hay algo
    // por delante), pero no la escribe.
    pub highlight_pipeline: wgpu::RenderPipeline,
    // Pipeline de la capa de nubes procedural (ver clouds.rs y
    // clouds_shader.wgsl). Reusa `uniform_bind_group_layout` igual que
    // highlight_pipeline: necesita view_proj + camera_pos + niebla, y
    // además `time` para animar el viento.
    pub clouds_pipeline: wgpu::RenderPipeline,
    // Quad único (6 vértices, sin index buffer) que forma la capa de
    // nubes; se crea una sola vez porque el vertex shader lo recentra
    // en la cámara cada frame usando `uniforms.camera_pos`.
    pub cloud_vertex_buffer: wgpu::Buffer,
    pub cloud_num_vertices: u32,
    pub depth_texture_view: wgpu::TextureView,

    pub uniform_buffer: wgpu::Buffer,
    pub uniform_bind_group: wgpu::BindGroup,
    // Atlas de texturas de bloques (grupo 1 del render_pipeline del
    // terreno, ver textures/loader.rs). Los otros pipelines (UI,
    // highlight, nubes) no lo usan, solo dibujan con color/vertex data.
    pub texture_atlas: TextureAtlas,
}

impl RenderState {
    /// Crea el device/queue/surface de wgpu y todos los pipelines +
    /// buffers asociados. `window` solo se usa acá (crear la surface y
    /// leer el tamaño inicial) — el resto de `State::new` (mundo, cámara,
    /// jugador, streaming) no lo necesita.
    pub async fn new(window: Arc<winit::window::Window>) -> Self {
        let size = window.inner_size();

        // Forzamos el backend GL explícitamente: en el hardware objetivo
        // (Intel UHD 600, Gemini Lake) OpenGL 4.5 tiene menos overhead de
        // driver que Vulkan para escenas simples, y es la ruta más estable
        // en Mesa para esta generación de GPU integrada.
        // En desktop forzamos GL (Intel UHD 600 / Gemini Lake: menos
        // overhead de driver que Vulkan para escenas simples en Mesa).
        // En Android preferimos Vulkan: el ANGLE/GLES de la mayoría de
        // los dispositivos da mucha peor latencia con wgpu que su
        // Vulkan nativo. Pero no lo forzamos a muerte: hay ROMs
        // custom/dispositivos viejos (confirmado en un HTC U11 con
        // Android 9 no oficial) donde el driver Vulkan del vendor está
        // roto o ausente aunque el chip lo soporte en teoría, así que si
        // Vulkan no encuentra adaptador compatible, caemos a GLES en vez
        // de crashear directo.
        //
        // En desktop hay una vuelta extra: wgpu-core 0.19.x tiene un bug
        // conocido (gfx-rs/wgpu#5225, #5272, #6165, #5294 — todos con el
        // mismo stacktrace) donde, si `create_surface` no logra conectar
        // el backend pedido con la ventana (típico en sesiones Wayland
        // donde el EGL de wgpu-hal no queda bien enchufado), en vez de
        // devolver un `CreateSurfaceError` prolijo hace un panic interno
        // ("called `Option::unwrap()` on a `None` value" en
        // wgpu-core/src/instance.rs:521). Como el panic pasa DENTRO de la
        // librería, no llega ni a devolver un `Result` que podamos
        // matchear: hay que envolver la llamada en `catch_unwind` y, si
        // explota, reintentar dejando que wgpu elija entre todos los
        // backends disponibles en el sistema en vez de forzar uno solo.
        #[cfg(not(target_os = "android"))]
        let (instance, surface) = {
            let gl_attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let gl_instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
                    backends: wgpu::Backends::GL,
                    flags: wgpu::InstanceFlags::default(),
                    memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
                    backend_options: wgpu::BackendOptions::default(),
                    display: None,
                });
                let gl_surface = gl_instance.create_surface(window.clone());
                (gl_instance, gl_surface)
            }));

            match gl_attempt {
                Ok((gl_instance, Ok(gl_surface))) => (gl_instance, gl_surface),
                other => {
                    match &other {
                        Ok((_, Err(e))) => log::warn!(
                            "No se pudo crear la superficie con GL forzado ({e:?}); \
                             reintentando dejando que wgpu elija el backend."
                        ),
                        Err(_) => log::warn!(
                            "wgpu-core panicó creando la superficie con GL forzado \
                             (bug conocido de wgpu 0.19 en algunos setups Wayland/EGL); \
                             reintentando dejando que wgpu elija el backend."
                        ),
                        _ => {}
                    }

                    // Soltamos la instancia/superficie de GL que falló
                    // antes de crear la siguiente, mismo motivo que en la
                    // rama Android más abajo: si la anterior sigue
                    // conectada a la ventana, la próxima puede fallar por
                    // "ya conectada" en vez de darnos un error limpio.
                    drop(other);

                    let fallback_instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
                        backends: wgpu::Backends::PRIMARY,
                        flags: wgpu::InstanceFlags::default(),
                        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
                        backend_options: wgpu::BackendOptions::default(),
                        display: None,
                    });
                    let fallback_surface = fallback_instance
                        .create_surface(window.clone())
                        .expect(
                            "No se pudo crear una superficie de render con ningún backend \
                             de wgpu (ni GL forzado ni el resto disponible en el sistema). \
                             Revisá los drivers Mesa/Vulkan; si estás en Wayland probá \
                             forzar X11 con la variable de entorno WINIT_UNIX_BACKEND=x11.",
                        );
                    (fallback_instance, fallback_surface)
                }
            }
        };
        #[cfg(target_os = "android")]
        let (instance, surface) = {
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
                backends: wgpu::Backends::VULKAN,
                flags: wgpu::InstanceFlags::default(),
                memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
                backend_options: wgpu::BackendOptions::default(),
                display: None,
            });
            let surface = instance.create_surface(window.clone()).unwrap();
            (instance, surface)
        };

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await;

        #[cfg(target_os = "android")]
        let (surface, adapter) = match adapter {
            Some(adapter) => (surface, adapter),
            None => {
                log::warn!(
                    "Vulkan no encontró un adaptador GPU compatible (driver \
                     roto/ausente en esta ROM/dispositivo); reintentando con GLES."
                );
                // Clave: soltar la superficie/instancia de Vulkan ANTES de
                // crear la de GLES. Las dos apuntan al mismo
                // `ANativeWindow`, y si la de Vulkan sigue conectada al
                // buffer queue nativo cuando intentamos conectar la de
                // GLES, Android devuelve "already connected" y la
                // superficie de GLES queda inválida (BadAlloc).
                drop(surface);
                drop(instance);

                let gles_instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
                    backends: wgpu::Backends::GL,
                    flags: wgpu::InstanceFlags::default(),
                    memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
                    backend_options: wgpu::BackendOptions::default(),
                    display: None,
                });
                let gles_surface = gles_instance.create_surface(window.clone()).unwrap();
                let gles_adapter = gles_instance
                    .request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference: wgpu::PowerPreference::HighPerformance,
                        compatible_surface: Some(&gles_surface),
                        force_fallback_adapter: false,
                    })
                    .await
                    .expect(
                        "Ni Vulkan ni GLES encontraron un adaptador GPU compatible en este dispositivo.",
                    );
                (gles_surface, gles_adapter)
            }
        };

        #[cfg(not(target_os = "android"))]
        let adapter = adapter.expect(
            "No se encontró ningún adaptador GPU compatible (ni con GL forzado ni con \
             el resto de backends disponibles). Verificá los drivers Mesa/Vulkan.",
        );

        log::info!("Adapter: {:?}", adapter.get_info());

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("device"),
                required_features: wgpu::Features::empty(),
                // Límites "downlevel" porque el backend GL en hardware
                // integrado no soporta todos los límites de wgpu por defecto.
                required_limits: wgpu::Limits::downlevel_webgl2_defaults()
                    .using_resolution(adapter.limits()),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
            })
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

        // --- Uniform buffer (matriz view-projection + niebla) ---
        let uniforms = Uniforms {
            view_proj: Mat4::IDENTITY.to_cols_array_2d(),
            camera_pos: [0.0, 0.0, 0.0],
            fog_start: 0.0,
            fog_color: SKY_COLOR,
            fog_end: 0.0,
            time: [0.0, 0.0, 0.0, 0.0],
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
                    // FRAGMENT además de VERTEX: el fragment shader del
                    // mundo (shader.wgsl) ahora también lee `camera_pos`/
                    // `fog_*` de este uniform para calcular la niebla.
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
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

        // Atlas de texturas de bloques: grupo 1 del render_pipeline del
        // terreno (grupo 0 sigue siendo el uniform de view_proj/niebla).
        // Solo el terreno lo necesita — UI, highlight y nubes no leen de
        // ningún atlas, así que sus pipelines ni se enteran de este grupo.
        let texture_atlas = TextureAtlas::load(&device, &queue);

        // --- Pipeline ---
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/shader.wgsl").into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pipeline_layout"),
            bind_group_layouts: &[Some(&uniform_bind_group_layout), Some(&texture_atlas.bind_group_layout)],
            immediate_size: 0,
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("render_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Vertex::desc()],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
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
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // --- Pipeline del overlay 2D: mira central (ambas plataformas) +
        // controles táctiles (solo Android) ---
        let ui_pipeline = {
            let ui_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("ui_shader"),
                source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/ui_shader.wgsl").into()),
            });
            let ui_pipeline_layout =
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("ui_pipeline_layout"),
                    // Sin bind groups: las posiciones ya vienen en NDC
                    // calculadas en CPU (ui_overlay.rs), no hace falta
                    // ninguna matriz ni uniform acá.
                    bind_group_layouts: &[],
                    immediate_size: 0,
                });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("ui_pipeline"),
                layout: Some(&ui_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &ui_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[ui_overlay::UiVertex::desc()],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &ui_shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: config.format,
                        // Alpha blending normal: el overlay es
                        // semitransparente y tiene que mezclarse con la
                        // escena 3D ya dibujada, no reemplazarla.
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
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
                // El render_pass donde se dibuja este pipeline comparte el
                // mismo depth attachment (Depth32Float) que render_pipeline
                // -- wgpu exige que TODOS los pipelines usados en una pass
                // declaren un depth_stencil compatible con sus attachments,
                // aunque no lo usen, o tira "Incompatible depth-stencil
                // attachment format" como en el crash. Para lograr "se
                // dibuja siempre encima, sin testear ni escribir
                // profundidad" hay que declarar el mismo formato pero con
                // depth_write_enabled: false y depth_compare: Always (en
                // vez de directamente omitir depth_stencil).
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::Always),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };

        // --- Pipeline del contorno del bloque apuntado (Fase 5) ---
        // Reusa `uniform_bind_group_layout` (view_proj) porque dibuja en
        // espacio de mundo real, no en NDC como el overlay 2D — necesita
        // la misma transformación de cámara que el terreno.
        let highlight_pipeline = {
            let highlight_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("highlight_shader"),
                source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/highlight_shader.wgsl").into()),
            });
            let highlight_pipeline_layout =
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("highlight_pipeline_layout"),
                    bind_group_layouts: &[Some(&uniform_bind_group_layout)],
                    immediate_size: 0,
                });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("highlight_pipeline"),
                layout: Some(&highlight_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &highlight_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[highlight::HighlightVertex::desc()],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &highlight_shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: config.format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::LineList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    unclipped_depth: false,
                    conservative: false,
                },
                // A diferencia del overlay 2D: SÍ testea profundidad
                // contra el mundo (para que el contorno quede oculto
                // detrás de terreno que lo tape), pero no la escribe —
                // así no interfiere con el depth de los chunks dibujados
                // después de él si el orden cambiara en algún momento.
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };

        // --- Pipeline de la capa de nubes ---
        // Igual que highlight_pipeline, reusa uniform_bind_group_layout:
        // dibuja en espacio de mundo real, así que necesita la misma
        // matriz view_proj (y además camera_pos/niebla/time, que
        // highlight_shader.wgsl ni siquiera declara pero que ya viven en
        // el mismo buffer).
        let clouds_pipeline = {
            let clouds_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("clouds_shader"),
                source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/clouds_shader.wgsl").into()),
            });
            let clouds_pipeline_layout =
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("clouds_pipeline_layout"),
                    bind_group_layouts: &[Some(&uniform_bind_group_layout)],
                    immediate_size: 0,
                });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("clouds_pipeline"),
                layout: Some(&clouds_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &clouds_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[CloudVertex::desc()],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &clouds_shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: config.format,
                        // Las nubes son translúcidas: se mezclan con lo
                        // que ya se dibujó (terreno + cielo) en vez de
                        // reemplazarlo.
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    // Sin cull: el plano se ve tanto desde arriba (si el
                    // jugador llega a volar por encima) como desde abajo
                    // (el caso normal).
                    cull_mode: None,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    unclipped_depth: false,
                    conservative: false,
                },
                // Igual que highlight_pipeline: testea profundidad contra
                // el terreno ya dibujado (para que una montaña alta pueda
                // taparlas) pero no la escribe, así no interfiere con
                // nada que se dibuje después.
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };

        let cloud_vertices = clouds::build_cloud_plane(CLOUD_PLANE_EXTENT);
        let cloud_num_vertices = cloud_vertices.len() as u32;
        let cloud_vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cloud_vertex_buffer"),
            contents: bytemuck::cast_slice(&cloud_vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });

        Self {
            surface,
            device,
            queue,
            config,
            size,
            render_pipeline,
            ui_pipeline,
            highlight_pipeline,
            clouds_pipeline,
            cloud_vertex_buffer,
            cloud_num_vertices,
            depth_texture_view,
            uniform_buffer,
            uniform_bind_group,
            texture_atlas,
        }
    }

    /// Pide la próxima textura de la surface para dibujar.
    ///
    /// Reemplaza al viejo `surface.get_current_texture()?` con
    /// `wgpu::SurfaceError`: desde que wgpu cambió `get_current_texture`
    /// para devolver el enum `CurrentSurfaceTexture` (con casos que ya no
    /// son "error" en el sentido de antes, como `Timeout`/`Occluded`),
    /// migramos toda esa lógica acá adentro para no repetirla en cada
    /// función de `lib.rs` que dibuja un frame (`render` y
    /// `render_crash_screen`).
    ///
    /// Devuelve:
    /// - `Some(Ok(texture))`: hay algo para dibujar este frame.
    /// - `Some(Err(_))`: pasó algo que el caller sí necesita reaccionar
    ///   (dispositivo perdido, error de validación).
    /// - `None`: no hay nada para dibujar este frame (timeout, ventana
    ///   oculta, o surface desactualizada — ya la reconfiguramos acá
    ///   mismo) pero tampoco es un error; el caller simplemente vuelve a
    ///   intentar en el próximo `RedrawRequested`.
    pub fn acquire_frame(&mut self) -> Option<Result<wgpu::SurfaceTexture, FrameError>> {
        match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture) => Some(Ok(texture)),
            wgpu::CurrentSurfaceTexture::Suboptimal(texture) => {
                // Sigue sirviendo para este frame, pero conviene
                // reconfigurar antes del próximo para volver a un estado
                // óptimo (tamaño/formato correctos).
                self.surface.configure(&self.device, &self.config);
                Some(Ok(texture))
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                // Nada para dibujar este frame; no es un error real.
                None
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                None
            }
            wgpu::CurrentSurfaceTexture::Lost => Some(Err(FrameError::Lost)),
            wgpu::CurrentSurfaceTexture::Validation => Some(Err(FrameError::Other(
                "error de validación al pedir la textura de la surface".to_string(),
            ))),
        }
    }

    /// Reconfigura la surface y recrea la depth texture con el nuevo
    /// tamaño. No hace nada si alguna dimensión es 0 (se puede recibir
    /// un resize a 0x0 al minimizar la ventana en desktop).
    pub fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width > 0 && new_size.height > 0 {
            self.size = new_size;
            self.config.width = new_size.width;
            self.config.height = new_size.height;
            self.surface.configure(&self.device, &self.config);
            self.depth_texture_view = create_depth_texture(&self.device, &self.config);
        }
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

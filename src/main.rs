/// main.rs
/// Punto de entrada. Arma la ventana (winit), inicializa wgpu forzando el
/// backend OpenGL (por la GPU integrada del Celeron N4000), genera un área
/// de chunks en paralelo con rayon, construye sus mallas con greedy
/// meshing, y corre el loop de render con una cámara de vuelo libre.

mod camera;
mod chunk;
mod mesher;
mod worldgen;

use camera::{projection_matrix, Camera};
use chunk::{CHUNK_SIZE_X, CHUNK_SIZE_Z};
use glam::Mat4;
use mesher::Vertex;
use rayon::prelude::*;
use std::sync::Arc;
use std::time::Instant;
use wgpu::util::DeviceExt;
use winit::{
    event::{DeviceEvent, ElementState, Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    keyboard::PhysicalKey,
    window::{CursorGrabMode, WindowBuilder},
};
use worldgen::WorldGenerator;

// Radio de chunks a generar alrededor del origen (bajo a propósito:
// en el Celeron N4000 preferimos ver menos mundo a buen framerate antes
// que un mundo enorme que trabe el frame).
const RENDER_RADIUS: i32 = 4;

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
    depth_texture_view: wgpu::TextureView,

    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,

    chunk_meshes: Vec<ChunkMesh>,
    camera: Camera,

    mouse_captured: bool,
    last_frame: Instant,
}

impl State {
    async fn new(window: Arc<winit::window::Window>) -> Self {
        let size = window.inner_size();

        // Forzamos el backend GL explícitamente: en el hardware objetivo
        // (Intel UHD 600, Gemini Lake) OpenGL 4.5 tiene menos overhead de
        // driver que Vulkan para escenas simples, y es la ruta más estable
        // en Mesa para esta generación de GPU integrada.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::GL,
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

        // --- Generación de mundo (paralela con rayon) ---
        log::info!("Generando terreno...");
        let generator = WorldGenerator::new(1337);
        let coords: Vec<(i32, i32)> = (-RENDER_RADIUS..=RENDER_RADIUS)
            .flat_map(|cx| (-RENDER_RADIUS..=RENDER_RADIUS).map(move |cz| (cx, cz)))
            .collect();

        // Cada chunk se genera y se mallea (greedy meshing) en paralelo,
        // aprovechando los 2 núcleos / 2 hilos físicos disponibles.
        let mesh_data: Vec<((i32, i32), mesher::MeshData)> = coords
            .par_iter()
            .map(|&(cx, cz)| {
                let chunk = generator.generate_chunk(cx, cz);
                let mesh = mesher::generate_mesh(&chunk);
                ((cx, cz), mesh)
            })
            .collect();

        log::info!(
            "Terreno generado: {} chunks, {} vértices totales",
            mesh_data.len(),
            mesh_data.iter().map(|(_, m)| m.vertices.len()).sum::<usize>()
        );

        let chunk_meshes: Vec<ChunkMesh> = mesh_data
            .into_iter()
            .filter(|(_, mesh)| !mesh.indices.is_empty())
            .map(|((cx, cz), mesh)| {
                // Desplazamos cada vértice a la posición de mundo del chunk
                // (esto podría hacerse con una matriz de instancia, pero
                // para el primer entregable lo dejamos simple).
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
            })
            .collect();

        let camera = Camera::new(glam::Vec3::new(8.0, 40.0, 8.0));

        Self {
            surface,
            device,
            queue,
            config,
            size,
            render_pipeline,
            depth_texture_view,
            uniform_buffer,
            uniform_bind_group,
            chunk_meshes,
            camera,
            mouse_captured: false,
            last_frame: Instant::now(),
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

        self.camera.update(dt);

        let aspect = self.config.width as f32 / self.config.height.max(1) as f32;
        let view_proj = projection_matrix(aspect) * self.camera.view_matrix();
        let uniforms = Uniforms {
            view_proj: view_proj.to_cols_array_2d(),
        };
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::cast_slice(&[uniforms]));
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

            for mesh in &self.chunk_meshes {
                render_pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                render_pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                render_pass.draw_indexed(0..mesh.num_indices, 0, 0..1);
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

fn main() {
    env_logger::init();

    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Poll);

    let window = Arc::new(
        WindowBuilder::new()
            .with_title("Voxel Engine - Fase 1")
            .with_inner_size(winit::dpi::LogicalSize::new(1280, 720))
            .build(&event_loop)
            .unwrap(),
    );

    let mut state = pollster::block_on(State::new(window.clone()));

    event_loop
        .run(move |event, elwt| match event {
            Event::WindowEvent {
                event: ref window_event,
                window_id,
            } if window_id == window.id() => match window_event {
                WindowEvent::CloseRequested => elwt.exit(),
                WindowEvent::Resized(physical_size) => state.resize(*physical_size),
                WindowEvent::MouseInput { state: btn_state, .. } => {
                    if *btn_state == ElementState::Pressed {
                        state.mouse_captured = true;
                        let _ = window
                            .set_cursor_grab(CursorGrabMode::Confined)
                            .or_else(|_| window.set_cursor_grab(CursorGrabMode::Locked));
                        window.set_cursor_visible(false);
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
                        Err(wgpu::SurfaceError::OutOfMemory) => elwt.exit(),
                        Err(e) => log::warn!("Error de render: {:?}", e),
                    }
                }
                _ => {}
            },
            Event::DeviceEvent {
                event: DeviceEvent::MouseMotion { delta },
                ..
            } => {
                if state.mouse_captured {
                    state.camera.process_mouse(delta.0, delta.1);
                }
            }
            Event::AboutToWait => window.request_redraw(),
            _ => {}
        })
        .unwrap();
}

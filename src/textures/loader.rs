/// loader.rs
/// Sube el atlas de texturas (assets/textures/atlas.png) a la GPU: crea la
/// `wgpu::Texture`, un sampler en modo "nearest" (para que se vea nítido
/// tipo pixel-art en vez de borroso al acercarse, como Minecraft) y el
/// bind group (grupo 1, separado del uniform que ya vive en el grupo 0 —
/// ver `shader.wgsl`).
///
/// El PNG se embebe en el binario con `include_bytes!` para no depender
/// de encontrar el archivo en disco en tiempo de ejecución (crítico en
/// Android, donde no hay un filesystem normal accesible por path relativo).
use image::GenericImageView;

const ATLAS_BYTES: &[u8] = include_bytes!("../../assets/textures/atlas.png");

pub struct TextureAtlas {
    pub bind_group_layout: wgpu::BindGroupLayout,
    pub bind_group: wgpu::BindGroup,
}

impl TextureAtlas {
    pub fn load(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let img = image::load_from_memory(ATLAS_BYTES)
            .expect("assets/textures/atlas.png inválido o corrupto");
        let rgba = img.to_rgba8();
        let (width, height) = img.dimensions();

        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("block_atlas_texture"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Srgb para que wgpu haga la conversión de espacio de color
            // igual que con los colores planos que reemplaza; si el atlas
            // se ve "lavado" en el futuro, es la primera constante a
            // revisar.
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &rgba,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4 * width),
                rows_per_image: Some(height),
            },
            size,
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        // Nearest en las 3 direcciones: mag/min para el look pixel-art
        // nítido, mipmap no importa porque no generamos mipmaps (el atlas
        // es chico y de bajo detalle). Repeat como red de seguridad: la
        // UV ya llega "envuelta" a 0..1 vía `fract()` en el shader, pero
        // dejar Repeat en vez de ClampToEdge evita un borde visible si esa
        // lógica cambia más adelante.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("block_atlas_sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("texture_bind_group_layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("texture_bind_group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        Self {
            bind_group_layout,
            bind_group,
        }
    }
}

//! Subida de frames I420 a la GPU y conversion a RGBA en un shader.
//!
//! El viewer decodifica VP8 a I420 (luma a resolucion completa + dos planos de croma a la
//! mitad) y lo tiene que pintar. Aqui vive ese camino: tres texturas R8Unorm + un shader
//! que aplica la matriz inversa de BT.601 en el propio pintado. El video no pasa por el
//! teselador de egui: se sube a textura y se pinta con un triangulo a pantalla completa,
//! que es el camino de baja latencia que fija el ADR-0001.

use eframe::wgpu;

/// Un frame I420 prestado, con el stride de cada plano.
///
/// Se modela aparte de `vhdesk_codec::I420Frame` porque el decodificador entrega los
/// planos con relleno por fila: el stride puede ser mayor que la anchura. `write_texture`
/// acepta ese stride directamente, asi que la subida no copia a un buffer apretado.
#[derive(Debug, Clone, Copy)]
pub struct I420Planes<'a> {
    /// Anchura en pixeles.
    pub width: u32,
    /// Altura en pixeles.
    pub height: u32,
    /// Plano de luminancia.
    pub y: &'a [u8],
    /// Plano de crominancia U.
    pub u: &'a [u8],
    /// Plano de crominancia V.
    pub v: &'a [u8],
    /// Bytes por fila del plano de luminancia; puede ser mayor que `width`.
    pub y_stride: u32,
    /// Bytes por fila de los planos de croma; puede ser mayor que la anchura de croma.
    pub uv_stride: u32,
}

/// Convierte frames I420 a RGBA en la GPU.
///
/// Las tres texturas de plano se crean una vez en [`VideoRenderer::new`] y se rellenan en
/// cada [`VideoRenderer::upload`], que no asigna nada por frame.
pub struct VideoRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    plano_y: wgpu::Texture,
    plano_u: wgpu::Texture,
    plano_v: wgpu::Texture,
    width: u32,
    height: u32,
}

/// Crea la textura de un plano, en R8 y apta para escribirla y muestrearla.
fn crear_plano(device: &wgpu::Device, width: u32, height: u32, etiqueta: &str) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(etiqueta),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

impl VideoRenderer {
    /// Crea el pipeline para un video de las dimensiones dadas.
    ///
    /// `target_format` **tiene que ser el de la superficie donde se va a pintar**, y no un
    /// valor fijo: wgpu exige que el formato del `ColorTargetState` coincida exactamente
    /// con el del attachment, y ese formato lo elige el backend segun la GPU y el sistema
    /// de ventanas. En la maquina de desarrollo con Radeon integrada y en la de RTX puede
    /// salir distinto, asi que el viewer lo consulta a eframe y lo registra al arrancar.
    ///
    /// El test de color usa `Rgba8Unorm` porque es el que permite comprobar los valores sin
    /// que una conversion sRGB los mueva por el camino.
    pub fn new(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        // Las dimensiones vienen del decodificador, siempre validas; un cero aqui es un
        // error de programacion, no un dato que haya que tolerar en tiempo de ejecucion.
        assert!(width > 0 && height > 0, "dimensiones de video nulas");

        let plano_y = crear_plano(device, width, height, "plano Y");
        let plano_u = crear_plano(device, width.div_ceil(2), height.div_ceil(2), "plano U");
        let plano_v = crear_plano(device, width.div_ceil(2), height.div_ceil(2), "plano V");

        let muestreador = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("muestreador I420"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            // Nearest en los tres planos: la uv cae en el centro de texel y asi no hay
            // interpolacion que emborrone el resultado. La subida de croma bilineal es una
            // mejora de calidad para la fase 4, no un fallo de correccion.
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let entrada_textura = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("layout I420"),
            entries: &[
                entrada_textura(0),
                entrada_textura(1),
                entrada_textura(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let vista_y = plano_y.create_view(&Default::default());
        let vista_u = plano_u.create_view(&Default::default());
        let vista_v = plano_v.create_view(&Default::default());

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bind group I420"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&vista_y),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&vista_u),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&vista_v),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&muestreador),
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shader I420"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader_i420.wgsl").into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pipeline I420"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("pipeline I420"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        Self {
            pipeline,
            bind_group,
            plano_y,
            plano_u,
            plano_v,
            width,
            height,
        }
    }

    /// Sube los tres planos a la GPU.
    ///
    /// `write_texture` acepta el stride de cada plano tal cual: la alineacion a
    /// `COPY_BYTES_PER_ROW_ALIGNMENT` (256 bytes) la piden las copias buffer<->textura, no
    /// `write_texture`. Por eso el stride de libvpx, que alinea a 16 o 32, sube sin rellenar
    /// filas ni copiar a un buffer apretado. El llamante es responsable de que `y` tenga al
    /// menos `y_stride * (height - 1) + width` bytes, y lo mismo para los planos de croma.
    pub fn upload(&mut self, queue: &wgpu::Queue, frame: &I420Planes<'_>) {
        subir_plano(
            queue,
            &self.plano_y,
            frame.y,
            frame.width,
            frame.height,
            frame.y_stride,
        );
        let ancho_croma = frame.width.div_ceil(2);
        let alto_croma = frame.height.div_ceil(2);
        subir_plano(
            queue,
            &self.plano_u,
            frame.u,
            ancho_croma,
            alto_croma,
            frame.uv_stride,
        );
        subir_plano(
            queue,
            &self.plano_v,
            frame.v,
            ancho_croma,
            alto_croma,
            frame.uv_stride,
        );
    }

    /// Emite las ordenes de dibujo en un paso ya abierto.
    ///
    /// Es lo que necesita el callback de pintado de egui, que entrega un `RenderPass` en
    /// curso en lugar de dejar abrir uno propio. El llamante decide el viewport, que es
    /// donde se aplica el encuadre con bandas negras.
    pub fn draw(&self, paso: &mut wgpu::RenderPass<'_>) {
        paso.set_pipeline(&self.pipeline);
        paso.set_bind_group(0, &self.bind_group, &[]);
        // Tres vertices sin buffer: el shader genera un triangulo que cubre la pantalla a
        // partir del indice. Un quad seria un vertice mas y una arista diagonal de mas.
        paso.draw(0..3, 0..1);
    }

    /// Pinta el frame subido en `destino`, abriendo su propio paso.
    ///
    /// Lo usa el test de color, que no tiene un paso de egui en el que meterse. En el
    /// binario no se llama: alli el paso lo abre egui y se dibuja con [`VideoRenderer::draw`].
    #[allow(dead_code)]
    pub fn render(&self, encoder: &mut wgpu::CommandEncoder, destino: &wgpu::TextureView) {
        let mut paso = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("paso I420"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: destino,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        self.draw(&mut paso);
    }

    /// Anchura del video para el que se creo.
    #[allow(dead_code)] // lo usara el callback de pintado del bloque E2.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Altura del video para el que se creo.
    #[allow(dead_code)]
    pub fn height(&self) -> u32 {
        self.height
    }
}

/// Escribe un plano en su textura con el stride dado.
fn subir_plano(
    queue: &wgpu::Queue,
    textura: &wgpu::Texture,
    datos: &[u8],
    width: u32,
    height: u32,
    stride: u32,
) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: textura,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        datos,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(stride),
            rows_per_image: None,
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::{I420Planes, VideoRenderer};
    use eframe::wgpu;
    use vhdesk_codec::I420Frame;

    /// Pide un adaptador y un dispositivo; `None` si el runner no tiene GPU, en cuyo caso
    /// los tests se omiten en vez de fallar.
    fn crear_dispositivo() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instancia = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let adaptador =
            pollster::block_on(instancia.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        pollster::block_on(adaptador.request_device(&wgpu::DeviceDescriptor::default())).ok()
    }

    /// Un frame BGRA de un solo color.
    fn liso(width: u32, height: u32, b: u8, g: u8, r: u8) -> Vec<u8> {
        let mut datos = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            datos.extend_from_slice(&[b, g, r, 255]);
        }
        datos
    }

    /// Sube el frame actual del renderer, lo pinta a una textura y lo lee de vuelta como
    /// RGBA apretado.
    fn renderizar(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut VideoRenderer,
        width: u32,
        height: u32,
    ) -> Vec<u8> {
        let objetivo = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("objetivo de prueba"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let vista = objetivo.create_view(&Default::default());

        // La copia textura->buffer SI exige bytes por fila alineados a 256, al contrario
        // que `write_texture`. Se lee a un buffer con relleno y se extrae lo apretado.
        let fila_alineada = (width * 4).div_ceil(256) * 256;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lectura de prueba"),
            size: u64::from(fila_alineada) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(&mut encoder, &vista);
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &objetivo,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(fila_alineada),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));

        let slice = buffer.slice(..);
        let (tx, rx) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |resultado| {
            let _ = tx.send(resultado);
        });
        let _ = device.poll(wgpu::PollType::Wait);
        rx.recv()
            .expect("canal de lectura")
            .expect("mapear lectura");

        let datos = slice.get_mapped_range();
        let mut salida = Vec::with_capacity((width * height * 4) as usize);
        for fila in 0..height {
            let inicio = (fila * fila_alineada) as usize;
            salida.extend_from_slice(&datos[inicio..inicio + (width * 4) as usize]);
        }
        salida
    }

    /// El viaje de ida y vuelta BGRA -> I420 (CPU) -> RGBA (GPU) conserva el color de
    /// referencia. Si la matriz o el rango del shader estuvieran mal, los primarios se
    /// desviarian mucho mas que el redondeo de 8 bits.
    #[test]
    fn el_viaje_de_ida_y_vuelta_mantiene_el_color_de_referencia() {
        let Some((device, queue)) = crear_dispositivo() else {
            eprintln!("sin adaptador grafico; se omite la comprobacion de color");
            return;
        };

        let (ancho, alto) = (16u32, 16u32);
        let mut renderer =
            VideoRenderer::new(&device, ancho, alto, wgpu::TextureFormat::Rgba8Unorm);

        // Orden (B, G, R), el mismo que usa la tabla de referencia del codec: rojo es
        // (0, 0, 255) y azul es (255, 0, 0).
        let referencias: &[(u8, u8, u8)] = &[
            (0, 0, 0),       // negro
            (255, 255, 255), // blanco
            (0, 0, 255),     // rojo
            (0, 255, 0),     // verde
            (255, 0, 0),     // azul
        ];

        for &(b, g, r) in referencias {
            let mut frame = I420Frame::new(ancho, alto).expect("crear frame");
            frame
                .fill_from_bgra(&liso(ancho, alto, b, g, r), (ancho * 4) as usize)
                .expect("convertir a I420");

            let planos = I420Planes {
                width: ancho,
                height: alto,
                y: frame.y(),
                u: frame.u(),
                v: frame.v(),
                y_stride: ancho,
                uv_stride: frame.chroma_width(),
            };
            renderer.upload(&queue, &planos);
            let rgba = renderizar(&device, &queue, &mut renderer, ancho, alto);

            let centro = (((alto / 2) * ancho + ancho / 2) * 4) as usize;
            let (r2, g2, b2) = (rgba[centro], rgba[centro + 1], rgba[centro + 2]);

            let deriva = |obtenido: u8, esperado: u8| obtenido.abs_diff(esperado);
            let (dr, dg, db) = (deriva(r2, r), deriva(g2, g), deriva(b2, b));
            println!(
                "BGR({b:>3},{g:>3},{r:>3}) -> RGBA({r2:>3},{g2:>3},{b2:>3})  deriva ({dr},{dg},{db})"
            );

            // BT.601 limitado con 8 bits cuesta como mucho un par de unidades por canal en
            // el viaje de ida y vuelta. Mas de eso es matriz o rango equivocados.
            assert!(
                dr <= 2 && dg <= 2 && db <= 2,
                "BGR({b},{g},{r}) da ({r2},{g2},{b2}), deriva ({dr},{dg},{db})"
            );
        }
    }

    /// `write_texture` acepta un stride no alineado a 256 y con relleno por fila, sin que
    /// ese relleno se cuele en la textura. Es la propiedad que permite subir el frame
    /// decodificado por libvpx sin copiarlo a un buffer apretado.
    #[test]
    fn write_texture_ignora_el_relleno_de_fila() {
        let Some((device, queue)) = crear_dispositivo() else {
            eprintln!("sin adaptador grafico; se omite la comprobacion de stride");
            return;
        };

        let (ancho, alto) = (16u32, 16u32);
        let mut renderer =
            VideoRenderer::new(&device, ancho, alto, wgpu::TextureFormat::Rgba8Unorm);

        // Plano Y blanco (235) con 8 bytes de relleno negro por fila: stride 24, que no es
        // multiplo de 256. Si write_texture leyera el relleno, las filas saldrian grises.
        let stride = ancho + 8;
        let mut y = vec![0u8; (stride * alto) as usize];
        for fila in 0..alto {
            let inicio = (fila * stride) as usize;
            y[inicio..inicio + ancho as usize].fill(235);
        }

        let ancho_croma = ancho.div_ceil(2);
        let alto_croma = alto.div_ceil(2);
        let u = vec![128u8; (ancho_croma * alto_croma) as usize];
        let v = u.clone();

        let planos = I420Planes {
            width: ancho,
            height: alto,
            y: &y,
            u: &u,
            v: &v,
            y_stride: stride,
            uv_stride: ancho_croma,
        };
        renderer.upload(&queue, &planos);
        let rgba = renderizar(&device, &queue, &mut renderer, ancho, alto);

        for indice in (0..rgba.len()).step_by(4) {
            let (r, g, b) = (rgba[indice], rgba[indice + 1], rgba[indice + 2]);
            assert!(
                r >= 234 && g >= 234 && b >= 234,
                "pixel {} no es blanco: ({r},{g},{b}); el relleno de fila se ha colado",
                indice / 4
            );
        }
    }
}

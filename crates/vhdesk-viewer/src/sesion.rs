//! La sesion del viewer: conecta, negocia, decodifica y sube a la GPU.
//!
//! # Por que hay un hilo aparte
//!
//! `eframe` se queda con el hilo principal, asi que la sesion vive en un hilo propio con su
//! runtime de tokio. Se comunican por [`Compartido`], que la interfaz solo lee.
//!
//! # El frame no cruza hilos
//!
//! La decodificacion y la **subida a textura ocurren en el mismo hilo**, uno detras de otro.
//! Es deliberado: el frame decodificado vive en un buffer interno de libvpx que deja de ser
//! valido en la siguiente llamada, asi que mandarlo a otro hilo obligaria a copiarlo
//! —3,1 MiB por frame a 1080p— y esa copia no compra nada, porque la subida a la GPU hay
//! que hacerla de todos modos. Lo que cruza al hilo de pintado no son pixeles, son texturas
//! ya rellenas.
//!
//! Por eso el runtime es de un solo hilo: el decodificador de libvpx no es `Send`, y el
//! bucle de video se ejecuta con `block_on` en vez de `spawn` precisamente para que no
//! tenga que serlo. Las tareas que si son `Send` (control, datagramas y entrada) se lanzan
//! aparte.
//!
//! # La entrada va en sentido contrario
//!
//! La interfaz no puede esperar a la red dentro de `update`, asi que empuja los eventos ya
//! traducidos a una cola y sigue pintando; una tarea del runtime los saca y los escribe en
//! el stream de input. La cola **no tiene tope** a proposito: los eventos de entrada son
//! diminutos y llegan como mucho unas decenas por segundo, y descartar una liberacion de
//! tecla por cola llena es exactamente el fallo que `ReleaseAll` existe para evitar. Si la
//! red se para, quien se llena no es la cola sino el buffer de QUIC, y la sesion termina.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use eframe::wgpu;
use vhdesk_codec::{VideoDecoder, open_decoder};
use vhdesk_proto::{
    AuthResponse, AuthResult, Cursor, Hello, KeyframeReason, KeyframeRequest, Message,
    PROTOCOL_VERSION, Role, VideoCodec,
};
use vhdesk_transport::{
    Endpoint, RecepcionVideo, Session, TransportError, install_crypto_provider,
};

use crate::video::{I420Planes, VideoRenderer};

/// Cola por la que la interfaz manda entrada al hilo de sesion.
///
/// Acepta `Message` y no `InputEvent` porque por el canal de input viaja tambien
/// `ReleaseAll`, que no es un evento a inyectar sino una orden sobre el estado de la
/// sesion.
pub type EmisorEntrada = tokio::sync::mpsc::UnboundedSender<Message>;

/// Codecs que este viewer sabe decodificar, en orden de preferencia.
const CODECS: &[VideoCodec] = &[VideoCodec::Vp8];

/// En que punto esta la sesion.
///
/// La interfaz lo pinta tal cual: **nunca se deja la ventana en negro sin explicacion**.
/// Una ventana negra es indistinguible de un cuelgue, y el usuario no tiene forma de saber
/// si esta esperando o si algo se rompio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Estado {
    /// Abriendo el socket y haciendo el handshake QUIC.
    Conectando(SocketAddr),
    /// Conexion establecida, intercambiando `Hello` y codec.
    Negociando,
    /// Handshake terminado; todavia no ha llegado ningun frame.
    ///
    /// Es un estado real y visible: entre el handshake y el primer keyframe pasa el tiempo
    /// que tarde el host en codificarlo, que con un keyframe de 1080p son decenas de ms, y
    /// mas si la pantalla remota esta quieta.
    Esperando,
    /// Llegando video.
    Activa,
    /// La sesion termino, de forma limpia o no.
    Terminada {
        /// Que paso, en texto para la ventana.
        motivo: String,
        /// Si termino porque el host cerro ordenadamente.
        limpia: bool,
    },
}

impl Estado {
    /// Texto para la ventana mientras no hay imagen.
    pub fn descripcion(&self) -> String {
        match self {
            Self::Conectando(destino) => format!("Conectando con {destino}..."),
            Self::Negociando => "Negociando la sesion...".to_owned(),
            Self::Esperando => "Conectado. Esperando el primer frame...".to_owned(),
            Self::Activa => String::new(),
            Self::Terminada {
                motivo,
                limpia: true,
            } => {
                format!("El host cerro la sesion.\n\n{motivo}")
            }
            Self::Terminada {
                motivo,
                limpia: false,
            } => {
                format!("Se perdio la conexion.\n\n{motivo}")
            }
        }
    }
}

/// Imagen del puntero remoto, tal y como la mando el host.
///
/// El alfa es transparencia normal, **sin premultiplicar**: es lo que produce la conversion
/// de `vhdesk-capture` y lo que espera `ColorImage::from_rgba_unmultiplied`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormaCursor {
    /// Anchura en pixeles.
    pub width: u32,
    /// Altura en pixeles.
    pub height: u32,
    /// Desplazamiento horizontal del punto activo dentro de la imagen.
    pub hotspot_x: u32,
    /// Desplazamiento vertical del punto activo dentro de la imagen.
    pub hotspot_y: u32,
    /// Pixeles RGBA, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// Posicion del cursor remoto, normalizada al rango 0..=1 del monitor servido.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CursorRemoto {
    /// Coordenada horizontal normalizada.
    pub x: f32,
    /// Coordenada vertical normalizada.
    pub y: f32,
    /// Si el host lo esta dibujando.
    pub visible: bool,
}

/// Lo que la sesion comparte con la interfaz.
///
/// La interfaz **solo lee**. Cada campo tiene su cerrojo por separado y no uno global: el
/// hilo de pintado toma el del renderer en cada frame, y no debe quedarse esperando a que
/// el de red termine de escribir una estadistica.
pub struct Compartido {
    estado: Mutex<Estado>,
    /// El renderer se crea al llegar el primer frame, cuando ya se conocen las dimensiones.
    renderer: Mutex<Option<VideoRenderer>>,
    dimensiones: Mutex<Option<(u32, u32)>>,
    cursor: Mutex<Option<CursorRemoto>>,
    forma_cursor: Mutex<Option<FormaCursor>>,
    /// Sube cada vez que llega una forma nueva.
    ///
    /// Es lo que le dice a la interfaz que tiene que rehacer la textura. Comparar las
    /// imagenes pixel a pixel para averiguarlo costaria mas que subirla otra vez, y guardar
    /// solo un `bool` perderia una forma si llegaran dos entre dos repintados.
    version_forma: AtomicU64,
    frames: AtomicU64,
    huecos: AtomicU64,
    keyframes_pedidos: AtomicU64,
    eventos_entrada: AtomicU64,
}

impl Compartido {
    fn nuevo(destino: SocketAddr) -> Self {
        Self {
            estado: Mutex::new(Estado::Conectando(destino)),
            renderer: Mutex::new(None),
            dimensiones: Mutex::new(None),
            cursor: Mutex::new(None),
            forma_cursor: Mutex::new(None),
            version_forma: AtomicU64::new(0),
            frames: AtomicU64::new(0),
            huecos: AtomicU64::new(0),
            keyframes_pedidos: AtomicU64::new(0),
            eventos_entrada: AtomicU64::new(0),
        }
    }

    /// Estado actual.
    pub fn estado(&self) -> Estado {
        // Un cerrojo envenenado significa que el hilo de red entro en panico. Aqui se
        // prefiere seguir pintando la ventana con el ultimo estado conocido a arrastrar el
        // panico hasta la interfaz.
        self.estado
            .lock()
            .map(|e| e.clone())
            .unwrap_or(Estado::Terminada {
                motivo: "el hilo de sesion entro en panico".to_owned(),
                limpia: false,
            })
    }

    /// Dimensiones del video, si ya llego algun frame.
    pub fn dimensiones(&self) -> Option<(u32, u32)> {
        self.dimensiones.lock().ok().and_then(|d| *d)
    }

    /// Ultima posicion conocida del cursor remoto.
    pub fn cursor(&self) -> Option<CursorRemoto> {
        self.cursor.lock().ok().and_then(|c| *c)
    }

    /// Version de la forma del cursor: cambia cuando hay una imagen nueva que subir.
    pub fn version_forma(&self) -> u64 {
        self.version_forma.load(Ordering::Relaxed)
    }

    /// Ejecuta `f` con la ultima forma recibida, si hay alguna.
    ///
    /// Se pasa por referencia en vez de devolver una copia porque un puntero de 32x32 son
    /// 4 KiB y la interfaz solo la necesita para construir la textura.
    pub fn con_forma<T>(&self, f: impl FnOnce(&FormaCursor) -> T) -> Option<T> {
        let guardia = self.forma_cursor.lock().ok()?;
        guardia.as_ref().map(f)
    }

    /// Frames decodificados desde el inicio.
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    /// Huecos de secuencia detectados.
    pub fn huecos(&self) -> u64 {
        self.huecos.load(Ordering::Relaxed)
    }

    /// Keyframes pedidos al host.
    pub fn keyframes_pedidos(&self) -> u64 {
        self.keyframes_pedidos.load(Ordering::Relaxed)
    }

    /// Eventos de entrada escritos en el stream, teclado y raton juntos.
    ///
    /// Es lo que permite distinguir en una prueba a mano "el host no reacciona" de "el
    /// viewer no esta capturando nada", que se ven igual desde la pantalla y piden arreglos
    /// completamente distintos.
    pub fn eventos_entrada(&self) -> u64 {
        self.eventos_entrada.load(Ordering::Relaxed)
    }

    /// Anota un evento de entrada enviado.
    pub fn anotar_entrada(&self) {
        self.eventos_entrada.fetch_add(1, Ordering::Relaxed);
    }

    /// Ejecuta `f` con el renderer, si ya existe.
    ///
    /// Lo usa el callback de pintado. Devuelve `None` si aun no hay video o si el cerrojo
    /// esta envenenado: en ambos casos no se pinta nada y la ventana ensena su estado.
    pub fn con_renderer<T>(&self, f: impl FnOnce(&VideoRenderer) -> T) -> Option<T> {
        let guardia = self.renderer.lock().ok()?;
        guardia.as_ref().map(f)
    }

    fn poner_estado(&self, nuevo: Estado, ctx: &eframe::egui::Context) {
        if let Ok(mut estado) = self.estado.lock() {
            *estado = nuevo;
        }
        // Sin esto, un cambio de estado no se veria hasta el siguiente repintado, que con
        // la ventana quieta puede no llegar nunca.
        ctx.request_repaint();
    }
}

/// Recursos de GPU que la sesion necesita para subir los frames.
#[derive(Clone)]
pub struct Gpu {
    /// Dispositivo de eframe.
    pub device: wgpu::Device,
    /// Cola de eframe.
    pub queue: wgpu::Queue,
    /// Formato de la superficie donde se pintara.
    pub formato: wgpu::TextureFormat,
}

/// Arranca la sesion en un hilo propio y devuelve el estado compartido y la cola de entrada.
///
/// No falla: los errores de conexion se reflejan en [`Estado::Terminada`] para que la
/// ventana pueda mostrarlos, en vez de abortar antes de que haya nada que mirar.
///
/// La cola de entrada existe desde antes de que haya conexion. Lo que se meta en ella
/// mientras se negocia se enviara en cuanto el stream este abierto; es una ventana de
/// milisegundos durante la cual la ventana ensena "Conectando" y nadie esta escribiendo.
pub fn arrancar(
    destino: SocketAddr,
    gpu: Gpu,
    ctx: eframe::egui::Context,
) -> (Arc<Compartido>, EmisorEntrada) {
    let compartido = Arc::new(Compartido::nuevo(destino));
    let para_hilo = Arc::clone(&compartido);
    let (emisor_entrada, receptor_entrada) = tokio::sync::mpsc::unbounded_channel();

    std::thread::Builder::new()
        .name("vhdesk-sesion".to_owned())
        .spawn(move || {
            // Un solo hilo a proposito: ver la nota de cabecera sobre el decodificador.
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    para_hilo.poner_estado(
                        Estado::Terminada {
                            motivo: format!("no se pudo crear el runtime: {error}"),
                            limpia: false,
                        },
                        &ctx,
                    );
                    return;
                }
            };

            let resultado =
                runtime.block_on(sesion(destino, &gpu, &para_hilo, &ctx, receptor_entrada));

            // Resumen al cerrar: es lo unico que queda de la sesion cuando la ventana ya
            // solo ensena el motivo, y es lo primero que se mira cuando algo fue mal.
            tracing::info!(
                frames = para_hilo.frames(),
                huecos = para_hilo.huecos(),
                keyframes_pedidos = para_hilo.keyframes_pedidos(),
                eventos_entrada = para_hilo.eventos_entrada(),
                "sesion terminada"
            );

            let estado = match resultado {
                Ok(()) => Estado::Terminada {
                    motivo: "La conexion se cerro sin errores.".to_owned(),
                    limpia: true,
                },
                Err(error) => Estado::Terminada {
                    motivo: format!("{error}"),
                    limpia: false,
                },
            };
            para_hilo.poner_estado(estado, &ctx);
        })
        .expect("no se pudo lanzar el hilo de sesion");

    (compartido, emisor_entrada)
}

/// Cuerpo de la sesion.
async fn sesion(
    destino: SocketAddr,
    gpu: &Gpu,
    compartido: &Arc<Compartido>,
    ctx: &eframe::egui::Context,
    receptor_entrada: tokio::sync::mpsc::UnboundedReceiver<Message>,
) -> Result<(), TransportError> {
    install_crypto_provider();

    // Puerto efimero: el viewer no necesita uno fijo.
    let endpoint = Endpoint::bind(SocketAddr::from(([0, 0, 0, 0], 0)))?;
    let sesion = endpoint.connect(destino).await?;
    tracing::info!(%destino, "conectado");

    compartido.poner_estado(Estado::Negociando, ctx);
    let mut control = sesion.open_control().await?;
    let codec = handshake(&mut control).await?;
    tracing::info!(?codec, "sesion negociada");

    let (mut emisor, receptor_control) = control.split();

    // El primer keyframe se pide en cuanto se puede: el host acaba de aceptar y su
    // codificador no tiene por que haber emitido ninguno todavia.
    emisor
        .send(&Message::KeyframeRequest(KeyframeRequest {
            monitor: 0,
            reason: KeyframeReason::Startup,
        }))
        .await?;
    compartido.keyframes_pedidos.fetch_add(1, Ordering::Relaxed);
    compartido.poner_estado(Estado::Esperando, ctx);

    // Las tareas que si son `Send`.
    let control_tarea = tokio::spawn(leer_control(
        receptor_control,
        Arc::clone(compartido),
        ctx.clone(),
    ));
    let datagramas = tokio::spawn(leer_datagramas(
        sesion.clone(),
        Arc::clone(compartido),
        ctx.clone(),
    ));
    let entrada = tokio::spawn(enviar_entrada(sesion.clone(), receptor_entrada));

    let resultado = bucle_video(&sesion, gpu, compartido, ctx, codec, &mut emisor).await;

    control_tarea.abort();
    datagramas.abort();
    entrada.abort();
    sesion.close();
    endpoint.wait_idle().await;

    resultado
}

/// Intercambia `Hello` y recoge el codec elegido por el host.
async fn handshake(
    control: &mut vhdesk_transport::ControlChannel,
) -> Result<VideoCodec, TransportError> {
    control
        .send(&Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            role: Role::Viewer,
            video_codecs: CODECS.to_vec(),
            audio_codecs: Vec::new(),
            peer_name: nombre_de_esta_maquina(),
        }))
        .await?;

    let saludo = control.recv().await?;
    let Message::Hello(hello) = saludo else {
        return Err(fallo(format!(
            "el host empezo con {} en vez de Hello",
            saludo.name()
        )));
    };
    if hello.protocol_version != PROTOCOL_VERSION {
        return Err(fallo(format!(
            "version de protocolo incompatible: el host habla la {} y este viewer la {PROTOCOL_VERSION}",
            hello.protocol_version
        )));
    }
    if hello.role != Role::Host {
        return Err(fallo(format!(
            "al otro lado hay un {:?}, no un host",
            hello.role
        )));
    }

    let respuesta = control.recv().await?;
    let Message::AuthResponse(AuthResponse {
        result,
        video_codec,
        ..
    }) = respuesta
    else {
        return Err(fallo(format!(
            "se esperaba AuthResponse y llego {}",
            respuesta.name()
        )));
    };

    // FASE 2: aqui hay que **rechazar** un `Accepted` que este viewer no haya solicitado.
    // Hoy no se envia ningun `AuthRequest`, asi que aceptar esto es exactamente el otro
    // lado de la deuda de seguridad que documenta el host: cerrar solo el emisor dejaria a
    // este viewer creyendose cualquier `Accepted` espontaneo. Ver los invariantes de
    // CLAUDE.md.
    if result != AuthResult::Accepted {
        return Err(fallo(format!("el host rechazo la sesion: {result:?}")));
    }

    video_codec.ok_or_else(|| fallo("el host acepto sin elegir codec de video".to_owned()))
}

/// Bucle principal: recibe, decodifica y sube a la GPU.
async fn bucle_video(
    sesion: &Session,
    gpu: &Gpu,
    compartido: &Arc<Compartido>,
    ctx: &eframe::egui::Context,
    codec: VideoCodec,
    emisor: &mut vhdesk_transport::ControlSender,
) -> Result<(), TransportError> {
    let mut decodificador = open_decoder(codec)
        .map_err(|e| fallo(format!("no se pudo abrir el decodificador: {e}")))?;
    let mut receptor = sesion.video_receiver();

    loop {
        match receptor.recv().await? {
            RecepcionVideo::Frame(frame) => {
                decodificar_y_subir(&mut decodificador, &frame.data, gpu, compartido, ctx)?;
            }
            RecepcionVideo::Hueco {
                esperado,
                recibido,
                pedir_keyframe,
            } => {
                compartido.huecos.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(esperado, recibido, "hueco en la secuencia de video");

                // La amortiguacion la lleva el receptor: aqui solo se obedece. Sin ella,
                // una red mala generaria una peticion por frame y el host respondería con
                // un keyframe de ~100 KB a cada una.
                if pedir_keyframe {
                    emisor
                        .send(&Message::KeyframeRequest(KeyframeRequest {
                            monitor: 0,
                            reason: KeyframeReason::Gap,
                        }))
                        .await?;
                    compartido.keyframes_pedidos.fetch_add(1, Ordering::Relaxed);
                    tracing::info!("keyframe pedido tras un hueco");
                }
            }
            RecepcionVideo::Descartado(motivo) => {
                tracing::trace!(?motivo, "frame descartado");
            }
        }
    }
}

/// Decodifica un frame y lo sube a la textura, sin copiarlo por el camino.
fn decodificar_y_subir(
    decodificador: &mut Box<dyn VideoDecoder>,
    datos: &[u8],
    gpu: &Gpu,
    compartido: &Arc<Compartido>,
    ctx: &eframe::egui::Context,
) -> Result<(), TransportError> {
    let Some(frame) = decodificador
        .decode(datos)
        .map_err(|e| fallo(format!("fallo al decodificar: {e}")))?
    else {
        // El decodificador todavia no tiene con que producir imagen; pasa antes del primer
        // keyframe y no es un error.
        return Ok(());
    };

    let planos = I420Planes {
        width: frame.width,
        height: frame.height,
        y: frame.y,
        u: frame.u,
        v: frame.v,
        y_stride: frame.y_stride as u32,
        uv_stride: frame.uv_stride as u32,
    };

    if let Ok(mut guardia) = compartido.renderer.lock() {
        let hay_que_crear = guardia
            .as_ref()
            .is_none_or(|r| r.width() != frame.width || r.height() != frame.height);

        if hay_que_crear {
            tracing::info!(
                ancho = frame.width,
                alto = frame.height,
                "creando el pipeline de video"
            );
            *guardia = Some(VideoRenderer::new(
                &gpu.device,
                frame.width,
                frame.height,
                gpu.formato,
            ));
            if let Ok(mut dimensiones) = compartido.dimensiones.lock() {
                *dimensiones = Some((frame.width, frame.height));
            }
        }

        if let Some(renderer) = guardia.as_mut() {
            renderer.upload(&gpu.queue, &planos);
        }
    }

    let anteriores = compartido.frames.fetch_add(1, Ordering::Relaxed);
    if anteriores == 0 {
        compartido.poner_estado(Estado::Activa, ctx);
    } else {
        // El repintado se pide desde aqui, que es donde se sabe que hay imagen nueva. Con
        // la ventana quieta egui no repinta por su cuenta, asi que sin esto el video se
        // congelaria aunque siguieran llegando frames.
        ctx.request_repaint();
    }

    Ok(())
}

/// Escribe en el stream de input lo que la interfaz vaya dejando en la cola.
///
/// El orden de la cola es el orden del cable: el stream de input es fiable y ordenado, asi
/// que un modificador encolado antes que su clic llega antes que su clic.
async fn enviar_entrada(
    sesion: Session,
    mut receptor: tokio::sync::mpsc::UnboundedReceiver<Message>,
) {
    let mut emisor = match sesion.open_input().await {
        Ok(emisor) => emisor,
        Err(error) => {
            tracing::warn!(%error, "no se pudo abrir el canal de entrada: la sesion sera de solo mirar");
            return;
        }
    };

    while let Some(mensaje) = receptor.recv().await {
        if let Err(error) = emisor.send(&mensaje).await {
            // La conexion se fue. No se insiste: el bucle de video vera el mismo error y es
            // el que decide como termina la sesion.
            tracing::debug!(%error, "termina el envio de entrada");
            return;
        }
    }
}

/// Atiende el canal de control mientras dure la sesion.
async fn leer_control(
    mut receptor: vhdesk_transport::ControlReceiver,
    compartido: Arc<Compartido>,
    ctx: eframe::egui::Context,
) {
    while let Ok(mensaje) = receptor.recv().await {
        match mensaje {
            // La forma llega por control y no por datagrama porque no cabe: un puntero de
            // 32x32 en RGBA son 4 KiB y el maximo de datagrama medido son 1414 bytes.
            Message::Cursor(Cursor::Shape {
                hotspot_x,
                hotspot_y,
                width,
                height,
                rgba,
            }) => {
                tracing::debug!(width, height, "forma de cursor recibida");
                if let Ok(mut forma) = compartido.forma_cursor.lock() {
                    *forma = Some(FormaCursor {
                        width: u32::from(width),
                        height: u32::from(height),
                        hotspot_x: u32::from(hotspot_x),
                        hotspot_y: u32::from(hotspot_y),
                        rgba,
                    });
                }
                compartido.version_forma.fetch_add(1, Ordering::Relaxed);
                ctx.request_repaint();
            }
            otro => tracing::debug!(mensaje = otro.name(), "mensaje de control"),
        }
    }
}

/// Atiende los datagramas: posicion del cursor y sondas.
async fn leer_datagramas(sesion: Session, compartido: Arc<Compartido>, ctx: eframe::egui::Context) {
    while let Ok(mensaje) = sesion.recv_datagram().await {
        match mensaje {
            Message::Cursor(Cursor::Position { x, y, .. }) => {
                if let Ok(mut cursor) = compartido.cursor.lock() {
                    *cursor = Some(CursorRemoto {
                        x,
                        y,
                        visible: true,
                    });
                }
            }
            Message::Cursor(Cursor::Hidden) => {
                // Anidado y no encadenado con `&&`: las let-chains se estabilizaron en
                // 1.88 y la MSRV de este workspace es 1.85. El job `msrv` del CI lo caza.
                if let Ok(mut cursor) = compartido.cursor.lock() {
                    if let Some(actual) = cursor.as_mut() {
                        actual.visible = false;
                    }
                }
            }
            otro => tracing::trace!(mensaje = otro.name(), "datagrama"),
        }
        // El cursor si se dibuja, asi que hay que repintar. Es lo que hace que el puntero
        // remoto se mueva sin esperar al siguiente frame de video, que es justamente por lo
        // que viaja aparte. egui agrupa las peticiones, asi que un raton de 1000 Hz no
        // produce mil repintados sino como mucho uno por refresco.
        ctx.request_repaint();
    }
}

fn fallo(mensaje: String) -> TransportError {
    // El transporte no tiene una variante para "el peer dijo algo que no encaja", y crearla
    // solo para esto ensuciaria su API. `Certificate` lleva un texto libre y es la que menos
    // miente de las que hay.
    TransportError::Certificate(mensaje)
}

/// Nombre legible de esta maquina, para que el host lo muestre.
fn nombre_de_esta_maquina() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "viewer".to_owned())
}

//! Viewer de mentira que verifica el host completo **antes de que exista ventana**.
//!
//! No simula nada: conecta de verdad, hace el handshake de verdad, decodifica el video con
//! libvpx de verdad y reporta lo que llego. Es lo que cierra el bloque E1.
//!
//! ```text
//! # en la maquina que sirve
//! cargo run -p vhdesk-host --release -- --listen 0.0.0.0:21118
//!
//! # en la otra (o en la misma, con 127.0.0.1)
//! cargo run -p vhdesk-host --release --example sumidero -- --connect 192.168.1.50:21118
//! ```
//!
//! Opciones: `--segundos N` cuanto durar, `--png N` volcar los N primeros frames
//! decodificados a disco para mirarlos con los ojos, y `--input` para ejercitar tambien el
//! camino de vuelta.
//!
//! **`--input` mueve el raton de verdad en la maquina servida**, asi que esta desactivado
//! por defecto: sin el, el sumidero abre el stream de input (que el host necesita para
//! completar el arranque) pero no manda nada por el.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use vhdesk_codec::{I420Frame, open_decoder};
use vhdesk_proto::{
    AudioCodec, Cursor, Hello, InputEvent, KeyframeReason, KeyframeRequest, Message,
    PROTOCOL_VERSION, Role, VideoCodec,
};
use vhdesk_transport::{
    Endpoint, MotivoDescarte, RecepcionVideo, Session, install_crypto_provider,
};

/// Cada cuantos frames recibidos se pide un keyframe a proposito.
///
/// No hace falta para ver imagen: sirve para ejercitar el cuarto disparador de keyframe, que
/// es el unico que no se dispara solo durante una sesion sana.
const PEDIR_KEYFRAME_CADA: u64 = 90;

struct Opciones {
    destino: SocketAddr,
    segundos: u64,
    pngs: u32,
    input: bool,
}

#[derive(Default)]
struct Cuenta {
    frames: u64,
    keyframes: u64,
    huecos: u64,
    descartes_emisor: u64,
    tardios: u64,
    bytes: u64,
    decodificados: u64,
    posiciones_cursor: u64,
    formas_cursor: u64,
    /// Tamanos por tipo, para separar el coste de un keyframe del de un inter.
    bytes_keyframe: u64,
    bytes_inter: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("VHDESK_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    install_crypto_provider();
    let opciones = opciones()?;

    let endpoint = Endpoint::bind(SocketAddr::from(([0, 0, 0, 0], 0)))?;
    let sesion = endpoint
        .connect(opciones.destino)
        .await
        .with_context(|| format!("conectar con {}", opciones.destino))?;
    println!("conectado con {}", sesion.remote_address());

    let (codec, control) = handshake(&sesion).await?;
    println!("codec negociado: {codec:?}\n");

    // El canal de control **se conserva abierto**: es por donde el host manda las formas de
    // cursor durante toda la sesion. Cerrarlo tras el handshake dejaria al host escribiendo
    // en un stream muerto.
    let (mut control_tx, control_rx) = control.split();

    // El host espera este stream para terminar de arrancar: hay que abrirlo aunque no se
    // vaya a mandar nada por el.
    let mut input = sesion
        .open_input()
        .await
        .context("abrir el canal de input")?;

    let cuenta = recibir(
        &sesion,
        codec,
        &opciones,
        &mut input,
        &mut control_tx,
        control_rx,
    )
    .await?;

    sesion.close();
    endpoint.wait_idle().await;

    informar(&cuenta, opciones.segundos);
    veredicto(&cuenta)
}

fn opciones() -> Result<Opciones> {
    let mut destino = None;
    let mut segundos = 10u64;
    let mut pngs = 0u32;
    let mut input = false;

    let mut argumentos = std::env::args().skip(1);
    while let Some(bandera) = argumentos.next() {
        let mut valor = || {
            argumentos
                .next()
                .with_context(|| format!("a {bandera} le falta el valor"))
        };
        match bandera.as_str() {
            "--connect" => destino = Some(valor()?.parse().context("direccion invalida")?),
            "--segundos" => segundos = valor()?.parse().context("segundos invalidos")?,
            "--png" => pngs = valor()?.parse().context("numero de png invalido")?,
            "--input" => input = true,
            otro => bail!("argumento desconocido: {otro}; usa --connect <ip:puerto>"),
        }
    }

    Ok(Opciones {
        destino: destino.context("falta --connect <ip:puerto>")?,
        segundos,
        pngs,
        input,
    })
}

/// Hello del viewer, respuesta del host y codec elegido.
///
/// Devuelve tambien el canal de control, que **no debe cerrarse**: sigue vivo toda la sesion
/// para las formas de cursor y las peticiones de keyframe.
async fn handshake(sesion: &Session) -> Result<(VideoCodec, vhdesk_transport::ControlChannel)> {
    let mut control = sesion.open_control().await.context("abrir el control")?;

    control
        .send(&Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            role: Role::Viewer,
            video_codecs: vec![VideoCodec::Vp8],
            audio_codecs: vec![AudioCodec::Opus],
            peer_name: "sumidero".to_owned(),
        }))
        .await
        .context("enviar el Hello")?;

    let respuesta = control.recv().await.context("esperar el Hello del host")?;
    let Message::Hello(hello) = respuesta else {
        bail!("el host respondio {} en vez de Hello", respuesta.name());
    };
    println!(
        "host: {} (protocolo {})",
        hello.peer_name, hello.protocol_version
    );

    let respuesta = control.recv().await.context("esperar el codec elegido")?;
    let Message::AuthResponse(auth) = respuesta else {
        bail!(
            "el host respondio {} en vez de AuthResponse",
            respuesta.name()
        );
    };

    let codec = auth
        .video_codec
        .context("el host acepto la sesion pero no eligio codec de video")?;

    Ok((codec, control))
}

#[allow(clippy::too_many_arguments)]
async fn recibir(
    sesion: &Session,
    codec: VideoCodec,
    opciones: &Opciones,
    input: &mut vhdesk_transport::InputSender,
    control_tx: &mut vhdesk_transport::ControlSender,
    control_rx: vhdesk_transport::ControlReceiver,
) -> Result<Cuenta> {
    let mut receptor = sesion.video_receiver();
    let mut decoder = open_decoder(codec).context("abrir el decodificador")?;
    let mut destino = I420Frame::new(2, 2).context("reservar el I420")?;
    let mut cuenta = Cuenta::default();
    let mut volcados = 0u32;

    let limite = Instant::now() + Duration::from_secs(opciones.segundos);
    println!("recibiendo durante {} s...\n", opciones.segundos);

    if opciones.input {
        // Un movimiento al centro del monitor servido: visible, inequivoco y reversible.
        input
            .send(&Message::InputEvent(InputEvent::MouseMoveAbsolute {
                monitor: 0,
                x: 0.5,
                y: 0.5,
            }))
            .await
            .context("enviar el movimiento de raton")?;
        println!("input -> raton al centro del monitor servido");
    }

    let posiciones = Arc::new(AtomicU64::new(0));
    let formas = Arc::new(AtomicU64::new(0));
    let tarea_posiciones = tokio::spawn(contar_posiciones(sesion.clone(), Arc::clone(&posiciones)));
    let tarea_formas = tokio::spawn(contar_formas(control_rx, Arc::clone(&formas)));

    while Instant::now() < limite {
        let restante = limite.saturating_duration_since(Instant::now());
        let Ok(recepcion) = tokio::time::timeout(restante, receptor.recv()).await else {
            break;
        };

        match recepcion.context("recibir video")? {
            RecepcionVideo::Frame(frame) => {
                cuenta.frames += 1;
                cuenta.bytes += frame.data.len() as u64;
                if frame.keyframe {
                    cuenta.keyframes += 1;
                    cuenta.bytes_keyframe += frame.data.len() as u64;
                } else {
                    cuenta.bytes_inter += frame.data.len() as u64;
                }

                if let Some(decodificado) = decoder
                    .decode(&frame.data)
                    .context("decodificar el frame")?
                {
                    cuenta.decodificados += 1;
                    if volcados < opciones.pngs {
                        decodificado
                            .copy_into(&mut destino)
                            .context("copiar el decodificado")?;
                        guardar_png(&destino, volcados)?;
                        volcados += 1;
                    }
                }

                if cuenta.frames % PEDIR_KEYFRAME_CADA == 0 {
                    pedir_keyframe(control_tx).await?;
                }
            }
            RecepcionVideo::Hueco {
                esperado,
                recibido,
                pedir_keyframe: hay_que_pedirlo,
            } => {
                cuenta.huecos += 1;
                tracing::debug!(esperado, recibido, "hueco en la secuencia");
                if hay_que_pedirlo {
                    pedir_keyframe(control_tx).await?;
                }
            }
            RecepcionVideo::Descartado(MotivoDescarte::EmisorDescarto) => {
                cuenta.descartes_emisor += 1;
            }
            RecepcionVideo::Descartado(MotivoDescarte::Tardio) => cuenta.tardios += 1,
        }
    }

    tarea_posiciones.abort();
    tarea_formas.abort();
    cuenta.posiciones_cursor = posiciones.load(Ordering::Relaxed);
    cuenta.formas_cursor = formas.load(Ordering::Relaxed);

    Ok(cuenta)
}

/// Pide un keyframe por el canal de control que ya esta abierto.
async fn pedir_keyframe(control: &mut vhdesk_transport::ControlSender) -> Result<()> {
    control
        .send(&Message::KeyframeRequest(KeyframeRequest {
            monitor: 0,
            reason: KeyframeReason::Gap,
        }))
        .await
        .context("pedir keyframe")?;
    Ok(())
}

/// Cuenta las posiciones de cursor, que llegan por datagrama.
async fn contar_posiciones(sesion: Session, cuenta: Arc<AtomicU64>) {
    while let Ok(mensaje) = sesion.recv_datagram().await {
        if matches!(
            mensaje,
            Message::Cursor(Cursor::Position { .. } | Cursor::Hidden)
        ) {
            cuenta.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Cuenta las formas de cursor, que llegan por el canal de control porque no caben en un
/// datagrama: un puntero de 32x32 en RGBA son 4 KB y el maximo medido ronda los 1400 bytes.
async fn contar_formas(mut control: vhdesk_transport::ControlReceiver, cuenta: Arc<AtomicU64>) {
    while let Ok(mensaje) = control.recv().await {
        match mensaje {
            Message::Cursor(Cursor::Shape { width, height, .. }) => {
                tracing::debug!(width, height, "forma de cursor nueva");
                cuenta.fetch_add(1, Ordering::Relaxed);
            }
            otro => tracing::debug!(mensaje = otro.name(), "control"),
        }
    }
}

/// Convierte I420 a RGB y guarda un PNG.
///
/// La conversion es BT.601 rango limitado, el mismo convenio con el que codifica el host.
/// Sirve de referencia contra la que comparar el shader del viewer en el bloque E2: si la
/// imagen del PNG esta bien y la de la ventana no, el fallo esta en el shader.
fn guardar_png(frame: &I420Frame, indice: u32) -> Result<()> {
    let (ancho, alto) = (frame.width() as usize, frame.height() as usize);
    let mut rgb = vec![0u8; ancho * alto * 3];
    let croma_ancho = frame.chroma_width() as usize;

    for y in 0..alto {
        for x in 0..ancho {
            let luma = f32::from(frame.y()[y * ancho + x]);
            let indice_croma = (y / 2) * croma_ancho + (x / 2);
            let u = f32::from(frame.u()[indice_croma]);
            let v = f32::from(frame.v()[indice_croma]);

            // Rango limitado: Y en 16..=235 y croma centrado en 128.
            let yy = 1.164 * (luma - 16.0);
            let uu = u - 128.0;
            let vv = v - 128.0;

            let destino = (y * ancho + x) * 3;
            rgb[destino] = (yy + 1.596 * vv).clamp(0.0, 255.0) as u8;
            rgb[destino + 1] = (yy - 0.392 * uu - 0.813 * vv).clamp(0.0, 255.0) as u8;
            rgb[destino + 2] = (yy + 2.017 * uu).clamp(0.0, 255.0) as u8;
        }
    }

    let nombre = format!("sumidero-{indice:03}.png");
    let archivo =
        std::fs::File::create(Path::new(&nombre)).with_context(|| format!("crear {nombre}"))?;
    let mut encoder = png::Encoder::new(
        std::io::BufWriter::new(archivo),
        frame.width(),
        frame.height(),
    );
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .context("cabecera del png")?
        .write_image_data(&rgb)
        .context("escribir el png")?;

    println!("volcado {nombre}");
    Ok(())
}

fn informar(cuenta: &Cuenta, segundos: u64) {
    let segundos_f = segundos.max(1) as f64;
    let inter = cuenta.frames - cuenta.keyframes;

    let mut filas: BTreeMap<&str, String> = BTreeMap::new();
    filas.insert("frames recibidos", cuenta.frames.to_string());
    filas.insert("frames decodificados", cuenta.decodificados.to_string());
    filas.insert("keyframes", cuenta.keyframes.to_string());
    filas.insert("huecos", cuenta.huecos.to_string());
    filas.insert("descartes del emisor", cuenta.descartes_emisor.to_string());
    filas.insert("frames tardios", cuenta.tardios.to_string());
    filas.insert("posiciones de cursor", cuenta.posiciones_cursor.to_string());

    println!("\n--- {segundos} s de sesion ---");
    for (nombre, valor) in &filas {
        println!("{nombre:>24}  {valor}");
    }

    println!(
        "{:>24}  {:.1}",
        "fps medios",
        cuenta.frames as f64 / segundos_f
    );
    println!(
        "{:>24}  {:.2} Mbps",
        "ancho de banda",
        (cuenta.bytes as f64 * 8.0) / segundos_f / 1_000_000.0
    );
    if cuenta.keyframes > 0 {
        println!(
            "{:>24}  {:.1} KB",
            "keyframe medio",
            cuenta.bytes_keyframe as f64 / cuenta.keyframes as f64 / 1024.0
        );
    }
    if inter > 0 {
        println!(
            "{:>24}  {:.1} KB",
            "inter medio",
            cuenta.bytes_inter as f64 / inter as f64 / 1024.0
        );
    }
}

fn veredicto(cuenta: &Cuenta) -> Result<()> {
    if cuenta.frames == 0 {
        bail!("no llego ningun frame: mueve algo en la pantalla del host y vuelve a probar");
    }
    if cuenta.keyframes == 0 {
        bail!("no llego ningun keyframe: el viewer no tendria por donde engancharse");
    }
    if cuenta.decodificados != cuenta.frames {
        bail!(
            "llegaron {} frames pero solo se decodificaron {}",
            cuenta.frames,
            cuenta.decodificados
        );
    }

    println!("\nOK: el pipeline del host entrega video decodificable");
    Ok(())
}

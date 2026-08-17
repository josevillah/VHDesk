//! Eco entre dos procesos: comprueba los cuatro canales de una sesion contra la red real.
//!
//! Los roles son los de verdad, no simetricos: **quien escucha hace de host** (recibe
//! input, envia video) y **quien conecta hace de viewer** (envia input, recibe video). Es
//! importante que sea asi, porque la direccion es lo unico que distingue un stream
//! unidireccional de input de uno de video.
//!
//! ```text
//! # en la maquina A
//! cargo run -p vhdesk-transport --example echo -- --listen 0.0.0.0:21118
//!
//! # en la maquina B
//! cargo run -p vhdesk-transport --example echo -- --connect 192.168.1.50:21118
//! ```
//!
//! Para probarlo en una sola maquina, `--connect 127.0.0.1:21118`.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use vhdesk_proto::{
    AudioCodec, Hello, InputEvent, KeyframeReason, KeyframeRequest, Message, MouseButton,
    PROTOCOL_VERSION, Ping, Role, VideoCodec,
};
use vhdesk_transport::{Endpoint, FrameSaliente, RecepcionVideo, Session, install_crypto_provider};

/// Frames de video que envia el host durante la prueba.
const FRAMES: u32 = 30;
/// Eventos de input que envia el viewer.
const EVENTOS: u32 = 10;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("VHDESK_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Explicito y lo primero: rustls no elige proveedor criptografico por su cuenta, y si
    // falta el fallo aparece mucho despues y en un sitio que no orienta.
    install_crypto_provider();

    match modo()? {
        Modo::Escuchar(addr) => hacer_de_host(addr).await,
        Modo::Conectar(addr) => hacer_de_viewer(addr).await,
    }
}

enum Modo {
    Escuchar(SocketAddr),
    Conectar(SocketAddr),
}

fn modo() -> Result<Modo> {
    let mut argumentos = std::env::args().skip(1);
    let bandera = argumentos.next().context("falta --listen o --connect")?;
    let valor = argumentos.next().context("falta la direccion")?;
    let addr: SocketAddr = valor.parse().context("direccion invalida")?;

    match bandera.as_str() {
        "--listen" => Ok(Modo::Escuchar(addr)),
        "--connect" => Ok(Modo::Conectar(addr)),
        otro => bail!("argumento desconocido: {otro}; usa --listen o --connect"),
    }
}

async fn hacer_de_host(addr: SocketAddr) -> Result<()> {
    let endpoint = Endpoint::bind(addr)?;
    println!("host escuchando en {}", endpoint.local_addr()?);

    let sesion = endpoint.accept().await?;
    println!("viewer conectado desde {}\n", sesion.remote_address());

    // Control: el host acepta, el viewer abre.
    let mut control = sesion.accept_control().await?;
    let saludo = control.recv().await?;
    println!("control  <- {}", saludo.name());
    control.send(&saludo).await?;
    println!("control  -> eco devuelto");

    let recepcion_input = tokio::spawn(recibir_input(sesion.clone()));
    let recepcion_datagramas = tokio::spawn(recibir_datagramas(sesion.clone()));

    // Video: lo envia el host. Un stream unidireccional por frame.
    let mut emisor = sesion.video_sender();
    println!("\nvideo    -> enviando {FRAMES} frames");
    for indice in 0..FRAMES {
        emisor.send_frame(frame_de_prueba(indice))?;
        // Sin esta pausa se encolarian los 30 de golpe y el descarte por obsolescencia se
        // dispararia siempre, que no es el comportamiento que se quiere observar.
        tokio::time::sleep(Duration::from_millis(33)).await;
    }
    println!(
        "video    -> {} descartados por obsoletos",
        emisor.descartados()
    );

    // Se da margen a que llegue lo que falte antes de cerrar.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let eventos = recepcion_input.await?;
    let datagramas = recepcion_datagramas.await?;

    println!("\ninput    <- {eventos} eventos recibidos");
    println!("datagrama<- {datagramas} recibidos");

    sesion.close();
    endpoint.wait_idle().await;

    if eventos == EVENTOS && datagramas > 0 {
        println!("\nOK: los cuatro canales funcionan");
        Ok(())
    } else {
        bail!("faltaron mensajes: {eventos} eventos y {datagramas} datagramas")
    }
}

async fn hacer_de_viewer(addr: SocketAddr) -> Result<()> {
    // Puerto efimero: el viewer no necesita uno fijo.
    let endpoint = Endpoint::bind(SocketAddr::from(([0, 0, 0, 0], 0)))?;
    println!(
        "viewer conectando a {addr} desde {}",
        endpoint.local_addr()?
    );

    let sesion = endpoint.connect(addr).await?;
    println!("conectado\n");

    // Control: ida y vuelta, comprobando que vuelve identico.
    let mut control = sesion.open_control().await?;
    let saludo = Message::Hello(Hello {
        protocol_version: PROTOCOL_VERSION,
        role: Role::Viewer,
        video_codecs: vec![VideoCodec::Vp8],
        audio_codecs: vec![AudioCodec::Opus],
        peer_name: "eco".to_owned(),
    });

    let ida = Instant::now();
    control.send(&saludo).await?;
    let vuelta = control.recv().await?;
    let rtt = ida.elapsed();

    if vuelta != saludo {
        bail!("el eco de control no coincide con lo enviado");
    }
    println!(
        "control  ida y vuelta correcta, {:.2} ms",
        rtt.as_secs_f64() * 1000.0
    );

    // Input: lo envia el viewer, con prioridad por encima del video.
    let mut input = sesion.open_input().await?;
    for indice in 0..EVENTOS {
        input
            .send(&Message::InputEvent(InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: indice % 2 == 0,
            }))
            .await?;
    }
    println!("input    -> {EVENTOS} eventos enviados");

    // Datagramas: posicion del cursor y sondas. La forma del cursor NO cabe aqui.
    if let Some(maximo) = sesion.max_datagram_size() {
        println!("datagrama maximo admitido: {maximo} bytes");
    }
    for indice in 0..EVENTOS {
        sesion.send_datagram(&Message::Ping(Ping {
            nonce: u64::from(indice),
            sent_us: 0,
        }))?;
    }
    println!("datagrama-> {EVENTOS} sondas enviadas");

    // Video: lo recibe el viewer, que ademas vigila los huecos de secuencia.
    let mut receptor = sesion.video_receiver();
    let mut recibidos = 0u32;
    let mut descartados = 0u32;
    let mut huecos = 0u32;
    let mut peticiones = 0u32;
    let limite = Instant::now() + Duration::from_secs(15);

    while recibidos + descartados + huecos < FRAMES && Instant::now() < limite {
        match receptor.recv().await {
            Ok(RecepcionVideo::Frame(frame)) => {
                recibidos += 1;
                if recibidos == 1 {
                    println!(
                        "\nvideo    <- primer frame: seq={}, {}x{}, {} bytes, keyframe={}",
                        frame.sequence,
                        frame.width,
                        frame.height,
                        frame.data.len(),
                        frame.keyframe
                    );
                }
            }
            Ok(RecepcionVideo::Descartado(_)) => descartados += 1,
            Ok(RecepcionVideo::Hueco {
                esperado,
                recibido,
                pedir_keyframe,
            }) => {
                huecos += 1;
                if pedir_keyframe {
                    peticiones += 1;
                    // Por el canal de control, que es fiable: una peticion perdida dejaria
                    // la sesion con imagen rota hasta el siguiente hueco.
                    control
                        .send(&Message::KeyframeRequest(KeyframeRequest {
                            monitor: 0,
                            reason: KeyframeReason::Gap,
                        }))
                        .await?;
                    println!(
                        "video    <- hueco: esperaba {esperado} y llego {recibido}; keyframe pedido"
                    );
                }
            }
            Err(_) => break,
        }
    }

    println!(
        "video    <- {recibidos} recibidos, {descartados} descartados, {huecos} huecos, \
         {peticiones} peticiones de keyframe"
    );

    sesion.close();
    endpoint.wait_idle().await;

    if recibidos > 0 {
        println!("\nOK: los cuatro canales funcionan");
        Ok(())
    } else {
        bail!("no llego ningun frame de video")
    }
}

async fn recibir_input(sesion: Session) -> u32 {
    let Ok(mut input) = sesion.accept_input().await else {
        return 0;
    };

    let mut recibidos = 0;
    while let Ok(mensaje) = input.recv().await {
        if matches!(mensaje, Message::InputEvent(_)) {
            recibidos += 1;
        }
    }
    recibidos
}

async fn recibir_datagramas(sesion: Session) -> u32 {
    let mut recibidos = 0;
    while let Ok(mensaje) = sesion.recv_datagram().await {
        if matches!(mensaje, Message::Ping(_) | Message::Cursor(_)) {
            recibidos += 1;
        }
    }
    recibidos
}

/// Frame sintetico con el tamano tipico de un inter-frame de 1080p.
///
/// No lleva numero de secuencia: lo pone el transporte, que es el unico que puede
/// garantizar que sea monotono por sesion.
fn frame_de_prueba(indice: u32) -> FrameSaliente {
    let relleno: Vec<u8> = (0..10_000u32).map(|b| (b ^ indice) as u8).collect();

    FrameSaliente {
        monitor: 0,
        codec: VideoCodec::Vp8,
        keyframe: indice == 0,
        timestamp_us: u64::from(indice) * 33_333,
        width: 1920,
        height: 1080,
        data: Bytes::from(relleno),
    }
}

//! Orquestacion de una sesion: handshake, hilos del pipeline y tareas de red.
//!
//! ```text
//!   [hilo captura] --ranura(1)--> [hilo conversion+encode] --> VideoSender --> red
//!         |
//!         +-- posicion de cursor --> watch(1) --> datagramas
//!         +-- forma de cursor -----> mpsc ------> canal de control
//!
//!   red --> InputReceiver --> [tarea input] --> InputInjector
//!   red --> ControlReceiver -> [tarea control] --> senal de keyframe
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use tokio::sync::{mpsc, watch};
use vhdesk_capture::MonitorInfo;
use vhdesk_input::open_injector;
use vhdesk_proto::{
    AuthResponse, AuthResult, Cursor, Hello, MAX_PEER_NAME_LEN, Message, PROTOCOL_VERSION, Role,
    VideoCodec, negotiate,
};
use vhdesk_transport::{ControlSender, SenalKeyframe, Session};

use crate::captura::{self, SalidaCursor};
use crate::cli::Cli;
use crate::codificacion::{self, Ajustes};
use crate::entrada;
use crate::ranura::Ranura;

/// Formas de cursor que pueden esperar a salir por el canal de control.
///
/// Una forma nueva es rara (cambia al pasar el puntero por un borde o una caja de texto),
/// asi que 16 es holgado. **Esta cola no descarta a proposito**: una forma perdida no se
/// recupera nunca, porque el sistema solo la manda cuando cambia.
const COLA_CONTROL: usize = 16;

/// Codecs de video que este host sabe producir, en orden de preferencia.
///
/// El host decide porque es quien codifica. FASE 4: delante de VP8 iran los de hardware.
const CODECS_PREFERIDOS: &[VideoCodec] = &[VideoCodec::Vp8];

/// Atiende una sesion completa hasta que se cierra.
///
/// # Errores
///
/// Devuelve error si el handshake falla o si alguna etapa no puede arrancar. El cierre de
/// la conexion **no** es un error: es el final normal.
pub async fn servir(cli: &Cli, sesion: Session, monitor: &MonitorInfo) -> Result<()> {
    let inicio = Instant::now();

    let mut control = sesion
        .accept_control()
        .await
        .context("aceptar el control")?;
    let codec = handshake(&mut control).await?;
    // A partir de aqui los dos sentidos del control los lleva gente distinta: una tarea
    // espera bloqueada a que llegue un `KeyframeRequest` mientras otra manda formas de
    // cursor.
    let (mut control_tx, mut control_rx) = control.split();

    let emisor = sesion.video_sender();
    let senal = emisor.senal_keyframe();

    // --- camino de ida: captura -> encode -> red -------------------------------------
    let ranura = Arc::new(Ranura::nueva());
    let parar = Arc::new(AtomicBool::new(false));

    let (pos_tx, pos_rx) = watch::channel(None);
    let (forma_tx, forma_rx) = mpsc::channel(COLA_CONTROL);

    let hilo_captura = {
        let (monitor, ranura, parar) = (monitor.clone(), Arc::clone(&ranura), Arc::clone(&parar));
        let indice = cli.monitor;
        std::thread::Builder::new()
            .name("vhdesk-captura".to_owned())
            .spawn(move || {
                let salida = SalidaCursor {
                    posicion: pos_tx,
                    forma: forma_tx,
                };
                captura::bucle(&monitor, indice, &ranura, &salida, &parar)
            })
            .context("lanzar el hilo de captura")?
    };

    let hilo_encode = {
        let ranura = Arc::clone(&ranura);
        let senal = senal.clone();
        let handle = tokio::runtime::Handle::current();
        let ajustes = Ajustes {
            codec,
            monitor: cli.monitor,
            bitrate_kbps: cli.bitrate_kbps,
            fps: cli.fps,
            inicio,
        };
        std::thread::Builder::new()
            .name("vhdesk-encode".to_owned())
            .spawn(move || codificacion::bucle(&ranura, &senal, emisor, &ajustes, &handle))
            .context("lanzar el hilo de codificacion")?
    };

    // --- tareas de red ----------------------------------------------------------------
    let mut tarea_input = tokio::spawn(atender_input(sesion.clone(), cli.monitor, monitor.clone()));
    let mut tarea_control =
        tokio::spawn(async move { escuchar_control(&mut control_rx, senal).await });
    let tarea_salida = tokio::spawn(async move { enviar_control(&mut control_tx, forma_rx).await });
    let tarea_cursor = tokio::spawn(enviar_cursor(sesion.clone(), pos_rx));

    tracing::info!(
        peer = %sesion.remote_address(),
        ?codec,
        monitor = %monitor.name,
        "sesion en marcha"
    );

    // El primero que termine da por acabada la sesion. Lo normal es que sea `cerrada`.
    let motivo = tokio::select! {
        error = sesion.cerrada() => format!("{error}"),
        resultado = &mut tarea_input => format!("termino el input: {resultado:?}"),
        resultado = &mut tarea_control => format!("termino el control: {resultado:?}"),
    };
    tracing::info!(%motivo, "la sesion termina");

    // --- cierre ordenado --------------------------------------------------------------
    sesion.close();
    parar.store(true, Ordering::Relaxed);
    // Cerrar la ranura es lo que despierta al hilo de encode, que si no se quedaria
    // bloqueado esperando un frame que ya nadie va a depositar.
    ranura.cerrar();

    tarea_input.abort();
    tarea_control.abort();
    tarea_salida.abort();
    tarea_cursor.abort();

    // Los `join` bloquean, y el de captura puede tardar lo que dure su espera de 100 ms.
    // Fuera del hilo del runtime para no quedarnos con un worker parado.
    tokio::task::spawn_blocking(move || {
        for (nombre, hilo) in [("captura", hilo_captura), ("encode", hilo_encode)] {
            match hilo.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::warn!(hilo = nombre, %error, "el hilo termino con error")
                }
                Err(_) => tracing::error!(hilo = nombre, "el hilo entro en panico"),
            }
        }
    })
    .await
    .context("esperar a los hilos del pipeline")?;

    Ok(())
}

/// Negocia version y codec con el viewer.
///
/// # El `AuthResponse` sin `AuthRequest` es deuda de seguridad, no una comodidad
///
/// El codec elegido vive en `AuthResponse.video_codec`, que es su sitio correcto en el
/// protocolo. Pero en la fase 1 no hay autenticacion, asi que ese `Accepted` sale **sin que
/// nadie haya presentado credenciales**. Eso es, literalmente, la forma de un bypass de
/// autenticacion, y hoy funciona porque el host no comprueba nada.
///
/// FASE 2, y hay que cerrarlo por los dos lados:
///
/// - el host **no debe emitir** `AuthResult::Accepted` sin un `AuthRequest` valido previo y
///   el consentimiento en pantalla de su dueno;
/// - la maquina de estados del **receptor** debe rechazar un `Accepted` que no haya
///   solicitado, en vez de creerselo.
///
/// Esta anotado tambien en los invariantes de seguridad de CLAUDE.md para que no se lea
/// como una simplificacion que ya funciona.
async fn handshake(control: &mut vhdesk_transport::ControlChannel) -> Result<VideoCodec> {
    let saludo = control
        .recv()
        .await
        .context("esperar el Hello del viewer")?;
    let Message::Hello(hello) = saludo else {
        bail!("el viewer empezo con {} en vez de Hello", saludo.name());
    };

    if hello.protocol_version != PROTOCOL_VERSION {
        bail!(
            "version de protocolo incompatible: el viewer habla la {} y este host la {}",
            hello.protocol_version,
            PROTOCOL_VERSION
        );
    }
    if hello.role != Role::Viewer {
        bail!(
            "quien conecta tiene que ser un viewer, no un {:?}",
            hello.role
        );
    }

    let codec = negotiate(CODECS_PREFERIDOS, &hello.video_codecs).with_context(|| {
        format!(
            "sin codec en comun: el viewer ofrece {:?} y este host produce {CODECS_PREFERIDOS:?}",
            hello.video_codecs
        )
    })?;

    tracing::info!(
        peer_name = %hello.peer_name,
        version = hello.protocol_version,
        "viewer identificado"
    );

    control
        .send(&Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            role: Role::Host,
            video_codecs: CODECS_PREFERIDOS.to_vec(),
            audio_codecs: Vec::new(),
            peer_name: nombre_de_esta_maquina(),
        }))
        .await
        .context("enviar el Hello del host")?;

    control
        .send(&Message::AuthResponse(AuthResponse {
            // FASE 2: esto pasa a depender de credenciales validas y del consentimiento del
            // dueno. Ver la nota de seguridad de esta funcion.
            result: AuthResult::Accepted,
            video_codec: Some(codec),
            // FASE 5: audio.
            audio_codec: None,
        }))
        .await
        .context("enviar el codec elegido")?;

    Ok(codec)
}

/// Nombre legible de esta maquina, para el dialogo de consentimiento de la fase 2.
///
/// No es un identificador ni autentica nada: lo elige la maquina y puede mentir. Se recorta
/// al maximo del protocolo para que un nombre absurdo no haga fallar la validacion.
fn nombre_de_esta_maquina() -> String {
    let mut nombre = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "vhdesk-host".to_owned());
    nombre.truncate(MAX_PEER_NAME_LEN);
    nombre
}

/// Recibe input y lo inyecta.
async fn atender_input(sesion: Session, indice: u8, monitor: MonitorInfo) -> Result<()> {
    let receptor = sesion.accept_input().await.context("aceptar el input")?;
    let mut injector = open_injector().context("abrir el injector de entrada")?;
    let geometria = entrada::geometria_de(&monitor);

    entrada::bucle(receptor, indice, geometria, injector.as_mut()).await
}

/// Atiende lo que llega por el canal de control.
async fn escuchar_control(
    control: &mut vhdesk_transport::ControlReceiver,
    senal: SenalKeyframe,
) -> Result<()> {
    loop {
        let mensaje = match control.recv().await {
            Ok(mensaje) => mensaje,
            Err(error) => {
                tracing::debug!(%error, "termina el canal de control");
                return Ok(());
            }
        };

        match mensaje {
            Message::KeyframeRequest(peticion) => {
                // Cuarto disparador de keyframe: la red de seguridad para lo que el emisor
                // no puede saber (decodificador desincronizado, perdida real de red).
                tracing::debug!(?peticion.reason, "el viewer pide keyframe");
                senal.pedir();
            }
            Message::Ping(_) | Message::Pong(_) => {
                // FASE 1, bloque F: las sondas de latencia se responden cuando haya con que
                // medirlas. Hoy el keepalive lo hace QUIC por debajo.
                tracing::trace!("sonda de latencia recibida");
            }
            otro => tracing::debug!(mensaje = otro.name(), "mensaje de control no esperado aqui"),
        }
    }
}

/// Envia por el canal de control lo que le pasen, empezando por las formas de cursor.
async fn enviar_control(
    control: &mut ControlSender,
    mut cola: mpsc::Receiver<Cursor>,
) -> Result<()> {
    while let Some(cursor) = cola.recv().await {
        control
            .send(&Message::Cursor(cursor))
            .await
            .context("enviar una forma de cursor")?;
    }
    Ok(())
}

/// Manda la posicion del cursor por datagrama cada vez que cambia.
///
/// Por datagrama porque es diminuta y caduca: si una se pierde, la siguiente la sustituye
/// por completo, y retransmitirla seria entregar tarde una posicion que ya no es la buena.
async fn enviar_cursor(sesion: Session, mut posicion: watch::Receiver<Option<Cursor>>) {
    while posicion.changed().await.is_ok() {
        let Some(cursor) = posicion.borrow_and_update().clone() else {
            continue;
        };

        if let Err(error) = sesion.send_datagram(&Message::Cursor(cursor)) {
            // Perder una posicion de cursor no justifica cortar nada: la siguiente llega en
            // milisegundos y la sustituye entera.
            tracing::trace!(%error, "no se pudo enviar la posicion del cursor");
        }
    }
}

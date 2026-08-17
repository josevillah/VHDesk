//! Los cuatro canales sobre una conexion QUIC real por loopback.
//!
//! No hacen falta dos procesos ni dos maquinas: es una conexion QUIC de verdad, con su
//! handshake TLS y su socket UDP, dentro del mismo test. Corre en CI, que es donde tiene
//! que corregir las regresiones.

use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use vhdesk_proto::{
    AudioCodec, Cursor, Hello, InputEvent, Message, MouseButton, PROTOCOL_VERSION, Ping, Role,
    VideoCodec,
};
use vhdesk_transport::{
    Endpoint, FrameSaliente, RecepcionVideo, Session, TransportError, install_crypto_provider,
};

/// Levanta las dos puntas de una sesion por loopback.
///
/// Devuelve `(host, viewer)` con los roles reales: el host escucha, recibe input y envia
/// video; el viewer conecta, envia input y recibe video.
async fn sesion() -> (Endpoint, Session, Endpoint, Session) {
    install_crypto_provider();

    let local = SocketAddr::from(([127, 0, 0, 1], 0));
    let host_endpoint = Endpoint::bind(local).expect("abrir el endpoint del host");
    let addr = host_endpoint.local_addr().expect("direccion local");

    let viewer_endpoint = Endpoint::bind(local).expect("abrir el endpoint del viewer");

    // El accept del host y el connect del viewer tienen que correr a la vez: si se espera
    // a uno antes de lanzar el otro, no se completa el handshake nunca.
    let aceptar = tokio::spawn(async move {
        let sesion = host_endpoint.accept().await.expect("aceptar");
        (host_endpoint, sesion)
    });

    let viewer = viewer_endpoint.connect(addr).await.expect("conectar");
    let (host_endpoint, host) = aceptar.await.expect("tarea de accept");

    (host_endpoint, host, viewer_endpoint, viewer)
}

fn saludo() -> Message {
    Message::Hello(Hello {
        protocol_version: PROTOCOL_VERSION,
        role: Role::Viewer,
        video_codecs: vec![VideoCodec::Vp8],
        audio_codecs: vec![AudioCodec::Opus],
        peer_name: "test".to_owned(),
    })
}

#[tokio::test]
async fn el_canal_de_control_hace_ida_y_vuelta() {
    let (_he, host, _ve, viewer) = sesion().await;

    // Se le pasa un clon a la tarea y se conserva `host` aqui: la conexion se cierra
    // cuando se suelta la ultima `Session`, y si la tarea se llevara la unica, al terminar
    // cerraria la conexion antes de que al viewer le diera tiempo a leer el eco.
    let host_eco = host.clone();
    let eco = tokio::spawn(async move {
        let mut control = host_eco.accept_control().await.expect("aceptar control");
        let recibido = control.recv().await.expect("recibir");
        control.send(&recibido).await.expect("responder");
        recibido
    });

    let mut control = viewer.open_control().await.expect("abrir control");
    control.send(&saludo()).await.expect("enviar");
    let vuelta = control.recv().await.expect("recibir eco");

    assert_eq!(vuelta, saludo());
    assert_eq!(eco.await.expect("tarea de eco"), saludo());
    drop(host);
}

#[tokio::test]
async fn el_input_llega_por_su_stream_y_en_orden() {
    let (_he, host, _ve, viewer) = sesion().await;

    let recepcion = tokio::spawn(async move {
        let mut input = host.accept_input().await.expect("aceptar input");
        let mut recibidos = Vec::new();
        for _ in 0..3 {
            recibidos.push(input.recv().await.expect("recibir input"));
        }
        recibidos
    });

    let mut input = viewer.open_input().await.expect("abrir input");
    let eventos = [
        Message::InputEvent(InputEvent::MouseMoveAbsolute {
            monitor: 0,
            x: 0.25,
            y: 0.5,
        }),
        Message::InputEvent(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        }),
        Message::InputEvent(InputEvent::Key {
            scancode: 0x0007_0004,
            pressed: true,
        }),
    ];
    for evento in &eventos {
        input.send(evento).await.expect("enviar input");
    }

    let recibidos = recepcion.await.expect("tarea de input");
    assert_eq!(recibidos, eventos, "el stream fiable conserva el orden");
}

fn frame(indice: u32, keyframe: bool, bytes: usize) -> FrameSaliente {
    FrameSaliente {
        monitor: 0,
        codec: VideoCodec::Vp8,
        keyframe,
        timestamp_us: u64::from(indice) * 33_333,
        width: 1920,
        height: 1080,
        data: Bytes::from(vec![indice as u8; bytes]),
    }
}

#[tokio::test]
async fn un_frame_de_video_llega_entero_por_su_propio_stream() {
    let (_he, host, _ve, viewer) = sesion().await;
    let mut receptor = viewer.video_receiver();

    let datos: Vec<u8> = (0..20_000u32).map(|b| b as u8).collect();
    let mut emisor = host.video_sender();
    emisor
        .send_frame(FrameSaliente {
            monitor: 0,
            codec: VideoCodec::Vp8,
            keyframe: true,
            timestamp_us: 12_345,
            width: 1920,
            height: 1080,
            data: Bytes::from(datos.clone()),
        })
        .expect("encolar frame");

    let RecepcionVideo::Frame(recibido) = receptor.recv().await.expect("recibir video") else {
        panic!("el frame se descarto cuando no deberia");
    };

    assert_eq!(recibido.sequence, 0, "el transporte numera desde cero");
    assert_eq!((recibido.width, recibido.height), (1920, 1080));
    assert!(recibido.keyframe);
    assert_eq!(recibido.timestamp_us, 12_345);
    assert_eq!(recibido.data.as_ref(), datos.as_slice());
}

#[tokio::test]
async fn varios_frames_seguidos_llegan_todos_cuando_hay_sitio() {
    let (_he, host, _ve, viewer) = sesion().await;
    let mut receptor = viewer.video_receiver();

    let mut emisor = host.video_sender();
    let mut recibidos = 0;

    // Se envia y se consume de forma intercalada, que es como funciona un viewer real. Si
    // se enviaran los cinco antes de leer ninguno, la cola del receptor (profundidad 4) se
    // llenaria y el ultimo se tiraria: comportamiento correcto, pero no lo que se mide
    // aqui.
    for esperado in 0..5u64 {
        emisor
            .send_frame(frame(esperado as u32, esperado == 0, 1024))
            .expect("encolar");

        match tokio::time::timeout(Duration::from_secs(5), receptor.recv()).await {
            Ok(Ok(RecepcionVideo::Frame(f))) => {
                assert_eq!(f.sequence, esperado, "la secuencia debe ser consecutiva");
                recibidos += 1;
            }
            otro => panic!("no llego el frame {esperado}: {otro:?}"),
        }
    }

    assert_eq!(recibidos, 5, "se perdieron frames sin haber congestion");
    assert_eq!(emisor.descartados(), 0);
    assert!(
        !emisor.keyframe_pendiente(),
        "sin descartes no deberia quedar keyframe pendiente"
    );
}

#[tokio::test]
async fn el_emisor_marca_keyframe_pendiente_al_descartar() {
    let (_he, host, _ve, _viewer) = sesion().await;
    let mut emisor = host.video_sender();

    // El primer frame ya viene con keyframe pendiente: sin el, el viewer no engancha.
    assert!(emisor.keyframe_pendiente());
    emisor.send_frame(frame(0, true, 1024)).expect("keyframe");
    assert!(!emisor.keyframe_pendiente());

    // Frames grandes en bucle apretado, sin dejar que salga ninguno: el emisor tiene que
    // ir abortando los anteriores.
    for indice in 1..40u32 {
        emisor
            .send_frame(frame(indice, false, 512 * 1024))
            .expect("encolar");
    }

    assert!(
        emisor.descartados() > 0,
        "el bucle apretado deberia haber forzado descartes"
    );
    assert!(
        emisor.keyframe_pendiente(),
        "tras descartar, el emisor sabe que rompio la cadena y debe forzar keyframe sin \
         esperar a que se lo pidan"
    );
}

#[tokio::test]
async fn un_hueco_se_detecta_y_pide_keyframe_una_sola_vez() {
    let (_he, host, _ve, viewer) = sesion().await;
    let mut receptor = viewer.video_receiver();
    let mut emisor = host.video_sender();

    // Keyframe inicial, que el receptor acepta y fija como referencia.
    emisor.send_frame(frame(0, true, 1024)).expect("keyframe");
    let RecepcionVideo::Frame(primero) = receptor.recv().await.expect("recibir") else {
        panic!("el keyframe inicial deberia aceptarse");
    };
    assert_eq!(primero.sequence, 0);

    // Frames grandes en bucle apretado para que el emisor descarte y aparezcan huecos.
    for indice in 1..40u32 {
        emisor
            .send_frame(frame(indice, false, 512 * 1024))
            .expect("encolar");
    }
    assert!(emisor.descartados() > 0, "hacian falta descartes");

    let mut huecos = 0;
    let mut peticiones = 0;

    // Se recogen unos cuantos resultados; los que lleguen seran una mezcla de descartes
    // del emisor y huecos, y lo que se comprueba es la amortiguacion.
    for _ in 0..10 {
        match tokio::time::timeout(Duration::from_secs(2), receptor.recv()).await {
            Ok(Ok(RecepcionVideo::Hueco { pedir_keyframe, .. })) => {
                huecos += 1;
                if pedir_keyframe {
                    peticiones += 1;
                }
            }
            Ok(Ok(_)) => {}
            _ => break,
        }
    }

    assert!(
        huecos > 0,
        "el receptor deberia haber detectado algun hueco"
    );
    assert_eq!(
        peticiones, 1,
        "con {huecos} huecos seguidos solo debe pedirse un keyframe: sin amortiguacion, \
         una red mala genera una tormenta de keyframes de ~100 KB"
    );
}

#[tokio::test]
async fn un_datagrama_pequeno_llega() {
    let (_he, host, _ve, viewer) = sesion().await;

    let sonda = Message::Ping(Ping {
        nonce: 0xabcd,
        sent_us: 99,
    });
    viewer.send_datagram(&sonda).expect("enviar datagrama");

    let recibido = tokio::time::timeout(Duration::from_secs(5), host.recv_datagram())
        .await
        .expect("no llego el datagrama")
        .expect("recibir");

    assert_eq!(recibido, sonda);
}

#[tokio::test]
async fn la_posicion_del_cursor_cabe_en_un_datagrama_pero_la_forma_no() {
    let (_he, _host, _ve, viewer) = sesion().await;

    // La posicion son unos pocos bytes.
    let posicion = Message::Cursor(Cursor::Position {
        monitor: 0,
        x: 0.5,
        y: 0.5,
    });
    viewer
        .send_datagram(&posicion)
        .expect("la posicion tiene que caber en un datagrama");

    // La forma no: un puntero de 32x32 en RGBA son 4 KB y el limite ronda los 1200 bytes.
    // Por eso la forma va por el canal de control y no por datagrama.
    let forma = Message::Cursor(Cursor::Shape {
        hotspot_x: 0,
        hotspot_y: 0,
        width: 32,
        height: 32,
        rgba: vec![0u8; 32 * 32 * 4],
    });

    assert!(
        matches!(
            viewer.send_datagram(&forma),
            Err(TransportError::DatagramTooLarge { .. })
        ),
        "la forma del cursor no debe poder enviarse por datagrama"
    );
}

#[tokio::test]
async fn el_endpoint_reporta_su_direccion_y_la_del_peer() {
    let (_he, host, _ve, viewer) = sesion().await;

    assert_eq!(host.remote_address().ip(), viewer.remote_address().ip());
    assert!(
        viewer.max_datagram_size().is_some(),
        "QUIC negocio datagramas"
    );
}

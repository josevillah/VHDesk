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

/// Una sesion sin nada de trafico **no se cae sola**.
///
/// Es el fallo mas espectacular y mas trivial de reproducir que puede tener este proyecto:
/// los keyframes son bajo demanda y una pantalla inmovil no genera frames, asi que un
/// usuario que se levante a por un cafe deja de producir trafico de video por completo. Con
/// los valores por defecto de quinn (`max_idle_timeout` de 30 s y `keep_alive_interval` en
/// `None`), la conexion moria en medio minuto y volver al teclado encontraba la sesion
/// cerrada.
///
/// El test tarda mas que el timeout a proposito: comprobar la configuracion en vez del
/// comportamiento no demostraria nada, porque el timeout efectivo es el minimo de lo que
/// anuncian los dos peers y basta con olvidarse de un lado para romperlo.
#[tokio::test]
async fn una_sesion_ociosa_sobrevive_al_timeout_de_inactividad() {
    let (_he, host, _ve, viewer) = sesion().await;

    // El canal se abre antes del silencio para que lo que se mida despues sea la conexion y
    // no el establecimiento del stream.
    let aceptar = tokio::spawn({
        let host = host.clone();
        async move { host.accept_control().await.expect("aceptar control") }
    });
    let mut control_viewer = viewer.open_control().await.expect("abrir control");
    control_viewer
        .send(&saludo())
        .await
        .expect("saludo inicial");
    let mut control_host = aceptar.await.expect("tarea");
    control_host.recv().await.expect("saludo inicial");

    // Silencio absoluto durante mas de lo que dura el timeout. Lo unico que puede circular
    // por aqui es el PING de keepalive de QUIC.
    let silencio = vhdesk_transport::IDLE_TIMEOUT + Duration::from_secs(3);
    tokio::time::sleep(silencio).await;

    // Y despues del silencio la conexion tiene que seguir sirviendo.
    control_viewer
        .send(&saludo())
        .await
        .expect("la conexion murio durante el silencio");

    let recibido = tokio::time::timeout(Duration::from_secs(5), control_host.recv())
        .await
        .expect("no llego nada tras el silencio")
        .expect("la conexion murio durante el silencio");

    assert_eq!(
        recibido.name(),
        "Hello",
        "tras {silencio:?} de inactividad la sesion tiene que seguir en pie"
    );
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
    // se enviaran los cinco antes de leer ninguno, la ranura del receptor desalojaria los
    // atrasados y solo sobreviviria el ultimo: comportamiento correcto y deliberado, pero
    // no lo que se mide aqui. Eso lo cubre `la_ranura_llena_desaloja_el_frame_viejo`.
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

/// Con la ranura llena sobrevive el frame **nuevo**, no el viejo.
///
/// Es el cambio de comportamiento del bloque E y merece quedar fijado contra la red real,
/// no solo en la funcion pura. Antes la cola descartaba lo que acababa de llegar y dejaba al
/// consumidor arrastrando imagen atrasada; ahora es al reves, que es lo que exige el
/// criterio de latencia: un frame retrasado ya no vale nada cuando se entrega.
#[tokio::test]
async fn la_ranura_llena_desaloja_el_frame_viejo() {
    let (_he, host, _ve, viewer) = sesion().await;
    let mut receptor = viewer.video_receiver();
    let mut emisor = host.video_sender();

    emisor.send_frame(frame(0, true, 1024)).expect("keyframe");
    match tokio::time::timeout(Duration::from_secs(5), receptor.recv()).await {
        Ok(Ok(RecepcionVideo::Frame(f))) => assert_eq!(f.sequence, 0),
        otro => panic!("no llego el keyframe inicial: {otro:?}"),
    }

    // Tres frames que llegan enteros mientras nadie los recoge. La pausa es para que cada
    // stream termine y el emisor no aborte el anterior: lo que se quiere observar es el
    // desalojo en el **receptor**, no el descarte en el emisor.
    for secuencia in 1..=3u32 {
        emisor
            .send_frame(frame(secuencia, false, 1024))
            .expect("encolar");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        emisor.descartados(),
        0,
        "el emisor no deberia haber abortado nada: la pausa le da tiempo de sobra"
    );

    // El unico que queda es el ultimo. Como es inter-frame y se saltaron dos, la politica
    // lo declara hueco, que es exactamente la senal que queremos: mejor un keyframe que
    // pintar imagen vieja.
    match tokio::time::timeout(Duration::from_secs(5), receptor.recv()).await {
        Ok(Ok(RecepcionVideo::Hueco {
            esperado, recibido, ..
        })) => {
            assert_eq!(esperado, 1);
            assert_eq!(
                recibido, 3,
                "sobrevivio el frame equivocado: la ranura desalojo el nuevo en vez del viejo"
            );
        }
        otro => panic!("se esperaba un hueco con el frame mas reciente: {otro:?}"),
    }
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

//! Ida y vuelta del backend VP8: codificar y decodificar de verdad, con libvpx.
//!
//! No van marcados `#[ignore]` como los de DXGI: estos no necesitan pantalla, solo la
//! biblioteca enlazada, asi que corren en CI y son la red de seguridad de todo el bloque B.

use vhdesk_codec::{
    CodecError, EncoderConfig, I420Frame, VideoDecoder, VideoEncoder, Vp8Decoder, Vp8Encoder,
};
use vhdesk_proto::VideoCodec;

const ANCHO: u32 = 320;
const ALTO: u32 = 240;

/// Genera un patron BGRA con un rectangulo desplazado segun `paso`.
///
/// Un frame liso se codificaria a casi nada y no probaria gran cosa; este tiene bordes y
/// movimiento, que es lo que ejercita al codificador de verdad.
fn escena(paso: u32) -> Vec<u8> {
    let mut datos = vec![0u8; (ANCHO * ALTO * 4) as usize];
    let desplazamiento = (paso * 7) % (ANCHO / 2);

    for y in 0..ALTO {
        for x in 0..ANCHO {
            let p = ((y * ANCHO + x) * 4) as usize;
            let dentro = (desplazamiento..desplazamiento + ANCHO / 3).contains(&x)
                && (ALTO / 4..ALTO * 3 / 4).contains(&y);

            let (b, g, r) = if dentro {
                (40u8, 200u8, 90u8)
            } else {
                (200, 60, 30)
            };

            datos[p] = b;
            datos[p + 1] = g;
            datos[p + 2] = r;
            datos[p + 3] = 255;
        }
    }

    datos
}

/// Escena completamente distinta de la anterior, para provocar un cambio de plano.
///
/// Un degradado diagonal no comparte nada con el rectangulo de [`escena`]: es lo que una
/// heuristica de deteccion de cambio de escena tiene que ver como corte.
fn escena_alterna(paso: u32) -> Vec<u8> {
    let mut datos = vec![0u8; (ANCHO * ALTO * 4) as usize];
    let fase = (paso % 64) as u8;

    for y in 0..ALTO {
        for x in 0..ANCHO {
            let p = ((y * ANCHO + x) * 4) as usize;
            let diagonal = ((x + y) % 256) as u8;

            datos[p] = diagonal.wrapping_add(fase);
            datos[p + 1] = diagonal.wrapping_mul(3);
            datos[p + 2] = 255u8.wrapping_sub(diagonal);
            datos[p + 3] = 255;
        }
    }

    datos
}

fn frame_i420(paso: u32) -> I420Frame {
    i420_desde(&escena(paso))
}

fn frame_i420_alterno(paso: u32) -> I420Frame {
    i420_desde(&escena_alterna(paso))
}

fn i420_desde(bgra: &[u8]) -> I420Frame {
    let mut frame = I420Frame::new(ANCHO, ALTO).expect("crear I420");
    frame
        .fill_from_bgra(bgra, (ANCHO * 4) as usize)
        .expect("convertir a I420");
    frame
}

fn codificador() -> Vp8Encoder {
    Vp8Encoder::new(EncoderConfig::lan(ANCHO, ALTO)).expect("crear codificador VP8")
}

/// Error absoluto medio entre el plano de luminancia original y el decodificado.
fn error_medio_luma(original: &I420Frame, decodificado: &[u8], stride: usize) -> f64 {
    let mut suma = 0u64;
    let mut cuantos = 0u64;

    for y in 0..ALTO as usize {
        let fila_original = &original.y()[y * ANCHO as usize..][..ANCHO as usize];
        let fila_decodificada = &decodificado[y * stride..][..ANCHO as usize];

        for (a, b) in fila_original.iter().zip(fila_decodificada) {
            suma += u64::from(a.abs_diff(*b));
            cuantos += 1;
        }
    }

    suma as f64 / cuantos as f64
}

#[test]
fn un_frame_codificado_se_decodifica_parecido_al_original() {
    let mut encoder = codificador();
    let mut decoder = Vp8Decoder::new().expect("crear decodificador VP8");

    let original = frame_i420(0);
    let comprimido = encoder
        .encode(&original, 0)
        .expect("codificar")
        .expect("el primer frame siempre produce salida");

    assert!(
        comprimido.keyframe,
        "el primer frame de una sesion tiene que ser keyframe"
    );
    assert!(!comprimido.data.is_empty());
    assert_eq!(comprimido.timestamp_us, 0);

    let decodificado = decoder
        .decode(&comprimido.data)
        .expect("decodificar")
        .expect("un keyframe siempre produce imagen");

    assert_eq!((decodificado.width, decodificado.height), (ANCHO, ALTO));

    let error = error_medio_luma(&original, decodificado.y, decodificado.y_stride);
    assert!(
        error < 8.0,
        "el error medio de luminancia es {error:.2}, demasiado para un keyframe: \
         apunta a planos cruzados o a un espacio de color equivocado, no a compresion"
    );
}

#[test]
fn comprime_de_verdad() {
    let mut encoder = codificador();
    let original = frame_i420(0);

    let comprimido = encoder
        .encode(&original, 0)
        .expect("codificar")
        .expect("hay salida");

    let sin_comprimir = original.y().len() + original.u().len() + original.v().len();
    assert!(
        comprimido.data.len() * 4 < sin_comprimir,
        "{} bytes comprimidos frente a {sin_comprimir} sin comprimir: no esta comprimiendo",
        comprimido.data.len()
    );
}

#[test]
fn una_secuencia_se_decodifica_entera_y_los_intermedios_no_son_keyframes() {
    let mut encoder = codificador();
    let mut decoder = Vp8Decoder::new().expect("crear decodificador");

    let mut keyframes = 0;
    let mut decodificados = 0;

    for paso in 0..30u32 {
        let frame = frame_i420(paso);
        let timestamp = u64::from(paso) * 16_666;

        let Some(comprimido) = encoder.encode(&frame, timestamp).expect("codificar") else {
            continue;
        };
        if comprimido.keyframe {
            keyframes += 1;
        }

        let salida = decoder.decode(&comprimido.data).expect("decodificar");
        if salida.is_some() {
            decodificados += 1;
        }
    }

    assert_eq!(decodificados, 30, "se perdieron frames por el camino");
    assert_eq!(
        keyframes, 1,
        "sin keyframes periodicos, de 30 frames solo el primero deberia serlo"
    );
}

/// Fija por escrito la decision de keyframes bajo demanda.
///
/// Son 300 frames, mas que los 240 que habria durado el intervalo periodico que se retiro
/// (4 s a 60 fps), y con cambios de escena bruscos cada 30 frames. Si alguien devuelve
/// `kf_mode` a `VPX_KF_AUTO`, este test falla por las dos razones a la vez: libvpx volveria
/// a insertar keyframes periodicos **y** por deteccion de cambio de escena, que en un
/// escritorio se dispara cada vez que el usuario cambia de ventana.
#[test]
fn no_hay_keyframes_periodicos_ni_por_cambio_de_escena() {
    let mut encoder = codificador();

    let mut keyframes = 0;
    for paso in 0..300u32 {
        // Cada 30 frames la pantalla entera cambia de contenido, que es justo lo que la
        // heuristica de cambio de escena de libvpx busca.
        let frame = if (paso / 30) % 2 == 0 {
            frame_i420(paso)
        } else {
            frame_i420_alterno(paso)
        };

        let Some(comprimido) = encoder
            .encode(&frame, u64::from(paso) * 16_666)
            .expect("codificar")
        else {
            continue;
        };
        if comprimido.keyframe {
            keyframes += 1;
        }
    }

    assert_eq!(
        keyframes, 1,
        "salieron {keyframes} keyframes en 300 frames; solo debe salir el de arranque, \
         porque nadie mas los pidio"
    );
}

/// Pantalla quieta mas peticion explicita: el keyframe tiene que salir.
///
/// Es el escenario de un viewer que se reengancha a una maquina inactiva, y es el caso mas
/// probable de todos. Si el host cortocircuitara por "no hay nada que codificar" antes de
/// mirar si hay un keyframe pedido, el viewer se quedaria esperando una imagen que no llega
/// nunca. Aqui se fija la mitad que le toca al codec: **el mismo frame, sin un solo pixel
/// distinto, tiene que producir keyframe cuando se pide**.
#[test]
fn con_la_pantalla_quieta_una_peticion_sigue_produciendo_keyframe() {
    let mut encoder = codificador();
    let quieto = frame_i420(0);

    encoder.encode(&quieto, 0).expect("primer frame");

    // Unos cuantos frames identicos: el codificador los comprime a casi nada.
    for paso in 1..10u32 {
        encoder
            .encode(&quieto, u64::from(paso) * 16_666)
            .expect("frame quieto");
    }

    encoder.request_keyframe();
    let salida = encoder
        .encode(&quieto, 10 * 16_666)
        .expect("codificar tras la peticion")
        .expect("una peticion de keyframe siempre produce salida, aunque nada haya cambiado");

    assert!(
        salida.keyframe,
        "con la pantalla quieta la peticion de keyframe se perdio: un viewer que se \
         reenganche a una maquina inactiva se quedaria con la pantalla en blanco"
    );
}

#[test]
fn request_keyframe_fuerza_el_siguiente_frame() {
    let mut encoder = codificador();

    encoder.encode(&frame_i420(0), 0).expect("primer frame");
    let segundo = encoder
        .encode(&frame_i420(1), 16_666)
        .expect("segundo frame")
        .expect("hay salida");
    assert!(
        !segundo.keyframe,
        "el segundo frame no deberia ser keyframe"
    );

    encoder.request_keyframe();
    let tercero = encoder
        .encode(&frame_i420(2), 33_333)
        .expect("tercer frame")
        .expect("hay salida");

    assert!(
        tercero.keyframe,
        "request_keyframe no forzo el keyframe del siguiente frame"
    );
}

#[test]
fn un_frame_de_otro_tamano_se_rechaza() {
    let mut encoder = codificador();
    let mut otro = I420Frame::new(ANCHO / 2, ALTO).expect("crear");
    otro.fill_from_bgra(
        &vec![0u8; (ANCHO / 2 * ALTO * 4) as usize],
        (ANCHO / 2 * 4) as usize,
    )
    .expect("convertir");

    assert!(matches!(
        encoder.encode(&otro, 0),
        Err(CodecError::DimensionsChanged { .. })
    ));
}

#[test]
fn el_codec_declarado_es_vp8() {
    assert_eq!(codificador().codec(), VideoCodec::Vp8);
    assert_eq!(Vp8Decoder::new().expect("crear").codec(), VideoCodec::Vp8);
}

// --- Entradas hostiles ----------------------------------------------------------------

#[test]
fn un_frame_vacio_se_rechaza_en_vez_de_cerrar_el_flujo() {
    let mut decoder = Vp8Decoder::new().expect("crear decodificador");

    // Para libvpx un buffer vacio significa "fin del flujo". Si se lo pasaramos tal cual,
    // un frame perdido en la red dejaria el decodificador inservible para el resto de la
    // sesion.
    assert!(matches!(
        decoder.decode(&[]),
        Err(CodecError::InvalidBitstream(_))
    ));
}

#[test]
fn un_flujo_corrupto_falla_sin_entrar_en_panico() {
    let mut encoder = codificador();
    let mut decoder = Vp8Decoder::new().expect("crear decodificador");

    let bueno = encoder
        .encode(&frame_i420(0), 0)
        .expect("codificar")
        .expect("hay salida");

    // Se corrompe un byte de cada diez del keyframe. No se comprueba que falle (algunas
    // corrupciones dan un flujo que el decodificador acepta), solo que no reviente.
    for posicion in (0..bueno.data.len()).step_by(10) {
        let mut roto = bueno.data.to_vec();
        roto[posicion] ^= 0xff;
        let _ = decoder.decode(&roto);
    }
}

#[test]
fn basura_arbitraria_no_hace_reventar_al_decodificador() {
    let mut decoder = Vp8Decoder::new().expect("crear decodificador");

    for semilla in 0..64u8 {
        let basura: Vec<u8> = (0..=255u8)
            .map(|i| i.wrapping_mul(semilla).wrapping_add(semilla))
            .collect();
        let _ = decoder.decode(&basura);
    }

    // Y sigue vivo despues de todo eso.
    let mut encoder = codificador();
    let bueno = encoder
        .encode(&frame_i420(0), 0)
        .expect("codificar")
        .expect("hay salida");
    assert!(
        decoder.decode(&bueno.data).is_ok(),
        "el decodificador quedo inservible tras recibir basura"
    );
}

#[test]
fn unas_dimensiones_a_cero_se_rechazan_al_crear_el_codificador() {
    assert!(matches!(
        Vp8Encoder::new(EncoderConfig::lan(0, ALTO)),
        Err(CodecError::InvalidDimensions { .. })
    ));
}

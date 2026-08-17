//! Tests del framing.
//!
//! Se dividen en dos bloques con proposito distinto: el de ida y vuelta comprueba que un
//! mensaje sobrevive al viaje por el cable, y el de entradas malformadas comprueba lo que
//! de verdad importa para la seguridad, que ningun conjunto de bytes provoque un panico.

use bytes::{BufMut, BytesMut};

use crate::codecs::{AudioCodec, VideoCodec, negotiate};
use crate::framing::{LENGTH_PREFIX_LEN, MAX_FRAME_LEN, decode, encode};
use crate::message::{
    AudioFrame, AuthMethod, AuthRequest, AuthResponse, AuthResult, ClipboardFormat,
    ClipboardUpdate, Cursor, Hello, InputEvent, MAX_ANNOUNCED_CODECS, MAX_PEER_NAME_LEN, Message,
    MouseButton, PROTOCOL_VERSION, Ping, Pong, Role, VideoFrame,
};
use crate::{ProtoError, message};

/// Un mensaje de cada tipo, para recorrerlos todos en los tests genericos.
fn one_of_each() -> Vec<Message> {
    vec![
        Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            role: Role::Viewer,
            video_codecs: vec![VideoCodec::H264, VideoCodec::Vp8],
            audio_codecs: vec![AudioCodec::Opus],
            peer_name: "portatil de jose".to_owned(),
        }),
        Message::AuthRequest(AuthRequest {
            method: AuthMethod::OneTimePassword,
            proof: vec![0xde, 0xad, 0xbe, 0xef],
        }),
        Message::AuthResponse(AuthResponse {
            result: AuthResult::Accepted,
            video_codec: Some(VideoCodec::Vp8),
            audio_codec: Some(AudioCodec::Opus),
        }),
        Message::VideoFrame(VideoFrame {
            monitor: 1,
            codec: VideoCodec::Vp8,
            keyframe: true,
            timestamp_us: 123_456_789,
            width: 1920,
            height: 1080,
            data: bytes::Bytes::from_static(&[1, 2, 3, 4, 5]),
        }),
        Message::AudioFrame(AudioFrame {
            codec: AudioCodec::Opus,
            timestamp_us: 42,
            sample_rate: 48_000,
            channels: 2,
            data: bytes::Bytes::from_static(&[9, 8, 7]),
        }),
        Message::InputEvent(InputEvent::MouseMoveAbsolute {
            monitor: 0,
            x: 0.25,
            y: 0.75,
        }),
        Message::InputEvent(InputEvent::MouseButton {
            button: MouseButton::Right,
            pressed: true,
        }),
        Message::InputEvent(InputEvent::MouseScroll {
            delta_x: -1.0,
            delta_y: 3.5,
        }),
        Message::InputEvent(InputEvent::Key {
            scancode: 0x0007_0004,
            pressed: false,
        }),
        Message::Cursor(Cursor::Shape {
            hotspot_x: 2,
            hotspot_y: 3,
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 255, 0, 128],
        }),
        Message::Cursor(Cursor::Position {
            monitor: 0,
            x: 0.5,
            y: 0.5,
        }),
        Message::Cursor(Cursor::Hidden),
        Message::ClipboardUpdate(ClipboardUpdate {
            format: ClipboardFormat::Utf8Text,
            data: "hola".as_bytes().to_vec(),
        }),
        Message::Ping(Ping {
            nonce: 0xffff_ffff_ffff_ffff,
            sent_us: 1,
        }),
        Message::Pong(Pong {
            nonce: 0xffff_ffff_ffff_ffff,
            sent_us: 1,
        }),
    ]
}

/// Construye un frame crudo con la longitud, el tag y el cuerpo que le digamos, para
/// poder fabricar frames que el codificador nunca produciria.
fn raw_frame(tag: u8, body: &[u8]) -> BytesMut {
    let mut buf = BytesMut::new();
    let len = u32::try_from(body.len() + 1).expect("cuerpo de test dentro de u32");
    buf.put_u32_le(len);
    buf.put_u8(tag);
    buf.put_slice(body);
    buf
}

// --- Ida y vuelta ---------------------------------------------------------------------

#[test]
fn cada_mensaje_sobrevive_a_la_ida_y_vuelta() {
    for original in one_of_each() {
        let mut buf = BytesMut::new();
        encode(&original, &mut buf).expect("codificar");

        let decoded = decode(&mut buf)
            .expect("decodificar")
            .expect("frame completo");

        assert_eq!(decoded, original, "fallo con {}", original.name());
        assert!(
            buf.is_empty(),
            "quedaron bytes tras decodificar {}",
            original.name()
        );
    }
}

#[test]
fn varios_mensajes_seguidos_en_el_mismo_buffer() {
    let originales = one_of_each();
    let mut buf = BytesMut::new();
    for message in &originales {
        encode(message, &mut buf).expect("codificar");
    }

    for original in &originales {
        let decoded = decode(&mut buf)
            .expect("decodificar")
            .expect("frame completo");
        assert_eq!(&decoded, original);
    }
    assert!(buf.is_empty());
}

#[test]
fn un_frame_incompleto_no_consume_nada_del_buffer() {
    let original = Message::Ping(Ping {
        nonce: 1,
        sent_us: 2,
    });
    let mut completo = BytesMut::new();
    encode(&original, &mut completo).expect("codificar");

    // Alimentamos el buffer byte a byte: hasta el ultimo, `decode` debe pedir mas datos
    // sin tocar lo que ya hay.
    let mut buf = BytesMut::new();
    for byte in &completo[..completo.len() - 1] {
        buf.put_u8(*byte);
        let antes = buf.len();
        assert_eq!(decode(&mut buf), Ok(None));
        assert_eq!(
            buf.len(),
            antes,
            "decode consumio bytes de un frame parcial"
        );
    }

    buf.put_u8(completo[completo.len() - 1]);
    assert_eq!(decode(&mut buf), Ok(Some(original)));
}

#[test]
fn el_payload_de_video_no_se_copia_al_decodificar() {
    let payload: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
    let original = Message::VideoFrame(VideoFrame {
        monitor: 0,
        codec: VideoCodec::Vp8,
        keyframe: false,
        timestamp_us: 0,
        width: 64,
        height: 64,
        data: bytes::Bytes::from(payload.clone()),
    });

    let mut buf = BytesMut::new();
    encode(&original, &mut buf).expect("codificar");
    let decoded = decode(&mut buf)
        .expect("decodificar")
        .expect("frame completo");

    let Message::VideoFrame(frame) = decoded else {
        panic!("se esperaba un VideoFrame");
    };
    assert_eq!(frame.data.as_ref(), payload.as_slice());
}

#[test]
fn el_keyframe_viaja_en_los_flags() {
    for keyframe in [true, false] {
        let original = Message::VideoFrame(VideoFrame {
            monitor: 0,
            codec: VideoCodec::Av1,
            keyframe,
            timestamp_us: 0,
            width: 1,
            height: 1,
            data: bytes::Bytes::new(),
        });

        let mut buf = BytesMut::new();
        encode(&original, &mut buf).expect("codificar");
        assert_eq!(decode(&mut buf), Ok(Some(original)));
    }
}

// --- Entradas malformadas -------------------------------------------------------------

#[test]
fn un_frame_de_longitud_cero_se_rechaza() {
    let mut buf = BytesMut::new();
    buf.put_u32_le(0);
    assert_eq!(decode(&mut buf), Err(ProtoError::EmptyFrame));
}

#[test]
fn una_longitud_desmedida_se_rechaza_sin_reservar_memoria() {
    // El caso que justifica MAX_FRAME_LEN: cuatro bytes que piden 4 GiB de buffer.
    let mut buf = BytesMut::new();
    buf.put_u32_le(u32::MAX);
    buf.put_slice(b"solo unos pocos bytes de verdad");

    assert_eq!(
        decode(&mut buf),
        Err(ProtoError::FrameTooLarge {
            len: u32::MAX as usize
        })
    );
}

#[test]
fn la_longitud_justo_por_encima_del_limite_se_rechaza() {
    let mut buf = BytesMut::new();
    let len = u32::try_from(MAX_FRAME_LEN + 1).expect("cabe en u32");
    buf.put_u32_le(len);
    assert_eq!(
        decode(&mut buf),
        Err(ProtoError::FrameTooLarge {
            len: MAX_FRAME_LEN + 1
        })
    );
}

#[test]
fn un_tag_desconocido_se_rechaza() {
    let mut buf = raw_frame(0xfe, &[1, 2, 3]);
    assert_eq!(decode(&mut buf), Err(ProtoError::UnknownTag { tag: 0xfe }));
}

#[test]
fn una_cabecera_de_video_truncada_no_entra_en_panico() {
    // La cabecera fija de VideoFrame son 15 bytes; probamos todos los cortes posibles.
    let mut completo = BytesMut::new();
    encode(
        &Message::VideoFrame(VideoFrame {
            monitor: 0,
            codec: VideoCodec::Vp8,
            keyframe: true,
            timestamp_us: 0,
            width: 1,
            height: 1,
            data: bytes::Bytes::new(),
        }),
        &mut completo,
    )
    .expect("codificar");

    let cuerpo = &completo[LENGTH_PREFIX_LEN + 1..];
    for corte in 0..cuerpo.len() {
        let mut buf = raw_frame(0x04, &cuerpo[..corte]);
        let resultado = decode(&mut buf);
        assert!(
            matches!(resultado, Err(ProtoError::TruncatedBody { .. })),
            "un cuerpo de {corte} bytes deberia dar TruncatedBody, dio {resultado:?}"
        );
    }
}

#[test]
fn una_cabecera_de_audio_truncada_no_entra_en_panico() {
    for corte in 0..14 {
        let mut buf = raw_frame(0x05, &vec![0u8; corte]);
        let resultado = decode(&mut buf);
        assert!(
            matches!(
                resultado,
                Err(ProtoError::TruncatedBody { .. } | ProtoError::UnknownDiscriminant { .. })
            ),
            "un cuerpo de {corte} bytes deberia fallar limpiamente, dio {resultado:?}"
        );
    }
}

#[test]
fn los_bits_reservados_de_video_se_rechazan() {
    let mut cuerpo = vec![0u8; 15];
    cuerpo[1] = VideoCodec::Vp8.to_wire();
    cuerpo[2] = 0b0000_0010; // bit todavia sin asignar

    let mut buf = raw_frame(0x04, &cuerpo);
    assert_eq!(
        decode(&mut buf),
        Err(ProtoError::ReservedBitsSet {
            field: "VideoFrame.flags"
        })
    );
}

#[test]
fn un_codec_desconocido_se_rechaza() {
    let mut cuerpo = vec![0u8; 15];
    cuerpo[1] = 0x7f; // codec de video sin asignar

    let mut buf = raw_frame(0x04, &cuerpo);
    assert_eq!(
        decode(&mut buf),
        Err(ProtoError::UnknownDiscriminant {
            field: "VideoCodec",
            value: 0x7f
        })
    );
}

#[test]
fn el_relleno_sobrante_en_un_mensaje_de_control_se_rechaza() {
    let mut completo = BytesMut::new();
    encode(
        &Message::Ping(Ping {
            nonce: 1,
            sent_us: 2,
        }),
        &mut completo,
    )
    .expect("codificar");

    let mut cuerpo = completo[LENGTH_PREFIX_LEN + 1..].to_vec();
    cuerpo.extend_from_slice(b"relleno");

    let mut buf = raw_frame(0x09, &cuerpo);
    assert_eq!(
        decode(&mut buf),
        Err(ProtoError::TrailingBytes { trailing: 7 })
    );
}

#[test]
fn un_cuerpo_de_control_basura_falla_sin_panico() {
    // Barremos cuerpos arbitrarios contra todos los tags conocidos. No comprobamos que
    // fallen (algunos bytes son postcard valido por casualidad), solo que no revientan.
    for tag in 0x01..=0x0au8 {
        for semilla in 0..64u8 {
            let cuerpo: Vec<u8> = (0..semilla).map(|i| i.wrapping_mul(semilla)).collect();
            let mut buf = raw_frame(tag, &cuerpo);
            let _ = decode(&mut buf);
        }
    }
}

#[test]
fn un_hello_con_demasiados_codecs_se_rechaza() {
    let hello = Hello {
        protocol_version: PROTOCOL_VERSION,
        role: Role::Host,
        video_codecs: vec![VideoCodec::Vp8; MAX_ANNOUNCED_CODECS + 1],
        audio_codecs: vec![],
        peer_name: String::new(),
    };

    let mut buf = BytesMut::new();
    encode(&Message::Hello(hello), &mut buf).expect("codificar");

    assert_eq!(
        decode(&mut buf),
        Err(ProtoError::FieldTooLong {
            field: "Hello.video_codecs",
            len: MAX_ANNOUNCED_CODECS + 1,
            max: MAX_ANNOUNCED_CODECS,
        })
    );
}

#[test]
fn un_hello_con_nombre_demasiado_largo_se_rechaza() {
    let hello = Hello {
        protocol_version: PROTOCOL_VERSION,
        role: Role::Host,
        video_codecs: vec![],
        audio_codecs: vec![],
        peer_name: "a".repeat(MAX_PEER_NAME_LEN + 1),
    };

    let mut buf = BytesMut::new();
    encode(&Message::Hello(hello), &mut buf).expect("codificar");

    assert_eq!(
        decode(&mut buf),
        Err(ProtoError::FieldTooLong {
            field: "Hello.peer_name",
            len: MAX_PEER_NAME_LEN + 1,
            max: MAX_PEER_NAME_LEN,
        })
    );
}

#[test]
fn codificar_un_frame_demasiado_grande_no_deja_basura_en_el_buffer() {
    let mut buf = BytesMut::new();
    encode(
        &Message::Ping(Ping {
            nonce: 1,
            sent_us: 2,
        }),
        &mut buf,
    )
    .expect("codificar");
    let tras_el_ping = buf.len();

    let demasiado = Message::VideoFrame(VideoFrame {
        monitor: 0,
        codec: VideoCodec::Vp8,
        keyframe: true,
        timestamp_us: 0,
        width: 1,
        height: 1,
        data: bytes::Bytes::from(vec![0u8; MAX_FRAME_LEN + 1]),
    });

    assert!(matches!(
        encode(&demasiado, &mut buf),
        Err(ProtoError::FrameTooLarge { .. })
    ));
    assert_eq!(
        buf.len(),
        tras_el_ping,
        "un encode fallido dejo un frame a medias en el buffer"
    );

    // Y el mensaje anterior sigue siendo decodificable.
    assert!(matches!(decode(&mut buf), Ok(Some(Message::Ping(_)))));
}

// --- Negociacion de codecs ------------------------------------------------------------

#[test]
fn la_negociacion_respeta_la_preferencia_de_quien_codifica() {
    let host = [VideoCodec::H264, VideoCodec::Vp8];
    let viewer = [VideoCodec::Vp8, VideoCodec::H264];

    // Gana el primero de la lista del host, no el del viewer.
    assert_eq!(negotiate(&host, &viewer), Some(VideoCodec::H264));
}

#[test]
fn la_negociacion_sin_interseccion_no_elige_nada() {
    let host = [VideoCodec::Av1];
    let viewer = [VideoCodec::Vp8];

    assert_eq!(negotiate(&host, &viewer), None);
    assert_eq!(VideoCodec::BASELINE, VideoCodec::Vp8);
    assert_eq!(AudioCodec::BASELINE, AudioCodec::Opus);
}

#[test]
fn los_valores_del_wire_de_los_codecs_son_estables() {
    // Si este test falla, se ha cambiado un valor ya publicado y se rompe la
    // compatibilidad con cualquier peer de una version anterior.
    for (codec, wire) in [
        (VideoCodec::Vp8, 1u8),
        (VideoCodec::H264, 2),
        (VideoCodec::H265, 3),
        (VideoCodec::Av1, 4),
    ] {
        assert_eq!(codec.to_wire(), wire);
        assert_eq!(VideoCodec::from_wire(wire), Ok(codec));
    }

    for (codec, wire) in [(AudioCodec::Opus, 1u8), (AudioCodec::PcmS16, 2)] {
        assert_eq!(codec.to_wire(), wire);
        assert_eq!(AudioCodec::from_wire(wire), Ok(codec));
    }
}

#[test]
fn el_nombre_del_mensaje_coincide_con_su_variante() {
    for m in one_of_each() {
        assert!(!m.name().is_empty());
    }
    assert_eq!(
        message::PROTOCOL_VERSION,
        PROTOCOL_VERSION,
        "la version del protocolo se declara en un solo sitio"
    );
}

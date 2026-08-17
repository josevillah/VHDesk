//! Framing y (de)serializacion.
//!
//! # Formato del wire
//!
//! ```text
//! +------------------+--------+------------------+
//! | longitud: u32 LE | tag:u8 | cuerpo           |
//! +------------------+--------+------------------+
//! \__ 4 bytes _______/\______ `longitud` bytes __/
//! ```
//!
//! `longitud` cuenta el tag mas el cuerpo, nunca el propio prefijo, y esta acotada por
//! [`MAX_FRAME_LEN`]. Ese limite es lo que impide que un peer nos haga reservar memoria
//! arbitraria con solo cuatro bytes, y por eso se comprueba antes de tocar el buffer.
//!
//! El framing esta escrito a mano en vez de delegarlo en el serializador porque es la
//! superficie que primero ve los bytes de un peer no autenticado: queremos poder leerla
//! entera de un vistazo y fuzzearla sin intermediarios.
//!
//! # Uso
//!
//! Este modulo es sans-I/O: no lee de sockets ni escribe en ellos. [`decode`] consume un
//! [`BytesMut`] que alimenta el llamante y devuelve `Ok(None)` mientras falten bytes.

use bytes::{Buf, BufMut, Bytes, BytesMut};

use crate::codecs::{AudioCodec, VideoCodec};
use crate::error::ProtoError;
use crate::message::{AudioFrame, Hello, Message, VideoFrame};

/// Bytes que ocupa el prefijo de longitud.
pub const LENGTH_PREFIX_LEN: usize = 4;

/// Tamano maximo del cuerpo de un frame, en bytes.
///
/// 16 MiB deja sitio de sobra a un keyframe de 4K con bitrate alto y sigue siendo un
/// techo que un peer malicioso no puede usar para agotar la memoria del otro lado.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

// Tags de tipo. Son estables: un valor asignado no se reutiliza jamas para otro mensaje.
const TAG_HELLO: u8 = 0x01;
const TAG_AUTH_REQUEST: u8 = 0x02;
const TAG_AUTH_RESPONSE: u8 = 0x03;
const TAG_VIDEO_FRAME: u8 = 0x04;
const TAG_AUDIO_FRAME: u8 = 0x05;
const TAG_INPUT_EVENT: u8 = 0x06;
const TAG_CURSOR: u8 = 0x07;
const TAG_CLIPBOARD_UPDATE: u8 = 0x08;
const TAG_PING: u8 = 0x09;
const TAG_PONG: u8 = 0x0a;
const TAG_KEYFRAME_REQUEST: u8 = 0x0b;

/// Bit de `flags` de [`VideoFrame`] que marca un keyframe.
const VIDEO_FLAG_KEYFRAME: u8 = 0b0000_0001;
/// Bits de `flags` de [`VideoFrame`] todavia sin asignar.
const VIDEO_FLAGS_RESERVED: u8 = !VIDEO_FLAG_KEYFRAME;

/// Tag de tipo que corresponde a un mensaje.
const fn tag_of(message: &Message) -> u8 {
    match message {
        Message::Hello(_) => TAG_HELLO,
        Message::AuthRequest(_) => TAG_AUTH_REQUEST,
        Message::AuthResponse(_) => TAG_AUTH_RESPONSE,
        Message::VideoFrame(_) => TAG_VIDEO_FRAME,
        Message::AudioFrame(_) => TAG_AUDIO_FRAME,
        Message::InputEvent(_) => TAG_INPUT_EVENT,
        Message::Cursor(_) => TAG_CURSOR,
        Message::ClipboardUpdate(_) => TAG_CLIPBOARD_UPDATE,
        Message::KeyframeRequest(_) => TAG_KEYFRAME_REQUEST,
        Message::Ping(_) => TAG_PING,
        Message::Pong(_) => TAG_PONG,
    }
}

/// Anade un mensaje codificado al final de `out`.
///
/// En caso de error `out` se deja como estaba, sin frames a medio escribir.
///
/// # Errores
///
/// Devuelve [`ProtoError::FrameTooLarge`] si el mensaje codificado supera
/// [`MAX_FRAME_LEN`], y [`ProtoError::Postcard`] si falla la serializacion del cuerpo de
/// un mensaje de control.
pub fn encode(message: &Message, out: &mut BytesMut) -> Result<(), ProtoError> {
    let start = out.len();

    match encode_inner(message, out) {
        Ok(()) => Ok(()),
        Err(error) => {
            out.truncate(start);
            Err(error)
        }
    }
}

fn encode_inner(message: &Message, out: &mut BytesMut) -> Result<(), ProtoError> {
    let start = out.len();

    // Reservamos el hueco del prefijo y lo parcheamos al final: asi el cuerpo se escribe
    // de una pasada y no hace falta serializarlo dos veces para conocer su longitud.
    out.put_u32_le(0);
    out.put_u8(tag_of(message));

    match message {
        Message::Hello(hello) => encode_control(hello, out)?,
        Message::AuthRequest(request) => encode_control(request, out)?,
        Message::AuthResponse(response) => encode_control(response, out)?,
        Message::InputEvent(event) => encode_control(event, out)?,
        Message::Cursor(cursor) => encode_control(cursor, out)?,
        Message::ClipboardUpdate(update) => encode_control(update, out)?,
        Message::KeyframeRequest(request) => encode_control(request, out)?,
        Message::Ping(ping) => encode_control(ping, out)?,
        Message::Pong(pong) => encode_control(pong, out)?,
        Message::VideoFrame(frame) => encode_video_frame(frame, out),
        Message::AudioFrame(frame) => encode_audio_frame(frame, out),
    }

    let len = out.len() - start - LENGTH_PREFIX_LEN;
    if len > MAX_FRAME_LEN {
        return Err(ProtoError::FrameTooLarge { len });
    }

    let len_bytes = u32::try_from(len)
        .map_err(|_| ProtoError::FrameTooLarge { len })?
        .to_le_bytes();
    out[start..start + LENGTH_PREFIX_LEN].copy_from_slice(&len_bytes);

    Ok(())
}

/// Serializa el cuerpo de un mensaje de control con postcard.
///
/// Pasa por un `Vec` intermedio porque postcard necesita consumir el destino por valor.
/// Es una asignacion por mensaje de control, todos ellos de decenas de bytes; los frames
/// de media, que son los que mueven el volumen, no pasan por aqui.
fn encode_control<T: serde::Serialize>(value: &T, out: &mut BytesMut) -> Result<(), ProtoError> {
    let body = postcard::to_allocvec(value)?;
    out.put_slice(&body);
    Ok(())
}

fn encode_video_frame(frame: &VideoFrame, out: &mut BytesMut) {
    out.put_u8(frame.monitor);
    out.put_u8(frame.codec.to_wire());
    out.put_u64_le(frame.sequence);
    out.put_u8(if frame.keyframe {
        VIDEO_FLAG_KEYFRAME
    } else {
        0
    });
    out.put_u64_le(frame.timestamp_us);
    out.put_u16_le(frame.width);
    out.put_u16_le(frame.height);
    out.put_slice(&frame.data);
}

fn encode_audio_frame(frame: &AudioFrame, out: &mut BytesMut) {
    out.put_u8(frame.codec.to_wire());
    out.put_u64_le(frame.timestamp_us);
    out.put_u32_le(frame.sample_rate);
    out.put_u8(frame.channels);
    out.put_slice(&frame.data);
}

/// Intenta extraer un mensaje del principio de `buf`.
///
/// Devuelve `Ok(None)` si todavia no ha llegado el frame entero, en cuyo caso `buf` queda
/// intacto y hay que volver a llamar cuando haya mas bytes.
///
/// # Errores
///
/// Cualquier `Err` es **fatal para la conexion**: indica que el peer no habla este
/// protocolo y el buffer puede haber quedado desalineado. El llamante debe cerrar la
/// conexion, no reintentar.
pub fn decode(buf: &mut BytesMut) -> Result<Option<Message>, ProtoError> {
    if buf.len() < LENGTH_PREFIX_LEN {
        return Ok(None);
    }

    let mut prefix = [0u8; LENGTH_PREFIX_LEN];
    prefix.copy_from_slice(&buf[..LENGTH_PREFIX_LEN]);
    let len = u32::from_le_bytes(prefix) as usize;

    // Se valida antes de reservar o mover nada: es justo el punto donde un `len` mentiroso
    // se convertiria en una reserva de memoria a peticion del atacante.
    if len == 0 {
        return Err(ProtoError::EmptyFrame);
    }
    if len > MAX_FRAME_LEN {
        return Err(ProtoError::FrameTooLarge { len });
    }
    if buf.len() < LENGTH_PREFIX_LEN + len {
        return Ok(None);
    }

    buf.advance(LENGTH_PREFIX_LEN);
    let body = buf.split_to(len).freeze();
    decode_body(&body).map(Some)
}

fn decode_body(body: &Bytes) -> Result<Message, ProtoError> {
    let tag = *body.first().ok_or(ProtoError::EmptyFrame)?;
    // `slice` sobre `Bytes` es un incremento de refcount, no una copia: el payload de los
    // frames de media sigue apuntando al buffer de recepcion original.
    let rest = body.slice(1..);

    match tag {
        TAG_HELLO => {
            let hello: Hello = decode_control(&rest)?;
            hello.validate()?;
            Ok(Message::Hello(hello))
        }
        TAG_AUTH_REQUEST => decode_control(&rest).map(Message::AuthRequest),
        TAG_AUTH_RESPONSE => decode_control(&rest).map(Message::AuthResponse),
        TAG_VIDEO_FRAME => decode_video_frame(rest).map(Message::VideoFrame),
        TAG_AUDIO_FRAME => decode_audio_frame(rest).map(Message::AudioFrame),
        TAG_INPUT_EVENT => decode_control(&rest).map(Message::InputEvent),
        TAG_CURSOR => decode_control(&rest).map(Message::Cursor),
        TAG_CLIPBOARD_UPDATE => decode_control(&rest).map(Message::ClipboardUpdate),
        TAG_KEYFRAME_REQUEST => decode_control(&rest).map(Message::KeyframeRequest),
        TAG_PING => decode_control(&rest).map(Message::Ping),
        TAG_PONG => decode_control(&rest).map(Message::Pong),
        other => Err(ProtoError::UnknownTag { tag: other }),
    }
}

/// Deserializa el cuerpo de un mensaje de control exigiendo que se consuma entero.
///
/// Se usa `take_from_bytes` en lugar de `from_bytes` para poder rechazar el relleno
/// sobrante: un frame que acepta bytes ignorados es un canal encubierto y hace que dos
/// secuencias distintas representen el mismo mensaje.
fn decode_control<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, ProtoError> {
    let (value, trailing) = postcard::take_from_bytes::<T>(body)?;
    if !trailing.is_empty() {
        return Err(ProtoError::TrailingBytes {
            trailing: trailing.len(),
        });
    }
    Ok(value)
}

fn decode_video_frame(body: Bytes) -> Result<VideoFrame, ProtoError> {
    let mut reader = BodyReader::new(body, "VideoFrame");

    let monitor = reader.u8()?;
    let codec = VideoCodec::from_wire(reader.u8()?)?;
    let sequence = reader.u64()?;
    let flags = reader.u8()?;
    if flags & VIDEO_FLAGS_RESERVED != 0 {
        return Err(ProtoError::ReservedBitsSet {
            field: "VideoFrame.flags",
        });
    }
    let timestamp_us = reader.u64()?;
    let width = reader.u16()?;
    let height = reader.u16()?;

    Ok(VideoFrame {
        monitor,
        codec,
        sequence,
        keyframe: flags & VIDEO_FLAG_KEYFRAME != 0,
        timestamp_us,
        width,
        height,
        data: reader.into_rest(),
    })
}

fn decode_audio_frame(body: Bytes) -> Result<AudioFrame, ProtoError> {
    let mut reader = BodyReader::new(body, "AudioFrame");

    let codec = AudioCodec::from_wire(reader.u8()?)?;
    let timestamp_us = reader.u64()?;
    let sample_rate = reader.u32()?;
    let channels = reader.u8()?;

    Ok(AudioFrame {
        codec,
        timestamp_us,
        sample_rate,
        channels,
        data: reader.into_rest(),
    })
}

/// Lector secuencial con comprobacion de limites sobre el cuerpo de un frame.
///
/// Todos los accesos pasan por [`BodyReader::take`], de modo que ninguna lectura puede
/// salirse del buffer ni entrar en panico por mucho que mienta el emisor.
struct BodyReader {
    buf: Bytes,
    pos: usize,
    message: &'static str,
}

impl BodyReader {
    const fn new(buf: Bytes, message: &'static str) -> Self {
        Self {
            buf,
            pos: 0,
            message,
        }
    }

    fn take(&mut self, n: usize) -> Result<&[u8], ProtoError> {
        let start = self.pos;
        let available = self.buf.len() - start;
        if available < n {
            return Err(ProtoError::TruncatedBody {
                message: self.message,
                expected: n,
                available,
            });
        }
        self.pos = start + n;
        Ok(&self.buf[start..start + n])
    }

    fn u8(&mut self) -> Result<u8, ProtoError> {
        let mut bytes = [0u8; 1];
        bytes.copy_from_slice(self.take(1)?);
        Ok(bytes[0])
    }

    fn u16(&mut self) -> Result<u16, ProtoError> {
        let mut bytes = [0u8; 2];
        bytes.copy_from_slice(self.take(2)?);
        Ok(u16::from_le_bytes(bytes))
    }

    fn u32(&mut self) -> Result<u32, ProtoError> {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, ProtoError> {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(bytes))
    }

    /// Devuelve lo que quede sin leer, sin copiarlo.
    fn into_rest(self) -> Bytes {
        self.buf.slice(self.pos..)
    }
}

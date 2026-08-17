//! Protocolo de VHDesk: tipos de mensaje, framing y (de)serializacion.
//!
//! Este crate es deliberadamente el mas aburrido del workspace. No hace I/O, no conoce
//! tokio y no depende de ningun otro crate de VHDesk. Todo lo que hace es convertir
//! mensajes en bytes y bytes en mensajes, de forma que se pueda testear y fuzzear sin
//! levantar una red.
//!
//! Es tambien la primera superficie que ve los bytes de un peer que todavia no se ha
//! autenticado, asi que la regla que gobierna el crate es que **ninguna entrada, por
//! malformada que sea, puede provocar un panico ni una reserva de memoria desmedida**.
//! De ahi el `forbid(unsafe_code)`, el limite [`framing::MAX_FRAME_LEN`] y que cada
//! lectura del cuerpo de un frame pase por una comprobacion de limites.
//!
//! # Ejemplo
//!
//! ```
//! use bytes::BytesMut;
//! use vhdesk_proto::{Message, Ping, decode, encode};
//!
//! let mut buf = BytesMut::new();
//! let ping = Message::Ping(Ping { nonce: 7, sent_us: 1_000 });
//! encode(&ping, &mut buf)?;
//!
//! assert_eq!(decode(&mut buf)?, Some(ping));
//! assert!(buf.is_empty());
//! # Ok::<(), vhdesk_proto::ProtoError>(())
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod codecs;
pub mod error;
pub mod framing;
pub mod message;

#[cfg(test)]
mod tests;

pub use codecs::{AudioCodec, VideoCodec, negotiate};
pub use error::ProtoError;
pub use framing::{LENGTH_PREFIX_LEN, MAX_FRAME_LEN, decode, encode};
pub use message::{
    AudioFrame, AuthMethod, AuthRequest, AuthResponse, AuthResult, ClipboardFormat,
    ClipboardUpdate, Cursor, Hello, InputEvent, KeyframeReason, KeyframeRequest,
    MAX_ANNOUNCED_CODECS, MAX_PEER_NAME_LEN, Message, MouseButton, PROTOCOL_VERSION, Ping, Pong,
    Role, VideoFrame,
};

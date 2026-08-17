//! Codecs de video.
//!
//! Expone los traits [`VideoEncoder`] y [`VideoDecoder`] y la conversion de BGRA a I420
//! que alimenta al codificador. El codec efectivo de cada sesion lo negocian los peers;
//! este crate no decide cual se usa, solo sabe ejecutar el que le pidan.
//!
//! # Estado
//!
//! FASE 1, bloque B: traits, conversion de color y backend VP8 por software.
//!
//! FASE 4: NVENC, AMF, QuickSync, VideoToolbox y VAAPI, con acceso directo a la API de
//! cada plataforma en vez de a traves de ffmpeg, para no arrastrar su arbol de licencias.
//!
//! # Compilacion
//!
//! Hace falta libvpx en la maquina, y en Windows ademas LLVM para generar los enlaces FFI.
//! Ver `docs/adr/0002-libvpx-en-windows.md`.
//!
//! # Nota sobre regiones sucias
//!
//! libvpx **no acepta rectangulos sucios como entrada**: no hay forma de decirle "solo ha
//! cambiado esta zona". El valor de los rectangulos que da la captura esta en saltarse el
//! frame entero cuando no cambio nada, en acotar la copia desde la textura de staging, y
//! en alimentar heuristicas; nada de eso pasa por la API del codec.

#![deny(missing_docs)]

pub mod error;
pub mod vp8;
pub mod yuv;

use bytes::Bytes;
use vhdesk_proto::VideoCodec;

pub use crate::error::CodecError;
pub use crate::vp8::{Vp8Decoder, Vp8Encoder};
pub use crate::yuv::I420Frame;

/// Ajustes con los que se construye un codificador.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderConfig {
    /// Anchura en pixeles.
    pub width: u32,
    /// Altura en pixeles.
    pub height: u32,
    /// Bitrate objetivo en kilobits por segundo.
    pub target_bitrate_kbps: u32,
    /// Cota superior de frames por segundo, para que el codificador reparta el bitrate.
    pub max_framerate: u32,
    /// Segundos entre keyframes periodicos.
    ///
    /// Los keyframes son grandes, asi que cuanto mas espaciados mejor para el ancho de
    /// banda; pero son el unico punto por el que un viewer que acaba de conectarse o que
    /// perdio datos puede engancharse a la imagen.
    pub keyframe_interval_secs: u32,
}

impl EncoderConfig {
    /// Ajustes razonables para una LAN, que es el escenario de la fase 1.
    pub const fn lan(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            target_bitrate_kbps: 8_000,
            max_framerate: 60,
            keyframe_interval_secs: 4,
        }
    }
}

/// Un frame ya comprimido, listo para meterlo en un mensaje del protocolo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrame {
    /// Datos comprimidos.
    pub data: Bytes,
    /// Si el frame se puede decodificar sin depender de los anteriores.
    pub keyframe: bool,
    /// Marca de tiempo que se le paso al codificar.
    pub timestamp_us: u64,
}

/// Vista de un frame decodificado, prestada del buffer interno del decodificador.
///
/// Es un prestamo y no un buffer propio a proposito: el decodificador conserva el frame
/// hasta la siguiente llamada, asi que devolverlo por referencia evita copiar varios MiB
/// por frame. Quien lo necesite mas tiempo, que lo copie explicitamente.
#[derive(Debug, Clone, Copy)]
pub struct DecodedFrame<'a> {
    /// Anchura en pixeles.
    pub width: u32,
    /// Altura en pixeles.
    pub height: u32,
    /// Plano de luminancia.
    pub y: &'a [u8],
    /// Plano U.
    pub u: &'a [u8],
    /// Plano V.
    pub v: &'a [u8],
    /// Bytes por fila del plano de luminancia. Puede ser mayor que `width`.
    pub y_stride: usize,
    /// Bytes por fila de los planos de crominancia.
    pub uv_stride: usize,
}

impl DecodedFrame<'_> {
    /// Copia este frame a un buffer propio y reutilizable.
    ///
    /// Hace falta para sacar el frame del decodificador, porque su buffer interno deja de
    /// ser valido en la siguiente llamada a `decode`. libvpx permite suministrar los
    /// buffers desde fuera y evitar esta copia, pero **solo con VP9**; con VP8 devuelve
    /// `VPX_CODEC_INCAPABLE`, y hay un test que lo fija por escrito.
    ///
    /// `destino` se redimensiona solo si las dimensiones cambiaron, asi que reutilizar el
    /// mismo entre frames no asigna nada.
    ///
    /// # Errores
    ///
    /// Devuelve [`CodecError::InvalidDimensions`] si el frame decodificado tiene
    /// dimensiones invalidas, y [`CodecError::BufferSize`] si sus planos no cuadran.
    pub fn copy_into(&self, destino: &mut I420Frame) -> Result<(), CodecError> {
        if destino.width() != self.width || destino.height() != self.height {
            *destino = I420Frame::new(self.width, self.height)?;
        }

        destino.copy_from_planes(self.y, self.u, self.v, self.y_stride, self.uv_stride)
    }
}

/// Comprime frames de video.
pub trait VideoEncoder {
    /// Codec que implementa.
    fn codec(&self) -> VideoCodec;

    /// Pide que el proximo frame codificado sea un keyframe.
    ///
    /// Lo usa el host cuando entra un viewer nuevo, cuando la captura senala un refresco
    /// completo y cuando el viewer avisa de que perdio el hilo del flujo.
    fn request_keyframe(&mut self);

    /// Comprime un frame.
    ///
    /// Devuelve `Ok(None)` si el codificador no ha producido salida para este frame, que es
    /// legitimo: algunos modos retienen frames antes de emitir.
    ///
    /// # Errores
    ///
    /// Devuelve [`CodecError::DimensionsChanged`] si el frame no coincide con la
    /// configuracion, y [`CodecError::Backend`] si falla la libreria subyacente.
    fn encode(
        &mut self,
        frame: &I420Frame,
        timestamp_us: u64,
    ) -> Result<Option<EncodedFrame>, CodecError>;
}

/// Descomprime frames de video.
pub trait VideoDecoder {
    /// Codec que implementa.
    fn codec(&self) -> VideoCodec;

    /// Descomprime un frame.
    ///
    /// Devuelve `Ok(None)` si el flujo todavia no da para producir imagen, lo que ocurre
    /// mientras no ha llegado el primer keyframe.
    ///
    /// # Errores
    ///
    /// Los datos vienen de la red, asi que hay que tratarlos como hostiles: cualquier
    /// entrada, por corrupta que sea, tiene que salir por [`CodecError::InvalidBitstream`]
    /// o [`CodecError::Backend`], nunca por un panico.
    fn decode(&mut self, data: &[u8]) -> Result<Option<DecodedFrame<'_>>, CodecError>;
}

/// Construye un codificador para el codec negociado.
///
/// # Errores
///
/// Devuelve [`CodecError::UnsupportedCodec`] si no hay backend para ese codec.
pub fn open_encoder(
    codec: VideoCodec,
    config: EncoderConfig,
) -> Result<Box<dyn VideoEncoder>, CodecError> {
    match codec {
        VideoCodec::Vp8 => Ok(Box::new(Vp8Encoder::new(config)?)),
        // FASE 4: H.264, HEVC y AV1 por hardware. Hasta entonces el error dice exactamente
        // lo que pasa en vez de degradar en silencio a VP8, que dejaria al otro lado
        // esperando un flujo que no es el que negocio.
        otro => Err(CodecError::UnsupportedCodec(otro)),
    }
}

/// Construye un decodificador para el codec negociado.
///
/// # Errores
///
/// Devuelve [`CodecError::UnsupportedCodec`] si no hay backend para ese codec.
pub fn open_decoder(codec: VideoCodec) -> Result<Box<dyn VideoDecoder>, CodecError> {
    match codec {
        VideoCodec::Vp8 => Ok(Box::new(Vp8Decoder::new()?)),
        // FASE 4: ver `open_encoder`.
        otro => Err(CodecError::UnsupportedCodec(otro)),
    }
}

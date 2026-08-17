//! Errores de codificacion y decodificacion.

use vhdesk_proto::VideoCodec;

/// Fallo al codificar o decodificar video.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CodecError {
    /// El codec pedido no tiene backend en esta compilacion o plataforma.
    #[error("no hay backend disponible para el codec {0:?}")]
    UnsupportedCodec(VideoCodec),

    /// Las dimensiones no sirven para codificar.
    #[error("dimensiones invalidas: {width}x{height}")]
    InvalidDimensions {
        /// Anchura pedida.
        width: u32,
        /// Altura pedida.
        height: u32,
    },

    /// El buffer de entrada no cuadra con las dimensiones declaradas.
    #[error("buffer de {actual} bytes para un frame de {width}x{height}, hacen falta {needed}")]
    BufferSize {
        /// Anchura del frame.
        width: u32,
        /// Altura del frame.
        height: u32,
        /// Bytes necesarios.
        needed: usize,
        /// Bytes recibidos.
        actual: usize,
    },

    /// Llego un frame con dimensiones distintas a las que se configuro el codec.
    ///
    /// En la fase 1 se trata como error; a partir de la fase 4, cuando el host soporte
    /// cambios de resolucion en caliente, habra que reconfigurar el codificador.
    #[error("se configuro para {expected_width}x{expected_height} y llego {width}x{height}")]
    DimensionsChanged {
        /// Anchura configurada.
        expected_width: u32,
        /// Altura configurada.
        expected_height: u32,
        /// Anchura recibida.
        width: u32,
        /// Altura recibida.
        height: u32,
    },

    /// Fallo interno de la libreria del codec.
    #[error("fallo del codec en {operation}: {detail}")]
    Backend {
        /// Operacion que fallo.
        operation: &'static str,
        /// Mensaje que dio la libreria.
        detail: String,
    },

    /// El flujo comprimido no se puede decodificar.
    ///
    /// Llega de la red, asi que hay que asumir que puede venir manipulado: un decodificador
    /// que entre en panico con un flujo corrupto es un fallo de disponibilidad remoto.
    #[error("flujo de video invalido: {0}")]
    InvalidBitstream(&'static str),
}

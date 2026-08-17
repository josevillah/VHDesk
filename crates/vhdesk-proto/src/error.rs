//! Errores de (de)serializacion del protocolo.

/// Fallo al codificar o decodificar un mensaje.
///
/// Todo `Err` que salga de [`crate::framing::decode`] debe considerarse **fatal para la
/// conexion**: significa que el peer envio algo que no encaja con el protocolo, y en ese
/// punto el buffer de recepcion puede haber quedado desalineado. La politica correcta es
/// cerrar la conexion, no intentar resincronizar.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtoError {
    /// La longitud declarada supera [`crate::framing::MAX_FRAME_LEN`].
    ///
    /// Es la primera linea de defensa contra el agotamiento de memoria: un peer
    /// malicioso no puede hacernos reservar un buffer arbitrario con solo cuatro bytes.
    #[error("longitud de frame {len} bytes: supera el maximo permitido")]
    FrameTooLarge {
        /// Longitud declarada en el prefijo.
        len: usize,
    },

    /// Un frame de longitud cero. Todo frame lleva al menos el tag de tipo.
    #[error("frame vacio: todo frame debe llevar al menos el tag de mensaje")]
    EmptyFrame,

    /// El tag de tipo no corresponde a ningun mensaje conocido.
    #[error("tag de mensaje desconocido: 0x{tag:02x}")]
    UnknownTag {
        /// Byte de tag recibido.
        tag: u8,
    },

    /// El cuerpo del mensaje termina antes de lo que exige su cabecera fija.
    #[error(
        "cuerpo de {message} truncado: se esperaban {expected} bytes mas, quedaban {available}"
    )]
    TruncatedBody {
        /// Nombre del mensaje que se estaba decodificando.
        message: &'static str,
        /// Bytes que faltaban por leer.
        expected: usize,
        /// Bytes que quedaban disponibles.
        available: usize,
    },

    /// Un campo enumerado trae un valor que esta fuera del rango conocido.
    #[error("valor {value} no valido para el campo {field}")]
    UnknownDiscriminant {
        /// Campo que se estaba decodificando.
        field: &'static str,
        /// Valor recibido.
        value: u8,
    },

    /// Bits reservados a uno.
    ///
    /// Rechazarlos desde el principio evita que un bit que hoy se ignora quede de facto
    /// asignado a "cero" y no podamos usarlo en una version futura.
    #[error("bits reservados a uno en el campo {field}")]
    ReservedBitsSet {
        /// Campo que contiene los bits reservados.
        field: &'static str,
    },

    /// Sobran bytes despues de decodificar el mensaje.
    ///
    /// Se rechaza en vez de ignorarse: un frame con relleno arbitrario es un canal
    /// encubierto y rompe la equivalencia entre un mensaje y su representacion.
    #[error("sobran {trailing} bytes al final del cuerpo del mensaje")]
    TrailingBytes {
        /// Numero de bytes sobrantes.
        trailing: usize,
    },

    /// Un campo de longitud variable supera su limite.
    #[error("el campo {field} tiene {len} elementos y el maximo es {max}")]
    FieldTooLong {
        /// Campo que excede el limite.
        field: &'static str,
        /// Longitud recibida.
        len: usize,
        /// Longitud maxima admitida.
        max: usize,
    },

    /// Fallo de postcard al (de)serializar el cuerpo de un mensaje de control.
    #[error("error de serializacion postcard: {0}")]
    Postcard(#[from] postcard::Error),
}

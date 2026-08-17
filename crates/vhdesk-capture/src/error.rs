//! Errores de captura.

/// Fallo al enumerar monitores o al capturar un frame.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CaptureError {
    /// No hay ningun monitor conectado al escritorio.
    #[error("no hay ningun monitor disponible")]
    NoMonitors,

    /// El monitor pedido no existe.
    #[error("no existe el monitor {adapter}:{output}")]
    UnknownMonitor {
        /// Indice del adaptador.
        adapter: u32,
        /// Indice del output dentro del adaptador.
        output: u32,
    },

    /// El sistema retiro la duplicacion y hay que reconstruirla.
    ///
    /// Ocurre al cambiar de resolucion, al arrancar un juego en pantalla completa
    /// exclusiva y al cambiar de sesion. No es fatal: se reinicializa y se sigue.
    #[error("la duplicacion de escritorio se perdio y hay que reinicializarla")]
    AccessLost,

    /// El sistema deniega el acceso a la duplicacion.
    ///
    /// Tipicamente porque el escritorio seguro (el prompt de UAC o la pantalla de
    /// bloqueo) esta delante. Persiste mientras dure esa situacion, asi que reintentar
    /// de inmediato no sirve de nada.
    #[error("el sistema denego el acceso a la duplicacion; suele ser el escritorio seguro")]
    AccessDenied,

    /// Este monitor no admite duplicacion de escritorio.
    #[error("la duplicacion de escritorio no esta soportada en este monitor")]
    Unsupported,

    /// La forma del puntero que envio el sistema no es coherente.
    #[error("forma de puntero invalida: {0}")]
    InvalidPointerShape(&'static str),

    /// El buffer de destino no da para el frame.
    #[error("buffer demasiado pequeno: hacen falta {needed} bytes y hay {available}")]
    BufferTooSmall {
        /// Bytes necesarios.
        needed: usize,
        /// Bytes disponibles.
        available: usize,
    },

    /// Fallo de una llamada a la API de Windows que no encaja en los casos anteriores.
    #[cfg(windows)]
    #[error("fallo de {operation}: {source}")]
    Windows {
        /// Nombre de la operacion que fallo.
        operation: &'static str,
        /// Error original de Windows.
        #[source]
        source: windows::core::Error,
    },

    /// La captura no esta implementada en esta plataforma.
    #[error("la captura de pantalla todavia no esta implementada en esta plataforma")]
    UnsupportedPlatform,
}

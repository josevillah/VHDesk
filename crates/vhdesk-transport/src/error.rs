//! Errores del transporte.

/// Fallo al establecer o usar una conexion.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TransportError {
    /// No se pudo abrir el socket UDP.
    #[error("no se pudo abrir el socket en {addr}: {source}")]
    Bind {
        /// Direccion que se intento abrir.
        addr: std::net::SocketAddr,
        /// Error del sistema.
        #[source]
        source: std::io::Error,
    },

    /// Fallo al establecer la conexion QUIC.
    #[error("no se pudo conectar con {addr}: {source}")]
    Connect {
        /// Direccion del peer.
        addr: std::net::SocketAddr,
        /// Error de quinn.
        #[source]
        source: quinn::ConnectionError,
    },

    /// El endpoint dejo de aceptar conexiones.
    #[error("el endpoint se cerro y ya no acepta conexiones")]
    EndpointClosed,

    /// La conexion se perdio.
    #[error("conexion perdida: {0}")]
    Connection(#[from] quinn::ConnectionError),

    /// Fallo al escribir en un stream.
    #[error("fallo al escribir en el stream: {0}")]
    Write(#[from] quinn::WriteError),

    /// Fallo al leer de un stream.
    #[error("fallo al leer del stream: {0}")]
    Read(#[from] quinn::ReadError),

    /// El peer cerro el stream antes de enviar un mensaje completo.
    #[error("el stream termino en mitad de un mensaje")]
    TruncatedStream,

    /// Un frame de video supera el tope admitido.
    #[error("frame de {len} bytes: supera el maximo del protocolo")]
    FrameTooLarge {
        /// Tamano recibido.
        len: usize,
    },

    /// El mensaje no cabe en un datagrama.
    ///
    /// Los datagramas QUIC no se fragmentan y estan acotados por la MTU del camino. Es la
    /// misma razon por la que el video va por streams, aplicada a mensajes que parecian
    /// pequenos y no lo son, como la forma del cursor.
    #[error("mensaje de {len} bytes: no cabe en un datagrama (maximo {max})")]
    DatagramTooLarge {
        /// Tamano del mensaje.
        len: usize,
        /// Maximo que admite la conexion.
        max: usize,
    },

    /// El peer no admite datagramas.
    #[error("el peer no admite datagramas")]
    DatagramsUnsupported,

    /// Fallo al enviar un datagrama.
    #[error("fallo al enviar el datagrama: {0}")]
    SendDatagram(#[from] quinn::SendDatagramError),

    /// El mensaje no se pudo codificar o decodificar.
    #[error("error de protocolo: {0}")]
    Proto(#[from] vhdesk_proto::ProtoError),

    /// Fallo al construir la identidad TLS de esta instalacion.
    #[error("no se pudo generar el certificado: {0}")]
    Certificate(String),

    /// Fallo al configurar rustls.
    #[error("configuracion TLS invalida: {0}")]
    Tls(#[from] rustls::Error),

    /// La configuracion TLS no sirve para QUIC.
    ///
    /// QUIC exige TLS 1.3 con una suite que tenga cifrado inicial; una configuracion que
    /// solo admita TLS 1.2 llega hasta aqui y falla.
    #[error("la configuracion TLS no vale para QUIC: {0}")]
    QuicCrypto(#[from] quinn::crypto::rustls::NoInitialCipherSuite),
}

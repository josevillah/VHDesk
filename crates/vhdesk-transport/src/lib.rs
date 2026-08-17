//! Transporte: QUIC, travesia de NAT y relay.
//!
//! # Estado
//!
//! FASE 1, bloque C: conexion punto a punto con los cuatro canales de la sesion.
//! **Sin autenticacion**: el certificado se genera en cada arranque y el cliente acepta
//! cualquiera. Ver la advertencia completa en [`tls`].
//!
//! FASE 2: pinning SPKI y autenticacion mutua.
//! FASE 3: rendezvous, hole punching y respaldo por relay.
//! FASE 4: bitrate adaptativo.
//!
//! # Dos decisiones de forma que es facil romper por descuido
//!
//! **Un solo [`Endpoint`], y por tanto un solo socket UDP**, para el rendezvous y para el
//! peer. El hole punching de la fase 3 solo funciona perforando desde el mismo socket cuya
//! direccion reflexiva observo el servidor.
//!
//! **El video no viaja en datagramas.** Un datagrama QUIC no se fragmenta y esta acotado
//! por la MTU del camino, asi que un frame no cabe. Va por un stream unidireccional por
//! frame, con `RESET_STREAM` para el que quede obsoleto. La misma regla alcanza a la forma
//! del cursor, que tampoco cabe y va por el canal de control.
//!
//! # Ejemplo
//!
//! ```no_run
//! use std::net::SocketAddr;
//! use vhdesk_transport::{Endpoint, install_crypto_provider};
//!
//! # async fn ejemplo() -> Result<(), vhdesk_transport::TransportError> {
//! // Explicito y al principio de main: rustls no elige proveedor por su cuenta.
//! install_crypto_provider();
//!
//! let endpoint = Endpoint::bind("0.0.0.0:21118".parse::<SocketAddr>().expect("direccion"))?;
//! let sesion = endpoint.accept().await?;
//! let mut control = sesion.accept_control().await?;
//! let mensaje = control.recv().await?;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod channels;
pub mod endpoint;
pub mod error;
pub mod session;
pub mod tls;

pub use crate::channels::{
    ControlChannel, InputReceiver, InputSender, RecepcionVideo, VideoSender,
};
pub use crate::endpoint::Endpoint;
pub use crate::error::TransportError;
pub use crate::session::Session;
pub use crate::tls::{ALPN, install_crypto_provider};

//! Servidor de rendezvous y relay de VHDesk.
//!
//! El invariante que gobierna este binario: **el relay reenvia datagramas UDP opacos y
//! jamas termina la conexion QUIC de los peers**. Es lo unico que sostiene la propiedad
//! extremo a extremo, porque el cifrado de la sesion es el propio TLS 1.3 de esa conexion
//! QUIC. Un dia que el relay pase a terminar la conexion, aunque sea "solo para depurar",
//! deja de ser ciego y la propiedad se pierde entera.

#![forbid(unsafe_code)]

use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    let filter = EnvFilter::try_from_env("VHDESK_LOG")
        .or_else(|_| EnvFilter::try_new("info"))
        .context("construir el filtro de trazas")?;
    tracing_subscriber::fmt().with_env_filter(filter).init();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        protocolo = vhdesk_proto::PROTOCOL_VERSION,
        "vhdesk-server"
    );

    // FASE 3: registro de IDs con heartbeat, coordinacion del hole punching y relay
    // ciego con cuotas por sesion.
    tracing::warn!("el servidor todavia no esta implementado; llega en la fase 3");

    Ok(())
}

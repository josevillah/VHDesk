//! Aplicacion de escritorio de VHDesk en la maquina que controla.

#![forbid(unsafe_code)]

pub mod video;

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
        "vhdesk-viewer"
    );

    // FASE 1: ventana egui + wgpu, decodificacion y captura de input local. El video se
    // subira a textura desde un callback de pintado de wgpu, sin pasar por el teselador
    // de egui: ese camino es el que fija la latencia percibida.
    tracing::warn!("el viewer todavia no esta implementado; llega en la fase 1");

    Ok(())
}

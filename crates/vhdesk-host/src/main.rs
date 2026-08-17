//! Daemon de VHDesk en la maquina controlada.

#![forbid(unsafe_code)]

use anyhow::Result;

mod telemetria {
    //! Inicializacion de trazas.
    //!
    //! El nombre es literal: son trazas locales para diagnostico del usuario. VHDesk no
    //! envia nada a ninguna parte, ni analytics ni informes de fallo.

    use anyhow::{Context, Result};
    use tracing_subscriber::EnvFilter;

    /// Configura el subscriber de `tracing` leyendo el filtro de `VHDESK_LOG`.
    pub fn init() -> Result<()> {
        let filter = EnvFilter::try_from_env("VHDESK_LOG")
            .or_else(|_| EnvFilter::try_new("info"))
            .context("construir el filtro de trazas")?;

        tracing_subscriber::fmt().with_env_filter(filter).init();
        Ok(())
    }
}

fn main() -> Result<()> {
    telemetria::init()?;

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        protocolo = vhdesk_proto::PROTOCOL_VERSION,
        "vhdesk-host"
    );

    // FASE 1: aqui va el bucle capture -> codec -> transport y el camino inverso
    // transport -> injector. Hoy el binario existe para que el workspace enlace y para
    // fijar la superficie de arranque, y no hace nada mas.
    tracing::warn!("el daemon todavia no esta implementado; llega en la fase 1");

    Ok(())
}

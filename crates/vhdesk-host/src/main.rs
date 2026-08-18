//! Daemon de VHDesk en la maquina controlada.
//!
//! ```text
//! vhdesk-host --listen 0.0.0.0:21118 --monitor 0
//! ```
//!
//! # Estado
//!
//! FASE 1, bloque E. **Sin autenticacion ni consentimiento**: cualquiera que alcance este
//! puerto ve la pantalla y controla el raton. Solo para una LAN de confianza. La fase 2 lo
//! cierra; ver la nota de seguridad en [`sesion`].

#![forbid(unsafe_code)]

mod captura;
mod cli;
mod codificacion;
mod cursor;
mod entrada;
mod ranura;
mod sesion;

use anyhow::{Context, Result, bail};
use vhdesk_capture::{MonitorInfo, ensure_dpi_awareness, enumerate_monitors};
use vhdesk_transport::{Endpoint, install_crypto_provider};

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

#[tokio::main]
async fn main() -> Result<()> {
    telemetria::init()?;

    let Some(cli) = cli::parsear(std::env::args().skip(1))? else {
        print!("{}", cli::AYUDA);
        return Ok(());
    };

    // Efecto global del proceso, y por eso se hace aqui y no dentro de una libreria. Sin
    // esto, con escalado de pantalla activo Windows reporta resoluciones virtualizadas y
    // las coordenadas de la captura dejan de cuadrar con las de la inyeccion: el sintoma es
    // "el raton no va donde apunto".
    if !ensure_dpi_awareness() {
        tracing::warn!("no se pudo declarar conciencia de DPI por monitor");
    }

    // Explicito y al principio: rustls no elige proveedor criptografico por su cuenta, y si
    // falta, el fallo aparece mucho despues y en un sitio que no orienta.
    install_crypto_provider();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        protocolo = vhdesk_proto::PROTOCOL_VERSION,
        "vhdesk-host"
    );

    let monitor = elegir_monitor(cli.monitor)?;
    tracing::info!(
        monitor = %monitor.name,
        adaptador = %monitor.adapter_name,
        ancho = monitor.width,
        alto = monitor.height,
        escala = monitor.scale,
        "monitor a servir"
    );

    let endpoint = Endpoint::bind(cli.listen).context("abrir el socket")?;
    tracing::warn!(
        direccion = %endpoint.local_addr()?,
        "escuchando SIN AUTENTICACION: cualquiera que alcance este puerto controla la maquina"
    );

    // Una sesion cada vez. El codificador guarda estado por sesion, asi que un viewer nuevo
    // arranca el pipeline entero de cero; el multi-viewer es de mas adelante.
    loop {
        let sesion = endpoint.accept().await.context("aceptar una conexion")?;
        tracing::info!(peer = %sesion.remote_address(), "viewer conectado");

        if let Err(error) = sesion::servir(&cli, sesion, &monitor).await {
            tracing::warn!(%error, "la sesion termino con error");
        }
        tracing::info!("esperando al siguiente viewer");
    }
}

/// Busca el monitor por indice dentro de la lista enumerada.
fn elegir_monitor(indice: u8) -> Result<MonitorInfo> {
    let monitores = enumerate_monitors().context("enumerar monitores")?;

    match monitores.get(usize::from(indice)) {
        Some(monitor) => Ok(monitor.clone()),
        None => {
            let disponibles: Vec<&str> = monitores.iter().map(|m| m.name.as_str()).collect();
            bail!(
                "no hay monitor con indice {indice}; hay {}: {disponibles:?}",
                monitores.len()
            )
        }
    }
}

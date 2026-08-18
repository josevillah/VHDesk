//! Aplicacion de escritorio de VHDesk en la maquina que controla.

#![forbid(unsafe_code)]

mod app;
mod cli;
mod encuadre;
mod sesion;
mod video;

use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    let filter = EnvFilter::try_from_env("VHDESK_LOG")
        .or_else(|_| EnvFilter::try_new("info"))
        .context("construir el filtro de trazas")?;
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let Some(cli) = cli::parsear(std::env::args().skip(1))? else {
        print!("{}", cli::AYUDA);
        return Ok(());
    };

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        protocolo = vhdesk_proto::PROTOCOL_VERSION,
        destino = %cli.connect,
        vsync = cli.vsync,
        "vhdesk-viewer"
    );

    let opciones = eframe::NativeOptions {
        vsync: cli.vsync,
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(format!("VHDesk — {}", cli.connect))
            .with_inner_size([1280.0, 720.0]),
        ..Default::default()
    };

    eframe::run_native(
        "vhdesk-viewer",
        opciones,
        Box::new(move |cc| {
            let render_state = cc
                .wgpu_render_state
                .as_ref()
                .ok_or_else(|| anyhow!("eframe arranco sin backend de wgpu"))?;

            // Se registra porque **no es un valor fijo**: lo elige el backend segun la GPU
            // y el sistema de ventanas, y el pipeline de video tiene que declararlo igual o
            // wgpu rechaza el paso de dibujo. Tenerlo en el log es lo que permite comparar
            // dos maquinas cuando una pinta y la otra no.
            let formato = render_state.target_format;
            tracing::info!(
                target_format = ?formato,
                adaptador = %render_state.adapter.get_info().name,
                backend = ?render_state.adapter.get_info().backend,
                "superficie de pintado"
            );

            let gpu = sesion::Gpu {
                device: render_state.device.clone(),
                queue: render_state.queue.clone(),
                formato,
            };

            let compartido = sesion::arrancar(cli.connect, gpu, cc.egui_ctx.clone());
            Ok(Box::new(app::App::nueva(Arc::clone(&compartido))) as Box<dyn eframe::App>)
        }),
    )
    .map_err(|error| anyhow!("no se pudo abrir la ventana: {error}"))
}

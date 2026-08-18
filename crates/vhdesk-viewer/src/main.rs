//! Aplicacion de escritorio de VHDesk en la maquina que controla.

#![forbid(unsafe_code)]

mod app;
mod cli;
mod cursor;
mod encuadre;
mod entrada;
mod sesion;
mod video;

use std::sync::Arc;
use std::sync::mpsc::Receiver;

use anyhow::{Context, Result, anyhow};
use tracing_subscriber::EnvFilter;
use vhdesk_input::TeclaCapturada;

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

    let mut opciones = eframe::NativeOptions {
        vsync: cli.vsync,
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(format!("VHDesk — {}", cli.connect))
            .with_inner_size([1280.0, 720.0]),
        ..Default::default()
    };

    // El contexto de egui no existe hasta que eframe crea la ventana, pero el enganche de
    // mensajes hay que construirlo antes. Esta celda es el puente: el enganche la lee para
    // pedir repintado, y la rellena la clausura de creacion unas lineas mas abajo.
    let contexto = Arc::new(std::sync::OnceLock::new());
    let teclado = instalar_captura_de_teclado(&mut opciones, &contexto);

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

            let _ = contexto.set(cc.egui_ctx.clone());
            registrar_captura_de_teclado();

            let gpu = sesion::Gpu {
                device: render_state.device.clone(),
                queue: render_state.queue.clone(),
                formato,
            };

            let (compartido, entrada) = sesion::arrancar(cli.connect, gpu, cc.egui_ctx.clone());
            Ok(
                Box::new(app::App::nueva(Arc::clone(&compartido), entrada, teclado))
                    as Box<dyn eframe::App>,
            )
        }),
    )
    .map_err(|error| anyhow!("no se pudo abrir la ventana: {error}"))
}

/// Engancha la captura de teclado al bucle de eventos y devuelve la cola por la que llegan
/// las teclas.
///
/// # Por que hace falta un enganche de mensajes
///
/// Raw Input entrega `WM_INPUT` al procedimiento de ventana, y la ventana la crea winit por
/// debajo de eframe: no tenemos un `WndProc` propio. `with_msg_hook` se ejecuta antes de
/// despachar cada mensaje y es el unico punto donde podemos verlo.
///
/// El enganche **no se traga nada**: mira si el mensaje es `WM_INPUT`, y devuelve siempre
/// `false`, que es lo que winit interpreta como "despacha con normalidad". El trabajo que
/// hace por mensaje es una comparacion de enteros, porque por aqui pasan todos los mensajes
/// de la ventana.
#[cfg(windows)]
fn instalar_captura_de_teclado(
    opciones: &mut eframe::NativeOptions,
    contexto: &Arc<std::sync::OnceLock<eframe::egui::Context>>,
) -> Receiver<TeclaCapturada> {
    use egui_winit::winit::platform::windows::EventLoopBuilderExtWindows;

    let (emisor, receptor) = std::sync::mpsc::channel();
    let contexto = Arc::clone(contexto);

    let enganche = vhdesk_input::hook_de_mensajes(move |tecla| {
        if emisor.send(tecla).is_ok() {
            // Sin esto, la tecla se quedaria en la cola hasta el siguiente repintado. En la
            // practica Windows manda tambien el `WM_KEYDOWN` normal —no usamos
            // `RIDEV_NOLEGACY`— y ese si despierta a egui, pero depender de un efecto
            // colateral para no perder pulsaciones seria pedirlo por accidente.
            if let Some(ctx) = contexto.get() {
                ctx.request_repaint();
            }
        }
    });

    opciones.event_loop_builder = Some(Box::new(move |constructor| {
        constructor.with_msg_hook(enganche);
    }));

    receptor
}

/// Registra el teclado en Raw Input, ya con la ventana creada.
///
/// Si Windows lo rechaza, la sesion sigue: se puede ver la pantalla remota y usar el raton.
/// Lo que **no** se hace es caer en un hook global de bajo nivel como respaldo; ver el
/// invariante de seguridad en `vhdesk_input::captura`.
#[cfg(windows)]
fn registrar_captura_de_teclado() {
    if let Err(error) = vhdesk_input::registrar_teclado() {
        tracing::error!(%error, "sin captura de teclado: la sesion sera de ver y raton");
    }
}

// FASE 8: el equivalente en X11 es `XI2` con `XISelectEvents` sobre la ventana con foco, y
// en macOS un monitor local de `NSEvent`. Hasta entonces el viewer compila en esas
// plataformas pero solo maneja el raton.
#[cfg(not(windows))]
fn instalar_captura_de_teclado(
    _opciones: &mut eframe::NativeOptions,
    _contexto: &Arc<std::sync::OnceLock<eframe::egui::Context>>,
) -> Receiver<TeclaCapturada> {
    let (emisor, receptor) = std::sync::mpsc::channel();
    drop(emisor);
    receptor
}

#[cfg(not(windows))]
fn registrar_captura_de_teclado() {
    tracing::warn!("esta plataforma todavia no captura teclado: solo raton");
}

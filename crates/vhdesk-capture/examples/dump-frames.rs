//! Herramienta de verificacion del bloque A: vuelca frames a PNG y ensena lo que DXGI
//! esta contando en cada ciclo.
//!
//! No es una prueba automatizada: es para mirar con los ojos que la captura funciona de
//! verdad, que los rectangulos sucios se corresponden con lo que se mueve en pantalla y
//! que el puntero **no** aparece pintado en los frames.
//!
//! ```text
//! cargo run -p vhdesk-capture --example dump-frames -- --list
//! cargo run -p vhdesk-capture --example dump-frames -- --monitor 0:0 --frames 30
//! ```

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use vhdesk_capture::{
    CaptureEvent, CursorShape, Frame, MonitorId, ensure_dpi_awareness, enumerate_monitors,
    open_capturer,
};

struct Opciones {
    monitor: Option<MonitorId>,
    frames: usize,
    salida: PathBuf,
    listar: bool,
    guardar: bool,
    rects: bool,
    silencioso: bool,
    reposo: Option<Duration>,
}

/// Tiempo de CPU consumido por este proceso, sumando usuario y kernel.
///
/// Se mide desde dentro y no con el administrador de tareas porque lo que interesa es el
/// coste del capturador, no el de la ventana que lo lanzo.
#[cfg(windows)]
fn cpu_del_proceso() -> Option<Duration> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    let mut creacion = FILETIME::default();
    let mut salida = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut usuario = FILETIME::default();

    // SAFETY: los cuatro destinos son variables locales vivas durante la llamada, y el
    // pseudo-handle de GetCurrentProcess siempre es valido y no hay que cerrarlo.
    unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creacion,
            &mut salida,
            &mut kernel,
            &mut usuario,
        )
    }
    .ok()?;

    // FILETIME cuenta en unidades de 100 ns repartidas en dos mitades de 32 bits.
    let a_nanos = |t: FILETIME| {
        ((u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)).saturating_mul(100)
    };

    Some(Duration::from_nanos(
        a_nanos(kernel).saturating_add(a_nanos(usuario)),
    ))
}

#[cfg(not(windows))]
fn cpu_del_proceso() -> Option<Duration> {
    None
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("VHDESK_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Esta llamada corresponde al binario, no a la libreria. Sin ella, con escalado activo
    // las dimensiones que se imprimen abajo no serian las reales del panel.
    if !ensure_dpi_awareness() {
        eprintln!("aviso: no se pudo declarar la conciencia de DPI del proceso");
    }

    let opciones = parsear_argumentos()?;
    let monitores = enumerate_monitors().context("enumerar monitores")?;

    if opciones.listar {
        for monitor in &monitores {
            println!(
                "{}  {}  {}x{}  en ({}, {})  escala {:.2}{}\n     adaptador: {}",
                monitor.id,
                monitor.name,
                monitor.width,
                monitor.height,
                monitor.position.0,
                monitor.position.1,
                monitor.scale,
                if monitor.primary { "  [principal]" } else { "" },
                monitor.adapter_name
            );
        }
        return Ok(());
    }

    let id = match opciones.monitor {
        Some(id) => id,
        None => {
            let elegido = monitores
                .iter()
                .find(|m| m.primary)
                .or_else(|| monitores.first())
                .context("no hay monitores")?;
            elegido.id
        }
    };

    if opciones.guardar && opciones.reposo.is_none() {
        std::fs::create_dir_all(&opciones.salida)
            .with_context(|| format!("crear {}", opciones.salida.display()))?;
    }

    let mut capturador = open_capturer(id).context("abrir la captura")?;

    if let Some(duracion) = opciones.reposo {
        return medir_en_reposo(capturador.as_mut(), duracion, opciones.rects);
    }
    let monitor = capturador.monitor().clone();
    println!(
        "capturando {} ({}) {}x{} escala {:.2}",
        monitor.id, monitor.name, monitor.width, monitor.height, monitor.scale
    );
    println!("mueve ventanas por la pantalla para ver rectangulos sucios pequenos\n");

    let mut volcados = 0usize;
    let mut timeouts = 0usize;
    let mut solo_cursor = 0usize;
    let mut formas_de_cursor = 0usize;
    let inicio = Instant::now();

    while volcados < opciones.frames {
        match capturador
            .next_frame(Duration::from_millis(500))
            .context("capturar")?
        {
            CaptureEvent::Frame(frame) => {
                volcados += 1;
                if !opciones.silencioso {
                    informar(&frame, volcados);
                }
                if opciones.rects {
                    for rect in frame.dirty.iter().take(12) {
                        println!(
                            "      sucio ({}, {}) - ({}, {})  {}x{}",
                            rect.left,
                            rect.top,
                            rect.right,
                            rect.bottom,
                            rect.width(),
                            rect.height()
                        );
                    }
                }

                if let Some(forma) = frame.cursor.as_ref().and_then(|c| c.shape.as_ref()) {
                    formas_de_cursor += 1;
                    if opciones.guardar {
                        guardar_cursor(&opciones.salida, formas_de_cursor, forma)?;
                    }
                }

                if opciones.guardar {
                    guardar_frame(&opciones.salida, volcados, &frame)?;
                }
            }
            CaptureEvent::CursorOnly(actualizacion) => {
                solo_cursor += 1;
                if let Some(forma) = actualizacion.shape.as_ref() {
                    formas_de_cursor += 1;
                    if opciones.guardar {
                        guardar_cursor(&opciones.salida, formas_de_cursor, forma)?;
                    }
                }
            }
            CaptureEvent::Timeout => timeouts += 1,
        }
    }

    let transcurrido = inicio.elapsed();
    println!(
        "\n{volcados} frames en {:.2} s ({:.1} fps de media)",
        transcurrido.as_secs_f64(),
        volcados as f64 / transcurrido.as_secs_f64()
    );
    println!("{solo_cursor} eventos de solo cursor, {timeouts} timeouts");
    println!("{formas_de_cursor} formas de puntero distintas");
    if opciones.guardar {
        println!("PNG en {}", opciones.salida.display());
    }

    Ok(())
}

/// Mide que hace el capturador cuando no pasa nada en pantalla.
///
/// Es el estado normal de un escritorio de trabajo, y lo que determina el consumo del host
/// en reposo. Un capturador que queme un nucleo esperando a que algo cambie es inaceptable
/// en una maquina donde alguien esta trabajando, aunque el resto del pipeline sea perfecto.
fn medir_en_reposo(
    capturador: &mut dyn vhdesk_capture::ScreenCapturer,
    duracion: Duration,
    detallar: bool,
) -> Result<()> {
    // Espera larga a proposito: es lo que hara el host. Con esperas cortas el bucle gira
    // en vacio y el consumo en reposo lo fija el bucle, no la captura.
    const ESPERA: Duration = Duration::from_millis(100);

    println!(
        "midiendo {:.0} s con la pantalla quieta; no toques el raton ni el teclado\n",
        duracion.as_secs_f64()
    );

    let cpu_inicial = cpu_del_proceso();
    let inicio = Instant::now();

    let (mut frames, mut timeouts, mut solo_cursor) = (0u64, 0u64, 0u64);
    // Desglose de los eventos de cursor, para poder decir *que* los dispara en vez de
    // limitarse a contarlos.
    let (mut con_forma, mut con_posicion_nueva) = (0u64, 0u64);
    let mut ultima_posicion = None;

    while inicio.elapsed() < duracion {
        match capturador.next_frame(ESPERA).context("capturar")? {
            CaptureEvent::Frame(_) => frames += 1,
            CaptureEvent::CursorOnly(actualizacion) => {
                solo_cursor += 1;
                if detallar && solo_cursor <= 10 {
                    println!(
                        "  cursor visible={} en ({}, {}){}",
                        actualizacion.visible,
                        actualizacion.position.x,
                        actualizacion.position.y,
                        if actualizacion.shape.is_some() {
                            "  forma nueva"
                        } else {
                            ""
                        }
                    );
                }
                if actualizacion.shape.is_some() {
                    con_forma += 1;
                }
                if ultima_posicion != Some(actualizacion.position) {
                    con_posicion_nueva += 1;
                    ultima_posicion = Some(actualizacion.position);
                }
            }
            CaptureEvent::Timeout => timeouts += 1,
        }
    }

    let transcurrido = inicio.elapsed();
    let segundos = transcurrido.as_secs_f64();

    println!("en {segundos:.1} s:");
    println!(
        "  {timeouts:>6} timeouts      ({:.1}/s)",
        timeouts as f64 / segundos
    );
    println!(
        "  {frames:>6} frames        ({:.1}/s)",
        frames as f64 / segundos
    );
    println!(
        "  {solo_cursor:>6} solo cursor   ({:.1}/s)",
        solo_cursor as f64 / segundos
    );
    if solo_cursor > 0 {
        println!(
            "         de ellos {con_forma} con forma nueva, {con_posicion_nueva} con posicion nueva"
        );
    }

    match (cpu_inicial, cpu_del_proceso()) {
        (Some(antes), Some(despues)) => {
            let consumida = despues.saturating_sub(antes);
            let nucleos = consumida.as_secs_f64() / segundos;
            println!(
                "\n  CPU del proceso: {:.0} ms ({:.2}% de un nucleo)",
                consumida.as_secs_f64() * 1000.0,
                nucleos * 100.0
            );
        }
        _ => println!("\n  CPU del proceso: no disponible en esta plataforma"),
    }

    if frames > 0 {
        println!(
            "\naviso: llegaron {frames} frames, asi que la pantalla NO estuvo quieta del \
             todo y el consumo medido esta inflado"
        );
    }

    Ok(())
}

fn informar(frame: &Frame, indice: usize) {
    let sucios: u64 = frame
        .dirty
        .iter()
        .map(|r| u64::from(r.width()) * u64::from(r.height()))
        .sum();
    let total = u64::from(frame.width) * u64::from(frame.height);
    let porcentaje = if total > 0 {
        sucios as f64 * 100.0 / total as f64
    } else {
        0.0
    };

    println!(
        "#{indice:<3} seq={:<5} {}x{} acum={:<2} {:>3} sucios ({porcentaje:>5.1}% de la pantalla) \
         {:>2} movidos{}{}",
        frame.sequence,
        frame.width,
        frame.height,
        frame.accumulated_frames,
        frame.dirty.len(),
        frame.moves.len(),
        if frame.full_refresh {
            "  REFRESCO COMPLETO"
        } else {
            ""
        },
        match &frame.cursor {
            Some(c) if c.shape.is_some() => "  forma de cursor nueva",
            Some(_) => "  cursor movido",
            None => "",
        }
    );
}

fn guardar_frame(directorio: &Path, indice: usize, frame: &Frame) -> Result<()> {
    let mut rgba = vec![0u8; frame.width as usize * frame.height as usize * 4];

    for y in 0..frame.height {
        let fila = frame.row(y).context("fila fuera del frame")?;
        let destino = y as usize * frame.width as usize * 4;
        for (x, pixel) in fila.chunks_exact(4).enumerate() {
            let o = destino + x * 4;
            // El frame viene en BGRA y PNG quiere RGBA.
            rgba[o] = pixel[2];
            rgba[o + 1] = pixel[1];
            rgba[o + 2] = pixel[0];
            rgba[o + 3] = 255;
        }
    }

    escribir_png(
        &directorio.join(format!("frame-{indice:04}.png")),
        frame.width,
        frame.height,
        &rgba,
    )
}

fn guardar_cursor(directorio: &Path, indice: usize, forma: &CursorShape) -> Result<()> {
    escribir_png(
        &directorio.join(format!("cursor-{indice:03}.png")),
        forma.width,
        forma.height,
        &forma.rgba,
    )
}

fn escribir_png(ruta: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
    let archivo = File::create(ruta).with_context(|| format!("crear {}", ruta.display()))?;
    let mut encoder = png::Encoder::new(BufWriter::new(archivo), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);

    let mut escritor = encoder.write_header().context("cabecera PNG")?;
    escritor.write_image_data(rgba).context("datos PNG")?;
    Ok(())
}

fn parsear_argumentos() -> Result<Opciones> {
    let mut opciones = Opciones {
        monitor: None,
        frames: 10,
        salida: PathBuf::from("target/capturas"),
        listar: false,
        guardar: true,
        rects: false,
        silencioso: false,
        reposo: None,
    };

    let mut argumentos = std::env::args().skip(1);
    while let Some(argumento) = argumentos.next() {
        match argumento.as_str() {
            "--list" => opciones.listar = true,
            "--no-save" => opciones.guardar = false,
            "--rects" => opciones.rects = true,
            "--idle" => {
                let valor = argumentos.next().context("--idle necesita segundos")?;
                let segundos: f64 = valor.parse().context("--idle debe ser un numero")?;
                opciones.reposo = Some(Duration::from_secs_f64(segundos));
            }
            // Imprimir por cada frame cambia la pantalla y genera los frames siguientes.
            // Con --quiet se puede observar una pantalla de verdad quieta.
            "--quiet" => opciones.silencioso = true,
            "--monitor" => {
                let valor = argumentos.next().context("--monitor necesita un valor")?;
                opciones.monitor = Some(parsear_monitor(&valor)?);
            }
            "--frames" => {
                let valor = argumentos.next().context("--frames necesita un valor")?;
                opciones.frames = valor.parse().context("--frames debe ser un numero")?;
            }
            "--out" => {
                let valor = argumentos.next().context("--out necesita un valor")?;
                opciones.salida = PathBuf::from(valor);
            }
            "--help" | "-h" => {
                println!(
                    "uso: dump-frames [--list] [--monitor ADAPTADOR:SALIDA] [--frames N] \
                     [--out DIR] [--no-save] [--rects] [--quiet] [--idle SEGUNDOS]"
                );
                std::process::exit(0);
            }
            otro => bail!("argumento desconocido: {otro}"),
        }
    }

    Ok(opciones)
}

fn parsear_monitor(valor: &str) -> Result<MonitorId> {
    // Se admite "0" como atajo de "0:0", que es el caso de una sola grafica.
    let (adaptador, salida) = match valor.split_once(':') {
        Some((a, s)) => (a, s),
        None => (valor, "0"),
    };

    Ok(MonitorId {
        adapter: adaptador.parse().context("indice de adaptador invalido")?,
        output: salida.parse().context("indice de salida invalido")?,
    })
}

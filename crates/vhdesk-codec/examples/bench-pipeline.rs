//! Mide el pipeline de codificacion sobre **capturas reales del escritorio**.
//!
//! Se captura de la pantalla en lugar de generar patrones sinteticos porque el coste del
//! codificador depende mucho del contenido: el ruido aleatorio es su peor caso y no se
//! parece en nada a un escritorio, y un degradado liso es su mejor caso y tampoco.
//!
//! Se reportan **percentiles y no solo medias**. Un codificador que tarda 8 ms de media
//! pero se va a 40 ms una vez por segundo produce un tiron perfectamente visible que la
//! media esconde. Para 60 fps el presupuesto por frame es de 16,7 ms **para todo el
//! pipeline**, asi que el p99 es la cifra que decide si el diseno aguanta.
//!
//! ```text
//! cargo run -p vhdesk-codec --example bench-pipeline --release
//! ```

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use vhdesk_capture::{CaptureEvent, ensure_dpi_awareness, enumerate_monitors, open_capturer};
use vhdesk_codec::{EncoderConfig, I420Frame, VideoDecoder, VideoEncoder, Vp8Decoder, Vp8Encoder};

/// Frames distintos que se guardan para alimentar al codificador.
///
/// No se guardan mas porque cada uno son 8,3 MiB: treinta ya son 250 MB.
const FRAMES_CAPTURADOS: usize = 30;
/// Frames de los que se recogen tiempos de captura, sin guardar sus pixeles.
///
/// Los tiempos ocupan 16 bytes, asi que aqui si se puede tener una muestra con la que un
/// p99 signifique algo: con 30 muestras el "p99" es literalmente el maximo.
const MUESTRAS_CAPTURA: usize = 300;
/// Pasadas sobre esa secuencia, para tener muestras suficientes para un p99 con sentido.
const PASADAS: usize = 10;
/// Cada cuantos frames se fuerza un keyframe durante la medida.
///
/// Con el intervalo real de produccion (4 s a 60 fps son 240 frames) saldrian dos o tres
/// keyframes en toda la ejecucion, y un p99 sobre tres muestras no significa nada. Se
/// fuerzan mas solo para poder caracterizar su coste, que es el tiron mas visible del
/// pipeline.
const KEYFRAME_CADA: u64 = 15;

fn main() -> Result<()> {
    if cfg!(debug_assertions) {
        bail!("compilado sin optimizar; estos numeros no sirven. Ejecutalo con --release");
    }

    ensure_dpi_awareness();

    let capturas = capturar()?;
    let (ancho, alto, stride) = (capturas.ancho, capturas.alto, capturas.stride);
    println!(
        "\n{} frames reales de {ancho}x{alto} capturados\n",
        capturas.frames.len()
    );

    let mut conversion = Vec::new();
    let mut encode_key = Vec::new();
    let mut encode_inter = Vec::new();
    // Se acumula dentro del bucle, no sumando los vectores despues: hay que emparejar los
    // dos tiempos **del mismo frame**, y en cuanto se ordenan para sacar percentiles ya no
    // se sabe cual iba con cual.
    let mut total = Vec::new();
    let mut copia_decodificado = Vec::new();
    let mut tamanos_key = Vec::new();
    let mut tamanos_inter = Vec::new();

    let mut map_wait = capturas.map_wait.clone();
    let mut download = capturas.download.clone();
    let mut i420 = I420Frame::new(ancho, alto)?;
    let mut destino = I420Frame::new(ancho, alto)?;
    let mut encoder = Vp8Encoder::new(EncoderConfig::lan(ancho, alto))?;
    let mut decoder = Vp8Decoder::new()?;

    let mut indice = 0u64;
    for _ in 0..PASADAS {
        for bgra in &capturas.frames {
            let timestamp = indice * 16_666;
            if indice > 0 && indice % KEYFRAME_CADA == 0 {
                encoder.request_keyframe();
            }
            indice += 1;

            let t0 = Instant::now();
            i420.fill_from_bgra(bgra, stride)?;
            let convertir = t0.elapsed();
            conversion.push(convertir);

            let t1 = Instant::now();
            let salida = encoder.encode(&i420, timestamp)?;
            let encode = t1.elapsed();
            total.push(convertir + encode);

            let Some(comprimido) = salida else { continue };
            if comprimido.keyframe {
                encode_key.push(encode);
                tamanos_key.push(comprimido.data.len());
            } else {
                encode_inter.push(encode);
                tamanos_inter.push(comprimido.data.len());
            }

            if let Some(decodificado) = decoder.decode(&comprimido.data)? {
                let t2 = Instant::now();
                decodificado.copy_into(&mut destino)?;
                copia_decodificado.push(t2.elapsed());
            }
        }
    }

    let presupuesto = Duration::from_micros(16_666);

    println!(
        "{:>28}  {:>8} {:>8} {:>8} {:>8}",
        "", "media", "p50", "p95", "p99"
    );
    informar("staging: espera a la GPU", &mut map_wait, presupuesto);
    informar("staging: descarga a memoria", &mut download, presupuesto);
    informar("BGRA -> I420", &mut conversion, presupuesto);
    informar("encode VP8 (keyframe)", &mut encode_key, presupuesto);
    informar("encode VP8 (inter)", &mut encode_inter, presupuesto);
    informar(
        "copia del decodificado",
        &mut copia_decodificado,
        presupuesto,
    );

    // Lo que de verdad decide si el diseno aguanta: los dos costes del mismo frame.
    println!();
    informar("conversion + encode", &mut total, presupuesto);

    informar_tamanos(&tamanos_key, &tamanos_inter);

    println!(
        "\npresupuesto por frame a 60 fps: {:.1} ms; a 30 fps: {:.1} ms",
        presupuesto.as_secs_f64() * 1000.0,
        presupuesto.as_secs_f64() * 2000.0
    );

    Ok(())
}

struct Capturas {
    frames: Vec<Vec<u8>>,
    ancho: u32,
    alto: u32,
    stride: usize,
    /// Espera a que la GPU termine la copia a la textura intermedia.
    map_wait: Vec<Duration>,
    /// Lectura del mapeo y escritura al buffer del pool.
    download: Vec<Duration>,
}

/// Recoge frames reales del monitor principal.
fn capturar() -> Result<Capturas> {
    let monitores = enumerate_monitors().context("enumerar monitores")?;
    let elegido = monitores
        .iter()
        .find(|m| m.primary)
        .or_else(|| monitores.first())
        .context("no hay monitores")?;

    let mut capturador = open_capturer(elegido.id).context("abrir la captura")?;
    println!(
        "capturando {} frames de {} ({}x{})",
        FRAMES_CAPTURADOS, elegido.name, elegido.width, elegido.height
    );
    println!("mueve ventanas mientras tanto para que el contenido sea representativo");

    let mut frames = Vec::with_capacity(FRAMES_CAPTURADOS);
    let mut map_wait = Vec::with_capacity(MUESTRAS_CAPTURA);
    let mut download = Vec::with_capacity(MUESTRAS_CAPTURA);
    let (mut ancho, mut alto, mut stride) = (0u32, 0u32, 0usize);
    let limite = Instant::now() + Duration::from_secs(60);

    while frames.len() < FRAMES_CAPTURADOS || map_wait.len() < MUESTRAS_CAPTURA {
        if Instant::now() > limite {
            bail!(
                "solo llegaron {} frames y {} muestras en 60 s; mueve algo en pantalla",
                frames.len(),
                map_wait.len()
            );
        }

        match capturador.next_frame(Duration::from_millis(500))? {
            CaptureEvent::Frame(frame) => {
                ancho = frame.width;
                alto = frame.height;
                stride = frame.stride;
                // El capturador ya cronometro esto por dentro; aqui solo se recoge.
                if map_wait.len() < MUESTRAS_CAPTURA {
                    map_wait.push(frame.timings.map_wait);
                    download.push(frame.timings.download);
                }
                // Se copian a memoria propia porque el buffer vuelve al pool al soltarlo.
                // Esto es preparacion, no esta dentro de ninguna medida.
                if frames.len() < FRAMES_CAPTURADOS {
                    frames.push(frame.buffer.to_vec());
                }
            }
            CaptureEvent::CursorOnly(_) | CaptureEvent::Timeout => continue,
        }
    }

    Ok(Capturas {
        frames,
        ancho,
        alto,
        stride,
        map_wait,
        download,
    })
}

fn informar(etiqueta: &str, muestras: &mut [Duration], presupuesto: Duration) {
    if muestras.is_empty() {
        println!("{etiqueta:>28}  (sin muestras)");
        return;
    }

    muestras.sort_unstable();
    let media = muestras.iter().sum::<Duration>() / muestras.len() as u32;
    let p99 = percentil(muestras, 0.99);

    println!(
        "{etiqueta:>28}  {:>7.2}ms {:>7.2}ms {:>7.2}ms {:>7.2}ms   n={}{}",
        ms(media),
        ms(percentil(muestras, 0.50)),
        ms(percentil(muestras, 0.95)),
        ms(p99),
        muestras.len(),
        if p99 > presupuesto {
            "  <-- p99 SE PASA"
        } else {
            ""
        }
    );
}

fn informar_tamanos(keyframes: &[usize], inter: &[usize]) {
    let media = |v: &[usize]| {
        if v.is_empty() {
            0.0
        } else {
            v.iter().sum::<usize>() as f64 / v.len() as f64
        }
    };

    let media_key = media(keyframes);
    let media_inter = media(inter);

    println!("\ntamano medio de frame comprimido:");
    println!(
        "  keyframe: {:>8.0} bytes  ({} muestras)",
        media_key,
        keyframes.len()
    );
    println!(
        "  inter:    {:>8.0} bytes  ({} muestras)",
        media_inter,
        inter.len()
    );

    // Bitrate con la mezcla real de keyframes e inter que produjo el codificador.
    let total: usize = keyframes.iter().sum::<usize>() + inter.iter().sum::<usize>();
    let cuantos = keyframes.len() + inter.len();
    if cuantos > 0 {
        let bits_por_frame = total as f64 * 8.0 / cuantos as f64;
        println!(
            "  bitrate a 60 fps: {:.1} Mbps   a 30 fps: {:.1} Mbps",
            bits_por_frame * 60.0 / 1_000_000.0,
            bits_por_frame * 30.0 / 1_000_000.0
        );
    }
}

/// Percentil por seleccion directa sobre la muestra ya ordenada.
///
/// Sin interpolacion: con cientos de muestras la diferencia es irrelevante y asi el numero
/// que se imprime es siempre un tiempo que ocurrio de verdad.
fn percentil(ordenado: &[Duration], p: f64) -> Duration {
    let indice = ((ordenado.len() as f64 - 1.0) * p).round() as usize;
    ordenado[indice.min(ordenado.len() - 1)]
}

fn ms(duracion: Duration) -> f64 {
    duracion.as_secs_f64() * 1000.0
}

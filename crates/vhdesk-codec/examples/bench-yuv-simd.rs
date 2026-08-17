//! Compara la conversion BGRA -> I420 escalar propia contra el crate `yuv`, que usa SIMD
//! despachado en tiempo de ejecucion.
//!
//! Sobre **frames reales del escritorio**, no ruido ni degradados: el coste de la
//! conversion no depende del contenido, pero el de todo lo demas si, y medir sobre lo
//! mismo que medimos en `bench-pipeline` mantiene las cifras comparables.
//!
//! ```text
//! cargo run -p vhdesk-codec --example bench-yuv-simd --release
//! ```
//!
//! El criterio de decision se fijo **antes** de ver estos numeros, y esta en las notas de
//! sesion de CLAUDE.md: se adopta `yuv` si su p99 baja de la mitad del escalar y los
//! valores de referencia BT.601 coinciden exactamente.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use vhdesk_capture::{CaptureEvent, ensure_dpi_awareness, enumerate_monitors, open_capturer};
use vhdesk_codec::I420Frame;
use yuv::{
    SharpYuvGammaTransfer, YuvChromaSubsampling, YuvConversionMode, YuvPlanarImageMut, YuvRange,
    YuvStandardMatrix, bgra_to_sharp_yuv420, bgra_to_yuv420,
};

const FRAMES_CAPTURADOS: usize = 20;
const CALENTAMIENTO: usize = 20;
const PASADAS: usize = 15;

fn main() -> Result<()> {
    if cfg!(debug_assertions) {
        bail!("compilado sin optimizar; estos numeros no sirven. Ejecutalo con --release");
    }

    ensure_dpi_awareness();

    let capturas = capturar()?;
    let (ancho, alto, stride) = (capturas.ancho, capturas.alto, capturas.stride);
    println!(
        "\n{} frames reales de {ancho}x{alto}, {CALENTAMIENTO} iteraciones de calentamiento\n",
        capturas.frames.len()
    );

    let escalar = medir_escalar(&capturas, ancho, alto, stride)?;
    // `Fast` es el algoritmo de libyuv, o sea el punto de comparacion con RustDesk.
    // `Balanced` es el modo por defecto del crate, mas preciso.
    let rapida = medir_yuv(&capturas, ancho, alto, stride, YuvConversionMode::Fast)?;
    let equilibrada = medir_yuv(&capturas, ancho, alto, stride, YuvConversionMode::Balanced)?;
    let sharp = medir_sharp(&capturas, ancho, alto, stride)?;

    println!(
        "{:>34}  {:>8} {:>8} {:>8} {:>8}",
        "", "media", "p50", "p95", "p99"
    );
    informar("escalar propia", &escalar);
    informar("yuv::bgra_to_yuv420 (Fast)", &rapida);
    informar("yuv::bgra_to_yuv420 (Balanced)", &equilibrada);
    informar("yuv::bgra_to_sharp_yuv420", &sharp);

    let p99_escalar = percentil(&escalar, 0.99);
    let umbral = p99_escalar / 2;
    println!(
        "\numbral de adopcion fijado de antemano: p99 <= {:.2} ms (la mitad del escalar)",
        ms(umbral)
    );

    for (nombre, muestras) in [
        ("bgra_to_yuv420 Fast", &rapida),
        ("bgra_to_yuv420 Balanced", &equilibrada),
        ("bgra_to_sharp_yuv420", &sharp),
    ] {
        let p99 = percentil(muestras, 0.99);
        println!(
            "  {nombre:<22} p99 {:>6.2} ms   {:.1}x mas rapida que el escalar   {}",
            ms(p99),
            p99_escalar.as_secs_f64() / p99.as_secs_f64(),
            if p99 <= umbral { "CUMPLE" } else { "NO CUMPLE" }
        );
    }

    Ok(())
}

fn medir_escalar(
    capturas: &Capturas,
    ancho: u32,
    alto: u32,
    stride: usize,
) -> Result<Vec<Duration>> {
    let mut destino = I420Frame::new(ancho, alto)?;

    for _ in 0..CALENTAMIENTO {
        destino.fill_from_bgra(&capturas.frames[0], stride)?;
    }

    let mut muestras = Vec::new();
    for _ in 0..PASADAS {
        for bgra in &capturas.frames {
            let inicio = Instant::now();
            destino.fill_from_bgra(bgra, stride)?;
            muestras.push(inicio.elapsed());
        }
    }
    Ok(muestras)
}

fn medir_yuv(
    capturas: &Capturas,
    ancho: u32,
    alto: u32,
    stride: usize,
    modo: YuvConversionMode,
) -> Result<Vec<Duration>> {
    let mut destino = YuvPlanarImageMut::<u8>::alloc(ancho, alto, YuvChromaSubsampling::Yuv420);
    let stride = stride as u32;

    for _ in 0..CALENTAMIENTO {
        convertir(&mut destino, &capturas.frames[0], stride, modo)?;
    }

    let mut muestras = Vec::new();
    for _ in 0..PASADAS {
        for bgra in &capturas.frames {
            let inicio = Instant::now();
            convertir(&mut destino, bgra, stride, modo)?;
            muestras.push(inicio.elapsed());
        }
    }
    Ok(muestras)
}

fn medir_sharp(capturas: &Capturas, ancho: u32, alto: u32, stride: usize) -> Result<Vec<Duration>> {
    let mut destino = YuvPlanarImageMut::<u8>::alloc(ancho, alto, YuvChromaSubsampling::Yuv420);
    let stride = stride as u32;

    for _ in 0..CALENTAMIENTO {
        bgra_to_sharp_yuv420(
            &mut destino,
            &capturas.frames[0],
            stride,
            YuvRange::Limited,
            YuvStandardMatrix::Bt601,
            SharpYuvGammaTransfer::Srgb,
        )?;
    }

    let mut muestras = Vec::new();
    for _ in 0..PASADAS {
        for bgra in &capturas.frames {
            let inicio = Instant::now();
            bgra_to_sharp_yuv420(
                &mut destino,
                bgra,
                stride,
                YuvRange::Limited,
                YuvStandardMatrix::Bt601,
                SharpYuvGammaTransfer::Srgb,
            )?;
            muestras.push(inicio.elapsed());
        }
    }
    Ok(muestras)
}

/// Misma configuracion de color que la implementacion propia: BT.601, rango limitado.
fn convertir(
    destino: &mut YuvPlanarImageMut<'_, u8>,
    bgra: &[u8],
    stride: u32,
    modo: YuvConversionMode,
) -> Result<()> {
    bgra_to_yuv420(
        destino,
        bgra,
        stride,
        YuvRange::Limited,
        YuvStandardMatrix::Bt601,
        modo,
    )?;
    Ok(())
}

struct Capturas {
    frames: Vec<Vec<u8>>,
    ancho: u32,
    alto: u32,
    stride: usize,
}

fn capturar() -> Result<Capturas> {
    let monitores = enumerate_monitors().context("enumerar monitores")?;
    let elegido = monitores
        .iter()
        .find(|m| m.primary)
        .or_else(|| monitores.first())
        .context("no hay monitores")?;

    let mut capturador = open_capturer(elegido.id).context("abrir la captura")?;
    println!(
        "capturando {FRAMES_CAPTURADOS} frames reales de {}",
        elegido.name
    );

    let mut frames = Vec::with_capacity(FRAMES_CAPTURADOS);
    let (mut ancho, mut alto, mut stride) = (0u32, 0u32, 0usize);
    let limite = Instant::now() + Duration::from_secs(30);

    while frames.len() < FRAMES_CAPTURADOS {
        if Instant::now() > limite {
            bail!("no llegaron {FRAMES_CAPTURADOS} frames en 30 s; mueve algo en pantalla");
        }
        match capturador.next_frame(Duration::from_millis(500))? {
            CaptureEvent::Frame(frame) => {
                ancho = frame.width;
                alto = frame.height;
                stride = frame.stride;
                frames.push(frame.buffer.to_vec());
            }
            CaptureEvent::CursorOnly(_) | CaptureEvent::Timeout => continue,
        }
    }

    Ok(Capturas {
        frames,
        ancho,
        alto,
        stride,
    })
}

fn informar(etiqueta: &str, muestras: &[Duration]) {
    let media = muestras.iter().sum::<Duration>() / muestras.len() as u32;
    println!(
        "{etiqueta:>34}  {:>7.2}ms {:>7.2}ms {:>7.2}ms {:>7.2}ms   n={}",
        ms(media),
        ms(percentil(muestras, 0.50)),
        ms(percentil(muestras, 0.95)),
        ms(percentil(muestras, 0.99)),
        muestras.len()
    );
}

fn percentil(muestras: &[Duration], p: f64) -> Duration {
    let mut ordenado = muestras.to_vec();
    ordenado.sort_unstable();
    let indice = ((ordenado.len() as f64 - 1.0) * p).round() as usize;
    ordenado[indice.min(ordenado.len() - 1)]
}

fn ms(duracion: Duration) -> f64 {
    duracion.as_secs_f64() * 1000.0
}

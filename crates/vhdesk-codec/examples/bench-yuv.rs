//! Mide lo que cuesta convertir BGRA a I420.
//!
//! Esta conversion esta en el camino de todos los frames y recorre 8,3 MiB por frame de
//! 1080p, asi que es candidata a ser el cuello de botella del pipeline. El numero de aqui
//! es la linea base contra la que comparar cualquier optimizacion de la fase 4, sea SIMD o
//! subirla a la GPU.
//!
//! ```text
//! cargo run -p vhdesk-codec --example bench-yuv --release
//! ```
//!
//! **En release.** En debug la conversion es un orden de magnitud mas lenta y el numero no
//! significa nada.

use std::time::Instant;

use anyhow::Result;
use vhdesk_codec::I420Frame;

/// Resoluciones que interesan: la del portatil de desarrollo, la que se anuncia como
/// objetivo, y una 4K para ver como escala.
const RESOLUCIONES: &[(u32, u32, &str)] = &[
    (1280, 720, "720p"),
    (1920, 1080, "1080p"),
    (2560, 1440, "1440p"),
    (3840, 2160, "4K"),
];

const ITERACIONES: u32 = 60;

fn main() -> Result<()> {
    if cfg!(debug_assertions) {
        eprintln!(
            "AVISO: compilado sin optimizar. Estos numeros no sirven para nada.\n\
             Ejecutalo con --release.\n"
        );
    }

    println!(
        "{:>7}  {:>12}  {:>10}  {:>12}  {:>10}",
        "", "por frame", "MiB/frame", "MiB/s", "fps max"
    );

    for (width, height, nombre) in RESOLUCIONES {
        let bgra = escritorio_sintetico(*width, *height);
        let mut frame = I420Frame::new(*width, *height)?;
        let stride = *width as usize * 4;

        // Una pasada previa para que el buffer este caliente y no medir el primer fallo de
        // cache de toda la imagen.
        frame.fill_from_bgra(&bgra, stride)?;

        let inicio = Instant::now();
        for _ in 0..ITERACIONES {
            frame.fill_from_bgra(&bgra, stride)?;
        }
        let transcurrido = inicio.elapsed();

        let por_frame = transcurrido / ITERACIONES;
        let mib = bgra.len() as f64 / (1024.0 * 1024.0);
        let mib_por_segundo = mib / por_frame.as_secs_f64();

        println!(
            "{nombre:>7}  {:>9.2} ms  {mib:>10.1}  {mib_por_segundo:>12.0}  {:>10.0}",
            por_frame.as_secs_f64() * 1000.0,
            1.0 / por_frame.as_secs_f64()
        );
    }

    println!(
        "\n'fps max' es el techo que impone solo esta conversion en un hilo, sin contar\n\
         captura, codificacion ni red."
    );

    Ok(())
}

/// Genera algo parecido a un escritorio: fondo liso, ventanas y texto.
///
/// Un degradado o ruido puro medirian lo mismo (la conversion no depende del contenido),
/// pero un patron reconocible evita que el compilador se invente atajos sobre datos
/// constantes.
fn escritorio_sintetico(width: u32, height: u32) -> Vec<u8> {
    let mut datos = vec![0u8; (width as usize) * (height as usize) * 4];

    for y in 0..height as usize {
        for x in 0..width as usize {
            let p = (y * width as usize + x) * 4;

            let en_ventana = x > width as usize / 8
                && x < width as usize * 3 / 4
                && y > height as usize / 8
                && y < height as usize * 3 / 4;
            let en_texto = en_ventana && (y / 3) % 4 == 0 && (x / 2) % 3 != 0;

            let (b, g, r) = if en_texto {
                (32, 32, 32)
            } else if en_ventana {
                (250, 250, 245)
            } else {
                (90, 60, 40)
            };

            datos[p] = b;
            datos[p + 1] = g;
            datos[p + 2] = r;
            datos[p + 3] = 255;
        }
    }

    datos
}

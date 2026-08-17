//! Prueba manual de la inyeccion de entrada. **Mueve el raton y escribe de verdad.**
//!
//! ```text
//! cargo run -p vhdesk-input --example prueba-input
//! ```
//!
//! Espera tres segundos antes de empezar, con cuenta atras, para dar tiempo a poner el foco
//! donde interese (un editor de texto vacio) o a abortar con Ctrl+C.
//!
//! # Sobre la parte de teclado
//!
//! Lo unico determinista es el ASCII basico: las letras y numeros salen igual en cualquier
//! distribucion porque la tecla fisica coincide. Todo lo demas **depende del mapa de
//! teclado del host** y hay que mirarlo a ojo, que es justo lo que se quiere comprobar. Un
//! test que fingiera conocer la distribucion pasaria siempre sin verificar nada.

use std::time::Duration;

use anyhow::{Context, Result};
use vhdesk_input::{InputInjector, open_injector};
use vhdesk_proto::MouseButton;

/// Suelta todo lo hundido cuando esto se destruye, **incluso si el ejemplo entra en
/// panico**.
///
/// No es paranoia: si el proceso aborta a mitad de la secuencia con Mayus hundido, la
/// maquina se queda asi hasta que alguien pulse y suelte esa tecla a mano, y el usuario no
/// tiene forma de saber que es lo que pasa.
struct Guardia<'a> {
    injector: &'a mut dyn InputInjector,
}

impl Drop for Guardia<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.injector.liberar_todo() {
            eprintln!("aviso: no se pudo liberar la entrada hundida: {error}");
        }
    }
}

/// Usage IDs de HID de las teclas que usa la prueba.
mod hid {
    pub const A: u32 = 0x04;
    pub const H: u32 = 0x0B;
    pub const O: u32 = 0x12;
    pub const L: u32 = 0x0F;
    pub const V: u32 = 0x19;
    pub const D: u32 = 0x07;
    pub const E: u32 = 0x08;
    pub const S: u32 = 0x16;
    pub const K: u32 = 0x0E;
    pub const ESPACIO: u32 = 0x2C;
    pub const ENTER: u32 = 0x28;
    pub const MAYUS_IZQ: u32 = 0xE1;
    pub const ALT_DER: u32 = 0xE6;
    pub const CTRL_IZQ: u32 = 0xE0;
    pub const FLECHA_IZQ: u32 = 0x50;
    pub const FLECHA_DER: u32 = 0x4F;
    pub const INICIO: u32 = 0x4A;
    pub const FIN: u32 = 0x4D;
    /// La tecla a la derecha de la P: acento en espanol, corchete en ingles.
    pub const ACENTO_O_CORCHETE: u32 = 0x2F;
    /// La tecla a la derecha de la L: enye en espanol, punto y coma en ingles.
    pub const ENYE_O_PUNTOCOMA: u32 = 0x33;
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("VHDESK_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let mut injector = open_injector().context("abrir el injector")?;

    cuenta_atras();

    let guardia = Guardia {
        injector: injector.as_mut(),
    };

    cuadrado_con_el_raton(guardia.injector)?;
    escribir_ascii(guardia.injector)?;
    teclas_para_mirar_a_ojo(guardia.injector)?;
    probar_rueda(guardia.injector)?;

    println!("\nlisto. Se libera todo lo que quedara hundido.");
    Ok(())
}

fn cuenta_atras() {
    println!("La prueba va a mover el raton y a escribir en la ventana que tenga el foco.");
    println!("Pon el foco donde quieras (un editor vacio) o corta con Ctrl+C.\n");

    for restantes in (1..=3).rev() {
        println!("  empieza en {restantes}...");
        std::thread::sleep(Duration::from_secs(1));
    }
    println!();
}

/// Dibuja un cuadrado moviendo el puntero por los cuatro lados.
///
/// Las coordenadas son pixeles fisicos del escritorio virtual, que es lo que espera el
/// injector. En una sesion real las traduciria el viewer desde su ventana.
fn cuadrado_con_el_raton(injector: &mut dyn InputInjector) -> Result<()> {
    println!("raton: dibujando un cuadrado de 400x400 desde (200, 200)");

    const ORIGEN: i32 = 200;
    const LADO: i32 = 400;
    const PASOS: i32 = 40;

    let lados = [
        (1, 0),  // derecha
        (0, 1),  // abajo
        (-1, 0), // izquierda
        (0, -1), // arriba
    ];

    let (mut x, mut y) = (ORIGEN, ORIGEN);
    injector.mouse_move_absolute(x, y)?;

    for (dx, dy) in lados {
        for _ in 0..PASOS {
            x += dx * (LADO / PASOS);
            y += dy * (LADO / PASOS);
            injector.mouse_move_absolute(x, y)?;
            std::thread::sleep(Duration::from_millis(8));
        }
    }

    Ok(())
}

/// Escribe "hola vhdesk" y "HOLA": lo unico que no depende de la distribucion.
fn escribir_ascii(injector: &mut dyn InputInjector) -> Result<()> {
    println!("teclado: escribiendo ASCII, que es lo unico determinista");

    let minusculas = [
        hid::H,
        hid::O,
        hid::L,
        hid::A,
        hid::ESPACIO,
        hid::V,
        hid::H,
        hid::D,
        hid::E,
        hid::S,
        hid::K,
    ];
    for tecla in minusculas {
        pulsar(injector, tecla)?;
    }

    pulsar(injector, hid::ENTER)?;

    // Mayusculas con Mayus mantenido: prueba que el modificador se mantiene entre eventos
    // y, sobre todo, que se suelta.
    injector.key(hid::MAYUS_IZQ, true)?;
    for tecla in [hid::H, hid::O, hid::L, hid::A] {
        pulsar(injector, tecla)?;
    }
    injector.key(hid::MAYUS_IZQ, false)?;

    pulsar(injector, hid::ENTER)?;
    Ok(())
}

/// Teclas cuyo resultado depende de la distribucion del host: hay que mirarlas.
fn teclas_para_mirar_a_ojo(injector: &mut dyn InputInjector) -> Result<()> {
    println!("\nteclado: lo que sigue depende de TU distribucion. Comprueba a ojo:");
    println!("  1. tecla a la derecha de la P  -> acento en espanol, corchete en ingles");
    println!("  2. tecla a la derecha de la L  -> enye en espanol, punto y coma en ingles");
    println!("  3. AltGr + esa misma tecla     -> tercera fila; si no sale nada, falta");
    println!("     KEYEVENTF_EXTENDEDKEY en AltGr");
    println!("  4. flechas, Inicio y Fin       -> el cursor debe moverse, no escribir numeros;");
    println!("     si escribe numeros, falta la marca de extendida");

    pulsar(injector, hid::ACENTO_O_CORCHETE)?;
    pulsar(injector, hid::ENYE_O_PUNTOCOMA)?;
    pulsar(injector, hid::ESPACIO)?;

    // AltGr: el caso donde la marca de extendida se nota. Sin ella, Windows lo trata como
    // Alt izquierdo y la tercera fila del teclado no sale.
    injector.key(hid::ALT_DER, true)?;
    pulsar(injector, hid::ENYE_O_PUNTOCOMA)?;
    injector.key(hid::ALT_DER, false)?;

    pulsar(injector, hid::ENTER)?;

    // Navegacion: si a estas les faltara la marca de extendida, escribirian los numeros del
    // teclado numerico en lugar de mover el cursor.
    println!("\nteclado: moviendo el cursor con Inicio, flechas y Fin");
    for tecla in [
        hid::INICIO,
        hid::FLECHA_DER,
        hid::FLECHA_DER,
        hid::FLECHA_IZQ,
        hid::FIN,
    ] {
        pulsar(injector, tecla)?;
    }

    // Ctrl+A, el atajo mas inofensivo que ejercita un modificador junto a una tecla.
    println!("teclado: Ctrl+A (seleccionar todo)");
    injector.key(hid::CTRL_IZQ, true)?;
    pulsar(injector, hid::A)?;
    injector.key(hid::CTRL_IZQ, false)?;

    // Se deselecciona para no dejar el editor con todo seleccionado, que invita a borrarlo
    // sin querer con la siguiente tecla.
    pulsar(injector, hid::FIN)?;

    Ok(())
}

fn probar_rueda(injector: &mut dyn InputInjector) -> Result<()> {
    println!("\nrueda: 3 muescas arriba, 3 abajo, y media muesca para probar fracciones");

    injector.mouse_scroll(0.0, 3.0)?;
    std::thread::sleep(Duration::from_millis(300));
    injector.mouse_scroll(0.0, -3.0)?;
    std::thread::sleep(Duration::from_millis(300));
    injector.mouse_scroll(0.0, 0.5)?;

    println!("rueda: horizontal, 2 muescas a la derecha (solo se ve si la ventana scrollea)");
    injector.mouse_scroll(2.0, 0.0)?;

    println!("\nraton: clic derecho y su liberacion");
    injector.mouse_button(MouseButton::Right, true)?;
    std::thread::sleep(Duration::from_millis(100));
    injector.mouse_button(MouseButton::Right, false)?;

    Ok(())
}

/// Pulsa y suelta una tecla con una pausa que la hace visible.
fn pulsar(injector: &mut dyn InputInjector, hid: u32) -> Result<()> {
    injector.key(hid, true)?;
    std::thread::sleep(Duration::from_millis(30));
    injector.key(hid, false)?;
    std::thread::sleep(Duration::from_millis(30));
    Ok(())
}

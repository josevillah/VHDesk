//! Comprobaciones contra DXGI de verdad.
//!
//! Todas van marcadas `#[ignore]` porque necesitan una sesion de escritorio con la que la
//! duplicacion funcione, y los runners de CI no la tienen. Dejarlas activas convertiria el
//! CI en un generador de fallos ajenos al codigo, que es la forma mas rapida de que la
//! gente deje de mirar el CI.
//!
//! Para ejecutarlas a mano en una maquina con pantalla:
//!
//! ```text
//! cargo test -p vhdesk-capture --test dxgi -- --ignored --nocapture
//! ```

#![cfg(windows)]

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use vhdesk_capture::{CaptureEvent, ensure_dpi_awareness, enumerate_monitors, open_capturer};

/// DXGI solo admite una duplicacion activa por output y proceso, y el ejecutor de tests
/// lanza los tests en paralelo. Sin esta serializacion, los que abren captura se pisan
/// entre ellos y fallan de forma intermitente por una razon que no tiene que ver con lo
/// que pretenden comprobar.
static DUPLICACION: Mutex<()> = Mutex::new(());

fn en_exclusiva() -> MutexGuard<'static, ()> {
    // Si otro test entro en panico con el guard cogido, el mutex queda envenenado. Aqui
    // eso no invalida nada: no hay estado compartido que proteger, solo exclusion.
    DUPLICACION.lock().unwrap_or_else(|e| e.into_inner())
}

/// Recoge el primer frame real, saltandose los timeouts de una pantalla quieta.
fn primer_frame(
    capturador: &mut dyn vhdesk_capture::ScreenCapturer,
    limite: Duration,
) -> Option<vhdesk_capture::Frame> {
    let hasta = Instant::now() + limite;
    while Instant::now() < hasta {
        match capturador.next_frame(Duration::from_millis(200)) {
            Ok(CaptureEvent::Frame(frame)) => return Some(frame),
            Ok(CaptureEvent::CursorOnly(_) | CaptureEvent::Timeout) => continue,
            Err(error) => panic!("la captura fallo: {error}"),
        }
    }
    None
}

#[test]
#[ignore = "necesita una sesion de escritorio; ejecutalo a mano con --ignored"]
fn se_enumera_al_menos_un_monitor_con_dimensiones_creibles() {
    let monitores = enumerate_monitors().expect("enumerar monitores");
    assert!(!monitores.is_empty());

    for monitor in &monitores {
        assert!(
            monitor.width > 0 && monitor.height > 0,
            "{} reporta {}x{}",
            monitor.id,
            monitor.width,
            monitor.height
        );
        assert!(
            monitor.scale > 0.0,
            "{} reporta una escala de {}",
            monitor.id,
            monitor.scale
        );
        assert!(!monitor.name.is_empty());
    }

    assert_eq!(
        monitores.iter().filter(|m| m.primary).count(),
        1,
        "deberia haber exactamente un monitor principal"
    );
}

#[test]
#[ignore = "necesita una sesion de escritorio; ejecutalo a mano con --ignored"]
fn el_primer_frame_es_un_refresco_completo_y_cuadra_con_el_monitor() {
    let _exclusiva = en_exclusiva();
    ensure_dpi_awareness();

    let monitores = enumerate_monitors().expect("enumerar monitores");
    let elegido = monitores
        .iter()
        .find(|m| m.primary)
        .or_else(|| monitores.first())
        .expect("hay al menos un monitor");

    let mut capturador = open_capturer(elegido.id).expect("abrir la captura");
    let esperado = capturador.monitor().clone();

    let frame = primer_frame(capturador.as_mut(), Duration::from_secs(5))
        .expect("ningun frame en 5 s: mueve algo en pantalla mientras corre el test");

    assert!(
        frame.full_refresh,
        "el primer frame tras abrir la duplicacion describe la pantalla entera, no un delta"
    );
    assert_eq!(frame.sequence, 1);
    assert_eq!(
        (frame.width, frame.height),
        (esperado.width, esperado.height)
    );
    assert_eq!(
        frame.stride,
        frame.width as usize * 4,
        "el capturador de Windows entrega las filas compactadas"
    );
    assert_eq!(frame.buffer.len(), frame.stride * frame.height as usize);
    assert!(
        frame.row(frame.height - 1).is_some(),
        "la ultima fila debe ser accesible"
    );
}

#[test]
#[ignore = "necesita una sesion de escritorio; ejecutalo a mano con --ignored"]
fn los_frames_siguientes_ya_no_son_refresco_completo() {
    let _exclusiva = en_exclusiva();
    ensure_dpi_awareness();

    let monitores = enumerate_monitors().expect("enumerar monitores");
    let mut capturador = open_capturer(monitores[0].id).expect("abrir la captura");

    let primero = primer_frame(capturador.as_mut(), Duration::from_secs(5))
        .expect("ningun frame: mueve algo en pantalla mientras corre el test");
    assert!(primero.full_refresh);
    drop(primero);

    let segundo = primer_frame(capturador.as_mut(), Duration::from_secs(5))
        .expect("ningun segundo frame: mueve algo en pantalla mientras corre el test");

    assert!(
        !segundo.full_refresh,
        "solo el primer frame de una duplicacion es refresco completo"
    );
    assert_eq!(segundo.sequence, 2);
}

#[test]
#[ignore = "necesita una sesion de escritorio; ejecutalo a mano con --ignored"]
fn el_pool_recicla_en_vez_de_asignar_por_frame() {
    let _exclusiva = en_exclusiva();
    ensure_dpi_awareness();

    let monitores = enumerate_monitors().expect("enumerar monitores");
    let mut capturador = open_capturer(monitores[0].id).expect("abrir la captura");

    // Se sueltan los frames segun llegan, que es lo que hara el host: si el pool funciona,
    // el segundo frame debe reutilizar la asignacion del primero.
    let primero = primer_frame(capturador.as_mut(), Duration::from_secs(5))
        .expect("ningun frame: mueve algo en pantalla mientras corre el test");
    let direccion = primero.buffer.as_ptr();
    drop(primero);

    let segundo = primer_frame(capturador.as_mut(), Duration::from_secs(5))
        .expect("ningun segundo frame: mueve algo en pantalla mientras corre el test");

    assert_eq!(
        segundo.buffer.as_ptr(),
        direccion,
        "cada frame esta asignando memoria nueva en vez de reciclar la del pool"
    );
}

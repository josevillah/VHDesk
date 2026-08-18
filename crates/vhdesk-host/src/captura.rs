//! El hilo de captura.
//!
//! Va en un hilo propio por dos razones independientes, y cualquiera de las dos bastaria:
//!
//! - **El capturador no es `Send`.** Los objetos de DXGI que guarda estan atados al hilo
//!   que los creo, asi que no puede vivir en una tarea de tokio, que migra entre hilos.
//! - **Bloquea esperando a la GPU.** El mapeo de lectura espera a que la GPU termine la
//!   copia: 3,14 ms de media y 9,18 de p99 sin hacer trabajo. Que ese bloqueo no frene al
//!   codificador que esta con el frame anterior es el motivo principal de todo el reparto
//!   en hilos.
//!
//! En reposo no gasta CPU: se queda dentro de `AcquireNextFrame` hasta que algo cambia.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::{mpsc, watch};
use vhdesk_capture::{CaptureEvent, CursorUpdate, MonitorInfo, open_capturer};
use vhdesk_proto::Cursor;

use crate::cursor;
use crate::ranura::{FrameAcumulado, Ranura};

/// Cuanto espera cada ciclo antes de comprobar si hay que parar.
///
/// Con la pantalla quieta, el hilo despierta como mucho diez veces por segundo y vuelve a
/// dormirse; el coste medido en reposo es practicamente cero.
const ESPERA: Duration = Duration::from_millis(100);

/// Por donde salen las actualizaciones de cursor.
pub struct SalidaCursor {
    /// Ultima posicion conocida. `watch` es exactamente una ranura de un hueco donde gana
    /// el valor nuevo, que es lo que queremos: la posicion anterior no vale nada.
    pub posicion: watch::Sender<Option<Cursor>>,
    /// Formas nuevas. **Esta cola no descarta**: una forma perdida no se recupera nunca.
    pub forma: mpsc::Sender<Cursor>,
}

/// Bucle de captura. Termina cuando `parar` se pone a `true` o la captura falla.
///
/// # Errores
///
/// Devuelve error si el monitor no se puede abrir o si la captura falla de forma que la
/// propia implementacion no ha sabido resolver.
pub fn bucle(
    monitor: &MonitorInfo,
    indice: u8,
    ranura: &Arc<Ranura>,
    salida: &SalidaCursor,
    parar: &AtomicBool,
) -> Result<()> {
    let mut capturador = open_capturer(monitor.id)
        .with_context(|| format!("abrir la captura de {}", monitor.name))?;

    let (ancho, alto) = (monitor.width, monitor.height);
    let mut frames = 0u64;

    while !parar.load(Ordering::Relaxed) {
        match capturador.next_frame(ESPERA).context("capturar")? {
            CaptureEvent::Frame(mut frame) => {
                // El cursor sale por su camino **antes** de que el frame entre en la ranura,
                // porque en la ranura el frame puede acabar descartado y con el se irian sus
                // actualizaciones de cursor.
                if let Some(actualizacion) = frame.cursor.take() {
                    reenviar_cursor(&actualizacion, indice, ancho, alto, salida);
                }
                ranura.depositar(FrameAcumulado::from(frame));
                frames += 1;
            }
            CaptureEvent::CursorOnly(actualizacion) => {
                // Solo se movio el puntero: no hay pixeles nuevos que codificar, y por eso
                // el sistema lo senala aparte.
                reenviar_cursor(&actualizacion, indice, ancho, alto, salida);
            }
            // Con la pantalla quieta es la respuesta normal y llega muchas veces por
            // segundo. No es un error.
            CaptureEvent::Timeout => {}
        }
    }

    tracing::debug!(frames, "el hilo de captura termina");
    Ok(())
}

fn reenviar_cursor(
    actualizacion: &CursorUpdate,
    monitor: u8,
    ancho: u32,
    alto: u32,
    salida: &SalidaCursor,
) {
    if let Some(shape) = &actualizacion.shape {
        // `try_send` y no `blocking_send`: bloquear la captura por el cursor seria pagar
        // con latencia de video una actualizacion que no la vale. La cola tiene sitio de
        // sobra para lo rara que es una forma nueva, asi que si se llena es que algo va mal
        // aguas abajo y hay que enterarse.
        if let Err(error) = salida.forma.try_send(cursor::forma(shape)) {
            tracing::warn!(%error, "se pierde una forma de cursor: el viewer se quedara con la anterior");
        }
    }

    // Un envio fallido aqui solo significa que la sesion se esta cerrando.
    let _ = salida
        .posicion
        .send(Some(cursor::posicion(actualizacion, monitor, ancho, alto)));
}

//! Captura de pantalla.
//!
//! Expone el trait [`ScreenCapturer`] y esconde detras de `#[cfg(target_os = ...)]` las
//! implementaciones por sistema. La logica de negocio nunca ve una API del sistema
//! operativo.
//!
//! - Windows: DXGI Desktop Duplication.
//! - Linux: portal PipeWire ScreenCast, con X11 XShm como respaldo. FASE 1b.
//! - macOS: ScreenCaptureKit. FASE 1b.
//!
//! Este crate hace FFI, asi que puede contener `unsafe`, siempre con un comentario
//! `// SAFETY:` que explique por que la llamada es correcta.
//!
//! # Modelo de hilos y de memoria
//!
//! El capturador **no es `Send`**: se construye y se usa en un mismo hilo, porque los
//! objetos del sistema que guarda estan atados a el. Lo que si cruza hilos es el
//! [`Frame`], cuyo buffer de pixeles es un handle prestado por un pool interno y devuelto
//! al soltarse. Esa es la razon de que el capturador sea dueno de sus buffers: un frame de
//! 1080p son 8,3 MiB, y asignarlo por frame a 60 fps dominaria el perfil.
//!
//! # Ejemplo
//!
//! ```no_run
//! use std::time::Duration;
//! use vhdesk_capture::{CaptureEvent, ensure_dpi_awareness, enumerate_monitors, open_capturer};
//!
//! // Efecto global del proceso: se llama desde main(), no desde una libreria.
//! ensure_dpi_awareness();
//!
//! let monitores = enumerate_monitors()?;
//! let mut capturador = open_capturer(monitores[0].id)?;
//!
//! match capturador.next_frame(Duration::from_millis(100))? {
//!     CaptureEvent::Frame(frame) => println!("{}x{}", frame.width, frame.height),
//!     CaptureEvent::CursorOnly(_) => println!("solo se movio el puntero"),
//!     CaptureEvent::Timeout => println!("pantalla quieta"),
//! }
//! # Ok::<(), vhdesk_capture::CaptureError>(())
//! ```

#![deny(missing_docs)]

pub mod cursor;
pub mod error;
pub mod frame;
pub mod pixels;
pub mod pool;

#[cfg(windows)]
mod win32;

#[cfg(not(windows))]
mod stub;

use std::time::Duration;

pub use crate::cursor::{CursorPosition, CursorShape, CursorUpdate, PointerShapeKind};
pub use crate::error::CaptureError;
pub use crate::frame::{CaptureEvent, Frame, MonitorId, MonitorInfo, MoveRect, Rect};
pub use crate::pool::{BufferPool, PooledBuffer};

/// Fuente de frames de un monitor.
///
/// Las implementaciones no son `Send`; ver la nota de hilos en la documentacion del crate.
pub trait ScreenCapturer {
    /// Monitor que esta capturando.
    fn monitor(&self) -> &MonitorInfo;

    /// Espera hasta `timeout` por novedades.
    ///
    /// Devolver [`CaptureEvent::Timeout`] es lo normal cuando la pantalla esta quieta, y
    /// no indica ningun problema.
    ///
    /// # Errores
    ///
    /// Los fallos recuperables (perdida de la duplicacion, escritorio seguro delante) se
    /// manejan dentro y se traducen a [`CaptureEvent::Timeout`]; un `Err` aqui indica un
    /// problema del que la implementacion no ha sabido salir.
    fn next_frame(&mut self, timeout: Duration) -> Result<CaptureEvent, CaptureError>;
}

/// Enumera los monitores conectados.
///
/// # Errores
///
/// Devuelve [`CaptureError::NoMonitors`] si no hay ninguno conectado al escritorio.
pub fn enumerate_monitors() -> Result<Vec<MonitorInfo>, CaptureError> {
    #[cfg(windows)]
    {
        win32::enumerate_monitors()
    }
    #[cfg(not(windows))]
    {
        stub::enumerate_monitors()
    }
}

/// Abre la captura de un monitor.
///
/// # Errores
///
/// Devuelve [`CaptureError::UnknownMonitor`] si el identificador no corresponde a ningun
/// monitor conectado.
pub fn open_capturer(id: MonitorId) -> Result<Box<dyn ScreenCapturer>, CaptureError> {
    #[cfg(windows)]
    {
        Ok(Box::new(win32::DxgiCapturer::new(id)?))
    }
    #[cfg(not(windows))]
    {
        stub::open_capturer(id)
    }
}

/// Declara el proceso como consciente del DPI por monitor.
///
/// **Tiene efecto sobre el proceso entero**, asi que esta funcion existe para que la llame
/// el `main` de un binario, nunca una libreria: cambiar la conciencia de DPI por debajo de
/// quien enlaza este crate seria una sorpresa desagradable. El capturador se limita a
/// comprobarlo y avisar por `warn!` si no esta puesto.
///
/// Sin esto, con escalado de pantalla activo Windows reporta resoluciones virtualizadas y
/// las coordenadas de la captura dejan de cuadrar con las que acepta la inyeccion de
/// input; el sintoma aparece como "el raton no va donde apunto".
///
/// Devuelve `true` si al terminar el proceso tiene la conciencia correcta. En plataformas
/// donde el concepto no existe devuelve `true` porque no hay nada que ajustar.
pub fn ensure_dpi_awareness() -> bool {
    #[cfg(windows)]
    {
        win32::dpi::ensure_dpi_awareness()
    }
    #[cfg(not(windows))]
    {
        true
    }
}

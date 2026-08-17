//! Implementacion para las plataformas que todavia no tienen captura.
//!
//! No devuelve datos falsos ni frames sinteticos: falla de forma explicita, porque un
//! capturador que finge funcionar convierte un "falta implementar esto" en una sesion
//! remota que se ve negra sin decir por que.

use crate::ScreenCapturer;
use crate::error::CaptureError;
use crate::frame::{MonitorId, MonitorInfo};

pub fn enumerate_monitors() -> Result<Vec<MonitorInfo>, CaptureError> {
    // FASE 1b: PipeWire ScreenCast con respaldo X11 XShm en Linux, ScreenCaptureKit en
    // macOS. Hasta entonces el error dice exactamente lo que pasa.
    Err(CaptureError::UnsupportedPlatform)
}

pub fn open_capturer(_id: MonitorId) -> Result<Box<dyn ScreenCapturer>, CaptureError> {
    // FASE 1b: ver arriba.
    Err(CaptureError::UnsupportedPlatform)
}

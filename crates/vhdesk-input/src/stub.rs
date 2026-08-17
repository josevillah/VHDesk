//! Implementacion para las plataformas que todavia no tienen inyeccion de entrada.
//!
//! Falla de forma explicita en lugar de aceptar los eventos y no hacer nada: un injector
//! que finge funcionar convierte un "falta implementar esto" en una sesion remota donde el
//! raton no responde y nadie sabe por que.

use crate::InputInjector;
use crate::error::InputError;

pub fn open_injector() -> Result<Box<dyn InputInjector>, InputError> {
    // FASE 1b: `uinput` en Linux, que funciona igual en X11 y en Wayland y necesita una
    // regla udev o pertenencia al grupo `input`, con el dispositivo declarado con
    // `ABS_X`/`ABS_Y` para que el posicionamiento absoluto sea fiable. `CGEvent` en macOS,
    // que ademas exige el permiso de Accesibilidad.
    Err(InputError::UnsupportedPlatform)
}

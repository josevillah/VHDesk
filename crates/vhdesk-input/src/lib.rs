//! Inyeccion de entrada en el host.
//!
//! Expone un trait `InputInjector` (raton absoluto, botones, rueda, teclas por scancode)
//! con una implementacion por sistema:
//!
//! - Windows: `SendInput`.
//! - Linux: `uinput`, que funciona igual en X11 y en Wayland. Requiere una regla udev o
//!   pertenencia al grupo `input`, y hay que declarar el dispositivo con `ABS_X`/`ABS_Y`:
//!   con eventos relativos el posicionamiento absoluto no es fiable.
//! - macOS: `CGEvent`.
//!
//! Este crate hace FFI, asi que puede contener `unsafe` con `// SAFETY:`.
//!
//! FASE 1: el trait y la implementacion de Windows.
//! FASE 6: escritorio seguro de Windows (UAC y pantalla de bloqueo), que `SendInput` no
//! alcanza desde una sesion de usuario normal.

#![deny(missing_docs)]

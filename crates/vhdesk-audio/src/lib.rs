//! Captura y reproduccion de audio.
//!
//! Lo que hay que capturar es el audio *de sistema* del host, no su microfono. cpal cubre
//! mas de lo que parecia:
//!
//! - Windows: cpal activa `AUDCLNT_STREAMFLAGS_LOOPBACK` solo con abrir un stream de
//!   entrada sobre un dispositivo de salida. No hace falta backend propio.
//! - macOS 14.6+: cpal soporta loopback de CoreAudio desde su version 0.18.
//! - macOS 13 a 14.6: no lo cubre; hay que capturar el audio via ScreenCaptureKit.
//! - Linux: el monitor de PipeWire aparece como un dispositivo de captura normal, asi que
//!   funciona, pero elegirlo bien en la enumeracion no es evidente.
//!
//! FASE 5: captura, reproduccion, Opus y sincronizacion con el video.

#![deny(missing_docs)]

//! Captura de pantalla.
//!
//! Expone un trait `ScreenCapturer` (enumerar monitores, capturar un frame con marca de
//! tiempo) y esconde detras de `#[cfg(target_os = ...)]` las implementaciones por
//! sistema. La logica de negocio nunca ve una API del sistema operativo.
//!
//! - Windows: DXGI Desktop Duplication.
//! - Linux: portal PipeWire ScreenCast, con X11 XShm como respaldo.
//! - macOS: ScreenCaptureKit.
//!
//! Este crate hace FFI, asi que **si** puede contener `unsafe`, siempre con un comentario
//! `// SAFETY:` que explique por que la llamada es correcta.
//!
//! FASE 1: el trait y la implementacion de Windows. FASE 4: regiones sucias.

#![deny(missing_docs)]

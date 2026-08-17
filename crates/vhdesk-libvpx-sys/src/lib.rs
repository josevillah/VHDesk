//! Enlaces FFI en crudo a libvpx.
//!
//! Este crate no tiene API propia: expone tal cual lo que genera bindgen a partir de las
//! cabeceras de libvpx, para el objetivo de compilacion real. La envoltura segura vive en
//! `vhdesk-codec`, que es quien decide como se usa.
//!
//! Existe en el workspace en lugar de usar un crate de terceros porque los enlaces
//! pregenerados que circulan suelen estar hechos en Linux, y en Windows x64 `size_t` no es
//! `unsigned long`. Ese desajuste desplaza campos dentro de las estructuras sin que el
//! compilador de Rust se entere. Ver `docs/adr/0002-libvpx-en-windows.md`.
//!
//! # Requisitos de compilacion
//!
//! - libvpx: en Windows por vcpkg (`vcpkg install --triplet x64-windows-static-md` en la
//!   raiz del repositorio) con `VPX_LIB_DIR` y `VPX_INCLUDE_DIR` definidas; en Linux y
//!   macOS por el paquete del sistema, que se localiza con pkg-config.
//! - LLVM, porque bindgen necesita libclang.

// Los enlaces generados no siguen las convenciones de nombres de Rust ni llevan
// documentacion: son la API de C tal cual.
#![allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    missing_docs
)]

include!(concat!(env!("OUT_DIR"), "/ffi.rs"));

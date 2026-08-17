//! Codecs de video.
//!
//! Expone los traits `VideoEncoder` y `VideoDecoder`, con un backend por software y, mas
//! adelante, backends por hardware seleccionados en tiempo de ejecucion con degradacion
//! ordenada a software.
//!
//! El codec efectivo de cada sesion lo negocian los peers; ver
//! [`vhdesk_proto::VideoCodec`](../vhdesk_proto/enum.VideoCodec.html). Este crate no
//! decide cual se usa, solo sabe ejecutar el que le pidan.
//!
//! Este crate hace FFI a libvpx, asi que puede contener `unsafe` con `// SAFETY:`.
//!
//! FASE 1: traits mas VP8 por software (libvpx).
//! FASE 4: NVENC, AMF, QuickSync, VideoToolbox y VAAPI, con acceso directo a la API de
//! cada plataforma en vez de a traves de ffmpeg, para no arrastrar su arbol de licencias.

#![deny(missing_docs)]

//! Identidad criptografica y confianza entre peers.
//!
//! Este crate no implementa primitivas: la capa AEAD y el handshake los pone TLS 1.3 a
//! traves de rustls, por la decision registrada en `docs/adr/0001-stack-inicial.md`. Lo
//! que vive aqui es todo lo que rustls no sabe hacer solo:
//!
//! - Generacion y persistencia del par de claves de la instalacion.
//! - Certificado autofirmado cuyo SPKI **es** la identidad del peer.
//! - Almacen de pinning TOFU y deteccion de cambio de clave.
//! - Verificador de certificado de servidor (lo usa el viewer contra el host) y
//!   verificador de certificado de **cliente** (lo usa el host contra el viewer): la
//!   autenticacion es mutua, TLS por defecto solo autenticaria a un lado.
//! - Derivacion y verificacion de contrasenas con Argon2id.
//!
//! FASE 2: todo lo anterior. En la fase 0 el crate esta vacio a proposito.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Transporte: QUIC, travesia de NAT y relay.
//!
//! Dos decisiones de forma que conviene tener presentes antes de escribir nada aqui:
//!
//! **Un solo `quinn::Endpoint`, y por tanto un solo puerto UDP,** para la conexion con el
//! servidor de rendezvous y para la conexion con el peer. El hole punching solo funciona
//! si se perfora desde el mismo socket cuya direccion reflexiva observo el servidor; con
//! sockets separados el mapeo NaT que aprendio el servidor no sirve de nada.
//!
//! **El video no viaja en datagramas QUIC.** Un datagrama no se fragmenta y esta acotado
//! por la MTU del camino, asi que un frame no cabe. La forma correcta es un stream
//! unidireccional por frame y `RESET_STREAM` cuando un frame queda obsoleto: se conserva
//! la recuperacion de perdidas dentro del frame y se descarta lo que ya no sirve. Los
//! datagramas quedan para lo diminuto y sin valor historico, como la posicion del cursor.
//!
//! FASE 1: conexion QUIC punto a punto con streams separados de control, video e input.
//! FASE 3: rendezvous, hole punching y respaldo por relay.
//! FASE 4: bitrate adaptativo y prioridad de streams.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

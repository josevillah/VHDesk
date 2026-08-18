//! Los canales de control e input.
//!
//! El reparto de primitivas de QUIC en una sesion:
//!
//! | canal | primitiva | por que |
//! |---|---|---|
//! | control | stream bidireccional fiable | pocos mensajes, todos importan, hacen falta en ambos sentidos |
//! | input | stream unidireccional fiable, prioridad alta | perder una pulsacion es inaceptable, y llegar tarde tambien |
//! | video | un stream unidireccional por frame | ver [`crate::video`] |
//! | cursor y sondas | datagramas | diminutos y sin valor historico: el ultimo invalida al anterior |

use bytes::{Bytes, BytesMut};
use quinn::{RecvStream, SendStream};
use vhdesk_proto::Message;

use crate::error::TransportError;

/// Prioridad del stream de input.
///
/// Por encima del video: un frame que llega 20 ms tarde se nota poco, una pulsacion que
/// llega 20 ms tarde se nota mucho. Con congestion, quinn atiende antes a los streams de
/// prioridad mas alta.
const PRIORIDAD_INPUT: i32 = 10;

/// Cuanto se lee de golpe de un stream.
const TROZO_LECTURA: usize = 64 * 1024;

/// Lee bytes hasta completar un mensaje del protocolo.
async fn recibir_mensaje(
    recv: &mut RecvStream,
    buf: &mut BytesMut,
) -> Result<Message, TransportError> {
    loop {
        // Puede quedar mas de un mensaje del ciclo anterior, asi que primero se intenta
        // decodificar lo que ya hay antes de pedir mas bytes a la red.
        if let Some(mensaje) = vhdesk_proto::decode(buf)? {
            return Ok(mensaje);
        }

        match recv.read_chunk(TROZO_LECTURA, true).await? {
            Some(trozo) => buf.extend_from_slice(&trozo.bytes),
            // El stream termino limpiamente pero el buffer tiene un mensaje a medias.
            None => return Err(TransportError::TruncatedStream),
        }
    }
}

pub(crate) fn codificar(mensaje: &Message) -> Result<Bytes, TransportError> {
    let mut buf = BytesMut::new();
    vhdesk_proto::encode(mensaje, &mut buf)?;
    Ok(buf.freeze())
}

/// Canal de control: bidireccional y fiable.
pub struct ControlChannel {
    send: SendStream,
    recv: RecvStream,
    buf: BytesMut,
}

impl ControlChannel {
    pub(crate) fn new(send: SendStream, recv: RecvStream) -> Self {
        Self {
            send,
            recv,
            buf: BytesMut::new(),
        }
    }

    /// Envia un mensaje de control.
    ///
    /// # Errores
    ///
    /// Devuelve [`TransportError::Write`] si el stream se cerro.
    pub async fn send(&mut self, mensaje: &Message) -> Result<(), TransportError> {
        let bytes = codificar(mensaje)?;
        self.send.write_all(&bytes).await?;
        Ok(())
    }

    /// Espera al siguiente mensaje de control.
    ///
    /// # Errores
    ///
    /// Devuelve [`TransportError::TruncatedStream`] si el peer cerro el stream en mitad de
    /// un mensaje.
    pub async fn recv(&mut self) -> Result<Message, TransportError> {
        recibir_mensaje(&mut self.recv, &mut self.buf).await
    }

    /// Separa el canal en sus dos mitades, para repartirlas entre tareas distintas.
    ///
    /// Hace falta en cuanto los dos sentidos los lleva gente distinta, que es el caso del
    /// host: una tarea espera bloqueada en `recv` a que llegue un `KeyframeRequest`
    /// mientras otra manda formas de cursor. Con el canal entero no se puede, porque las
    /// dos operaciones piden `&mut self` y una de ellas esta permanentemente en vuelo.
    ///
    /// El stream QUIC de debajo ya venia partido en dos; esto solo deja de esconderlo.
    pub fn split(self) -> (ControlSender, ControlReceiver) {
        (
            ControlSender { send: self.send },
            ControlReceiver {
                recv: self.recv,
                buf: self.buf,
            },
        )
    }
}

/// Mitad emisora de un [`ControlChannel`] partido.
pub struct ControlSender {
    send: SendStream,
}

impl ControlSender {
    /// Envia un mensaje de control.
    ///
    /// # Errores
    ///
    /// Devuelve [`TransportError::Write`] si el stream se cerro.
    pub async fn send(&mut self, mensaje: &Message) -> Result<(), TransportError> {
        let bytes = codificar(mensaje)?;
        self.send.write_all(&bytes).await?;
        Ok(())
    }
}

/// Mitad receptora de un [`ControlChannel`] partido.
pub struct ControlReceiver {
    recv: RecvStream,
    buf: BytesMut,
}

impl ControlReceiver {
    /// Espera al siguiente mensaje de control.
    ///
    /// # Errores
    ///
    /// Devuelve [`TransportError::TruncatedStream`] si el peer cerro el stream en mitad de
    /// un mensaje.
    pub async fn recv(&mut self) -> Result<Message, TransportError> {
        recibir_mensaje(&mut self.recv, &mut self.buf).await
    }
}

/// Extremo emisor del canal de input.
pub struct InputSender {
    send: SendStream,
}

impl InputSender {
    pub(crate) fn new(send: SendStream) -> Self {
        send.set_priority(PRIORIDAD_INPUT)
            .expect("el stream acaba de abrirse y no puede estar cerrado");
        Self { send }
    }

    /// Envia un evento de entrada.
    ///
    /// # Errores
    ///
    /// Devuelve [`TransportError::Write`] si el stream se cerro.
    pub async fn send(&mut self, mensaje: &Message) -> Result<(), TransportError> {
        let bytes = codificar(mensaje)?;
        self.send.write_all(&bytes).await?;
        Ok(())
    }
}

/// Extremo receptor del canal de input.
pub struct InputReceiver {
    recv: RecvStream,
    buf: BytesMut,
}

impl InputReceiver {
    pub(crate) fn new(recv: RecvStream) -> Self {
        Self {
            recv,
            buf: BytesMut::new(),
        }
    }

    /// Espera al siguiente evento de entrada.
    ///
    /// # Errores
    ///
    /// Devuelve [`TransportError::TruncatedStream`] si el peer cerro el stream en mitad de
    /// un mensaje.
    pub async fn recv(&mut self) -> Result<Message, TransportError> {
        recibir_mensaje(&mut self.recv, &mut self.buf).await
    }
}

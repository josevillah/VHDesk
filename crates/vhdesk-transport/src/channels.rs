//! Los cuatro canales de una sesion, cada uno sobre la primitiva de QUIC que le encaja.
//!
//! | canal | primitiva | por que |
//! |---|---|---|
//! | control | stream bidireccional fiable | pocos mensajes, todos importan, hacen falta en ambos sentidos |
//! | input | stream unidireccional fiable, prioridad alta | perder una pulsacion es inaceptable, y llegar tarde tambien |
//! | video | **un stream unidireccional por frame** | permite tirar un frame obsoleto sin arrastrar a los siguientes |
//! | cursor y sondas | datagramas | diminutos y sin valor historico: el ultimo invalida al anterior |
//!
//! El video merece explicacion. Un solo stream para todos los frames tendria bloqueo de
//! cabecera de linea: un frame retransmitiendose detendria a todos los posteriores. Un
//! stream por frame aisla ese fallo, y ademas permite `RESET_STREAM` sobre el frame viejo
//! cuando llega uno nuevo y el anterior aun no ha salido, que es justo la politica de
//! descarte que pide el criterio de latencia.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::{Bytes, BytesMut};
use quinn::{Connection, RecvStream, SendStream};
use tokio::task::JoinHandle;
use vhdesk_proto::{Message, VideoFrame};

use crate::error::TransportError;

/// Prioridad del stream de input.
///
/// Por encima del video: un frame que llega 20 ms tarde se nota poco, una pulsacion que
/// llega 20 ms tarde se nota mucho. Con congestion, quinn atiende antes a los streams de
/// prioridad mas alta.
const PRIORIDAD_INPUT: i32 = 10;

/// Prioridad de los streams de video, la de referencia.
const PRIORIDAD_VIDEO: i32 = 0;

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

fn codificar(mensaje: &Message) -> Result<Bytes, TransportError> {
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

/// Emisor de video: un stream unidireccional por frame.
///
/// `send_frame` **no espera** a que el frame salga por la red. Devuelve inmediatamente y
/// deja la escritura en una tarea, de modo que el hilo del codificador nunca se queda
/// bloqueado por la congestion de la red. Si al llegar el frame siguiente el anterior
/// todavia no ha salido, se aborta: eso cierra su stream con `RESET_STREAM` y el frame
/// viejo desaparece en lugar de retrasar al nuevo.
pub struct VideoSender {
    conn: Connection,
    en_vuelo: Option<JoinHandle<()>>,
    descartados: Arc<AtomicU64>,
}

impl VideoSender {
    pub(crate) fn new(conn: Connection) -> Self {
        Self {
            conn,
            en_vuelo: None,
            descartados: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Encola un frame para enviarlo, descartando el anterior si aun no ha salido.
    ///
    /// # Errores
    ///
    /// Devuelve [`TransportError::Proto`] si el frame no se puede codificar.
    pub fn send_frame(&mut self, frame: VideoFrame) -> Result<(), TransportError> {
        // Se codifica aqui, de forma sincrona, para que un frame invalido falle en la
        // llamada y no en una tarea suelta donde el error no tendria a quien reportarse.
        let bytes = codificar(&Message::VideoFrame(frame))?;

        if let Some(anterior) = self.en_vuelo.take()
            && !anterior.is_finished()
        {
            // Abortar suelta el `SendStream` sin haberlo terminado, y quinn envia
            // RESET_STREAM al soltarlo. Ese es el mecanismo de descarte.
            anterior.abort();
            self.descartados.fetch_add(1, Ordering::Relaxed);
        }

        let conn = self.conn.clone();
        self.en_vuelo = Some(tokio::spawn(async move {
            match conn.open_uni().await {
                Ok(mut stream) => {
                    let _ = stream.set_priority(PRIORIDAD_VIDEO);
                    if stream.write_all(&bytes).await.is_ok() {
                        // `finish` no espera confirmacion del peer: solo marca el final.
                        let _ = stream.finish();
                    }
                    // El stream se suelta aqui. Si la tarea llega hasta este punto, el
                    // frame quedo entregado a quinn entero.
                }
                Err(error) => {
                    tracing::debug!(%error, "no se pudo abrir el stream de video");
                }
            }
        }));

        Ok(())
    }

    /// Frames descartados por llegar uno nuevo antes de que saliera el anterior.
    ///
    /// Es la senal de que el enlace no da para el ritmo al que se esta codificando.
    pub fn descartados(&self) -> u64 {
        self.descartados.load(Ordering::Relaxed)
    }
}

/// Resultado de esperar un frame de video.
#[derive(Debug)]
pub enum RecepcionVideo {
    /// Llego un frame completo.
    Frame(Box<VideoFrame>),
    /// El emisor descarto este frame. No es un error.
    ///
    /// Hoy no se distingue "descartado por obsoleto" de "descartado por un fallo del
    /// emisor": el descarte ocurre al soltar el `SendStream` sin terminarlo, y ahi quinn
    /// pone su propio codigo de `RESET_STREAM`. FASE 4: si hace falta separarlos para las
    /// estadisticas de congestion, habra que resetear explicitamente con un codigo propio
    /// en vez de apoyarse en el `Drop`.
    Descartado,
}

/// Espera al siguiente frame de video que llegue por la conexion.
///
/// # Errores
///
/// Devuelve [`TransportError::FrameTooLarge`] si el emisor manda mas de lo que admite el
/// protocolo, y [`TransportError::Connection`] si la conexion se pierde.
pub async fn recibir_frame(conn: &Connection) -> Result<RecepcionVideo, TransportError> {
    let mut recv = conn.accept_uni().await?;

    // El tope es el del protocolo mas el prefijo de longitud: no se reserva de golpe, es
    // un limite para que un emisor hostil no nos haga crecer sin fin.
    let limite = vhdesk_proto::MAX_FRAME_LEN + vhdesk_proto::LENGTH_PREFIX_LEN;

    let datos = match recv.read_to_end(limite).await {
        Ok(datos) => datos,
        Err(quinn::ReadToEndError::TooLong) => {
            return Err(TransportError::FrameTooLarge { len: limite });
        }
        Err(quinn::ReadToEndError::Read(quinn::ReadError::Reset(_))) => {
            // El emisor lo abandono a proposito. Es el funcionamiento normal bajo
            // congestion, no un fallo.
            return Ok(RecepcionVideo::Descartado);
        }
        Err(quinn::ReadToEndError::Read(error)) => return Err(error.into()),
    };

    let mut buf = BytesMut::from(&datos[..]);
    match vhdesk_proto::decode(&mut buf)? {
        Some(Message::VideoFrame(frame)) => Ok(RecepcionVideo::Frame(Box::new(frame))),
        // Un stream de video que trae otra cosa es un peer que no habla este protocolo.
        Some(otro) => Err(TransportError::Proto(
            vhdesk_proto::ProtoError::UnknownTag {
                tag: otro.name().as_bytes()[0],
            },
        )),
        None => Err(TransportError::TruncatedStream),
    }
}

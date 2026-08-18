//! Una sesion establecida con un peer.

use std::net::SocketAddr;

use bytes::BytesMut;
use quinn::Connection;
use vhdesk_proto::Message;

use crate::channels::{ControlChannel, InputReceiver, InputSender};
use crate::error::TransportError;
use crate::video::{VideoReceiver, VideoSender};

/// Conexion viva con un peer, con sus cuatro canales.
///
/// Quien conecta **abre** los streams y quien escucha los **acepta**. No es simetrico a
/// proposito: QUIC necesita que alguien abra primero, y hacerlo explicito evita el
/// interbloqueo de los dos lados esperando al otro.
///
/// # Como se distinguen los streams unidireccionales
///
/// Input y video son los dos unidireccionales, y `accept_uni` no dice de cual se trata.
/// Lo que los separa es la **direccion**: el input va siempre del viewer al host y el video
/// siempre del host al viewer, asi que en cada extremo solo puede llegar uno de los dos.
/// El host acepta unidireccionales y todos son input; el viewer acepta unidireccionales y
/// todos son video.
///
/// Es una invariante del diseno, no una casualidad, y se apoya en ella para no gastar un
/// byte de etiqueta por stream.
///
/// **Se rompe en los dos sentidos**, y hay un caso concreto ya previsto: la transferencia
/// de archivos de la fase 5 va del viewer al host, asi que necesitara su propio canal
/// unidireccional en esa direccion y el host dejara de poder asumir que todo lo que acepta
/// es input. Lo mismo pasaria si el host necesitara un unidireccional propio ademas del
/// video. En cuanto ocurra cualquiera de los dos hay que introducir una etiqueta de canal
/// al principio de cada stream, antes de anadir el canal nuevo, no despues.
///
/// # Cuidado al repartirla entre tareas
///
/// `Session` se clona barato, pero **la conexion se cierra cuando se suelta el ultimo
/// clon**. Si se mueve la unica `Session` a una tarea y esa tarea termina, la conexion se
/// cae aunque el resto del programa siga usandola, y los datos que quedaran en vuelo se
/// pierden. Al repartirla entre tareas hay que conservar un clon vivo mientras la sesion
/// deba seguir en pie.
#[derive(Clone)]
pub struct Session {
    conn: Connection,
}

impl Session {
    pub(crate) const fn new(conn: Connection) -> Self {
        Self { conn }
    }

    /// Direccion del peer.
    pub fn remote_address(&self) -> SocketAddr {
        self.conn.remote_address()
    }

    /// Abre el canal de control. Lo hace quien inicia la conexion.
    ///
    /// # Errores
    ///
    /// Devuelve [`TransportError::Connection`] si la conexion se perdio.
    pub async fn open_control(&self) -> Result<ControlChannel, TransportError> {
        let (send, recv) = self.conn.open_bi().await?;
        Ok(ControlChannel::new(send, recv))
    }

    /// Acepta el canal de control. Lo hace quien escucha.
    ///
    /// # Errores
    ///
    /// Devuelve [`TransportError::Connection`] si la conexion se perdio.
    pub async fn accept_control(&self) -> Result<ControlChannel, TransportError> {
        let (send, recv) = self.conn.accept_bi().await?;
        Ok(ControlChannel::new(send, recv))
    }

    /// Abre el canal de input, con prioridad por encima del video.
    ///
    /// # Errores
    ///
    /// Devuelve [`TransportError::Connection`] si la conexion se perdio.
    pub async fn open_input(&self) -> Result<InputSender, TransportError> {
        let send = self.conn.open_uni().await?;
        Ok(InputSender::new(send))
    }

    /// Acepta el canal de input.
    ///
    /// # Errores
    ///
    /// Devuelve [`TransportError::Connection`] si la conexion se perdio.
    pub async fn accept_input(&self) -> Result<InputReceiver, TransportError> {
        let recv = self.conn.accept_uni().await?;
        Ok(InputReceiver::new(recv))
    }

    /// Crea el emisor de video de esta sesion.
    pub fn video_sender(&self) -> VideoSender {
        VideoSender::new(self.conn.clone())
    }

    /// Crea el receptor de video de esta sesion.
    ///
    /// **Uno por sesion.** Lleva la cuenta de la secuencia y arranca la tarea que acepta
    /// los streams entrantes; con dos, cada uno veria la mitad de los frames y ambos
    /// creerian que faltan huecos por todas partes.
    pub fn video_receiver(&self) -> VideoReceiver {
        VideoReceiver::new(self.conn.clone())
    }

    /// Envia un mensaje por datagrama.
    ///
    /// Solo para mensajes diminutos cuyo valor caduca: posicion del cursor y sondas de
    /// latencia. **La forma del cursor no cabe** —un puntero de 32x32 en RGBA son 4 KB y
    /// el limite ronda los 1200 bytes—, asi que va por el canal de control.
    ///
    /// # Errores
    ///
    /// Devuelve [`TransportError::DatagramTooLarge`] si el mensaje no cabe, y
    /// [`TransportError::DatagramsUnsupported`] si el peer no admite datagramas.
    pub fn send_datagram(&self, mensaje: &Message) -> Result<(), TransportError> {
        let mut buf = BytesMut::new();
        vhdesk_proto::encode(mensaje, &mut buf)?;

        let maximo = self
            .conn
            .max_datagram_size()
            .ok_or(TransportError::DatagramsUnsupported)?;
        if buf.len() > maximo {
            return Err(TransportError::DatagramTooLarge {
                len: buf.len(),
                max: maximo,
            });
        }

        self.conn.send_datagram(buf.freeze())?;
        Ok(())
    }

    /// Espera al siguiente datagrama.
    ///
    /// # Errores
    ///
    /// Devuelve [`TransportError::Connection`] si la conexion se perdio.
    pub async fn recv_datagram(&self) -> Result<Message, TransportError> {
        let datos = self.conn.read_datagram().await?;

        let mut buf = BytesMut::from(&datos[..]);
        // Un datagrama es un mensaje entero por definicion: si no completa, el emisor no
        // habla este protocolo.
        vhdesk_proto::decode(&mut buf)?.ok_or(TransportError::TruncatedStream)
    }

    /// Tamano maximo de datagrama que admite la conexion ahora mismo.
    pub fn max_datagram_size(&self) -> Option<usize> {
        self.conn.max_datagram_size()
    }

    /// Espera a que la conexion se cierre, por la razon que sea.
    ///
    /// Es lo que permite a quien orquesta la sesion enterarse de que termino sin tener que
    /// vigilar cada canal por separado. Incluye el cierre por inactividad, que con la
    /// pantalla quieta seria el final normal si no fuera por el keepalive; ver
    /// [`crate::KEEPALIVE`].
    pub async fn cerrada(&self) -> TransportError {
        TransportError::Connection(self.conn.closed().await)
    }

    /// Cierra la sesion avisando al peer.
    pub fn close(&self) {
        self.conn.close(0u32.into(), b"fin de sesion");
    }
}

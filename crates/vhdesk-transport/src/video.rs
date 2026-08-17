//! El canal de video: un stream unidireccional por frame, con numero de secuencia.
//!
//! # Por que hace falta un numero de secuencia
//!
//! QUIC garantiza el orden **dentro** de un stream, no **entre** streams. Con un stream
//! por frame, el frame N+1 puede llegar completo antes que el N si al N se le pierde un
//! paquete y hay que retransmitirlo. Y eso no es un defecto del diseno: es la consecuencia
//! inevitable de haber elegido stream-por-frame precisamente para evitar el bloqueo de
//! cabecera de linea.
//!
//! Ademas el emisor **descarta frames a proposito** cuando se le acumulan, asi que los
//! huecos en la secuencia son el camino normal de degradacion, no una excepcion.
//!
//! Decodificar un inter-frame cuya referencia falta no da error: da imagen corrupta en
//! silencio. El numero de secuencia es lo unico que permite detectarlo.
//!
//! # Politica del receptor: latencia por encima de completitud
//!
//! | situacion | decision |
//! |---|---|
//! | `seq <= ultimo` | descartar sin decodificar: llego tarde y empeoraria la imagen |
//! | `seq == ultimo + 1` | aceptar |
//! | `seq > ultimo + 1` y es keyframe | aceptar: el keyframe repara la cadena |
//! | `seq > ultimo + 1` y es inter | **hueco**: no decodificar, pedir keyframe |
//! | primer frame de la sesion y no es keyframe | hueco |
//!
//! **No hay buffer de reordenacion.** Esperar al frame que falta es latencia, y en video en
//! vivo un frame retrasado ya no vale nada cuando llega.
//!
//! Al detectar un hueco **no se avanza el ultimo aceptado**, asi que si el frame que
//! faltaba venia solo reordenado y llega justo despues, encaja como `ultimo + 1` y se
//! decodifica con normalidad.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use quinn::Connection;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use vhdesk_proto::{Message, VideoCodec, VideoFrame};

use crate::error::TransportError;

/// Prioridad de los streams de video, la de referencia.
const PRIORIDAD_VIDEO: i32 = 0;

/// Frames terminados que se guardan mientras el consumidor no los recoge.
///
/// Corto a proposito: acumular frames es acumular latencia. Un consumidor que se retrase
/// mas de cuatro frames vera huecos, y eso es correcto: mas vale saltar adelante que
/// arrastrar imagen vieja.
const PROFUNDIDAD_COLA: usize = 4;

/// Cada cuanto se reintenta la peticion de keyframe si no llega ninguno.
const REINTENTO_KEYFRAME: Duration = Duration::from_secs(1);

/// Un frame listo para enviar, **sin numero de secuencia**.
///
/// El numero lo pone [`VideoSender`], que es el unico que puede garantizar que sea
/// monotono por sesion. Este tipo existe para que no sea representable un frame con una
/// secuencia puesta por quien no debe: ignorar en silencio un campo que alguien relleno
/// seria una API que miente.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameSaliente {
    /// Monitor del que procede.
    pub monitor: u8,
    /// Codec con el que esta codificado.
    pub codec: VideoCodec,
    /// Si es decodificable por si solo.
    pub keyframe: bool,
    /// Instante de captura, en microsegundos desde el arranque de la sesion.
    pub timestamp_us: u64,
    /// Anchura en pixeles.
    pub width: u16,
    /// Altura en pixeles.
    pub height: u16,
    /// Datos codificados.
    pub data: Bytes,
}

/// Emisor de video: un stream unidireccional por frame.
///
/// `send_frame` **no espera** a que el frame salga por la red: deja la escritura en una
/// tarea, de modo que el hilo del codificador nunca se bloquea por congestion. Si al llegar
/// el frame siguiente el anterior todavia no ha salido, se aborta, lo que cierra su stream
/// con `RESET_STREAM`.
///
/// # El emisor no espera a que le pidan el keyframe
///
/// Cuando se aborta el frame N, el codificador ya uso N como referencia de N+1, asi que
/// N+1 es indecodificable. **El emisor lo sabe en ese mismo instante.** Esperar a que el
/// receptor detecte el hueco, mande la peticion y llegue el keyframe cuesta un RTT entero
/// de imagen rota, y es evitable: [`VideoSender::keyframe_pendiente`] se pone a `true` al
/// abortar y el host fuerza el keyframe en el frame siguiente por su cuenta.
pub struct VideoSender {
    conn: Connection,
    en_vuelo: Option<JoinHandle<()>>,
    siguiente_seq: u64,
    keyframe_pendiente: bool,
    descartados: Arc<AtomicU64>,
}

impl VideoSender {
    pub(crate) fn new(conn: Connection) -> Self {
        Self {
            conn,
            en_vuelo: None,
            siguiente_seq: 0,
            // El primer frame de una sesion tiene que ser keyframe: sin el, el viewer no
            // tiene por donde engancharse.
            keyframe_pendiente: true,
            descartados: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Si el proximo frame tiene que ser keyframe.
    ///
    /// El host debe consultarlo **antes de codificar** y, si es `true`, pedirle un keyframe
    /// al codificador. Se pone a `true` al arrancar la sesion y cada vez que se descarta un
    /// frame, porque en ese momento la cadena de referencias queda rota.
    pub const fn keyframe_pendiente(&self) -> bool {
        self.keyframe_pendiente
    }

    /// Encola un frame, descartando el anterior si aun no ha salido.
    ///
    /// # Errores
    ///
    /// Devuelve [`TransportError::Proto`] si el frame no se puede codificar.
    pub fn send_frame(&mut self, frame: FrameSaliente) -> Result<(), TransportError> {
        let sequence = self.siguiente_seq;
        self.siguiente_seq += 1;

        let del_wire = VideoFrame {
            monitor: frame.monitor,
            codec: frame.codec,
            sequence,
            keyframe: frame.keyframe,
            timestamp_us: frame.timestamp_us,
            width: frame.width,
            height: frame.height,
            data: frame.data,
        };

        // Se codifica de forma sincrona para que un frame invalido falle en la llamada y no
        // en una tarea suelta donde el error no tendria a quien reportarse.
        let mut buf = BytesMut::new();
        vhdesk_proto::encode(&Message::VideoFrame(del_wire), &mut buf)?;
        let bytes = buf.freeze();

        if let Some(anterior) = self.en_vuelo.take()
            && !anterior.is_finished()
        {
            // Abortar suelta el `SendStream` sin terminarlo, y quinn envia RESET_STREAM al
            // soltarlo. Ese es el mecanismo de descarte.
            anterior.abort();
            self.descartados.fetch_add(1, Ordering::Relaxed);
            // Y aqui esta la clave: acabamos de romper la cadena de referencias, y lo
            // sabemos nosotros antes que nadie.
            self.keyframe_pendiente = true;
        }

        if frame.keyframe {
            self.keyframe_pendiente = false;
        }

        let conn = self.conn.clone();
        self.en_vuelo = Some(tokio::spawn(async move {
            match conn.open_uni().await {
                Ok(mut stream) => {
                    let _ = stream.set_priority(PRIORIDAD_VIDEO);
                    if stream.write_all(&bytes).await.is_ok() {
                        let _ = stream.finish();
                    }
                }
                Err(error) => tracing::debug!(%error, "no se pudo abrir el stream de video"),
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

/// Por que no se entrego un frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotivoDescarte {
    /// El emisor abandono el stream por obsoleto, o fallo al enviarlo.
    EmisorDescarto,
    /// Llego con un numero de secuencia que ya habiamos superado.
    Tardio,
}

/// Resultado de esperar un frame de video.
#[derive(Debug)]
pub enum RecepcionVideo {
    /// Frame utilizable: la cadena de referencias esta intacta.
    Frame(Box<VideoFrame>),
    /// No hay frame, y no es un error.
    Descartado(MotivoDescarte),
    /// Falta al menos un frame y la cadena de referencias esta rota.
    ///
    /// El frame recibido **no se entrega**: decodificarlo daria imagen corrupta.
    Hueco {
        /// Numero de secuencia que tocaba.
        esperado: u64,
        /// El que llego.
        recibido: u64,
        /// Si toca pedir un keyframe por el canal de control.
        ///
        /// Ya viene amortiguado: ante varios huecos seguidos solo el primero lo pone a
        /// `true`, para no generar una tormenta de keyframes justo cuando la red va mal.
        pedir_keyframe: bool,
    },
}

/// Decision de la politica de orden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// El frame es utilizable.
    Aceptar,
    /// Llego tarde: su numero ya fue superado.
    DescartarTardio,
    /// Falta al menos un frame.
    Hueco {
        /// Numero que tocaba.
        esperado: u64,
    },
}

/// Lleva la cuenta del ultimo frame aceptado y decide que hacer con cada uno que llega.
///
/// Es pura y sin estado externo, de forma que se puede testear entera sin levantar red.
#[derive(Debug, Default)]
pub struct PoliticaOrden {
    ultimo: Option<u64>,
}

impl PoliticaOrden {
    /// Crea una politica sin ningun frame aceptado todavia.
    pub const fn nueva() -> Self {
        Self { ultimo: None }
    }

    /// Ultimo numero de secuencia aceptado.
    pub const fn ultimo(&self) -> Option<u64> {
        self.ultimo
    }

    /// Decide que hacer con un frame recien llegado.
    pub fn evaluar(&mut self, sequence: u64, keyframe: bool) -> Decision {
        let Some(ultimo) = self.ultimo else {
            // Primer frame de la sesion: solo sirve si es keyframe.
            if keyframe {
                self.ultimo = Some(sequence);
                return Decision::Aceptar;
            }
            return Decision::Hueco { esperado: 0 };
        };

        if sequence <= ultimo {
            return Decision::DescartarTardio;
        }

        if sequence == ultimo + 1 || keyframe {
            // Un keyframe repara la cadena aunque haya saltado numeros.
            self.ultimo = Some(sequence);
            return Decision::Aceptar;
        }

        // Hueco: **no se avanza `ultimo`**. Si el frame que falta venia solo reordenado y
        // llega justo despues, encajara como `ultimo + 1` y se aceptara.
        Decision::Hueco {
            esperado: ultimo + 1,
        }
    }
}

/// Evita pedir un keyframe por cada hueco.
///
/// Sin esto, una red mala genera un hueco por frame y se responderia con un keyframe de
/// ~100 KB por cada uno, que es justo lo que esa red no puede tragar. Pura y testeable con
/// un reloj inyectado.
#[derive(Debug)]
pub struct AmortiguadorKeyframe {
    esperando: bool,
    ultima_peticion: Option<Instant>,
    reintento: Duration,
}

impl Default for AmortiguadorKeyframe {
    fn default() -> Self {
        Self::nuevo(REINTENTO_KEYFRAME)
    }
}

impl AmortiguadorKeyframe {
    /// Crea un amortiguador con el intervalo de reintento dado.
    pub const fn nuevo(reintento: Duration) -> Self {
        Self {
            esperando: false,
            ultima_peticion: None,
            reintento,
        }
    }

    /// Si toca pedir un keyframe ahora.
    ///
    /// Devuelve `true` la primera vez, y despues solo si ha pasado el intervalo de
    /// reintento sin que llegara ninguno; el reintento existe porque el host podria
    /// ignorar la peticion, no porque pueda perderse: el canal de control es fiable.
    pub fn debe_pedir(&mut self, ahora: Instant) -> bool {
        match self.ultima_peticion {
            Some(anterior) if self.esperando && ahora.duration_since(anterior) < self.reintento => {
                false
            }
            _ => {
                self.esperando = true;
                self.ultima_peticion = Some(ahora);
                true
            }
        }
    }

    /// Avisa de que llego un keyframe: la siguiente peticion vuelve a ser inmediata.
    pub fn keyframe_recibido(&mut self) {
        self.esperando = false;
        self.ultima_peticion = None;
    }
}

/// Receptor de video: acepta los streams entrantes **en paralelo** y aplica la politica.
///
/// Aceptarlos en serie reintroduciria el bloqueo de cabecera de linea que stream-por-frame
/// venia a evitar: un frame lento detendria la lectura de todos los posteriores.
pub struct VideoReceiver {
    rx: mpsc::Receiver<Resultado>,
    politica: PoliticaOrden,
    amortiguador: AmortiguadorKeyframe,
    _aceptador: JoinHandle<()>,
}

/// Lo que una tarea de lectura entrega al receptor.
type Resultado = Result<Box<VideoFrame>, MotivoDescarte>;

impl VideoReceiver {
    pub(crate) fn new(conn: Connection) -> Self {
        let (tx, rx) = mpsc::channel(PROFUNDIDAD_COLA);

        let aceptador = tokio::spawn(async move {
            while let Ok(recv) = conn.accept_uni().await {
                let tx = tx.clone();
                // Una tarea por stream: es lo que conserva la concurrencia.
                tokio::spawn(async move {
                    let resultado = leer_frame(recv).await;
                    // Si la cola esta llena se tira este frame en vez de esperar. Esperar
                    // seria acumular latencia, que es exactamente lo que no queremos.
                    //
                    // No se reporta como descarte porque no hace falta: el frame nunca
                    // entra en la cola, asi que el consumidor lo vera como un salto en la
                    // secuencia y la politica lo tratara como cualquier otro hueco. Es la
                    // misma senal por el mismo camino.
                    if tx.try_send(resultado).is_err() {
                        tracing::debug!("cola de video llena; se descarta el frame");
                    }
                });
            }
        });

        Self {
            rx,
            politica: PoliticaOrden::nueva(),
            amortiguador: AmortiguadorKeyframe::default(),
            _aceptador: aceptador,
        }
    }

    /// Espera al siguiente resultado de video.
    ///
    /// # Errores
    ///
    /// Devuelve [`TransportError::EndpointClosed`] cuando la conexion se cierra y no van a
    /// llegar mas frames.
    pub async fn recv(&mut self) -> Result<RecepcionVideo, TransportError> {
        let resultado = self.rx.recv().await.ok_or(TransportError::EndpointClosed)?;

        let frame = match resultado {
            Ok(frame) => frame,
            Err(motivo) => return Ok(RecepcionVideo::Descartado(motivo)),
        };

        match self.politica.evaluar(frame.sequence, frame.keyframe) {
            Decision::Aceptar => {
                if frame.keyframe {
                    self.amortiguador.keyframe_recibido();
                }
                Ok(RecepcionVideo::Frame(frame))
            }
            Decision::DescartarTardio => Ok(RecepcionVideo::Descartado(MotivoDescarte::Tardio)),
            Decision::Hueco { esperado } => Ok(RecepcionVideo::Hueco {
                esperado,
                recibido: frame.sequence,
                pedir_keyframe: self.amortiguador.debe_pedir(Instant::now()),
            }),
        }
    }

    /// Ultimo numero de secuencia aceptado, para diagnostico.
    pub const fn ultimo_aceptado(&self) -> Option<u64> {
        self.politica.ultimo()
    }
}

async fn leer_frame(mut recv: quinn::RecvStream) -> Resultado {
    let limite = vhdesk_proto::MAX_FRAME_LEN + vhdesk_proto::LENGTH_PREFIX_LEN;

    let datos = match recv.read_to_end(limite).await {
        Ok(datos) => datos,
        // El emisor lo abandono a proposito, o la conexion se fue. En ambos casos aqui no
        // hay frame y no hay nada que decodificar.
        Err(_) => return Err(MotivoDescarte::EmisorDescarto),
    };

    let mut buf = BytesMut::from(&datos[..]);
    match vhdesk_proto::decode(&mut buf) {
        Ok(Some(Message::VideoFrame(frame))) => Ok(Box::new(frame)),
        _ => Err(MotivoDescarte::EmisorDescarto),
    }
}

#[cfg(test)]
mod tests {
    use super::{AmortiguadorKeyframe, Decision, PoliticaOrden};
    use std::time::{Duration, Instant};

    #[test]
    fn el_primer_frame_tiene_que_ser_keyframe() {
        let mut politica = PoliticaOrden::nueva();

        assert_eq!(
            politica.evaluar(0, false),
            Decision::Hueco { esperado: 0 },
            "un inter-frame como primer frame no tiene referencia sobre la que apoyarse"
        );
        assert_eq!(politica.evaluar(0, true), Decision::Aceptar);
        assert_eq!(politica.ultimo(), Some(0));
    }

    #[test]
    fn la_secuencia_consecutiva_se_acepta() {
        let mut politica = PoliticaOrden::nueva();
        politica.evaluar(10, true);

        for seq in 11..=15 {
            assert_eq!(politica.evaluar(seq, false), Decision::Aceptar, "seq {seq}");
        }
        assert_eq!(politica.ultimo(), Some(15));
    }

    #[test]
    fn un_frame_viejo_se_descarta_sin_tocar_el_ultimo() {
        let mut politica = PoliticaOrden::nueva();
        politica.evaluar(10, true);
        politica.evaluar(11, false);

        assert_eq!(politica.evaluar(11, false), Decision::DescartarTardio);
        assert_eq!(politica.evaluar(5, false), Decision::DescartarTardio);
        assert_eq!(
            politica.evaluar(5, true),
            Decision::DescartarTardio,
            "ni siquiera un keyframe viejo debe retroceder la secuencia"
        );
        assert_eq!(politica.ultimo(), Some(11));
    }

    #[test]
    fn un_salto_con_inter_frame_es_hueco() {
        let mut politica = PoliticaOrden::nueva();
        politica.evaluar(10, true);

        assert_eq!(
            politica.evaluar(13, false),
            Decision::Hueco { esperado: 11 }
        );
        assert_eq!(
            politica.ultimo(),
            Some(10),
            "un hueco no avanza el ultimo aceptado"
        );
    }

    #[test]
    fn un_keyframe_repara_la_cadena_aunque_salte_numeros() {
        let mut politica = PoliticaOrden::nueva();
        politica.evaluar(10, true);

        assert_eq!(politica.evaluar(20, true), Decision::Aceptar);
        assert_eq!(politica.ultimo(), Some(20));
    }

    #[test]
    fn un_frame_reordenado_que_llega_justo_despues_se_acepta() {
        // Este es el caso que justifica no avanzar `ultimo` al detectar un hueco: llega el
        // 12 antes que el 11, y cuando el 11 aparece encaja y se decodifica.
        let mut politica = PoliticaOrden::nueva();
        politica.evaluar(10, true);

        assert_eq!(
            politica.evaluar(12, false),
            Decision::Hueco { esperado: 11 }
        );
        assert_eq!(politica.evaluar(11, false), Decision::Aceptar);
        assert_eq!(politica.ultimo(), Some(11));
    }

    #[test]
    fn varios_huecos_seguidos_solo_piden_un_keyframe() {
        let mut amortiguador = AmortiguadorKeyframe::nuevo(Duration::from_secs(1));
        let ahora = Instant::now();

        assert!(amortiguador.debe_pedir(ahora), "el primero si pide");
        for milisegundos in [1u64, 10, 100, 500, 999] {
            assert!(
                !amortiguador.debe_pedir(ahora + Duration::from_millis(milisegundos)),
                "no deberia repetir a los {milisegundos} ms"
            );
        }
    }

    #[test]
    fn se_reintenta_si_no_llega_ningun_keyframe() {
        let mut amortiguador = AmortiguadorKeyframe::nuevo(Duration::from_secs(1));
        let ahora = Instant::now();

        assert!(amortiguador.debe_pedir(ahora));
        assert!(
            amortiguador.debe_pedir(ahora + Duration::from_millis(1_001)),
            "pasado el reintento hay que volver a pedir por si el host la ignoro"
        );
    }

    #[test]
    fn tras_recibir_keyframe_la_siguiente_peticion_es_inmediata() {
        let mut amortiguador = AmortiguadorKeyframe::nuevo(Duration::from_secs(1));
        let ahora = Instant::now();

        assert!(amortiguador.debe_pedir(ahora));
        amortiguador.keyframe_recibido();

        assert!(
            amortiguador.debe_pedir(ahora + Duration::from_millis(1)),
            "resuelto el hueco anterior, un hueco nuevo debe poder pedir enseguida"
        );
    }
}

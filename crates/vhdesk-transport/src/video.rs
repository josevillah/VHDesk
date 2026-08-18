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
//!
//! Los frames ya leidos esperan al consumidor en una ranura de [`CAPACIDAD_RANURA`]
//! huecos que, al llenarse, **desaloja el de secuencia menor**. Ver alli el razonamiento
//! de por que la capacidad es la que es y por que el descarte va en esa direccion.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use quinn::Connection;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use vhdesk_proto::{Message, VideoCodec, VideoFrame};

use crate::error::TransportError;

/// Prioridad de los streams de video, la de referencia.
const PRIORIDAD_VIDEO: i32 = 0;

/// Frames terminados que se retienen mientras el consumidor no los recoge.
///
/// **Uno.** Esta cifra es un compromiso entre dos cosas que es facil confundir, y por eso
/// esta aqui con nombre en vez de escrita en medio del codigo:
///
/// - **Retraso del consumidor**: si el consumidor va lento, cada hueco de cola es un frame
///   entero de latencia añadida (33 ms a 30 fps) que ademas ya no vale nada cuando se
///   entrega. De este lado la capacidad correcta es 1, y por eso se empieza en 1.
/// - **Jitter entre streams paralelos**: los frames se leen en tareas concurrentes, asi
///   que dos pueden terminar casi a la vez aunque se emitieran separados. Con capacidad 1,
///   ese solapamiento sub-frame que antes se absorbia ahora produce un hueco, y un hueco
///   cuesta un keyframe de ~100 KB.
///
/// Lo que hace aceptable el 1 es **quien consume**: no es el hilo de pintado sino el de
/// decodificacion, que drena en un par de milisegundos y vuelve a esperar. La ventana de
/// colision pasa de "un frame de render" a "un decode", y a esa escala el solapamiento
/// solo ocurre cuando ya estamos en regimen degradado, donde saltar hacia delante es
/// justamente lo que queremos.
///
/// FASE 1, bloque F: medir 1 contra 2 con latencia extremo a extremo y keyframes por
/// segundo, en vez de discutirlo. Subirlo a 2 es cambiar este numero y nada mas.
const CAPACIDAD_RANURA: usize = 1;

/// Cada cuanto se reintenta la peticion de keyframe si no llega ninguno.
const REINTENTO_KEYFRAME: Duration = Duration::from_secs(1);

/// Aviso compartido de que el proximo frame tiene que ser keyframe.
///
/// Existe porque las tres cosas que pueden pedir un keyframe viven en sitios distintos y
/// ninguna puede llamar a las otras:
///
/// - el [`VideoSender`], cuando aborta un stream y rompe la cadena de referencias;
/// - la tarea que lee el canal de control, cuando llega un `KeyframeRequest` del viewer;
/// - el hilo de captura, cuando la captura senala `full_refresh`.
///
/// Quien lo consume es el hilo de codificacion, que lo consulta **antes de codificar**.
///
/// # Por que se consulta y no se consume
///
/// [`SenalKeyframe::pendiente`] no borra la peticion: la borra [`VideoSender::send_frame`]
/// cuando el keyframe **sale de verdad**. Si el hilo de codificacion la consumiera al
/// leerla, un fallo de codificacion entre medias perderia la peticion y el viewer se
/// quedaria esperando una imagen que ya nadie va a mandar.
#[derive(Debug, Clone)]
pub struct SenalKeyframe(Arc<AtomicBool>);

impl SenalKeyframe {
    /// Crea la senal con el valor inicial dado.
    ///
    /// Arranca en `true` en una sesion nueva: sin un primer keyframe el viewer no tiene
    /// por donde engancharse.
    pub fn nueva(pendiente: bool) -> Self {
        Self(Arc::new(AtomicBool::new(pendiente)))
    }

    /// Pide un keyframe. Es idempotente: varias peticiones seguidas producen uno solo.
    pub fn pedir(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Si hay un keyframe pendiente de emitir.
    pub fn pendiente(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Da la peticion por atendida. La llama el emisor cuando el keyframe sale.
    fn atendida(&self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

impl Default for SenalKeyframe {
    fn default() -> Self {
        Self::nueva(true)
    }
}

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
    senal: SenalKeyframe,
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
            senal: SenalKeyframe::nueva(true),
            descartados: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Si el proximo frame tiene que ser keyframe.
    ///
    /// El host debe consultarlo **antes de codificar** y, si es `true`, pedirle un keyframe
    /// al codificador. Se pone a `true` al arrancar la sesion y cada vez que se descarta un
    /// frame, porque en ese momento la cadena de referencias queda rota.
    pub fn keyframe_pendiente(&self) -> bool {
        self.senal.pendiente()
    }

    /// Handle de la senal de keyframe, para quien tenga que pedirlo desde otra tarea.
    ///
    /// Lo necesitan la tarea de control (que recibe `KeyframeRequest` del viewer) y el hilo
    /// de captura (que ve el `full_refresh`), porque ninguno de los dos tiene acceso a este
    /// emisor.
    pub fn senal_keyframe(&self) -> SenalKeyframe {
        self.senal.clone()
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
            self.senal.pedir();
        }

        // El orden importa y es este: primero se anota la rotura que acaba de provocar el
        // descarte y despues se da por atendida si **este** frame es keyframe, porque un
        // keyframe repara la cadena que el descarte rompio.
        if frame.keyframe {
            self.senal.atendida();
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
    ranura: Arc<Ranura>,
    politica: PoliticaOrden,
    amortiguador: AmortiguadorKeyframe,
    _aceptador: JoinHandle<()>,
}

/// Lo que una tarea de lectura entrega al receptor.
type Resultado = Result<Box<VideoFrame>, MotivoDescarte>;

impl VideoReceiver {
    pub(crate) fn new(conn: Connection) -> Self {
        let ranura = Arc::new(Ranura::nueva(CAPACIDAD_RANURA));
        let de_las_tareas = Arc::clone(&ranura);

        let aceptador = tokio::spawn(async move {
            while let Ok(recv) = conn.accept_uni().await {
                let ranura = Arc::clone(&de_las_tareas);
                // Una tarea por stream: es lo que conserva la concurrencia.
                tokio::spawn(async move {
                    ranura.depositar(leer_frame(recv).await);
                });
            }
            // La conexion se cerro: hay que despertar al consumidor o se quedaria
            // esperando un frame que ya no puede llegar.
            de_las_tareas.cerrar();
        });

        Self {
            ranura,
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
        let resultado = self
            .ranura
            .recoger()
            .await
            .ok_or(TransportError::EndpointClosed)?;

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

/// Cola acotada de [`CAPACIDAD_RANURA`] frames que, al llenarse, **descarta el mas viejo**.
///
/// No es un `mpsc` porque un canal no deja desalojar lo que ya tiene dentro, y aqui el
/// descarte tiene que ir en la direccion contraria a la habitual: cuando no cabe todo, lo
/// que sobra es el frame **viejo**, no el que acaba de llegar. Un canal que descarta el
/// nuevo deja al consumidor arrastrando imagen atrasada, que es justo lo que la politica de
/// latencia de este proyecto prohibe.
///
/// "Mas viejo" es **el de numero de secuencia menor**, no el que llego antes. La diferencia
/// importa porque los streams se leen en tareas paralelas y el N+1 puede terminar antes que
/// el N: el orden de llegada no es el orden temporal del contenido.
struct Ranura {
    estado: Mutex<EstadoRanura>,
    aviso: Notify,
    capacidad: usize,
}

struct EstadoRanura {
    cola: VecDeque<Resultado>,
    cerrada: bool,
}

impl Ranura {
    fn nueva(capacidad: usize) -> Self {
        Self {
            estado: Mutex::new(EstadoRanura {
                cola: VecDeque::with_capacity(capacidad.max(1) + 1),
                cerrada: false,
            }),
            aviso: Notify::new(),
            capacidad: capacidad.max(1),
        }
    }

    /// Deja un resultado, desalojando el mas viejo si ya no cabe.
    fn depositar(&self, resultado: Resultado) {
        {
            let Ok(mut estado) = self.estado.lock() else {
                // Mutex envenenado: hubo un panico en otra tarea mientras lo tenia. Perder
                // este frame es preferible a propagar el panico por todo el receptor.
                return;
            };
            if estado.cerrada {
                return;
            }

            estado.cola.push_back(resultado);

            while estado.cola.len() > self.capacidad {
                let secuencias: Vec<Option<u64>> = estado
                    .cola
                    .iter()
                    .map(|entrada| entrada.as_ref().ok().map(|frame| frame.sequence))
                    .collect();
                let indice = indice_a_desalojar(&secuencias);
                estado.cola.remove(indice);

                // No se reporta como descarte: el consumidor lo vera como un salto en la
                // secuencia y la politica lo tratara como cualquier otro hueco. Es la misma
                // senal por el mismo camino.
                tracing::debug!(
                    capacidad = self.capacidad,
                    "ranura de video llena; se desaloja el frame mas viejo"
                );
            }
        }

        self.aviso.notify_one();
    }

    /// Recoge el siguiente resultado, esperando si no hay ninguno.
    ///
    /// Devuelve `None` cuando la ranura se cerro y ya no queda nada dentro.
    async fn recoger(&self) -> Option<Resultado> {
        loop {
            // `notified()` se arma **antes** de mirar la cola: al reves habria una ventana
            // en la que un deposito entre la mirada y la espera se perderia y el consumidor
            // se quedaria dormido con un frame delante.
            let espera = self.aviso.notified();

            {
                let Ok(mut estado) = self.estado.lock() else {
                    return None;
                };
                if let Some(resultado) = estado.cola.pop_front() {
                    return Some(resultado);
                }
                if estado.cerrada {
                    return None;
                }
            }

            espera.await;
        }
    }

    /// Marca la ranura como cerrada y despierta a quien estuviera esperando.
    fn cerrar(&self) {
        if let Ok(mut estado) = self.estado.lock() {
            estado.cerrada = true;
        }
        self.aviso.notify_waiters();
        self.aviso.notify_one();
    }
}

/// Indice de la entrada que hay que desalojar cuando la ranura se pasa de capacidad.
///
/// Pura para poder testearla sin red. Las reglas, en orden:
///
/// 1. Los avisos sin frame (`None`, o sea un descarte del emisor) se van primero: son
///    informativos y no llevan imagen que perder.
/// 2. Entre los frames, el de **secuencia menor**, que es el mas atrasado.
/// 3. A igualdad, el que este mas cerca del frente, para que la cola no se estanque.
fn indice_a_desalojar(secuencias: &[Option<u64>]) -> usize {
    let mut mejor = 0usize;

    for (indice, secuencia) in secuencias.iter().enumerate() {
        let peor_que_el_actual = match (secuencia, &secuencias[mejor]) {
            (None, Some(_)) => true,
            (Some(candidata), Some(actual)) => candidata < actual,
            _ => false,
        };
        if peor_que_el_actual {
            mejor = indice;
        }
    }

    mejor
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
    use super::{AmortiguadorKeyframe, Decision, PoliticaOrden, SenalKeyframe, indice_a_desalojar};
    use std::time::{Duration, Instant};

    #[test]
    fn se_desaloja_el_frame_de_secuencia_menor_no_el_que_llego_antes() {
        // Los streams se leen en paralelo, asi que el orden de llegada no es el temporal:
        // aqui el 8 llego primero y el 7 despues, y el que sobra es el 7.
        assert_eq!(indice_a_desalojar(&[Some(8), Some(7)]), 1);
        assert_eq!(indice_a_desalojar(&[Some(7), Some(8)]), 0);
        assert_eq!(indice_a_desalojar(&[Some(9), Some(4), Some(6)]), 1);
    }

    #[test]
    fn un_aviso_sin_frame_se_desaloja_antes_que_cualquier_frame() {
        // Un `Err` es un descarte del emisor: informativo, sin imagen que perder. Tirarlo
        // antes que un frame de verdad es siempre la eleccion correcta.
        assert_eq!(indice_a_desalojar(&[Some(1), None]), 1);
        assert_eq!(indice_a_desalojar(&[None, Some(1)]), 0);
        assert_eq!(
            indice_a_desalojar(&[None, None]),
            0,
            "a igualdad se va el mas cercano al frente, para que la cola no se estanque"
        );
    }

    #[test]
    fn la_senal_de_keyframe_arranca_pedida_y_solo_la_borra_quien_la_atiende() {
        // Arranca pedida porque sin el primer keyframe el viewer no tiene por donde
        // engancharse.
        let senal = SenalKeyframe::default();
        assert!(senal.pendiente());

        senal.atendida();
        assert!(!senal.pendiente());

        // Consultarla no la consume: si el codificador fallara entre la consulta y la
        // emision, la peticion tiene que seguir viva.
        senal.pedir();
        assert!(senal.pendiente());
        assert!(senal.pendiente());

        // Y varias peticiones seguidas producen un solo keyframe, no una tormenta.
        senal.pedir();
        senal.pedir();
        senal.atendida();
        assert!(!senal.pendiente());
    }

    #[test]
    fn los_clones_de_la_senal_comparten_estado() {
        // Es lo que permite que la tarea de control y el hilo de captura pidan keyframe sin
        // tener acceso al emisor.
        let senal = SenalKeyframe::nueva(false);
        let copia = senal.clone();

        copia.pedir();
        assert!(senal.pendiente(), "la peticion de un clon no llego al otro");
    }

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

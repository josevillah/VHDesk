//! Mensajes del protocolo VHDesk.
//!
//! Los mensajes se dividen en dos familias segun como viajan por el cable:
//!
//! - **Control** (`Hello`, `AuthRequest`, `AuthResponse`, `InputEvent`, `Cursor`,
//!   `ClipboardUpdate`, `Ping`, `Pong`): estructuras pequenas, serializadas con postcard.
//! - **Media** (`VideoFrame`, `AudioFrame`): cabecera fija escrita a mano seguida de un
//!   payload opaco. No pasan por serde porque el payload es el 99,9% del trafico y
//!   atravesar serde obligaria a copiarlo; asi el decodificador devuelve un [`Bytes`] que
//!   apunta al buffer original sin copiar nada.

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use crate::codecs::{AudioCodec, VideoCodec};

/// Version del protocolo que implementa este crate.
///
/// Se incrementa ante cualquier cambio incompatible del formato del wire.
///
/// - **1**: version inicial.
/// - **2**: `VideoFrame` gana `sequence`, y se anade `KeyframeRequest`. Sin el numero de
///   secuencia el receptor no puede saber que le falta un frame, y con un stream por frame
///   los huecos son el camino normal de degradacion, no una excepcion.
/// - **3**: se anade [`ReleaseAll`]. Sin el, una tecla que estaba hundida cuando el viewer
///   pierde el foco se queda hundida en la maquina remota para siempre.
pub const PROTOCOL_VERSION: u16 = 3;

/// Numero maximo de codecs que un peer puede anunciar en su `Hello`.
///
/// Existe para que la lista de capacidades no sea un vector de amplificacion.
pub const MAX_ANNOUNCED_CODECS: usize = 32;

/// Longitud maxima en bytes del nombre legible que anuncia un peer.
pub const MAX_PEER_NAME_LEN: usize = 128;

/// Papel que juega un peer en la sesion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    /// La maquina controlada: captura pantalla e inyecta input.
    Host,
    /// La maquina que controla: muestra el video y envia input.
    Viewer,
}

/// Primer mensaje de la conexion, en ambos sentidos.
///
/// Se envia despues del handshake TLS y antes de la autenticacion: sirve para acordar
/// version y capacidades, nunca para conceder acceso.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// Version del protocolo que habla el emisor.
    pub protocol_version: u16,
    /// Papel del emisor en esta sesion.
    pub role: Role,
    /// Codecs de video que el emisor puede manejar, en orden de preferencia.
    pub video_codecs: Vec<VideoCodec>,
    /// Codecs de audio que el emisor puede manejar, en orden de preferencia.
    pub audio_codecs: Vec<AudioCodec>,
    /// Nombre legible del peer, para mostrarlo en el dialogo de consentimiento.
    ///
    /// No es un identificador ni sirve para autenticar: lo elige el peer y puede mentir.
    pub peer_name: String,
}

impl Hello {
    /// Comprueba los limites de longitud de los campos variables.
    ///
    /// Se llama al decodificar. Los limites son bajos a proposito: nada legitimo se
    /// acerca a ellos y mantienen acotado lo que un peer no autenticado puede hacernos
    /// reservar, porque `Hello` llega antes de cualquier autenticacion.
    ///
    /// # Errores
    ///
    /// Devuelve [`ProtoError::FieldTooLong`](crate::ProtoError::FieldTooLong) si algun
    /// campo supera su maximo.
    pub fn validate(&self) -> Result<(), crate::ProtoError> {
        check_len(
            "Hello.video_codecs",
            self.video_codecs.len(),
            MAX_ANNOUNCED_CODECS,
        )?;
        check_len(
            "Hello.audio_codecs",
            self.audio_codecs.len(),
            MAX_ANNOUNCED_CODECS,
        )?;
        check_len("Hello.peer_name", self.peer_name.len(), MAX_PEER_NAME_LEN)
    }
}

fn check_len(field: &'static str, len: usize, max: usize) -> Result<(), crate::ProtoError> {
    if len > max {
        return Err(crate::ProtoError::FieldTooLong { field, len, max });
    }
    Ok(())
}

/// Metodo con el que el viewer intenta autenticarse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum AuthMethod {
    /// Contrasena permanente configurada por el dueno del host.
    Password,
    /// Contrasena de un solo uso, rotatoria, mostrada en la pantalla del host.
    OneTimePassword,
}

/// Intento de autenticacion del viewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthRequest {
    /// Metodo elegido.
    pub method: AuthMethod,
    /// Prueba de conocimiento de la contrasena.
    ///
    /// El formato lo define la fase 2 junto con el esquema Argon2id. Aqui es opaco a
    /// proposito: `vhdesk-proto` no debe saber nada de criptografia.
    pub proof: Vec<u8>,
}

/// Resultado de un intento de autenticacion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum AuthResult {
    /// Credenciales correctas y consentimiento concedido: la sesion arranca.
    Accepted,
    /// Credenciales incorrectas.
    Denied,
    /// Credenciales correctas; el host esta esperando a que su dueno acepte en pantalla.
    ///
    /// No es un estado terminal: llegara otro `AuthResponse` con la decision.
    AwaitingConsent,
    /// El dueno del host rechazo la conexion.
    ConsentDenied,
    /// El dialogo de consentimiento expiro sin respuesta.
    ConsentTimeout,
    /// Demasiados intentos fallidos; el host esta aplicando backoff.
    RateLimited,
}

/// Respuesta del host a un [`AuthRequest`], y punto donde se cierra la negociacion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthResponse {
    /// Veredicto del host.
    pub result: AuthResult,
    /// Codec de video elegido para la sesion.
    ///
    /// Lo decide el host, que es quien codifica, a partir de la interseccion entre su
    /// lista y la que el viewer anuncio en su `Hello`. Solo es `Some` cuando `result` es
    /// [`AuthResult::Accepted`].
    pub video_codec: Option<VideoCodec>,
    /// Codec de audio elegido para la sesion, con el mismo criterio.
    pub audio_codec: Option<AudioCodec>,
}

/// Un frame de video codificado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFrame {
    /// Indice del monitor del host al que pertenece el frame.
    pub monitor: u8,
    /// Codec con el que esta codificado.
    ///
    /// Va en cada frame y no solo en la negociacion inicial para que el decodificador no
    /// dependa de un estado de sesion que podria estar desincronizado, y para permitir en
    /// el futuro cambiar de codec en caliente sin tocar el formato.
    pub codec: VideoCodec,
    /// Numero de frame dentro de la sesion, monotono y sin huecos en el emisor.
    ///
    /// **Es lo unico que permite al receptor saber que le falta un frame.** QUIC garantiza
    /// el orden dentro de un stream, pero no entre streams distintos, y aqui cada frame va
    /// por el suyo: el frame N+1 puede llegar completo antes que el N si el N pierde un
    /// paquete y hay que retransmitirlo. Ademas el emisor descarta frames a proposito
    /// cuando se acumulan, asi que los huecos son el camino normal de degradacion.
    ///
    /// Decodificar un inter-frame cuya referencia falta no da error: da imagen corrupta.
    /// Por eso este campo existe.
    ///
    /// Lo asigna el transporte, no quien construye el frame.
    pub sequence: u64,
    /// Si el frame es decodificable por si solo.
    pub keyframe: bool,
    /// Instante de captura, en microsegundos desde el arranque de la sesion.
    ///
    /// Relativo a la sesion y no al reloj de pared: es lo unico que necesita el
    /// calculo de latencia y evita filtrar la hora local del host.
    pub timestamp_us: u64,
    /// Anchura en pixeles.
    pub width: u16,
    /// Altura en pixeles.
    pub height: u16,
    /// Datos codificados.
    pub data: Bytes,
}

/// Un frame de audio codificado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFrame {
    /// Codec con el que esta codificado.
    pub codec: AudioCodec,
    /// Instante de captura, en microsegundos desde el arranque de la sesion.
    pub timestamp_us: u64,
    /// Frecuencia de muestreo en Hz.
    pub sample_rate: u32,
    /// Numero de canales.
    pub channels: u8,
    /// Datos codificados.
    pub data: Bytes,
}

/// Boton del raton.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum MouseButton {
    /// Boton principal.
    Left,
    /// Boton central, normalmente la rueda.
    Middle,
    /// Boton secundario.
    Right,
    /// Cuarto boton, "atras" en la mayoria de ratones.
    Back,
    /// Quinto boton, "adelante".
    Forward,
}

/// Evento de entrada que el viewer envia al host para que lo inyecte.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum InputEvent {
    /// Movimiento absoluto del puntero.
    ///
    /// Las coordenadas van normalizadas al rango `0.0..=1.0` sobre el monitor indicado,
    /// no en pixeles. Asi el viewer no necesita conocer la resolucion real del host ni
    /// hay que reenviar eventos cuando el host cambia de resolucion a mitad de sesion.
    MouseMoveAbsolute {
        /// Monitor de destino.
        monitor: u8,
        /// Posicion horizontal normalizada.
        x: f32,
        /// Posicion vertical normalizada.
        y: f32,
    },
    /// Pulsacion o liberacion de un boton del raton.
    MouseButton {
        /// Boton afectado.
        button: MouseButton,
        /// `true` al pulsar, `false` al soltar.
        pressed: bool,
    },
    /// Desplazamiento de rueda, en "clicks" de rueda (no en pixeles).
    MouseScroll {
        /// Desplazamiento horizontal.
        delta_x: f32,
        /// Desplazamiento vertical.
        delta_y: f32,
    },
    /// Pulsacion o liberacion de una tecla.
    ///
    /// Se identifica por scancode fisico y no por caracter: el mapa de teclado que
    /// importa es el del host, y traducir en el viewer romperia cualquier distribucion
    /// que no coincida entre las dos maquinas.
    Key {
        /// Scancode fisico segun la tabla USB HID.
        scancode: u32,
        /// `true` al pulsar, `false` al soltar.
        pressed: bool,
    },
}

/// Actualizacion del cursor del host.
///
/// La forma viaja aparte de los frames de video para que el viewer pueda dibujar el
/// cursor localmente y este se sienta instantaneo, sin esperar al siguiente frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Cursor {
    /// Nueva imagen de cursor.
    Shape {
        /// Desplazamiento horizontal del punto activo dentro de la imagen.
        hotspot_x: u16,
        /// Desplazamiento vertical del punto activo dentro de la imagen.
        hotspot_y: u16,
        /// Anchura de la imagen en pixeles.
        width: u16,
        /// Altura de la imagen en pixeles.
        height: u16,
        /// Pixeles RGBA, `width * height * 4` bytes.
        rgba: Vec<u8>,
    },
    /// Nueva posicion del cursor, normalizada igual que en
    /// [`InputEvent::MouseMoveAbsolute`].
    Position {
        /// Monitor en el que esta el cursor.
        monitor: u8,
        /// Posicion horizontal normalizada.
        x: f32,
        /// Posicion vertical normalizada.
        y: f32,
    },
    /// El cursor no es visible en el host.
    Hidden,
}

/// Formato del contenido del portapapeles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ClipboardFormat {
    /// Texto plano en UTF-8.
    Utf8Text,
    /// Imagen PNG.
    ImagePng,
}

/// Contenido nuevo del portapapeles de uno de los dos lados.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardUpdate {
    /// Formato del contenido.
    pub format: ClipboardFormat,
    /// Contenido.
    pub data: Vec<u8>,
}

/// Por que se pide un keyframe.
///
/// Se distinguen para que las estadisticas de la fase 4 puedan separar "la red va mal" de
/// "el decodificador se perdio", que piden soluciones distintas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum KeyframeReason {
    /// Arranque de sesion: el viewer aun no tiene ninguna imagen.
    Startup,
    /// Falta un frame en la secuencia y la cadena de referencias esta rota.
    Gap,
    /// El decodificador fallo y no puede seguir con los inter-frames que le lleguen.
    DecoderError,
}

/// Peticion de keyframe del viewer al host.
///
/// Es la red de seguridad para lo que el emisor **no puede saber**: perdida real de red,
/// desincronizacion del decodificador y arranque de sesion. Cuando el propio host descarta
/// un frame, no hace falta pedirselo: el ya sabe que rompio la cadena y fuerza el keyframe
/// por su cuenta, lo que ahorra un RTT entero de imagen rota.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyframeRequest {
    /// Monitor cuyo flujo hay que refrescar.
    pub monitor: u8,
    /// Motivo, para diagnostico y estadisticas.
    pub reason: KeyframeReason,
}

/// Suelta en el host todo lo que el viewer tenga hundido.
///
/// # Por que existe y por que viaja por el canal de input
///
/// Si el viewer pierde el foco, se minimiza, se cierra o se le cae la conexion con una
/// tecla pulsada, el host se queda con esa tecla hundida **para siempre**. Con Ctrl o Alt
/// la maquina remota queda practicamente inservible, y el sintoma aparece *despues* de que
/// la sesion terminara, asi que nadie lo relaciona con la causa.
///
/// El caso que mejor lo ilustra es Alt+Tab: el Alt viaja al host justo antes de que la
/// ventana del viewer pierda el foco, asi que el `ReleaseAll` que viene detras es lo unico
/// que evita dejar un Alt hundido al otro lado.
///
/// Va por el **canal de input** y no por el de control para que quede ordenado detras de la
/// ultima pulsacion. Por control seria un stream distinto, y QUIC no ordena entre streams:
/// podria adelantar a la tecla que venia a soltar y dejarla hundida igualmente.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseAll;

/// Sonda de latencia.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ping {
    /// Valor arbitrario que el receptor debe devolver tal cual en el [`Pong`].
    pub nonce: u64,
    /// Instante de envio, en microsegundos desde el arranque de la sesion.
    pub sent_us: u64,
}

/// Respuesta a un [`Ping`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pong {
    /// El mismo `nonce` que traia el `Ping`.
    pub nonce: u64,
    /// El mismo `sent_us` que traia el `Ping`, para calcular el RTT sin guardar estado.
    pub sent_us: u64,
}

/// Cualquier mensaje del protocolo.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Message {
    /// Ver [`Hello`].
    Hello(Hello),
    /// Ver [`AuthRequest`].
    AuthRequest(AuthRequest),
    /// Ver [`AuthResponse`].
    AuthResponse(AuthResponse),
    /// Ver [`VideoFrame`].
    VideoFrame(VideoFrame),
    /// Ver [`AudioFrame`].
    AudioFrame(AudioFrame),
    /// Ver [`InputEvent`].
    InputEvent(InputEvent),
    /// Ver [`Cursor`].
    Cursor(Cursor),
    /// Ver [`ClipboardUpdate`].
    ClipboardUpdate(ClipboardUpdate),
    /// Ver [`KeyframeRequest`].
    KeyframeRequest(KeyframeRequest),
    /// Ver [`ReleaseAll`].
    ReleaseAll(ReleaseAll),
    /// Ver [`Ping`].
    Ping(Ping),
    /// Ver [`Pong`].
    Pong(Pong),
}

impl Message {
    /// Nombre del mensaje, para mensajes de error y trazas.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Hello(_) => "Hello",
            Self::AuthRequest(_) => "AuthRequest",
            Self::AuthResponse(_) => "AuthResponse",
            Self::VideoFrame(_) => "VideoFrame",
            Self::AudioFrame(_) => "AudioFrame",
            Self::InputEvent(_) => "InputEvent",
            Self::Cursor(_) => "Cursor",
            Self::ClipboardUpdate(_) => "ClipboardUpdate",
            Self::KeyframeRequest(_) => "KeyframeRequest",
            Self::ReleaseAll(_) => "ReleaseAll",
            Self::Ping(_) => "Ping",
            Self::Pong(_) => "Pong",
        }
    }
}

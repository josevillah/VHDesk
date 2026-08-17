//! Tipos que describen monitores, frames y lo que ocurre en cada ciclo de captura.

use std::time::{Duration, Instant};

use crate::cursor::CursorUpdate;
use crate::pool::PooledBuffer;

/// Identifica un monitor por el adaptador grafico que lo posee y su indice de salida.
///
/// El adaptador forma parte de la identidad y no es un detalle: en portatiles con grafica
/// hibrida cada monitor pertenece a un adaptador distinto, y pedir la duplicacion de un
/// output al adaptador equivocado falla con `DXGI_ERROR_UNSUPPORTED`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MonitorId {
    /// Indice del adaptador grafico.
    pub adapter: u32,
    /// Indice del output dentro de ese adaptador.
    pub output: u32,
}

impl std::fmt::Display for MonitorId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.adapter, self.output)
    }
}

/// Descripcion de un monitor conectado.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorInfo {
    /// Identificador con el que abrir la captura de este monitor.
    pub id: MonitorId,
    /// Nombre del dispositivo tal y como lo da el sistema (`\\.\DISPLAY1`).
    ///
    /// No es el nombre comercial del panel: obtener ese requiere consultar el EDID, y no
    /// aporta nada hasta que haya un selector de monitor en la interfaz (fase 5).
    pub name: String,
    /// Nombre del adaptador grafico que posee este monitor.
    ///
    /// Sirve para diagnostico: en portatiles hibridos dice cual de las dos graficas hay
    /// que usar, y delata a los adaptadores virtuales, que se comportan distinto en cuanto
    /// a metadatos de regiones sucias.
    pub adapter_name: String,
    /// Anchura en pixeles fisicos.
    pub width: u32,
    /// Altura en pixeles fisicos.
    pub height: u32,
    /// Posicion de la esquina superior izquierda en el escritorio virtual.
    pub position: (i32, i32),
    /// Factor de escala de la interfaz (1.0 = 96 ppp, 1.5 = 150%).
    pub scale: f32,
    /// Si es el monitor principal.
    pub primary: bool,
}

/// Rectangulo en coordenadas del monitor, con el borde derecho e inferior excluidos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    /// Borde izquierdo, incluido.
    pub left: i32,
    /// Borde superior, incluido.
    pub top: i32,
    /// Borde derecho, excluido.
    pub right: i32,
    /// Borde inferior, excluido.
    pub bottom: i32,
}

impl Rect {
    /// Anchura, o cero si el rectangulo esta invertido.
    pub const fn width(&self) -> u32 {
        if self.right > self.left {
            (self.right - self.left) as u32
        } else {
            0
        }
    }

    /// Altura, o cero si el rectangulo esta invertido.
    pub const fn height(&self) -> u32 {
        if self.bottom > self.top {
            (self.bottom - self.top) as u32
        } else {
            0
        }
    }

    /// Si el rectangulo no cubre ningun pixel.
    pub const fn is_empty(&self) -> bool {
        self.width() == 0 || self.height() == 0
    }
}

/// Region que el sistema movio de un sitio a otro de la pantalla sin redibujarla.
///
/// **Ninguna logica puede depender de que existan.** Muchos drivers no los emiten jamas;
/// medido en la Radeon integrada del portatil de desarrollo, cero move rects en 20 frames
/// incluso durante un scroll, que es el caso donde deberian aparecer por definicion.
///
/// Si alguna vez se aprovechan, tiene que ser como camino opcional con respaldo
/// obligatorio: la ruta que trata el movimiento como region sucia normal debe seguir
/// existiendo y siendo correcta, porque en la mayoria de las maquinas sera la unica que se
/// ejecute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MoveRect {
    /// Esquina superior izquierda de donde estaba la region antes.
    pub source: (i32, i32),
    /// Donde esta la region ahora.
    pub destination: Rect,
}

/// Desglose del coste de bajar un frame de la GPU.
///
/// Se separan los dos sumandos porque responden a preguntas distintas y solo uno de ellos
/// es optimizable desde aqui: el primero es esperar a la GPU y el segundo es ancho de
/// banda de memoria. Confundirlos lleva a "optimizar" una copia que en realidad estaba
/// esperando a otra cosa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaptureTimings {
    /// Copia a la textura intermedia mas el mapeo.
    ///
    /// El mapeo de lectura **bloquea hasta que la GPU termina la copia**, asi que este
    /// tiempo es sobre todo espera, no trabajo de la CPU.
    pub map_wait: Duration,
    /// Lectura del mapeo y escritura al buffer del pool.
    ///
    /// Esto si es ancho de banda: se leen `stride * height` bytes de memoria mapeada de la
    /// GPU, que es notablemente mas lenta que la memoria normal, y se escriben otros
    /// tantos en el buffer del pool.
    pub download: Duration,
}

impl CaptureTimings {
    /// Coste total de bajar el frame.
    pub fn total(&self) -> Duration {
        self.map_wait + self.download
    }
}

/// Un frame capturado.
#[derive(Debug)]
pub struct Frame {
    /// Pixeles en BGRA, `stride * height` bytes, prestados del pool del capturador.
    pub buffer: PooledBuffer,
    /// Anchura en pixeles.
    pub width: u32,
    /// Altura en pixeles.
    pub height: u32,
    /// Bytes por fila. **Puede ser mayor que `width * 4`**: el sistema alinea las filas y
    /// dar por hecho lo contrario corrompe la imagen en cuanto la resolucion no es
    /// multiplo de la alineacion.
    pub stride: usize,
    /// Numero de frame desde que se abrio la captura.
    ///
    /// Sirve para que el consumidor detecte los frames que ha descartado; ver la nota de
    /// [`Frame::dirty`].
    pub sequence: u64,
    /// Momento en que se recogio el frame de la duplicacion.
    pub captured_at: Instant,
    /// Desglose de lo que costo traer los pixeles de la GPU a memoria de sistema.
    pub timings: CaptureTimings,
    /// Momento en que el sistema presento el frame, en unidades del contador de alta
    /// resolucion de Windows.
    ///
    /// Es anterior a `captured_at` y mide el retardo que ya traia el frame antes de que
    /// nosotros lo tocaramos. Sin este dato, cualquier medida de latencia se atribuye
    /// entera a nuestro pipeline.
    pub presented_at_qpc: i64,
    /// Si este frame describe la pantalla entera en lugar de un delta.
    ///
    /// Es `true` en el primer frame de la captura y en el primero tras cada
    /// reinicializacion. En esos casos los rectangulos sucios **no** describen todo lo que
    /// cambio, asi que tratarlos como delta deja basura en pantalla. El codificador debe
    /// emitir un keyframe cuando esto es `true`.
    pub full_refresh: bool,
    /// Cuantos frames fusiono el sistema desde la captura anterior.
    ///
    /// Un valor mayor que uno significa que vamos por detras del ritmo de la pantalla: el
    /// sistema junto varias presentaciones en la que acabamos de recoger. Cuando ocurre,
    /// los rectangulos sucios se fusionan tambien, y tienden a degenerar en uno solo que
    /// cubre la pantalla entera. Es la senal de que el consumidor es el cuello de botella.
    pub accumulated_frames: u32,
    /// Regiones que cambiaron.
    ///
    /// **Son acumulativas desde la captura anterior.** Si el consumidor descarta un frame,
    /// no puede descartar sus rectangulos: tiene que acumularlos sobre los del siguiente,
    /// o el frame que si codifique quedara incompleto.
    pub dirty: Vec<Rect>,
    /// Regiones desplazadas. Ver la advertencia de [`MoveRect`].
    pub moves: Vec<MoveRect>,
    /// Cambio del puntero que acompana a este frame, si lo hubo.
    pub cursor: Option<CursorUpdate>,
}

impl Frame {
    /// Devuelve la fila `y` recortada a los pixeles visibles, sin el relleno del `stride`.
    ///
    /// Devuelve `None` si `y` cae fuera del frame.
    pub fn row(&self, y: u32) -> Option<&[u8]> {
        let start = (y as usize).checked_mul(self.stride)?;
        let end = start.checked_add((self.width as usize).checked_mul(4)?)?;
        self.buffer.get(start..end)
    }
}

/// Lo que devuelve un ciclo de captura.
///
/// Deliberadamente **no** es `#[non_exhaustive]`: todos los consumidores viven en este
/// workspace y queremos que anadir una variante rompa la compilacion alli donde haya que
/// tratarla, en vez de caer en un brazo comodin que la ignore en silencio.
#[derive(Debug)]
pub enum CaptureEvent {
    /// Hay pixeles nuevos.
    Frame(Frame),
    /// Solo cambio el puntero; los pixeles del escritorio son los mismos.
    ///
    /// Es un caso frecuente y barato: el sistema lo senala aparte para no obligarnos a
    /// recodificar una pantalla que no ha cambiado.
    CursorOnly(CursorUpdate),
    /// Se agoto el tiempo de espera sin novedades.
    ///
    /// **No es un error.** Con la pantalla quieta es la respuesta normal, y llega muchas
    /// veces por segundo.
    Timeout,
}

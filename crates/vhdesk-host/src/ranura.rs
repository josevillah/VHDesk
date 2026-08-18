//! La ranura de un hueco que separa la captura de la codificacion.
//!
//! # Por que un hueco y no una cola
//!
//! Con etapas concurrentes, los FPS los marca la etapa mas lenta, pero **la latencia es la
//! suma de todas mas lo que los frames esperen en las colas**. Un buffer profundo mejora la
//! grafica de FPS y empeora la sesion: cada hueco ocupado son 33 ms mas de retardo a 30 fps,
//! y en video en vivo un frame retrasado ya no vale nada cuando llega.
//!
//! Asi que aqui cabe uno. Si la codificacion no ha recogido el anterior cuando llega uno
//! nuevo, el viejo se tira.
//!
//! # Lo que NO se puede tirar con el
//!
//! Los rectangulos sucios que da DXGI son **acumulativos desde la captura anterior**. Si se
//! descarta un frame y con el sus rectangulos, el frame que si se codifique describira solo
//! lo que cambio desde el descartado, y todo lo que cambio antes se queda sin repintar: en
//! pantalla se ve como basura que no se va.
//!
//! Lo mismo, y peor, con `full_refresh`: marca que el frame describe la pantalla entera en
//! vez de un delta, y perderlo significa no emitir el keyframe que le corresponde.
//!
//! Por eso al descartar no se tira el frame entero, sino solo **sus pixeles**: los
//! metadatos se absorben en el que se queda. Esa es toda la razon de ser de este modulo, y
//! [`Metadatos::absorber_descartado`] es puro justamente para poder fijarlo con tests.

use std::sync::{Condvar, Mutex};
use std::time::Instant;

use vhdesk_capture::{Frame, PooledBuffer, Rect};

/// Tope de rectangulos sucios que se conservan por separado.
///
/// Pasado ese numero se colapsan en su envolvente. Es una perdida de precision segura: un
/// rectangulo mas grande de la cuenta hace repintar de mas, mientras que uno mas pequeno
/// dejaria basura. El tope existe porque en una racha de descartes la lista crece sin
/// limite, y una lista de miles de rectangulos cuesta mas de recorrer que lo que ahorra.
const MAX_RECTANGULOS: usize = 64;

/// Metadatos de un frame de captura, acumulables a lo largo de los descartes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metadatos {
    /// Si el frame describe la pantalla entera en vez de un delta.
    pub full_refresh: bool,
    /// Regiones que cambiaron desde el ultimo frame **codificado**, no desde el capturado.
    pub dirty: Vec<Rect>,
    /// Cuantos frames se descartaron para llegar a este.
    pub descartados: u32,
    /// Cuantas presentaciones del sistema fusiono la captura.
    pub accumulated_frames: u32,
}

impl Metadatos {
    /// Absorbe los metadatos de un frame que se acaba de descartar.
    ///
    /// `self` son los del frame que se conserva, y `descartado` los del que se tira. Tras la
    /// llamada, `self` describe todo lo que cambio desde el ultimo frame codificado.
    pub fn absorber_descartado(&mut self, descartado: &Self) {
        // `full_refresh` es pegajoso: si alguno de los dos lo traia, el frame resultante
        // tampoco es un delta valido y hace falta keyframe.
        self.full_refresh |= descartado.full_refresh;
        self.accumulated_frames += descartado.accumulated_frames;
        self.descartados = descartado.descartados + 1;

        // Los del descartado van delante porque son anteriores en el tiempo. No cambia el
        // resultado (la union no tiene orden) pero hace legible el volcado en las trazas.
        let mut union = descartado.dirty.clone();
        union.append(&mut self.dirty);
        self.dirty = acotar(union);
    }

    /// Si hay algo que repintar.
    pub fn hay_cambios(&self) -> bool {
        self.full_refresh || self.dirty.iter().any(|r| !r.is_empty())
    }
}

/// Deja la lista en [`MAX_RECTANGULOS`] o menos, colapsando en la envolvente si hace falta.
fn acotar(rectangulos: Vec<Rect>) -> Vec<Rect> {
    if rectangulos.len() <= MAX_RECTANGULOS {
        return rectangulos;
    }

    match envolvente(&rectangulos) {
        Some(caja) => vec![caja],
        None => Vec::new(),
    }
}

/// Rectangulo mas pequeno que contiene a todos los no vacios.
fn envolvente(rectangulos: &[Rect]) -> Option<Rect> {
    rectangulos
        .iter()
        .filter(|r| !r.is_empty())
        .copied()
        .reduce(|a, b| Rect {
            left: a.left.min(b.left),
            top: a.top.min(b.top),
            right: a.right.max(b.right),
            bottom: a.bottom.max(b.bottom),
        })
}

/// Un frame de captura con los metadatos de todos los que se descartaron antes que el.
#[derive(Debug)]
pub struct FrameAcumulado {
    /// Pixeles BGRA, prestados del pool del capturador.
    pub buffer: PooledBuffer,
    /// Anchura en pixeles.
    pub width: u32,
    /// Altura en pixeles.
    pub height: u32,
    /// Bytes por fila, que pueden ser mas que `width * 4`.
    pub stride: usize,
    /// Cuando se recogio de la duplicacion. Es el origen del reloj de latencia del frame.
    pub captured_at: Instant,
    /// Metadatos acumulados.
    pub meta: Metadatos,
}

impl From<Frame> for FrameAcumulado {
    fn from(frame: Frame) -> Self {
        Self {
            buffer: frame.buffer,
            width: frame.width,
            height: frame.height,
            stride: frame.stride,
            captured_at: frame.captured_at,
            meta: Metadatos {
                full_refresh: frame.full_refresh,
                dirty: frame.dirty,
                descartados: 0,
                accumulated_frames: frame.accumulated_frames,
            },
        }
    }
}

/// Ranura de un hueco entre la captura y la codificacion.
///
/// Las dos puntas son hilos sincronos (el capturador no es `Send` y el codificador es
/// trabajo de CPU puro), asi que esto es un `Mutex` con `Condvar` y no un canal de tokio.
pub struct Ranura {
    estado: Mutex<Estado>,
    aviso: Condvar,
}

#[derive(Default)]
struct Estado {
    frame: Option<FrameAcumulado>,
    cerrada: bool,
}

impl Ranura {
    /// Crea una ranura vacia.
    pub fn nueva() -> Self {
        Self {
            estado: Mutex::new(Estado::default()),
            aviso: Condvar::new(),
        }
    }

    /// Deja un frame, descartando los pixeles del anterior y quedandose sus metadatos.
    ///
    /// Nunca bloquea: la captura no debe frenarse porque el codificador vaya por detras.
    pub fn depositar(&self, mut frame: FrameAcumulado) {
        {
            let Ok(mut estado) = self.estado.lock() else {
                // Mutex envenenado: hubo un panico en el otro hilo. Perder este frame es
                // preferible a propagar el panico a la captura.
                return;
            };
            if estado.cerrada {
                return;
            }

            if let Some(anterior) = estado.frame.take() {
                frame.meta.absorber_descartado(&anterior.meta);
                // El buffer del descartado se suelta aqui y vuelve al pool del capturador.
            }
            estado.frame = Some(frame);
        }

        self.aviso.notify_one();
    }

    /// Espera al siguiente frame. Devuelve `None` cuando la ranura se cierra.
    pub fn recoger(&self) -> Option<FrameAcumulado> {
        let mut estado = self.estado.lock().ok()?;

        loop {
            if let Some(frame) = estado.frame.take() {
                return Some(frame);
            }
            if estado.cerrada {
                return None;
            }
            estado = self.aviso.wait(estado).ok()?;
        }
    }

    /// Cierra la ranura y despierta a quien estuviera esperando.
    pub fn cerrar(&self) {
        if let Ok(mut estado) = self.estado.lock() {
            estado.cerrada = true;
            // El frame que hubiera dentro se suelta ahora: su buffer vuelve al pool antes
            // de que el capturador se cierre.
            estado.frame = None;
        }
        self.aviso.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_RECTANGULOS, Metadatos};
    use vhdesk_capture::Rect;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    fn con_sucios(dirty: Vec<Rect>) -> Metadatos {
        Metadatos {
            dirty,
            ..Metadatos::default()
        }
    }

    #[test]
    fn los_rectangulos_del_descartado_no_se_pierden() {
        // La invariante del bloque: si se tiran los rectangulos del frame descartado, la
        // zona que cambio entonces se queda sin repintar y deja basura en pantalla.
        let mut conservado = con_sucios(vec![rect(100, 100, 200, 200)]);
        let descartado = con_sucios(vec![rect(0, 0, 50, 50)]);

        conservado.absorber_descartado(&descartado);

        assert_eq!(
            conservado.dirty,
            vec![rect(0, 0, 50, 50), rect(100, 100, 200, 200)],
            "la region del frame descartado tiene que seguir marcada como sucia"
        );
    }

    #[test]
    fn el_full_refresh_del_descartado_es_pegajoso() {
        // Perder un `full_refresh` significa no emitir el keyframe que le tocaba, y el
        // viewer se queda con un delta aplicado sobre una imagen que ya no vale.
        let mut conservado = Metadatos::default();
        let descartado = Metadatos {
            full_refresh: true,
            ..Metadatos::default()
        };

        conservado.absorber_descartado(&descartado);

        assert!(conservado.full_refresh);
        assert!(conservado.hay_cambios());
    }

    #[test]
    fn los_descartes_encadenados_se_cuentan_y_acumulan() {
        // Tres frames descartados seguidos: el cuarto tiene que llevar los rectangulos de
        // los tres y saber cuantos fueron.
        let mut vivo = con_sucios(vec![rect(0, 0, 10, 10)]);

        for indice in 1..4 {
            let mut siguiente = con_sucios(vec![rect(indice * 10, 0, indice * 10 + 5, 5)]);
            siguiente.absorber_descartado(&vivo);
            vivo = siguiente;
        }

        assert_eq!(vivo.descartados, 3);
        assert_eq!(vivo.dirty.len(), 4, "faltan rectangulos de la cadena");
    }

    #[test]
    fn las_presentaciones_fusionadas_se_suman() {
        let mut conservado = Metadatos {
            accumulated_frames: 2,
            ..Metadatos::default()
        };
        conservado.absorber_descartado(&Metadatos {
            accumulated_frames: 3,
            ..Metadatos::default()
        });

        assert_eq!(conservado.accumulated_frames, 5);
    }

    #[test]
    fn una_racha_larga_queda_acotada_y_sigue_cubriendolo_todo() {
        // Dos propiedades a la vez, y las dos importan:
        //
        // - la lista **no crece sin limite** por muchos descartes encadenados que haya;
        // - lo que queda **sigue cubriendo** todo lo que cambio desde el ultimo frame
        //   codificado, porque colapsar es perder precision, no perder region.
        //
        // No se exige que quede en un solo rectangulo: tras colapsar, la lista vuelve a
        // crecer de una en una hasta el siguiente colapso. Lo que se exige es la cota.
        let ultimo = MAX_RECTANGULOS as i32 + 20;
        let mut vivo = con_sucios(vec![rect(0, 0, 1, 1)]);

        for indice in 1..ultimo {
            let mut siguiente = con_sucios(vec![rect(indice, indice, indice + 1, indice + 1)]);
            siguiente.absorber_descartado(&vivo);
            vivo = siguiente;
        }

        assert!(
            vivo.dirty.len() <= MAX_RECTANGULOS,
            "la lista crecio hasta {} rectangulos: en una racha de descartes acabaria \
             costando mas de recorrer que lo que ahorra",
            vivo.dirty.len()
        );

        let cobertura = super::envolvente(&vivo.dirty).expect("hay rectangulos");
        assert_eq!(
            (cobertura.left, cobertura.top),
            (0, 0),
            "se perdio el principio de la racha: esa zona se quedaria sin repintar"
        );
        assert_eq!(
            (cobertura.right, cobertura.bottom),
            (ultimo, ultimo),
            "se perdio el final de la racha"
        );
    }

    #[test]
    fn sin_rectangulos_ni_refresco_no_hay_nada_que_codificar() {
        assert!(!Metadatos::default().hay_cambios());
        assert!(
            !con_sucios(vec![rect(5, 5, 5, 5)]).hay_cambios(),
            "un rectangulo vacio no es un cambio"
        );
        assert!(con_sucios(vec![rect(0, 0, 1, 1)]).hay_cambios());
    }
}

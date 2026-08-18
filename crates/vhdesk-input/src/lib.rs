//! Entrada de teclado y raton: **capturarla** en el viewer e **inyectarla** en el host.
//!
//! Las dos direcciones viven juntas porque comparten la tabla de traduccion de teclas
//! ([`scancodes`]) y son inversas exactas la una de la otra. Separarlas en dos crates
//! duplicaria esa tabla, y una tabla duplicada que discrepa produce teclas que escriben
//! otra cosa.
//!
//! | direccion | quien la usa | trait o funcion | Windows |
//! |---|---|---|---|
//! | inyectar | host | [`InputInjector`] | `SendInput` |
//! | capturar | viewer | [`captura_win32::hook_de_mensajes`] | Raw Input |
//!
//! La inyeccion esconde detras de `#[cfg]` las implementaciones por sistema: `SendInput` en
//! Windows, y `uinput` y `CGEvent` en la fase 8. La captura tiene hoy **solo la de
//! Windows**, porque su forma no es la de un trait: depende de como cada sistema de
//! ventanas entregue los eventos. FASE 8: el equivalente en X11 es `XI2` con
//! `XISelectEvents` sobre la ventana con foco, y en macOS `NSEvent` con monitores locales.
//!
//! Este crate hace FFI, asi que puede contener `unsafe` con `// SAFETY:`.
//!
//! # Contrato de coordenadas
//!
//! [`InputInjector::mouse_move_absolute`] recibe **pixeles fisicos del escritorio virtual
//! del host**, con su origen, que puede ser negativo si hay un monitor a la izquierda del
//! principal.
//!
//! **No recibe coordenadas de la ventana del viewer.** Traducir desde el pixel donde el
//! usuario tiene el raton en su ventana, teniendo en cuenta el escalado, el tamano de la
//! ventana y el monitor remoto que se este viendo, es responsabilidad del bloque E. Aqui
//! entrarian coordenadas ya traducidas.
//!
//! # Limitacion conocida: los atajos que Windows se queda por el camino
//!
//! El viewer captura con Raw Input **sin** `RIDEV_NOLEGACY`, asi que Windows sigue
//! procesando localmente los mensajes de teclado normales. Consecuencia: algunos atajos
//! actuan en la maquina **local** ademas de viajar al host. En la fase 1 se asume y se
//! documenta, en vez de fingir que funciona:
//!
//! | atajo | que pasa hoy |
//! |---|---|
//! | **Alt+Tab** | viaja al host **y** cambia de aplicacion aqui. Al perder el foco sale un `ReleaseAll`, asi que el Alt no queda hundido al otro lado, pero el remoto ve un Alt+Tab a medias |
//! | **Tecla Windows** | igual: viaja, y ademas abre el menu de inicio local |
//! | **Ctrl+Alt+Supr** | **no se captura nunca**, ni con Raw Input ni con un hook. Es la secuencia de atencion segura y ningun proceso de usuario puede verla |
//!
//! Capturar el teclado entero exigiria `RIDEV_NOLEGACY`, que dejaria sin teclado a la
//! propia interfaz del viewer. Ctrl+Alt+Supr no lo arregla ni eso: para enviarlo al host
//! hara falta un comando explicito del protocolo. Las dos cosas son de fases posteriores.
//!
//! # Limitacion conocida: la distribucion de teclado que manda es la del host
//!
//! El protocolo lleva **scancodes**, o sea teclas fisicas, no caracteres. Eso es lo
//! correcto para atajos y para juegos: `Ctrl+Z` es la misma posicion fisica en cualquier
//! teclado, y `WASD` sigue siendo un cuadrado.
//!
//! Y es lo **equivocado para escribir texto**: si el viewer tiene el teclado en espanol y
//! el host en ingles, pulsar la arroba produce otra cosa, porque la tecla fisica se
//! interpreta con el mapa de teclado del host.
//!
//! FASE 5: la salida es `KEYEVENTF_UNICODE`, que inyecta un caracter concreto sin depender
//! de la distribucion. Lo habitual en productos serios es ofrecer **los dos modos**:
//! scancode para atajos y juegos, unicode para texto. Eso implica una **variante nueva de
//! `InputEvent` en el protocolo** que lleve un `char` en lugar de un scancode; queda
//! escrito aqui para que quien la anada sepa por que existe y no la confunda con un
//! duplicado de la que ya hay.
//!
//! # Teclas pegadas
//!
//! Si el viewer se cierra, pierde el foco o se cae la conexion con una tecla pulsada, el
//! host se queda con esa tecla hundida para siempre. Con Ctrl o Alt la maquina remota queda
//! inservible y el sintoma aparece **despues** de que la sesion terminara, asi que nadie lo
//! relaciona con la causa.
//!
//! Por eso el injector lleva registro de lo que hunde y expone
//! [`InputInjector::liberar_todo`]. Los tres disparadores son cierre de sesion, error de
//! conexion y **perdida de foco del viewer**; este ultimo el host no puede detectarlo, asi
//! que el viewer lo anuncia con `ReleaseAll` por el canal de input.

#![deny(missing_docs)]

pub mod captura;
pub mod coords;
pub mod error;
pub mod estado;
pub mod scancodes;

#[cfg(windows)]
pub mod captura_win32;

#[cfg(windows)]
mod win32;

#[cfg(not(windows))]
mod stub;

use vhdesk_proto::MouseButton;

pub use crate::captura::{MotivoDescarte, TeclaCapturada, TeclaCruda, decodificar};
pub use crate::coords::{EscritorioVirtual, MonitorFisico, Normalizada, a_pixeles, normalizar};
pub use crate::error::InputError;
pub use crate::estado::{Liberacion, RegistroPulsaciones};
pub use crate::scancodes::{TeclaSet1, hid_a_set1, set1_a_hid};

#[cfg(windows)]
pub use crate::captura_win32::{hook_de_mensajes, registrar_teclado};

/// Inyecta eventos de raton y teclado en la maquina local.
pub trait InputInjector {
    /// Mueve el puntero a una posicion absoluta del escritorio virtual.
    ///
    /// Las coordenadas son pixeles fisicos; ver el contrato en la documentacion del crate.
    /// Los puntos fuera del escritorio se recortan al borde en lugar de rechazarse.
    ///
    /// # Errores
    ///
    /// Devuelve [`InputError::Bloqueado`] si el sistema no acepto el evento.
    fn mouse_move_absolute(&mut self, x: i32, y: i32) -> Result<(), InputError>;

    /// Pulsa o suelta un boton del raton.
    ///
    /// # Errores
    ///
    /// Devuelve [`InputError::Bloqueado`] si el sistema no acepto el evento.
    fn mouse_button(&mut self, button: MouseButton, pressed: bool) -> Result<(), InputError>;

    /// Gira la rueda.
    ///
    /// Los valores son **muescas**, no unidades crudas: una muesca completa es `1.0`. Se
    /// admiten fracciones porque los ratones de precision las producen. Positivo es hacia
    /// arriba en vertical y hacia la derecha en horizontal.
    ///
    /// # Errores
    ///
    /// Devuelve [`InputError::Bloqueado`] si el sistema no acepto el evento.
    fn mouse_scroll(&mut self, muescas_x: f32, muescas_y: f32) -> Result<(), InputError>;

    /// Pulsa o suelta una tecla, identificada por su usage ID de USB HID.
    ///
    /// # Errores
    ///
    /// Devuelve [`InputError::TeclaNoSoportada`] si la tecla no tiene equivalente en esta
    /// plataforma, y [`InputError::Bloqueado`] si el sistema no acepto el evento.
    fn key(&mut self, hid: u32, pressed: bool) -> Result<(), InputError>;

    /// Suelta todo lo que este hundido.
    ///
    /// Ver la seccion de teclas pegadas en la documentacion del crate: **hay que llamarlo**
    /// al cerrar sesion, al perder el foco y ante error de conexion.
    ///
    /// # Errores
    ///
    /// Devuelve [`InputError::Bloqueado`] si el sistema no acepto los eventos. Aunque
    /// falle, el registro queda vacio: reintentar con el mismo estado no arreglaria nada y
    /// dejaria el registro creciendo.
    fn liberar_todo(&mut self) -> Result<(), InputError>;
}

/// Crea el injector de esta plataforma.
///
/// El objeto devuelto es `Send`: el host recibe el input en una tarea asincrona, que migra
/// entre hilos del runtime, asi que un injector atado a un hilo concreto no serviria. Las
/// tres implementaciones lo cumplen sin esfuerzo porque ninguna guarda estado del sistema
/// operativo: `SendInput`, `uinput` y `CGEvent` reciben todo lo que necesitan en cada
/// llamada. **No es `Sync`**, y no debe serlo: dos hilos inyectando a la vez desordenarian
/// las pulsaciones y corromperian el registro de teclas hundidas.
///
/// # Errores
///
/// Devuelve [`InputError::UnsupportedPlatform`] en las plataformas sin implementacion.
pub fn open_injector() -> Result<Box<dyn InputInjector + Send>, InputError> {
    #[cfg(windows)]
    {
        Ok(Box::new(win32::SendInputInjector::new()?))
    }
    #[cfg(not(windows))]
    {
        stub::open_injector()
    }
}

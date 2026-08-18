//! Captura de teclado en Windows con Raw Input.
//!
//! Aqui esta todo lo que habla con el sistema; la decision de que tecla es cada evento vive
//! en [`crate::captura`], que es puro y se testea en CI.
//!
//! # Como llegan los mensajes hasta aqui
//!
//! `WM_INPUT` se entrega al procedimiento de ventana, y la ventana la crea winit por
//! debajo de eframe, asi que no tenemos un `WndProc` propio donde interceptarlo. Lo que si
//! hay es `EventLoopBuilderExtWindows::with_msg_hook`, que se ejecuta **antes** de
//! despachar cada mensaje. [`hook_de_mensajes`] construye ese enganche.
//!
//! El hook **no se traga ningun mensaje**: mira si es `WM_INPUT` y sale, y devuelve siempre
//! `false`, que es lo que winit interpreta como "sigue despachando normalmente". Tragarse un
//! mensaje aqui romperia la ventana de formas dificiles de atribuir, porque el sintoma
//! aparece en winit y la causa esta en este archivo.
//!
//! # El puntero crudo no sale de este crate
//!
//! `with_msg_hook` entrega un `*const c_void`. `vhdesk-viewer` tiene
//! `#![forbid(unsafe_code)]` y no puede desreferenciarlo, asi que este modulo no expone una
//! funcion que reciba el puntero: expone el **cierre entero ya construido**, que el viewer
//! le pasa a winit sin tocar su contenido. El `unsafe` se queda donde puede justificarse.

use core::ffi::c_void;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTDEVICE_FLAGS, RAWINPUTHEADER,
    RID_INPUT, RIM_TYPEKEYBOARD, RegisterRawInputDevices,
};
use windows::Win32::UI::WindowsAndMessaging::{MSG, WM_INPUT};

use crate::captura::{MotivoDescarte, TeclaCapturada, TeclaCruda, decodificar};
use crate::error::InputError;

/// Pagina de usos "Generic Desktop Controls" de HID.
const USAGE_PAGE_GENERIC: u16 = 0x01;
/// Uso "Keyboard" dentro de esa pagina.
const USAGE_KEYBOARD: u16 = 0x06;

/// Registra el teclado del sistema como fuente de Raw Input para este proceso.
///
/// # Los dos parametros que son decisiones y no detalles
///
/// - **`dwFlags` vale cero.** En particular **sin `RIDEV_INPUTSINK`**, que es lo que haria
///   llegar pulsaciones aunque el foco estuviera en otra aplicacion. Sin esa marca, el
///   ambito de la captura es exactamente el que debe ser: nuestra ventana mientras tiene el
///   foco. Ver el invariante de seguridad en [`crate::captura`].
/// - **`hwndTarget` es nulo**, que Windows interpreta como "la ventana que tenga el foco de
///   teclado". Asi no hace falta sacarle el `HWND` a eframe ni sincronizar con la creacion
///   de la ventana.
///
/// Tampoco se usa `RIDEV_NOLEGACY`: suprimiria los mensajes de teclado normales, y con
/// ellos el teclado de la propia interfaz del viewer. El precio es que Windows sigue
/// atendiendo localmente Alt+Tab y la tecla Windows; ver la seccion de atajos locales en
/// [`crate::captura`].
///
/// El registro dura lo que dure el proceso. No se deshace al terminar porque no hay nada
/// que limpiar: Windows lo suelta al cerrarse el proceso.
///
/// # Errores
///
/// Devuelve [`InputError::RegistroDeCaptura`] si Windows rechaza el registro.
pub fn registrar_teclado() -> Result<(), InputError> {
    let dispositivos = [RAWINPUTDEVICE {
        usUsagePage: USAGE_PAGE_GENERIC,
        usUsage: USAGE_KEYBOARD,
        dwFlags: RAWINPUTDEVICE_FLAGS(0),
        hwndTarget: HWND::default(),
    }];

    let tamano = u32::try_from(size_of::<RAWINPUTDEVICE>()).map_err(|_| {
        InputError::RegistroDeCaptura("tamano de RAWINPUTDEVICE absurdo".to_owned())
    })?;

    // SAFETY: `dispositivos` es un slice vivo de `RAWINPUTDEVICE` correctamente
    // inicializados, y `tamano` es el tamano real de un elemento, que es lo que la API pide
    // para poder validar la version de la estructura.
    unsafe { RegisterRawInputDevices(&dispositivos, tamano) }
        .map_err(|error| InputError::RegistroDeCaptura(error.to_string()))
}

/// Construye el enganche de mensajes que hay que pasarle a winit.
///
/// `al_recibir` se invoca **en el hilo del bucle de eventos**, una vez por tecla
/// reconocida. Debe ser barato: cualquier cosa que bloquee aqui bloquea la ventana entera.
/// Lo esperable es empujar la tecla a una cola y volver.
///
/// El cierre devuelto siempre responde `false`, o sea "winit, despacha este mensaje como si
/// yo no existiera".
pub fn hook_de_mensajes(
    mut al_recibir: impl FnMut(TeclaCapturada) + 'static,
) -> impl FnMut(*const c_void) -> bool + 'static {
    move |mensaje: *const c_void| {
        if let Some(cruda) = leer_teclado(mensaje) {
            match decodificar(&cruda) {
                Ok(tecla) => al_recibir(tecla),
                // Es el unico descarte que senala un hueco nuestro: una tecla real que la
                // tabla no sabe traducir. Los demas son rarezas normales de Windows y
                // llenarian el log en cada Impr Pant.
                Err(MotivoDescarte::Desconocida) => tracing::warn!(
                    make_code = format_args!("0x{:02x}", cruda.make_code),
                    flags = format_args!("0x{:02x}", cruda.flags),
                    "tecla sin equivalente en la tabla: no se envia al host"
                ),
                Err(motivo) => tracing::trace!(?motivo, "evento de Raw Input descartado"),
            }
        }

        // Siempre. Este hook observa, no consume.
        false
    }
}

/// Extrae los campos del teclado de un mensaje, o `None` si el mensaje no nos incumbe.
///
/// Se separa del cierre para que el `unsafe` quepa de un vistazo junto a su justificacion.
fn leer_teclado(mensaje: *const c_void) -> Option<TeclaCruda> {
    // SAFETY: winit documenta `with_msg_hook` como "se ejecuta antes de despachar un
    // mensaje" y pasa el puntero al `MSG` que acaba de sacar de la cola. Es valido y esta
    // alineado durante toda la llamada, y aqui solo se lee.
    let mensaje = unsafe { mensaje.cast::<MSG>().as_ref()? };

    // La comprobacion mas barata primero: por este hook pasan **todos** los mensajes de la
    // ventana, y la inmensa mayoria no son nuestros.
    if mensaje.message != WM_INPUT {
        return None;
    }

    let mut datos = RAWINPUT::default();
    let mut tamano = u32::try_from(size_of::<RAWINPUT>()).ok()?;
    let cabecera = u32::try_from(size_of::<RAWINPUTHEADER>()).ok()?;

    // SAFETY: en un `WM_INPUT`, `lParam` es el `HRAWINPUT` que identifica los datos, tal y
    // como lo documenta Windows. `datos` es un `RAWINPUT` valido y con espacio propio, y
    // `tamano` dice cuanto cabe, asi que la API no puede escribir mas alla.
    let escritos = unsafe {
        GetRawInputData(
            HRAWINPUT(mensaje.lParam.0 as *mut c_void),
            RID_INPUT,
            Some((&raw mut datos).cast::<c_void>()),
            &raw mut tamano,
            cabecera,
        )
    };

    // Devuelve `-1` como `u32` ante cualquier fallo, y el caso realista es un dispositivo
    // HID cuyos datos no caben en un `RAWINPUT`. No es un teclado, asi que no nos importa.
    if escritos == u32::MAX {
        return None;
    }

    if datos.header.dwType != RIM_TYPEKEYBOARD.0 {
        return None;
    }

    // SAFETY: la union se lee como teclado solo despues de comprobar que `dwType` dice que
    // eso es lo que contiene, que es justamente el discriminante que Windows rellena.
    let teclado = unsafe { datos.data.keyboard };

    Some(TeclaCruda {
        make_code: teclado.MakeCode,
        flags: teclado.Flags,
        vkey: teclado.VKey,
    })
}

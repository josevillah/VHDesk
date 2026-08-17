//! Inyeccion de entrada en Windows con `SendInput`.
//!
//! # Lo que esto NO puede hacer
//!
//! `SendInput` inyecta en la sesion del usuario que ejecuta el proceso, y Windows lo
//! bloquea en dos situaciones que el usuario final vivira como "a veces no responde":
//!
//! - **Ventanas de mayor nivel de integridad** (cualquier cosa lanzada como
//!   administrador). Es UIPI, y `SendInput` no devuelve error: devuelve un numero de
//!   eventos insertados menor del pedido. Por eso se comprueba el valor de retorno.
//! - **El escritorio seguro**: el prompt de UAC y la pantalla de bloqueo viven en otro
//!   escritorio al que un proceso de sesion de usuario no llega.
//!
//! La solucion a las dos es la misma y es de la **fase 6**: un servicio que corra como
//! SYSTEM y se ate al escritorio de entrada activo.

use vhdesk_proto::MouseButton;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSE_EVENT_FLAGS,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
    MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use crate::InputInjector;
use crate::coords::{EscritorioVirtual, normalizar};
use crate::error::InputError;
use crate::estado::{Liberacion, RegistroPulsaciones};
use crate::scancodes::{TeclaSet1, hid_a_set1};

/// Unidades de rueda que equivalen a una muesca.
///
/// Windows lo define como `WHEEL_DELTA`. Los ratones de precision mandan fracciones de
/// muesca, y por eso la API de este crate recibe muescas en coma flotante en lugar de
/// unidades crudas: confundir ambas cosas da desplazamientos absurdos de 120 veces.
const UNIDADES_POR_MUESCA: f32 = 120.0;

/// Valor de `mouseData` para el cuarto boton del raton.
const XBUTTON1: i32 = 0x0001;
/// Valor de `mouseData` para el quinto boton.
const XBUTTON2: i32 = 0x0002;

/// Injector de entrada basado en `SendInput`.
pub struct SendInputInjector {
    registro: RegistroPulsaciones,
}

impl SendInputInjector {
    /// Crea un injector.
    ///
    /// # Errores
    ///
    /// Devuelve [`InputError::EscritorioDesconocido`] si el sistema no reporta un
    /// escritorio virtual con dimensiones utiles, que es lo que pasa en una sesion sin
    /// escritorio.
    pub fn new() -> Result<Self, InputError> {
        // Se comprueba al construir y no en el primer movimiento: fallar al abrir la
        // sesion es mucho mas facil de diagnosticar que fallar al mover el raton.
        let escritorio = escritorio_virtual()?;
        tracing::debug!(
            x = escritorio.x,
            y = escritorio.y,
            ancho = escritorio.ancho,
            alto = escritorio.alto,
            "escritorio virtual"
        );

        Ok(Self {
            registro: RegistroPulsaciones::nuevo(),
        })
    }
}

/// Consulta la geometria del escritorio virtual.
///
/// Se consulta en cada movimiento en lugar de cachearla: enchufar o desenchufar un monitor
/// la cambia, y una cache obsoleta manda el raton al sitio equivocado sin dar ningun error.
/// La llamada cuesta unos cientos de nanosegundos frente a un evento de red por movimiento.
fn escritorio_virtual() -> Result<EscritorioVirtual, InputError> {
    // SAFETY: `GetSystemMetrics` solo lee un valor del sistema a partir de un indice
    // constante; no recibe punteros ni asigna nada.
    let (x, y, ancho, alto) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };

    if ancho <= 0 || alto <= 0 {
        return Err(InputError::EscritorioDesconocido);
    }

    Ok(EscritorioVirtual { x, y, ancho, alto })
}

fn input_raton(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, datos: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: datos as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn input_tecla(tecla: TeclaSet1, pulsada: bool) -> INPUT {
    let mut flags: KEYBD_EVENT_FLAGS = KEYEVENTF_SCANCODE;
    if tecla.extendida {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !pulsada {
        flags |= KEYEVENTF_KEYUP;
    }

    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                // Cero porque con KEYEVENTF_SCANCODE Windows ignora el codigo virtual y
                // usa `wScan`. Es justo lo que queremos: la tecla fisica, no el caracter.
                wVk: VIRTUAL_KEY(0),
                wScan: tecla.scancode,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Envia un lote de eventos de forma atomica respecto a otros hilos.
///
/// Que sea un lote importa: `SendInput` garantiza que nada se intercala entre los eventos
/// de una misma llamada. Soltar Ctrl y Alt en dos llamadas dejaria una ventana en la que
/// otro proceso puede meter un evento, y ahi es donde aparecen los atajos fantasma.
fn enviar(eventos: &[INPUT]) -> Result<(), InputError> {
    if eventos.is_empty() {
        return Ok(());
    }

    let esperados = eventos.len() as u32;

    // SAFETY: se pasa un slice vivo y el tamano de `INPUT` que la API espera para poder
    // recorrerlo. `SendInput` no conserva el puntero despues de volver.
    let insertados = unsafe { SendInput(eventos, size_of::<INPUT>() as i32) };

    if insertados != esperados {
        tracing::warn!(
            insertados,
            esperados,
            "el sistema bloqueo parte de la entrada sintetica; casi siempre es UIPI, con una \
             ventana elevada en primer plano"
        );
        return Err(InputError::Bloqueado {
            insertados,
            esperados,
        });
    }

    Ok(())
}

/// Banderas de pulsar y soltar un boton, y el valor de `mouseData` que necesita.
const fn banderas_boton(boton: MouseButton, pulsado: bool) -> (MOUSE_EVENT_FLAGS, i32) {
    match boton {
        MouseButton::Left if pulsado => (MOUSEEVENTF_LEFTDOWN, 0),
        MouseButton::Left => (MOUSEEVENTF_LEFTUP, 0),
        MouseButton::Middle if pulsado => (MOUSEEVENTF_MIDDLEDOWN, 0),
        MouseButton::Middle => (MOUSEEVENTF_MIDDLEUP, 0),
        MouseButton::Right if pulsado => (MOUSEEVENTF_RIGHTDOWN, 0),
        MouseButton::Right => (MOUSEEVENTF_RIGHTUP, 0),
        // Los dos botones laterales comparten bandera y se distinguen por `mouseData`.
        MouseButton::Back if pulsado => (MOUSEEVENTF_XDOWN, XBUTTON1),
        MouseButton::Back => (MOUSEEVENTF_XUP, XBUTTON1),
        MouseButton::Forward if pulsado => (MOUSEEVENTF_XDOWN, XBUTTON2),
        MouseButton::Forward => (MOUSEEVENTF_XUP, XBUTTON2),
        // `MouseButton` es `#[non_exhaustive]`: una variante nueva no debe inyectar un
        // boton cualquiera.
        _ => (MOUSE_EVENT_FLAGS(0), 0),
    }
}

impl InputInjector for SendInputInjector {
    fn mouse_move_absolute(&mut self, x: i32, y: i32) -> Result<(), InputError> {
        let escritorio = escritorio_virtual()?;
        let punto = normalizar(x, y, &escritorio);

        // MOUSEEVENTF_MOVE es imprescindible: sin el, las otras dos banderas describen
        // "un movimiento absoluto sobre el escritorio virtual" que no llega a ocurrir, y
        // la llamada devuelve exito sin mover nada. Es de los fallos mas desconcertantes
        // de esta API porque no deja ninguna pista.
        let flags = MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK;

        enviar(&[input_raton(flags, punto.x, punto.y, 0)])
    }

    fn mouse_button(&mut self, button: MouseButton, pressed: bool) -> Result<(), InputError> {
        let (flags, datos) = banderas_boton(button, pressed);
        if flags == MOUSE_EVENT_FLAGS(0) {
            return Ok(());
        }

        enviar(&[input_raton(flags, 0, 0, datos)])?;
        // Se anota despues de que el envio saliera bien: si el sistema lo bloqueo, el
        // boton no esta hundido y anotarlo generaria una liberacion fantasma.
        self.registro.anotar_boton(button, pressed);
        Ok(())
    }

    fn mouse_scroll(&mut self, muescas_x: f32, muescas_y: f32) -> Result<(), InputError> {
        let mut eventos = Vec::with_capacity(2);

        let unidades_y = (muescas_y * UNIDADES_POR_MUESCA).round() as i32;
        if unidades_y != 0 {
            eventos.push(input_raton(MOUSEEVENTF_WHEEL, 0, 0, unidades_y));
        }

        let unidades_x = (muescas_x * UNIDADES_POR_MUESCA).round() as i32;
        if unidades_x != 0 {
            eventos.push(input_raton(MOUSEEVENTF_HWHEEL, 0, 0, unidades_x));
        }

        enviar(&eventos)
    }

    fn key(&mut self, hid: u32, pressed: bool) -> Result<(), InputError> {
        let tecla = hid_a_set1(hid)?;

        enviar(&[input_tecla(tecla, pressed)])?;
        self.registro.anotar_tecla(hid, pressed);
        Ok(())
    }

    fn liberar_todo(&mut self) -> Result<(), InputError> {
        let acciones = self.registro.liberar_todo();
        if acciones.is_empty() {
            return Ok(());
        }

        tracing::info!(cuantas = acciones.len(), "liberando entrada hundida");

        // Todo en un solo lote: si se soltara en llamadas sueltas, entre una y otra podria
        // colarse un evento con los modificadores todavia hundidos.
        let eventos: Vec<INPUT> = acciones
            .into_iter()
            .filter_map(|accion| match accion {
                Liberacion::Tecla(hid) => hid_a_set1(hid).ok().map(|t| input_tecla(t, false)),
                Liberacion::Boton(boton) => {
                    let (flags, datos) = banderas_boton(boton, false);
                    (flags != MOUSE_EVENT_FLAGS(0)).then(|| input_raton(flags, 0, 0, datos))
                }
            })
            .collect();

        enviar(&eventos)
    }
}

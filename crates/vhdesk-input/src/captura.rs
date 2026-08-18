//! Captura de teclado en el viewer: decodificacion de lo que entrega Raw Input.
//!
//! Este modulo es **puro**. No toca el sistema operativo: recibe los campos de un
//! `RAWKEYBOARD` ya leidos y decide que tecla es, o por que se descarta. La parte que habla
//! con Windows vive en `captura_win32`, y la separacion es lo que permite testear en CI
//! —donde no hay ventana ni foco— los casos raros que en una prueba manual solo aparecen si
//! se te ocurre pulsar justo esa tecla.
//!
//! # Por que el viewer no usa los eventos de teclado de egui
//!
//! Se probo y no sirve, por tres motivos que se acumulan:
//!
//! - `egui::Key` **no tiene variantes para los modificadores**, y el mapeo de winit no
//!   traduce `ControlLeft`. Pulsar Ctrl a secas no emite ningun evento.
//! - `egui::Modifiers` no distingue izquierdo de derecho, asi que AltGr llegaria al host
//!   como Alt izquierdo y dejaria de escribir la tercera fila de un teclado no-US.
//! - `egui::Key` es con perdida: `Enter` y el Intro del numerico son la misma variante, y
//!   lo mismo `Slash` con la division del numerico.
//!
//! Raw Input entrega el scancode fisico del conjunto 1 mas la marca de extendida, que es
//! exactamente el par que [`crate::scancodes`] sabe traducir sin perder informacion.
//!
//! # Invariante de seguridad: sin `RIDEV_INPUTSINK`, y nunca un hook global
//!
//! El registro se hace **sin** `RIDEV_INPUTSINK`, asi que solo llegan pulsaciones mientras
//! nuestra ventana tiene el foco. Queda **prohibido** usar un hook global de bajo nivel
//! (`WH_KEYBOARD_LL` o equivalente), que capturaria el teclado de todo el sistema aunque el
//! foco este en otra aplicacion. Esa es la forma de un keylogger y choca de frente con el
//! invariante de no implementar capacidades propias de un RAT.

use crate::scancodes::{TeclaSet1, set1_a_hid};

/// La pulsacion es una liberacion, no una pulsacion.
///
/// En Raw Input el `MakeCode` **no** lleva el bit de ruptura: la liberacion viaja aqui.
pub const RI_KEY_BREAK: u16 = 0x01;
/// El scancode va precedido de `E0`, o sea que es una tecla extendida.
pub const RI_KEY_E0: u16 = 0x02;
/// El scancode va precedido de `E1`. Solo lo usa Pausa/Interrumpir.
pub const RI_KEY_E1: u16 = 0x04;

// Las tres constantes anteriores **no estan proyectadas por el crate `windows`**, aunque si
// lo estan `RAWKEYBOARD` y el resto de la API. Se declaran aqui con su valor de ABI, que es
// estable desde Windows XP, igual que se hizo con `MONITORINFOF_PRIMARY` en la captura de
// pantalla. Viven en el modulo puro a proposito: asi los tests las usan sin depender de
// Windows.

/// Codigo de tecla virtual que Windows usa como relleno.
///
/// Aparece en las secuencias de varios scancodes: el evento no corresponde a ninguna tecla
/// y su unico proposito es acompanar al que si la lleva.
const VKEY_RELLENO: u16 = 0xFF;

/// Scancode del Mayus izquierdo. Con `E0` delante no es una tecla: es el shift falso.
const SHIFT_IZQUIERDO: u16 = 0x2A;
/// Scancode del Mayus derecho, con la misma salvedad.
const SHIFT_DERECHO: u16 = 0x36;

/// Una tecla fisica ya identificada, lista para viajar por el cable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeclaCapturada {
    /// Usage ID de la pagina de teclado de USB HID.
    pub hid: u32,
    /// `true` al pulsar, `false` al soltar.
    pub pulsada: bool,
}

/// Los campos de un `RAWKEYBOARD` que hacen falta para identificar la tecla.
///
/// Es una copia deliberada y no un alias del tipo de Windows: mantiene este modulo puro y
/// permite construir en un test los casos raros que de otro modo habria que provocar
/// pulsando la tecla correcta en la maquina correcta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeclaCruda {
    /// Scancode del conjunto 1, **sin** el prefijo `E0`/`E1` y sin el bit de ruptura.
    pub make_code: u16,
    /// Combinacion de [`RI_KEY_BREAK`], [`RI_KEY_E0`] y [`RI_KEY_E1`].
    pub flags: u16,
    /// Codigo de tecla virtual. Solo se mira para detectar el relleno.
    pub vkey: u16,
}

/// Por que un evento de Raw Input no produce ninguna tecla.
///
/// Se distinguen para que quien llama pueda elegir el nivel de traza: una tecla
/// desconocida merece un `warn!` porque significa que falta una entrada en la tabla; los
/// otros tres son funcionamiento normal de Windows y llenarian el log de ruido.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MotivoDescarte {
    /// Prefijo `E1`: la secuencia de Pausa/Interrumpir.
    ///
    /// En el conjunto 1, Pausa es `E1 1D 45 E1 9D C5`, o sea varios scancodes para una
    /// sola tecla. No encaja en el modelo de un scancode por evento ni de este lado ni del
    /// lado de la inyeccion, donde ya estaba documentada como limitacion conocida.
    SecuenciaE1,

    /// El shift falso que Windows inyecta delante de algunas teclas extendidas.
    ///
    /// Impr Pant llega como `E0 2A` seguido de `E0 37`, y las teclas del numerico producen
    /// algo parecido segun el estado de Bloq Num. Ese primer evento no es ninguna tecla que
    /// el usuario haya tocado: reenviarlo pondria un Mayus fantasma en la maquina remota.
    ShiftFalso,

    /// Evento de relleno de una secuencia de varios scancodes.
    Relleno,

    /// La tecla no esta en la tabla de traduccion.
    ///
    /// Es el unico motivo que senala un hueco nuestro y no una rareza de Windows.
    Desconocida,
}

/// Identifica la tecla que hay detras de un evento de Raw Input.
///
/// # Errores
///
/// Devuelve [`MotivoDescarte`] cuando el evento no corresponde a ninguna tecla enviable.
/// **Descartar es lo correcto**: aproximar la tecla al vecino mas parecido escribiria en la
/// maquina remota algo que nadie pidio.
pub fn decodificar(cruda: &TeclaCruda) -> Result<TeclaCapturada, MotivoDescarte> {
    // El orden de estas comprobaciones importa. El relleno y la secuencia E1 llevan
    // `make_code` que por si solos parecerian teclas legitimas, asi que se filtran antes de
    // llegar a la tabla.
    if cruda.vkey == VKEY_RELLENO {
        return Err(MotivoDescarte::Relleno);
    }
    if cruda.flags & RI_KEY_E1 != 0 {
        return Err(MotivoDescarte::SecuenciaE1);
    }
    if cruda.make_code == 0 {
        return Err(MotivoDescarte::Relleno);
    }

    let extendida = cruda.flags & RI_KEY_E0 != 0;

    // El shift falso lo rechazaria tambien la tabla, porque los dos Mayus reales son
    // `0x2A` y `0x36` **sin** marca. Se filtra explicitamente para no reportarlo como tecla
    // desconocida: cada Impr Pant emitiria un `warn!` que no senala ningun problema.
    if extendida && (cruda.make_code == SHIFT_IZQUIERDO || cruda.make_code == SHIFT_DERECHO) {
        return Err(MotivoDescarte::ShiftFalso);
    }

    let hid = set1_a_hid(TeclaSet1 {
        scancode: cruda.make_code,
        extendida,
    })
    .ok_or(MotivoDescarte::Desconocida)?;

    Ok(TeclaCapturada {
        hid,
        // Raw Input marca la liberacion en los flags; la pulsacion es la ausencia de esa
        // marca, no un valor propio (`RI_KEY_MAKE` vale 0).
        pulsada: cruda.flags & RI_KEY_BREAK == 0,
    })
}

#[cfg(test)]
mod tests {
    use super::{MotivoDescarte, RI_KEY_BREAK, RI_KEY_E0, RI_KEY_E1, TeclaCruda, decodificar};

    fn cruda(make_code: u16, flags: u16) -> TeclaCruda {
        TeclaCruda {
            make_code,
            flags,
            vkey: 0x41, // Cualquier cosa que no sea el relleno.
        }
    }

    #[test]
    fn una_pulsacion_normal_da_su_hid() {
        // 0x1E es la 'a' del conjunto 1, y 0x04 su usage ID de HID.
        let tecla = decodificar(&cruda(0x1E, 0)).expect("la 'a' deberia traducirse");
        assert_eq!(tecla.hid, 0x04);
        assert!(tecla.pulsada);
    }

    #[test]
    fn la_liberacion_viene_en_los_flags_y_no_en_el_make_code() {
        // El error natural aqui es esperar el bit de ruptura dentro del scancode, como en
        // el teclado PS/2 crudo. Raw Input ya lo ha sacado a los flags, y si alguien
        // "arregla" esto restando 0x80 al make_code, este test lo caza.
        let tecla = decodificar(&cruda(0x1E, RI_KEY_BREAK)).expect("traducir");
        assert_eq!(tecla.hid, 0x04, "sigue siendo la misma tecla");
        assert!(!tecla.pulsada, "con RI_KEY_BREAK es una liberacion");
    }

    #[test]
    fn la_marca_de_extendida_distingue_los_modificadores() {
        // El caso que de verdad se nota: sin esto AltGr llega como Alt izquierdo y deja de
        // escribir la tercera fila de un teclado no-US.
        let alt_izquierdo = decodificar(&cruda(0x38, 0)).expect("traducir");
        let alt_derecho = decodificar(&cruda(0x38, RI_KEY_E0)).expect("traducir");

        assert_eq!(alt_izquierdo.hid, 0xE2);
        assert_eq!(alt_derecho.hid, 0xE6);
    }

    #[test]
    fn el_shift_falso_de_impr_pant_se_descarta_como_tal() {
        // La secuencia real es `E0 2A` y despues `E0 37`. El primero no es ninguna tecla
        // que el usuario haya tocado.
        assert_eq!(
            decodificar(&cruda(0x2A, RI_KEY_E0)),
            Err(MotivoDescarte::ShiftFalso),
            "reenviarlo pondria un Mayus fantasma en la maquina remota"
        );
        assert_eq!(
            decodificar(&cruda(0x36, RI_KEY_E0)),
            Err(MotivoDescarte::ShiftFalso)
        );

        // Y el segundo si es Impr Pant, que en HID es 0x46.
        let impr_pant = decodificar(&cruda(0x37, RI_KEY_E0)).expect("traducir");
        assert_eq!(impr_pant.hid, 0x46);
    }

    #[test]
    fn los_mayus_de_verdad_siguen_pasando() {
        // La regla del shift falso solo aplica con la marca de extendida. Si alguien la
        // ampliara a los `0x2A`/`0x36` normales, el viewer se quedaria sin Mayus.
        assert_eq!(decodificar(&cruda(0x2A, 0)).expect("traducir").hid, 0xE1);
        assert_eq!(decodificar(&cruda(0x36, 0)).expect("traducir").hid, 0xE5);
    }

    #[test]
    fn la_secuencia_de_pausa_se_descarta_entera() {
        // `E1 1D 45`: el 0x1D con E1 parece Ctrl y el 0x45 parece Bloq Num. Si el prefijo
        // E1 no se mirara **antes** que la tabla, Pausa inyectaria un Ctrl en el host.
        for make_code in [0x1D, 0x45] {
            assert_eq!(
                decodificar(&cruda(make_code, RI_KEY_E1)),
                Err(MotivoDescarte::SecuenciaE1),
                "0x{make_code:02x} con E1 no es la tecla que su scancode aparenta"
            );
        }
    }

    #[test]
    fn el_relleno_se_descarta_mire_lo_que_mire_el_scancode() {
        let relleno = TeclaCruda {
            make_code: 0x1E,
            flags: 0,
            vkey: 0xFF,
        };
        assert_eq!(decodificar(&relleno), Err(MotivoDescarte::Relleno));

        assert_eq!(decodificar(&cruda(0, 0)), Err(MotivoDescarte::Relleno));
    }

    #[test]
    fn una_tecla_fuera_de_la_tabla_se_distingue_del_resto_de_descartes() {
        // Este es el unico motivo que senala un hueco nuestro, y por eso el viewer lo
        // registra con `warn!` mientras los otros van en `trace!`.
        assert_eq!(
            decodificar(&cruda(0x7F, 0)),
            Err(MotivoDescarte::Desconocida)
        );
    }
}

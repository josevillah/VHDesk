//! Traduccion entre scancodes USB HID y scancodes PS/2 del conjunto 1, en los dos sentidos.
//!
//! El protocolo lleva por el cable el **usage ID de la pagina de teclado de USB HID**
//! (pagina 0x07), que es el identificador neutral de una tecla fisica. `SendInput` con
//! `KEYEVENTF_SCANCODE` quiere otra cosa: scancodes **PS/2 del conjunto 1**. Son dos
//! espacios de nombres distintos y hay que traducir.
//!
//! Se traduce aqui, en el crate de plataforma, y no en el protocolo, porque Linux con
//! `uinput` y macOS con `CGEvent` tienen cada uno el suyo y tendrian que traducir igual
//! desde cualquier cosa que eligieramos. HID es la unica opcion que no privilegia a una
//! plataforma sobre las otras.
//!
//! # Por que la marca de extendida vive en esta misma tabla
//!
//! En el conjunto 1, las teclas "extendidas" se codifican con un prefijo `E0` delante del
//! mismo scancode que otra tecla distinta. `SendInput` lo expresa pasando el scancode base
//! y marcando `KEYEVENTF_EXTENDEDKEY`.
//!
//! Eso significa que **el scancode por si solo no identifica la tecla**: Ctrl izquierdo y
//! Ctrl derecho son los dos 0x1D, y Alt izquierdo y derecho son los dos 0x38; lo unico que
//! los separa es la marca. Con una lista de extendidas aparte, indexada por scancode, seria
//! literalmente imposible distinguirlos. Por eso cada entrada devuelve el par completo.
//!
//! Si la marca falta, el sintoma es sutil en vez de roto: en teclados no-US, AltGr deja de
//! producir los caracteres de la tercera fila.
//!
//! # Las dos direcciones, y por que solo hay una tabla
//!
//! El host traduce HID -> conjunto 1 para inyectar con `SendInput`; el viewer traduce
//! conjunto 1 -> HID, porque Raw Input le entrega el `MakeCode` del conjunto 1 y por el
//! cable viaja HID. Son inversas exactas la una de la otra.
//!
//! **La inversa no se transcribe: se deriva.** [`set1_a_hid`] recorre el dominio de
//! [`hid_a_set1`] y devuelve el primer HID cuya traduccion coincide. Una segunda tabla
//! escrita a mano seria un centenar de lineas de datos duplicados que pueden discrepar en
//! silencio, y el sintoma de una discrepancia es una tecla que escribe otra cosa. Derivarla
//! hace que discrepar sea imposible, y deja a los tests comprobar **propiedades** en vez de
//! la transcripcion.
//!
//! El coste es un barrido de ~110 comparaciones por tecla pulsada, que al lado de mandar un
//! mensaje por la red no se mide.

use crate::error::InputError;

/// Una tecla fisica tal y como la quiere `SendInput`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeclaSet1 {
    /// Scancode del conjunto 1, sin el prefijo `E0`.
    pub scancode: u16,
    /// Si hay que marcar `KEYEVENTF_EXTENDEDKEY`.
    pub extendida: bool,
}

const fn normal(scancode: u16) -> TeclaSet1 {
    TeclaSet1 {
        scancode,
        extendida: false,
    }
}

const fn extendida(scancode: u16) -> TeclaSet1 {
    TeclaSet1 {
        scancode,
        extendida: true,
    }
}

/// Traduce un usage ID de HID al scancode del conjunto 1.
///
/// # Errores
///
/// Devuelve [`InputError::TeclaNoSoportada`] para las teclas sin equivalente directo. Se
/// prefiere fallar de forma visible a inyectar una tecla parecida: escribir un caracter
/// que nadie pidio es peor que no escribir nada.
///
/// **Pausa/Interrumpir (HID 0x48) no esta soportada**: en el conjunto 1 no es un scancode
/// sino la secuencia `E1 1D 45 E1 9D C5`, que no encaja en el modelo de un scancode por
/// evento de esta API. Se documenta como limitacion conocida en lugar de aproximarla mal.
pub const fn hid_a_set1(hid: u32) -> Result<TeclaSet1, InputError> {
    let tecla = match hid {
        // Letras, en el orden de la tabla HID (a..z), que no es el del teclado.
        0x04 => normal(0x1E), // a
        0x05 => normal(0x30), // b
        0x06 => normal(0x2E), // c
        0x07 => normal(0x20), // d
        0x08 => normal(0x12), // e
        0x09 => normal(0x21), // f
        0x0A => normal(0x22), // g
        0x0B => normal(0x23), // h
        0x0C => normal(0x17), // i
        0x0D => normal(0x24), // j
        0x0E => normal(0x25), // k
        0x0F => normal(0x26), // l
        0x10 => normal(0x32), // m
        0x11 => normal(0x31), // n
        0x12 => normal(0x18), // o
        0x13 => normal(0x19), // p
        0x14 => normal(0x10), // q
        0x15 => normal(0x13), // r
        0x16 => normal(0x1F), // s
        0x17 => normal(0x14), // t
        0x18 => normal(0x16), // u
        0x19 => normal(0x2F), // v
        0x1A => normal(0x11), // w
        0x1B => normal(0x2D), // x
        0x1C => normal(0x15), // y
        0x1D => normal(0x2C), // z

        // Numeros de la fila superior.
        0x1E => normal(0x02), // 1
        0x1F => normal(0x03), // 2
        0x20 => normal(0x04), // 3
        0x21 => normal(0x05), // 4
        0x22 => normal(0x06), // 5
        0x23 => normal(0x07), // 6
        0x24 => normal(0x08), // 7
        0x25 => normal(0x09), // 8
        0x26 => normal(0x0A), // 9
        0x27 => normal(0x0B), // 0

        0x28 => normal(0x1C), // Enter
        0x29 => normal(0x01), // Escape
        0x2A => normal(0x0E), // Retroceso
        0x2B => normal(0x0F), // Tabulador
        0x2C => normal(0x39), // Espacio

        // Signos. Los nombres son los de la distribucion US porque asi los nombra HID; la
        // tecla fisica es la misma en cualquier distribucion, y lo que escriba depende del
        // mapa de teclado del host.
        0x2D => normal(0x0C), // -
        0x2E => normal(0x0D), // =
        0x2F => normal(0x1A), // [
        0x30 => normal(0x1B), // ]
        0x31 => normal(0x2B), // \
        0x32 => normal(0x2B), // # no-US, misma tecla fisica que la anterior
        0x33 => normal(0x27), // ;
        0x34 => normal(0x28), // '
        0x35 => normal(0x29), // `
        0x36 => normal(0x33), // ,
        0x37 => normal(0x34), // .
        0x38 => normal(0x35), // /
        0x39 => normal(0x3A), // Bloq Mayus

        // Funcion.
        0x3A => normal(0x3B), // F1
        0x3B => normal(0x3C), // F2
        0x3C => normal(0x3D), // F3
        0x3D => normal(0x3E), // F4
        0x3E => normal(0x3F), // F5
        0x3F => normal(0x40), // F6
        0x40 => normal(0x41), // F7
        0x41 => normal(0x42), // F8
        0x42 => normal(0x43), // F9
        0x43 => normal(0x44), // F10
        0x44 => normal(0x57), // F11
        0x45 => normal(0x58), // F12

        0x46 => extendida(0x37), // Impr Pant
        0x47 => normal(0x46),    // Bloq Despl
        // 0x48 Pausa: secuencia especial, ver la nota de la funcion.

        // Bloque de navegacion. Todas extendidas: comparten scancode con el teclado
        // numerico y la marca es lo unico que las distingue.
        0x49 => extendida(0x52), // Insert
        0x4A => extendida(0x47), // Inicio
        0x4B => extendida(0x49), // RePag
        0x4C => extendida(0x53), // Suprimir
        0x4D => extendida(0x4F), // Fin
        0x4E => extendida(0x51), // AvPag
        0x4F => extendida(0x4D), // Derecha
        0x50 => extendida(0x4B), // Izquierda
        0x51 => extendida(0x50), // Abajo
        0x52 => extendida(0x48), // Arriba

        // Teclado numerico.
        0x53 => normal(0x45),    // Bloq Num
        0x54 => extendida(0x35), // / del numerico: comparte scancode con la / normal
        0x55 => normal(0x37),    // *
        0x56 => normal(0x4A),    // -
        0x57 => normal(0x4E),    // +
        0x58 => extendida(0x1C), // Intro del numerico: comparte con el Enter normal
        0x59 => normal(0x4F),    // 1
        0x5A => normal(0x50),    // 2
        0x5B => normal(0x51),    // 3
        0x5C => normal(0x4B),    // 4
        0x5D => normal(0x4C),    // 5
        0x5E => normal(0x4D),    // 6
        0x5F => normal(0x47),    // 7
        0x60 => normal(0x48),    // 8
        0x61 => normal(0x49),    // 9
        0x62 => normal(0x52),    // 0
        0x63 => normal(0x53),    // .

        0x64 => normal(0x56),    // \ adicional de los teclados no-US
        0x65 => extendida(0x5D), // Menu contextual
        0x67 => normal(0x59),    // = del numerico

        // Modificadores. Aqui se ve por que la marca tiene que venir con el scancode:
        // los pares izquierdo/derecho de Ctrl y Alt comparten scancode.
        0xE0 => normal(0x1D),    // Ctrl izquierdo
        0xE1 => normal(0x2A),    // Mayus izquierdo
        0xE2 => normal(0x38),    // Alt izquierdo
        0xE3 => extendida(0x5B), // Windows izquierda
        0xE4 => extendida(0x1D), // Ctrl derecho: mismo scancode que el izquierdo
        0xE5 => normal(0x36),    // Mayus derecho
        0xE6 => extendida(0x38), // Alt derecho (AltGr): mismo scancode que el izquierdo
        0xE7 => extendida(0x5C), // Windows derecha

        _ => return Err(InputError::TeclaNoSoportada { hid }),
    };

    Ok(tecla)
}

/// Rangos de usage ID que la tabla de [`hid_a_set1`] puede reconocer.
///
/// No son "todas las teclas de HID": son el dominio que este crate traduce. Existen para
/// que [`set1_a_hid`] sepa por donde barrer sin recorrer los 2^32 valores posibles de un
/// `u32`.
const DOMINIO_HID: [(u32, u32); 2] = [(0x04, 0x67), (0xE0, 0xE7)];

/// Traduce un scancode del conjunto 1 al usage ID de HID que le corresponde.
///
/// Es la inversa de [`hid_a_set1`], y la usa el **viewer**: Raw Input entrega el `MakeCode`
/// del conjunto 1 y por el cable viaja HID.
///
/// Devuelve `None` para los scancodes que esta tabla no reconoce. Quien llama debe
/// **descartar** la tecla, no aproximarla: enviar una tecla parecida escribe en la maquina
/// remota algo que nadie pidio.
///
/// # Las dos teclas que comparten scancode
///
/// HID 0x31 (la barra invertida) y 0x32 (la almohadilla de los teclados no-US) son la misma
/// tecla fisica y la tabla las manda las dos al `0x2B` normal, asi que la inversa tiene que
/// elegir. Elige **0x31**, que es la que produce cualquier teclado real: 0x32 solo aparece
/// en descripciones de HID que distinguen variantes de serigrafia. El host las inyecta
/// identicas, asi que la eleccion no cambia lo que se escribe al otro lado.
pub fn set1_a_hid(tecla: TeclaSet1) -> Option<u32> {
    // Barrido ascendente sobre el dominio. Ascendente **importa**: es lo que hace que el
    // empate 0x31/0x32 se resuelva por 0x31, y no por el orden en que estuvieran escritas
    // las ramas del `match`.
    for (desde, hasta) in DOMINIO_HID {
        for hid in desde..=hasta {
            if let Ok(candidata) = hid_a_set1(hid) {
                if candidata == tecla {
                    return Some(hid);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{DOMINIO_HID, TeclaSet1, hid_a_set1, set1_a_hid};
    use crate::error::InputError;

    fn tecla(hid: u32) -> super::TeclaSet1 {
        hid_a_set1(hid).expect("la tecla deberia estar en la tabla")
    }

    /// El unico HID cuya ida y vuelta no es la identidad, y por que.
    ///
    /// 0x31 y 0x32 son la misma tecla fisica y la tabla las manda al mismo scancode, asi
    /// que la inversa tiene que elegir una. Se fija aqui para que la excepcion sea
    /// explicita en vez de un caso que alguien descubra fallando.
    const ALIAS: (u32, u32) = (0x32, 0x31);

    #[test]
    fn la_traduccion_de_ida_y_vuelta_devuelve_la_misma_tecla() {
        // Recorre **toda** la tabla, no una muestra: es lo unico que garantiza que las dos
        // direcciones no puedan discrepar. Una discrepancia no da error, da una tecla que
        // escribe otra cosa.
        let mut recorridas = 0;

        for (desde, hasta) in DOMINIO_HID {
            for hid in desde..=hasta {
                let Ok(tecla) = hid_a_set1(hid) else {
                    continue;
                };
                recorridas += 1;

                let vuelta = set1_a_hid(tecla).unwrap_or_else(|| {
                    panic!("HID 0x{hid:02x} traduce a {tecla:?} pero la inversa no lo reconoce")
                });

                let esperado = if hid == ALIAS.0 { ALIAS.1 } else { hid };
                assert_eq!(
                    vuelta, esperado,
                    "HID 0x{hid:02x} -> {tecla:?} -> 0x{vuelta:02x}"
                );
            }
        }

        assert!(
            recorridas > 100,
            "solo se recorrieron {recorridas} teclas: el dominio se ha quedado corto"
        );
    }

    #[test]
    fn todo_scancode_reconocido_vuelve_a_si_mismo() {
        // La direccion contraria, y la que de verdad usa el viewer: barrido de los 256
        // scancodes por los dos estados de la marca de extendida. Todo lo que la inversa
        // reconozca tiene que traducir de vuelta al **mismo par**, marca incluida.
        let mut reconocidos = 0;

        for scancode in 0..=0xFFu16 {
            for extendida in [false, true] {
                let tecla = TeclaSet1 {
                    scancode,
                    extendida,
                };
                let Some(hid) = set1_a_hid(tecla) else {
                    continue;
                };
                reconocidos += 1;

                let vuelta = hid_a_set1(hid).expect("la inversa devolvio un HID fuera de la tabla");
                assert_eq!(
                    vuelta, tecla,
                    "{tecla:?} -> HID 0x{hid:02x} -> {vuelta:?}: la marca de extendida o el                      scancode cambiaron por el camino"
                );
            }
        }

        assert!(
            reconocidos > 100,
            "solo se reconocieron {reconocidos} pares: la inversa se ha quedado corta"
        );
    }

    #[test]
    fn los_modificadores_izquierdo_y_derecho_no_se_confunden_al_volver() {
        // El caso que justifica que la marca viaje con el scancode, visto desde la inversa:
        // el mismo 0x1D tiene que dar Ctrl izquierdo o derecho segun la marca. Si esto se
        // rompe, AltGr llega al host como Alt izquierdo y deja de escribir la tercera fila.
        for (scancode, izquierdo, derecho) in [(0x1D, 0xE0, 0xE4), (0x38, 0xE2, 0xE6)] {
            assert_eq!(
                set1_a_hid(TeclaSet1 {
                    scancode,
                    extendida: false
                }),
                Some(izquierdo)
            );
            assert_eq!(
                set1_a_hid(TeclaSet1 {
                    scancode,
                    extendida: true
                }),
                Some(derecho)
            );
        }
    }

    #[test]
    fn un_scancode_que_no_existe_no_se_aproxima() {
        for tecla in [
            // El shift falso que Windows inyecta delante de Impr Pant: 0x2A **con** E0. La
            // tecla real es 0x2A sin marca, asi que este par no esta en la tabla y la
            // inversa no debe inventarselo.
            TeclaSet1 {
                scancode: 0x2A,
                extendida: true,
            },
            // La tecla Windows solo existe como extendida.
            TeclaSet1 {
                scancode: 0x5B,
                extendida: false,
            },
            TeclaSet1 {
                scancode: 0x00,
                extendida: false,
            },
            TeclaSet1 {
                scancode: 0xFF,
                extendida: true,
            },
        ] {
            assert_eq!(
                set1_a_hid(tecla),
                None,
                "{tecla:?} deberia descartarse: enviar una tecla parecida escribe en la                  maquina remota algo que nadie pidio"
            );
        }
    }

    #[test]
    fn las_teclas_de_navegacion_van_marcadas_como_extendidas() {
        // Flechas, Insert, Suprimir, Inicio, Fin, RePag, AvPag.
        for hid in [0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F, 0x50, 0x51, 0x52] {
            assert!(
                tecla(hid).extendida,
                "HID 0x{hid:02x} deberia ser extendida: sin la marca, en teclados no-US el \
                 comportamiento es sutilmente erroneo"
            );
        }
    }

    #[test]
    fn los_modificadores_derechos_comparten_scancode_con_los_izquierdos() {
        // Este es el test que justifica que la marca viva en la misma entrada: si la
        // extension se dedujera del scancode, estos pares serian indistinguibles.
        let ctrl_izq = tecla(0xE0);
        let ctrl_der = tecla(0xE4);
        assert_eq!(ctrl_izq.scancode, ctrl_der.scancode);
        assert!(!ctrl_izq.extendida && ctrl_der.extendida);

        let alt_izq = tecla(0xE2);
        let alt_der = tecla(0xE6);
        assert_eq!(alt_izq.scancode, alt_der.scancode);
        assert!(
            !alt_izq.extendida && alt_der.extendida,
            "sin esta distincion, AltGr deja de producir la tercera fila de un teclado no-US"
        );
    }

    #[test]
    fn la_division_del_numerico_es_extendida_y_el_intro_tambien() {
        let barra_numerico = tecla(0x54);
        let barra_normal = tecla(0x38);
        assert_eq!(barra_numerico.scancode, barra_normal.scancode);
        assert!(barra_numerico.extendida && !barra_normal.extendida);

        let intro_numerico = tecla(0x58);
        let enter = tecla(0x28);
        assert_eq!(intro_numerico.scancode, enter.scancode);
        assert!(intro_numerico.extendida && !enter.extendida);
    }

    #[test]
    fn las_teclas_windows_son_extendidas() {
        assert!(tecla(0xE3).extendida);
        assert!(tecla(0xE7).extendida);
    }

    #[test]
    fn las_letras_y_numeros_no_son_extendidos() {
        // 0x04..=0x27 son las letras y los numeros de la fila superior.
        for hid in 0x04..=0x27u32 {
            assert!(
                !tecla(hid).extendida,
                "HID 0x{hid:02x} no deberia ser extendida"
            );
        }
    }

    #[test]
    fn cada_tecla_del_bloque_de_navegacion_tiene_su_gemela_en_el_numerico() {
        // Los pares comparten scancode y solo se distinguen por la marca. Si esta tabla se
        // edita mal, este test lo caza.
        for (navegacion, numerico) in [
            (0x49, 0x62), // Insert / 0
            (0x4A, 0x5F), // Inicio / 7
            (0x4B, 0x61), // RePag / 9
            (0x4C, 0x63), // Suprimir / .
            (0x4D, 0x59), // Fin / 1
            (0x4E, 0x5B), // AvPag / 3
            (0x4F, 0x5E), // Derecha / 6
            (0x50, 0x5C), // Izquierda / 4
            (0x51, 0x5A), // Abajo / 2
            (0x52, 0x60), // Arriba / 8
        ] {
            let n = tecla(navegacion);
            let k = tecla(numerico);
            assert_eq!(
                n.scancode, k.scancode,
                "HID 0x{navegacion:02x} y 0x{numerico:02x} deberian compartir scancode"
            );
            assert!(n.extendida && !k.extendida);
        }
    }

    #[test]
    fn pausa_y_las_teclas_desconocidas_se_rechazan() {
        // Pausa es una secuencia de varios scancodes y no encaja en esta API.
        assert!(matches!(
            hid_a_set1(0x48),
            Err(InputError::TeclaNoSoportada { hid: 0x48 })
        ));

        for hid in [0x00, 0x01, 0x03, 0x66, 0xFF, 0x1_0000] {
            assert!(
                matches!(hid_a_set1(hid), Err(InputError::TeclaNoSoportada { .. })),
                "HID 0x{hid:02x} deberia rechazarse en vez de inyectar una tecla parecida"
            );
        }
    }
}

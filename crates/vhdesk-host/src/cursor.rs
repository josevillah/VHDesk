//! El cursor, que viaja **fuera** del pipeline de video.
//!
//! # Por que no va pegado al frame
//!
//! Dos razones, y la segunda es la que obliga.
//!
//! La primera es la de siempre: la duplicacion de escritorio no compone el puntero sobre
//! los pixeles, y eso nos conviene. Mandandolo aparte, el viewer lo dibuja localmente y el
//! cursor se mueve al ritmo del raton del usuario en vez de al ritmo de los frames que
//! llegan por la red. Componerlo lo ataria a la latencia del video, que es justo lo que
//! hace que un escritorio remoto se sienta lento.
//!
//! La segunda es una consecuencia directa de la ranura de un hueco. Si el cursor viajara
//! con el frame, la regla de descartar el frame viejo se llevaria por delante tambien su
//! actualizacion de cursor. Con la **posicion** daria igual, porque vale la ultima; pero
//! **una forma perdida no se recupera nunca**: el sistema solo la manda cuando cambia, asi
//! que el viewer se quedaria dibujando la flecha para siempre encima de una caja de texto.
//!
//! De ahi el reparto de canales:
//!
//! | que | canal | por que |
//! |---|---|---|
//! | posicion | datagrama, ranura de 1 | diminuta y caduca: la ultima invalida a la anterior |
//! | forma | control, cola que no descarta | rara, grande (4 KB no caben en un datagrama) y **irrepetible** |

use vhdesk_capture::{CursorShape, CursorUpdate};
use vhdesk_proto::Cursor;

/// Convierte la posicion del puntero a la del protocolo, normalizada al monitor.
///
/// Devuelve [`Cursor::Hidden`] si el puntero no se esta dibujando en este monitor.
///
/// La normalizacion usa `ancho - 1` como denominador por la misma razon que el resto del
/// proyecto: hay `ancho` posiciones pero `ancho - 1` intervalos, y con el denominador
/// ingenuo el ultimo pixel de cada borde se vuelve inalcanzable.
pub fn posicion(update: &CursorUpdate, monitor: u8, ancho: u32, alto: u32) -> Cursor {
    if !update.visible {
        return Cursor::Hidden;
    }

    Cursor::Position {
        monitor,
        x: normalizar(update.position.x, ancho),
        y: normalizar(update.position.y, alto),
    }
}

fn normalizar(valor: i32, tamano: u32) -> f32 {
    let Some(intervalos) = tamano.checked_sub(1).filter(|i| *i > 0) else {
        return 0.0;
    };

    // El puntero puede quedar fuera del monitor capturado mientras cruza hacia otro; se
    // recorta en vez de mandar coordenadas fuera de rango que el viewer tendria que
    // interpretar.
    (valor as f32 / intervalos as f32).clamp(0.0, 1.0)
}

/// Convierte la imagen del puntero a la del protocolo.
///
/// La forma va por el canal de control y no por datagrama porque no cabe: un puntero de
/// 32x32 en RGBA son 4 KB y el maximo de datagrama medido en este proyecto es de 1414
/// bytes.
pub fn forma(shape: &CursorShape) -> Cursor {
    Cursor::Shape {
        // Las dimensiones de un cursor no se acercan ni de lejos a 65535; la saturacion es
        // por no tener un `as` que trunque en silencio si algun dia llegara algo absurdo.
        hotspot_x: shape.hotspot_x.min(u32::from(u16::MAX)) as u16,
        hotspot_y: shape.hotspot_y.min(u32::from(u16::MAX)) as u16,
        width: shape.width.min(u32::from(u16::MAX)) as u16,
        height: shape.height.min(u32::from(u16::MAX)) as u16,
        rgba: shape.rgba.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::{forma, posicion};
    use vhdesk_capture::{CursorPosition, CursorShape, CursorUpdate};
    use vhdesk_proto::Cursor;

    fn update(x: i32, y: i32, visible: bool) -> CursorUpdate {
        CursorUpdate {
            visible,
            position: CursorPosition { x, y },
            shape: None,
        }
    }

    #[test]
    fn las_esquinas_del_monitor_dan_los_extremos_del_rango() {
        assert_eq!(
            posicion(&update(0, 0, true), 0, 1920, 1080),
            Cursor::Position {
                monitor: 0,
                x: 0.0,
                y: 0.0
            }
        );
        assert_eq!(
            posicion(&update(1919, 1079, true), 0, 1920, 1080),
            Cursor::Position {
                monitor: 0,
                x: 1.0,
                y: 1.0
            },
            "el ultimo pixel tiene que dar 1.0 exacto, no 0,9995"
        );
    }

    #[test]
    fn un_puntero_invisible_se_reporta_como_oculto() {
        assert_eq!(
            posicion(&update(100, 100, false), 0, 1920, 1080),
            Cursor::Hidden
        );
    }

    #[test]
    fn una_posicion_fuera_del_monitor_se_recorta() {
        // Pasa de verdad mientras el puntero cruza hacia otro monitor.
        let Cursor::Position { x, y, .. } = posicion(&update(-50, 5000, true), 0, 1920, 1080)
        else {
            panic!("deberia ser una posicion");
        };
        assert_eq!((x, y), (0.0, 1.0));
    }

    #[test]
    fn un_monitor_degenerado_no_divide_por_cero() {
        let Cursor::Position { x, y, .. } = posicion(&update(0, 0, true), 0, 1, 0) else {
            panic!("deberia ser una posicion");
        };
        assert_eq!((x, y), (0.0, 0.0));
    }

    #[test]
    fn la_forma_conserva_hotspot_dimensiones_y_pixeles() {
        let shape = CursorShape {
            width: 32,
            height: 32,
            hotspot_x: 4,
            hotspot_y: 6,
            rgba: vec![7u8; 32 * 32 * 4],
        };

        let Cursor::Shape {
            hotspot_x,
            hotspot_y,
            width,
            height,
            rgba,
        } = forma(&shape)
        else {
            panic!("deberia ser una forma");
        };

        assert_eq!((hotspot_x, hotspot_y), (4, 6));
        assert_eq!((width, height), (32, 32));
        assert_eq!(rgba.len(), 32 * 32 * 4);
    }
}

//! Traduccion de lo que hace el usuario aqui a eventos que el host puede inyectar.
//!
//! # Los dos caminos, y por que el teclado no viene de egui
//!
//! El **raton** llega por los eventos de egui, que ya trae la posicion en puntos y los
//! botones con su estado. El **teclado** no: llega por Raw Input a traves del enganche de
//! mensajes de `vhdesk-input`, porque `egui::Key` pierde informacion que aqui es
//! imprescindible (no tiene variantes de modificador, no distingue izquierdo de derecho y
//! confunde el Intro del numerico con el normal). El detalle esta en
//! [`vhdesk_input::captura`].
//!
//! # El orden entre los dos caminos
//!
//! Son caminos distintos y **nada garantiza el orden relativo entre ellos**. Eso rompe
//! cosas cotidianas: Ctrl+clic, Mayus+clic para seleccionar un rango, arrastrar con un
//! modificador pulsado. Si el clic sale antes que el Ctrl que lo acompanaba, el host ve un
//! clic normal.
//!
//! **La regla es: en cada frame se drena primero el teclado y se envia en orden, y solo
//! despues se procesa y se envia el raton.** Con eso el modificador siempre precede al clic,
//! salvo que ambos ocurran en el mismo frame **y** el clic haya sido estrictamente anterior,
//! que es raro y de impacto bajo.
//!
//! **Limitacion que queda escrita a proposito**: el orden exacto dentro de un mismo frame no
//! esta garantizado entre los dos caminos. La salida conocida, si algun dia molesta en la
//! practica, es que el enganche de mensajes observe **tambien** los mensajes de raton y
//! lleve un contador de orden comun a los dos. Hoy no se hace: costaria interceptar
//! `WM_MOUSEMOVE` y compania para arreglar un caso que todavia no se ha visto fallar.
//!
//! # Que se fusiona y que no
//!
//! Un raton de 1000 Hz genera mil posiciones por segundo y solo cuenta la ultima, asi que se
//! envia **como mucho una posicion por frame**. Los **botones no se fusionan nunca**: cada
//! pulsacion y cada liberacion importan, y cada una lleva delante el movimiento a su propio
//! punto para que un arrastre no acabe pulsando donde no era.

use eframe::egui;
use vhdesk_proto::{InputEvent, MouseButton};

use crate::encuadre::{Encuadre, a_pixel_remoto};

/// Puntos de egui que equivalen a una muesca de rueda.
///
/// Solo se usa con los dispositivos que reportan pixeles en vez de lineas, que en Windows
/// son los paneles tactiles de precision. Es el valor por defecto de `line_scroll_speed` de
/// egui, o sea la misma equivalencia que aplica la propia interfaz.
const PUNTOS_POR_MUESCA: f32 = 50.0;

/// Lo que la interfaz observo del raton en un frame, ya en pixeles fisicos de la ventana.
///
/// Se construye a partir de egui en [`muestrear`] y se traduce en [`Traductor::traducir`].
/// La separacion es lo que permite testear la traduccion —que es donde estan las decisiones
/// delicadas— sin montar un contexto de egui.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Muestra {
    /// Ultima posicion conocida del puntero. Ya viene fusionada por egui.
    pub puntero: Option<(f32, f32)>,
    /// Pulsaciones y liberaciones, en orden, cada una con el punto donde ocurrio.
    pub botones: Vec<(MouseButton, bool, (f32, f32))>,
    /// Muescas de rueda acumuladas en el frame.
    pub rueda: (f32, f32),
}

/// Convierte los eventos de egui de este frame en una [`Muestra`] en pixeles fisicos.
///
/// egui trabaja en puntos y el encuadre en pixeles fisicos, que es lo que quiere el
/// viewport de wgpu; la conversion se hace aqui, una sola vez, en vez de arrastrar el factor
/// de escala por todo el modulo.
pub fn muestrear(entrada: &egui::InputState) -> Muestra {
    let ppp = entrada.pixels_per_point();
    let a_fisico = |p: egui::Pos2| (p.x * ppp, p.y * ppp);

    let mut muestra = Muestra {
        puntero: entrada.pointer.latest_pos().map(a_fisico),
        ..Muestra::default()
    };

    for evento in &entrada.events {
        match evento {
            egui::Event::PointerButton {
                pos,
                button,
                pressed,
                ..
            } => {
                if let Some(boton) = boton_del_protocolo(*button) {
                    muestra.botones.push((boton, *pressed, a_fisico(*pos)));
                }
            }
            egui::Event::MouseWheel { unit, delta, .. } => {
                let muescas = match unit {
                    egui::MouseWheelUnit::Line => *delta,
                    egui::MouseWheelUnit::Point => *delta / PUNTOS_POR_MUESCA,
                    // Una pagina no tiene equivalente en muescas sin saber el alto del
                    // contenido remoto, que aqui no se conoce. Windows no genera este caso.
                    egui::MouseWheelUnit::Page => egui::Vec2::ZERO,
                };
                // El eje vertical coincide: en egui y en Windows, positivo es rueda hacia
                // arriba. El **horizontal va al reves**: en egui positivo significa que el
                // contenido se mueve a la derecha, y en Windows que la rueda se inclina a la
                // derecha, que mueve el contenido a la izquierda.
                muestra.rueda.0 -= muescas.x;
                muestra.rueda.1 += muescas.y;
            }
            _ => {}
        }
    }

    muestra
}

/// Traduce un boton de egui al del protocolo.
///
/// Devuelve `None` para los botones que el protocolo no contempla, en lugar de
/// aproximarlos: un boton lateral traducido a "izquierdo" haria clics que nadie pidio.
fn boton_del_protocolo(boton: egui::PointerButton) -> Option<MouseButton> {
    match boton {
        egui::PointerButton::Primary => Some(MouseButton::Left),
        egui::PointerButton::Secondary => Some(MouseButton::Right),
        egui::PointerButton::Middle => Some(MouseButton::Middle),
        egui::PointerButton::Extra1 => Some(MouseButton::Back),
        egui::PointerButton::Extra2 => Some(MouseButton::Forward),
    }
}

/// Convierte lo observado en eventos del protocolo, recordando lo justo entre frames.
#[derive(Debug, Default)]
pub struct Traductor {
    /// Ultimo pixel remoto que se envio, para no repetir la misma posicion.
    ultimo_punto: Option<(i32, i32)>,
    /// Botones que este viewer ha enviado como pulsados y todavia no ha soltado.
    hundidos: Vec<MouseButton>,
}

impl Traductor {
    /// Traductor sin nada recordado.
    pub const fn nuevo() -> Self {
        Self {
            ultimo_punto: None,
            hundidos: Vec::new(),
        }
    }

    /// Olvida lo recordado entre frames.
    ///
    /// Se llama al perder el foco, junto con el `ReleaseAll` que se manda al host. Los dos
    /// olvidos importan: los botones porque el host acaba de soltarlos, y la posicion porque
    /// mientras no mirabamos el cursor remoto ha podido moverlo otro, y al volver hay que
    /// reenviarla aunque el raton local no se haya movido.
    pub fn olvidar(&mut self) {
        self.ultimo_punto = None;
        self.hundidos.clear();
    }

    /// Traduce lo observado en un frame a eventos del protocolo, en orden de envio.
    ///
    /// Los puntos que caen sobre una banda negra **no producen movimiento ni pulsacion**.
    /// Recortarlos al borde convertiria un clic despistado en la banda en un clic en la
    /// esquina del escritorio remoto, que es donde viven la X de cerrar y las esquinas
    /// activas.
    pub fn traducir(
        &mut self,
        muestra: &Muestra,
        encuadre: &Encuadre,
        video: (u32, u32),
    ) -> Vec<InputEvent> {
        let mut salida = Vec::new();

        for (boton, pulsado, punto) in &muestra.botones {
            let dentro = a_pixel_remoto(*punto, encuadre, video).is_some();

            if *pulsado {
                // Una pulsacion sobre la banda no se manda, y por eso tampoco se anota: la
                // liberacion que venga despues no tendra nada que soltar.
                if !dentro {
                    continue;
                }
                self.mover_a(*punto, encuadre, video, &mut salida);
                if !self.hundidos.contains(boton) {
                    self.hundidos.push(*boton);
                }
            } else {
                // **La liberacion se envia caiga donde caiga**, siempre que la pulsacion se
                // hubiera enviado. Suprimirla por estar sobre una banda dejaria el boton
                // hundido en la maquina remota, que es la version con raton de las teclas
                // pegadas. Si el punto esta dentro se mueve primero, para que el clic
                // termine donde el usuario lo solto.
                if !self.hundidos.contains(boton) {
                    continue;
                }
                self.hundidos.retain(|b| b != boton);
                if dentro {
                    self.mover_a(*punto, encuadre, video, &mut salida);
                }
            }

            salida.push(InputEvent::MouseButton {
                button: *boton,
                pressed: *pulsado,
            });
        }

        if let Some(punto) = muestra.puntero {
            self.mover_a(punto, encuadre, video, &mut salida);
        }

        // La rueda va al final y solo con el puntero sobre el video: en el host actua sobre
        // lo que haya bajo el cursor remoto, asi que mandarla mientras se apunta a una banda
        // haria girar algo que el usuario no esta mirando.
        let sobre_el_video = muestra
            .puntero
            .is_some_and(|p| a_pixel_remoto(p, encuadre, video).is_some());
        if sobre_el_video && muestra.rueda != (0.0, 0.0) {
            salida.push(InputEvent::MouseScroll {
                delta_x: muestra.rueda.0,
                delta_y: muestra.rueda.1,
            });
        }

        salida
    }

    /// Anade un movimiento si el punto esta sobre el video y no es donde ya estabamos.
    fn mover_a(
        &mut self,
        punto: (f32, f32),
        encuadre: &Encuadre,
        video: (u32, u32),
        salida: &mut Vec<InputEvent>,
    ) {
        let Some(pixel) = a_pixel_remoto(punto, encuadre, video) else {
            return;
        };
        if self.ultimo_punto == Some(pixel) {
            return;
        }
        self.ultimo_punto = Some(pixel);

        salida.push(InputEvent::MouseMoveAbsolute {
            monitor: 0,
            x: normalizar_eje(pixel.0, video.0),
            y: normalizar_eje(pixel.1, video.1),
        });
    }
}

/// Convierte un pixel remoto al rango `0.0..=1.0` que viaja por el protocolo.
///
/// El denominador es `tamano - 1`, que es el inverso exacto de lo que hace el host en
/// `vhdesk_input::a_pixeles`. Con el denominador ingenuo, el ultimo pixel de cada borde
/// quedaria inalcanzable, y ahi viven el boton de inicio, la X de cerrar y las esquinas
/// activas.
fn normalizar_eje(pixel: i32, tamano: u32) -> f32 {
    let intervalos = tamano.saturating_sub(1);
    if intervalos == 0 {
        return 0.0;
    }
    (pixel as f32 / intervalos as f32).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::{Muestra, Traductor, normalizar_eje};
    use crate::encuadre::{Encuadre, encuadrar};
    use vhdesk_proto::{InputEvent, MouseButton};

    const VIDEO: (u32, u32) = (1920, 1080);

    /// Ventana mas alta que el video: deja bandas negras arriba y abajo.
    fn encuadre_con_bandas() -> Encuadre {
        encuadrar(VIDEO, (960.0, 1000.0))
    }

    fn con_puntero(punto: (f32, f32)) -> Muestra {
        Muestra {
            puntero: Some(punto),
            ..Muestra::default()
        }
    }

    #[test]
    fn una_posicion_repetida_no_se_reenvia() {
        // El puntero de egui ya viene fusionado, pero la ventana repinta muchas veces sin
        // que el raton se mueva. Reenviar el mismo punto seria un mensaje por frame para no
        // decir nada.
        let e = encuadre_con_bandas();
        let mut t = Traductor::nuevo();
        let muestra = con_puntero((480.0, 500.0));

        assert_eq!(t.traducir(&muestra, &e, VIDEO).len(), 1);
        assert!(
            t.traducir(&muestra, &e, VIDEO).is_empty(),
            "la segunda vez no hay nada nuevo que contar"
        );
    }

    #[test]
    fn tras_perder_el_foco_la_posicion_se_reenvia() {
        // Mientras no mirabamos, el cursor remoto ha podido moverlo otro. Al volver hay que
        // reenviar la posicion aunque el raton local siga donde estaba.
        let e = encuadre_con_bandas();
        let mut t = Traductor::nuevo();
        let muestra = con_puntero((480.0, 500.0));

        assert_eq!(t.traducir(&muestra, &e, VIDEO).len(), 1);
        t.olvidar();
        assert_eq!(
            t.traducir(&muestra, &e, VIDEO).len(),
            1,
            "sin esto el raton remoto se quedaria donde lo dejo el otro"
        );
    }

    #[test]
    fn un_punto_sobre_la_banda_negra_no_produce_nada() {
        let e = encuadre_con_bandas();
        let mut t = Traductor::nuevo();

        // y=10 esta por encima del video, que empieza en y=230.
        let muestra = Muestra {
            puntero: Some((480.0, 10.0)),
            botones: vec![(MouseButton::Left, true, (480.0, 10.0))],
            rueda: (0.0, 3.0),
        };

        assert!(
            t.traducir(&muestra, &e, VIDEO).is_empty(),
            "recortar al borde convertiria un clic despistado en la banda en un clic en la \
             esquina del escritorio remoto"
        );
    }

    #[test]
    fn la_liberacion_sale_aunque_caiga_en_la_banda() {
        // La version con raton de las teclas pegadas: si el usuario pulsa sobre el video,
        // arrastra hasta la banda y suelta, suprimir la liberacion dejaria el boton hundido
        // en la maquina remota.
        let e = encuadre_con_bandas();
        let mut t = Traductor::nuevo();

        let pulsar = Muestra {
            puntero: Some((480.0, 500.0)),
            botones: vec![(MouseButton::Left, true, (480.0, 500.0))],
            ..Muestra::default()
        };
        t.traducir(&pulsar, &e, VIDEO);

        let soltar_en_la_banda = Muestra {
            puntero: Some((480.0, 10.0)),
            botones: vec![(MouseButton::Left, false, (480.0, 10.0))],
            ..Muestra::default()
        };
        let eventos = t.traducir(&soltar_en_la_banda, &e, VIDEO);

        assert_eq!(
            eventos,
            vec![InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: false
            }],
            "la liberacion tiene que salir, y sin movimiento a la banda delante"
        );
    }

    #[test]
    fn una_liberacion_sin_su_pulsacion_no_se_inventa() {
        // Pulsar sobre la banda no manda nada, asi que la liberacion posterior tampoco debe
        // mandarse: seria un boton que se suelta sin haberse pulsado nunca.
        let e = encuadre_con_bandas();
        let mut t = Traductor::nuevo();

        let pulsar_en_la_banda = Muestra {
            puntero: Some((480.0, 10.0)),
            botones: vec![(MouseButton::Left, true, (480.0, 10.0))],
            ..Muestra::default()
        };
        t.traducir(&pulsar_en_la_banda, &e, VIDEO);

        let soltar_dentro = Muestra {
            puntero: Some((480.0, 500.0)),
            botones: vec![(MouseButton::Left, false, (480.0, 500.0))],
            ..Muestra::default()
        };
        let eventos = t.traducir(&soltar_dentro, &e, VIDEO);

        assert!(
            !eventos.iter().any(|e| matches!(
                e,
                InputEvent::MouseButton {
                    button: MouseButton::Left,
                    ..
                }
            )),
            "no se solto nada porque nunca se pulso: {eventos:?}"
        );
    }

    #[test]
    fn cada_boton_lleva_delante_el_movimiento_a_su_propio_punto() {
        // Si el clic se enviara con la posicion final del frame en vez de con la suya, un
        // arrastre rapido pulsaria donde no era.
        let e = encuadre_con_bandas();
        let mut t = Traductor::nuevo();

        let muestra = Muestra {
            // El puntero acabo el frame lejos de donde se hizo clic.
            puntero: Some((900.0, 700.0)),
            botones: vec![(MouseButton::Left, true, (100.0, 300.0))],
            ..Muestra::default()
        };
        let eventos = t.traducir(&muestra, &e, VIDEO);

        assert!(
            matches!(eventos.first(), Some(InputEvent::MouseMoveAbsolute { .. })),
            "el movimiento al punto del clic va primero: {eventos:?}"
        );
        assert!(
            matches!(eventos.get(1), Some(InputEvent::MouseButton { .. })),
            "y el boton justo detras: {eventos:?}"
        );
        assert!(
            matches!(eventos.get(2), Some(InputEvent::MouseMoveAbsolute { .. })),
            "y despues el movimiento a donde acabo el puntero: {eventos:?}"
        );
    }

    #[test]
    fn la_rueda_solo_gira_si_se_apunta_al_video() {
        let e = encuadre_con_bandas();
        let mut t = Traductor::nuevo();

        let sobre_la_banda = Muestra {
            puntero: Some((480.0, 10.0)),
            rueda: (0.0, 2.0),
            ..Muestra::default()
        };
        assert!(t.traducir(&sobre_la_banda, &e, VIDEO).is_empty());

        let sobre_el_video = Muestra {
            puntero: Some((480.0, 500.0)),
            rueda: (0.0, 2.0),
            ..Muestra::default()
        };
        let eventos = t.traducir(&sobre_el_video, &e, VIDEO);
        assert!(
            eventos.contains(&InputEvent::MouseScroll {
                delta_x: 0.0,
                delta_y: 2.0
            }),
            "{eventos:?}"
        );
    }

    #[test]
    fn las_esquinas_del_video_se_normalizan_a_los_extremos() {
        // Es el contrato con `vhdesk_input::a_pixeles`, que multiplica por `tamano - 1`.
        // Si los dos denominadores no coincidieran, el error crecería hacia los bordes y el
        // ultimo pixel seria inalcanzable justo donde estan la X de cerrar y el boton de
        // inicio.
        assert_eq!(normalizar_eje(0, 1920), 0.0);
        assert_eq!(normalizar_eje(1919, 1920), 1.0);
        assert_eq!(normalizar_eje(1079, 1080), 1.0);

        // Un video de un solo pixel no tiene intervalos: no debe dividir entre cero.
        assert_eq!(normalizar_eje(0, 1), 0.0);
    }

    #[test]
    fn el_movimiento_a_la_esquina_llega_como_uno_exacto() {
        let e = encuadrar(VIDEO, (960.0, 540.0));
        let mut t = Traductor::nuevo();

        let eventos = t.traducir(&con_puntero((0.0, 0.0)), &e, VIDEO);
        assert_eq!(
            eventos,
            vec![InputEvent::MouseMoveAbsolute {
                monitor: 0,
                x: 0.0,
                y: 0.0
            }]
        );

        let eventos = t.traducir(&con_puntero((959.9, 539.9)), &e, VIDEO);
        assert_eq!(
            eventos,
            vec![InputEvent::MouseMoveAbsolute {
                monitor: 0,
                x: 1.0,
                y: 1.0
            }],
            "la esquina opuesta tiene que ser alcanzable"
        );
    }
}

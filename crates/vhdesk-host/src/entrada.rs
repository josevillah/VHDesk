//! El camino de vuelta: eventos del viewer inyectados en esta maquina.
//!
//! # Teclas pegadas
//!
//! Es el fallo mas grave que puede dejar este modulo, y aparece **despues** de que la
//! sesion termine, asi que nadie lo relaciona con la causa: si el viewer se va con una
//! tecla pulsada, el host se queda con ella hundida para siempre. Con Ctrl o Alt la maquina
//! queda practicamente inservible.
//!
//! Los tres disparadores de [`InputInjector::liberar_todo`] y quien los provoca:
//!
//! | disparador | de donde viene |
//! |---|---|
//! | cierre de sesion | el host lo detecta solo: el stream de input termina |
//! | error de conexion | el host lo detecta solo: la lectura falla |
//! | **perdida de foco del viewer** | **solo puede venir del viewer** |
//!
//! Los dos primeros se resuelven en [`bucle`], que llama a `liberar_todo` pase lo que pase
//! al salir. El tercero no lo puede saber el host, asi que el viewer lo dice enviando un
//! [`ReleaseAll`] por el canal de input, y aqui se atiende sin terminar la sesion: perder el
//! foco es cotidiano y no significa que el viewer se vaya.

use anyhow::Result;
use vhdesk_input::{InputInjector, MonitorFisico, a_pixeles};
use vhdesk_proto::{InputEvent, Message};
use vhdesk_transport::InputReceiver;

/// Aplica un evento del protocolo al injector.
///
/// Los eventos de un monitor que este host no esta sirviendo se ignoran en silencio: el
/// multi-monitor es de la fase 5 y hasta entonces un viewer podria mandarlos por error.
///
/// # Errores
///
/// Devuelve [`vhdesk_input::InputError`] si el sistema rechaza el evento. Una tecla sin
/// equivalente en esta plataforma **no** es motivo para cortar la sesion.
pub fn aplicar(
    evento: InputEvent,
    monitor_servido: u8,
    geometria: &MonitorFisico,
    injector: &mut dyn InputInjector,
) -> Result<(), vhdesk_input::InputError> {
    match evento {
        InputEvent::MouseMoveAbsolute { monitor, x, y } => {
            if monitor != monitor_servido {
                return Ok(());
            }
            let (px, py) = a_pixeles(x, y, geometria);
            injector.mouse_move_absolute(px, py)
        }
        InputEvent::MouseButton { button, pressed } => injector.mouse_button(button, pressed),
        InputEvent::MouseScroll { delta_x, delta_y } => injector.mouse_scroll(delta_x, delta_y),
        InputEvent::Key { scancode, pressed } => injector.key(scancode, pressed),
        // El enum es `#[non_exhaustive]`: una variante que este host no conoce (por ejemplo
        // la de texto Unicode de la fase 5) se ignora en vez de cortar la sesion.
        otro => {
            tracing::debug!(?otro, "evento de entrada no soportado todavia");
            Ok(())
        }
    }
}

/// Recibe eventos de entrada y los inyecta hasta que el canal termina.
///
/// Al volver, **siempre** ha llamado a `liberar_todo`, con exito o con error.
///
/// # Errores
///
/// Devuelve error si por el canal de input llega algo que no es un evento de entrada, que
/// significa que el peer no habla este protocolo. Perder la conexion **no** es un error:
/// es como termina una sesion normal.
pub async fn bucle(
    mut receptor: InputReceiver,
    monitor_servido: u8,
    geometria: MonitorFisico,
    injector: &mut (dyn InputInjector + Send),
) -> Result<()> {
    let resultado = recibir(&mut receptor, monitor_servido, &geometria, injector).await;

    // Pase lo que pase: fin limpio, error de red o panico del otro lado. Es el unico sitio
    // por el que pasan todos los finales de este camino.
    if let Err(error) = injector.liberar_todo() {
        tracing::warn!(%error, "no se pudo soltar lo que quedaba hundido");
    } else {
        tracing::debug!("soltado todo lo que quedaba hundido");
    }

    resultado
}

async fn recibir(
    receptor: &mut InputReceiver,
    monitor_servido: u8,
    geometria: &MonitorFisico,
    injector: &mut (dyn InputInjector + Send),
) -> Result<()> {
    loop {
        let mensaje = match receptor.recv().await {
            Ok(mensaje) => mensaje,
            // El viewer cerro o la conexion se fue: es como termina una sesion normal.
            Err(error) => {
                tracing::debug!(%error, "termina el canal de input");
                return Ok(());
            }
        };

        let evento = match mensaje {
            Message::InputEvent(evento) => evento,
            // El viewer perdio el foco, se minimizo o bloquearon su sesion. La sesion sigue
            // viva: lo unico que pide es que nada quede hundido mientras el no mira.
            Message::ReleaseAll(_) => {
                if let Err(error) = injector.liberar_todo() {
                    tracing::warn!(%error, "no se pudo soltar lo hundido al perder el foco");
                } else {
                    tracing::debug!("el viewer perdio el foco: soltado todo lo hundido");
                }
                continue;
            }
            // El canal de input solo lleva eventos de entrada. Cualquier otra cosa es un
            // peer que no habla este protocolo.
            otro => anyhow::bail!("mensaje inesperado en el canal de input: {}", otro.name()),
        };

        if let Err(error) = aplicar(evento, monitor_servido, geometria, injector) {
            // Una tecla sin equivalente o un evento que el sistema rechaza no justifica
            // tirar la sesion: el usuario preferira seguir controlando la maquina.
            tracing::debug!(%error, "el sistema rechazo un evento de entrada");
        }
    }
}

/// Traduce la informacion del monitor capturado a la geometria que necesita la inyeccion.
pub fn geometria_de(monitor: &vhdesk_capture::MonitorInfo) -> MonitorFisico {
    MonitorFisico {
        x: monitor.position.0,
        y: monitor.position.1,
        ancho: monitor.width,
        alto: monitor.height,
    }
}

#[cfg(test)]
mod tests {
    use super::{aplicar, bucle};
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};
    use vhdesk_input::{InputError, InputInjector, MonitorFisico};
    use vhdesk_proto::{InputEvent, Message, MouseButton};

    /// Injector de mentira que solo anota lo que le piden.
    ///
    /// No simula la inyeccion: la sustituye, que es lo que permite testear la traduccion de
    /// coordenadas y el filtrado por monitor sin mover el raton de quien ejecute la suite.
    #[derive(Default)]
    struct Espia {
        movimientos: Vec<(i32, i32)>,
        botones: Vec<(MouseButton, bool)>,
        teclas: Vec<(u32, bool)>,
        liberaciones: u32,
    }

    impl InputInjector for Espia {
        fn mouse_move_absolute(&mut self, x: i32, y: i32) -> Result<(), InputError> {
            self.movimientos.push((x, y));
            Ok(())
        }
        fn mouse_button(&mut self, button: MouseButton, pressed: bool) -> Result<(), InputError> {
            self.botones.push((button, pressed));
            Ok(())
        }
        fn mouse_scroll(&mut self, _x: f32, _y: f32) -> Result<(), InputError> {
            Ok(())
        }
        fn key(&mut self, hid: u32, pressed: bool) -> Result<(), InputError> {
            self.teclas.push((hid, pressed));
            Ok(())
        }
        fn liberar_todo(&mut self) -> Result<(), InputError> {
            self.liberaciones += 1;
            Ok(())
        }
    }

    /// Injector que comparte su registro con el test, para poder mirarlo despues de que el
    /// bucle se haya llevado la referencia mutable.
    #[derive(Clone, Default)]
    struct EspiaCompartido(Arc<Mutex<Espia>>);

    impl EspiaCompartido {
        fn ver<T>(&self, f: impl FnOnce(&Espia) -> T) -> T {
            f(&self.0.lock().expect("registro del espia"))
        }
    }

    impl InputInjector for EspiaCompartido {
        fn mouse_move_absolute(&mut self, x: i32, y: i32) -> Result<(), InputError> {
            self.0.lock().expect("registro").mouse_move_absolute(x, y)
        }
        fn mouse_button(&mut self, button: MouseButton, pressed: bool) -> Result<(), InputError> {
            self.0
                .lock()
                .expect("registro")
                .mouse_button(button, pressed)
        }
        fn mouse_scroll(&mut self, x: f32, y: f32) -> Result<(), InputError> {
            self.0.lock().expect("registro").mouse_scroll(x, y)
        }
        fn key(&mut self, hid: u32, pressed: bool) -> Result<(), InputError> {
            self.0.lock().expect("registro").key(hid, pressed)
        }
        fn liberar_todo(&mut self) -> Result<(), InputError> {
            self.0.lock().expect("registro").liberar_todo()
        }
    }

    /// Espera a que se cumpla una condicion, o falla el test antes de colgarse.
    async fn esperar_a(mut condicion: impl FnMut() -> bool, que: &str) {
        for _ in 0..500 {
            if condicion() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("no llego a cumplirse: {que}");
    }

    /// Al irse el viewer con una tecla pulsada, el host **tiene** que soltarla.
    ///
    /// Es el fallo mas grave de este camino y el mas dificil de relacionar con su causa,
    /// porque el sintoma aparece despues de que la sesion termine: con Ctrl o Alt hundidos,
    /// cada clic pasa a ser un clic con Ctrl y la maquina queda inservible sin que nada lo
    /// explique.
    ///
    /// Va contra una conexion QUIC de verdad, no contra un canal de mentira, porque lo que
    /// se comprueba es justamente que el final de un stream real desemboque en
    /// `liberar_todo`.
    #[tokio::test]
    async fn al_terminar_la_sesion_se_sueltan_las_teclas_hundidas() {
        vhdesk_transport::install_crypto_provider();

        let local = SocketAddr::from(([127, 0, 0, 1], 0));
        let host_endpoint = vhdesk_transport::Endpoint::bind(local).expect("endpoint del host");
        let addr = host_endpoint.local_addr().expect("direccion");
        let viewer_endpoint = vhdesk_transport::Endpoint::bind(local).expect("endpoint del viewer");

        let espia = EspiaCompartido::default();
        let mut copia = espia.clone();

        // Todo el lado del host va en su tarea. **`accept_input` no retorna hasta que el
        // viewer escribe** en el stream, no cuando lo abre: esperarlo desde el hilo que
        // todavia tiene que enviar es un interbloqueo, y ademas es exactamente por lo que en
        // el host de verdad esto vive en una tarea aparte.
        let servidor = tokio::spawn(async move {
            let sesion = host_endpoint.accept().await.expect("aceptar");
            let receptor = sesion.accept_input().await.expect("aceptar input");
            bucle(receptor, 0, GEOMETRIA, &mut copia)
                .await
                .expect("bucle");
            // La sesion se conserva viva hasta aqui: soltarla antes cerraria la conexion por
            // debajo del propio bucle que se esta midiendo.
            drop(sesion);
        });

        let viewer = viewer_endpoint.connect(addr).await.expect("conectar");
        let mut input = viewer.open_input().await.expect("abrir input");

        // Ctrl izquierdo hundido, y nadie va a mandar nunca el evento de soltarlo.
        input
            .send(&Message::InputEvent(InputEvent::Key {
                scancode: 0x0007_00e0,
                pressed: true,
            }))
            .await
            .expect("enviar la pulsacion");

        // Se espera a que la pulsacion se haya inyectado antes de cortar: cerrar durante el
        // vuelo mediria una carrera en vez de la liberacion.
        esperar_a(
            || espia.ver(|e| !e.teclas.is_empty()),
            "la pulsacion inyectada",
        )
        .await;

        // Y ahora el viewer desaparece de golpe, que es lo que pasa cuando alguien cierra la
        // ventana o se le cae la red.
        viewer.close();
        drop(viewer_endpoint);

        tokio::time::timeout(std::time::Duration::from_secs(5), servidor)
            .await
            .expect("el bucle no termino tras irse el viewer")
            .expect("tarea del host");

        espia.ver(|e| {
            assert_eq!(
                e.teclas,
                vec![(0x0007_00e0, true)],
                "la pulsacion no llego a inyectarse"
            );
            assert_eq!(
                e.liberaciones, 1,
                "el host se quedo con Ctrl hundido: la maquina remota queda inservible y el \
                 sintoma aparece cuando ya nadie lo relaciona con la sesion"
            );
        });
    }

    /// Un `ReleaseAll` suelta las teclas **sin** terminar la sesion.
    ///
    /// Perder el foco es cotidiano —Alt+Tab, minimizar, bloquear la pantalla— y no
    /// significa que el viewer se vaya. Si esto cortara la sesion, el mecanismo que existe
    /// para evitar teclas pegadas seria peor que la enfermedad.
    #[tokio::test]
    async fn release_all_suelta_lo_hundido_y_la_sesion_sigue_viva() {
        vhdesk_transport::install_crypto_provider();

        let local = SocketAddr::from(([127, 0, 0, 1], 0));
        let host_endpoint = vhdesk_transport::Endpoint::bind(local).expect("endpoint del host");
        let addr = host_endpoint.local_addr().expect("direccion");
        let viewer_endpoint = vhdesk_transport::Endpoint::bind(local).expect("endpoint del viewer");

        let espia = EspiaCompartido::default();
        let mut copia = espia.clone();

        let servidor = tokio::spawn(async move {
            let sesion = host_endpoint.accept().await.expect("aceptar");
            let receptor = sesion.accept_input().await.expect("aceptar input");
            bucle(receptor, 0, GEOMETRIA, &mut copia)
                .await
                .expect("bucle");
            drop(sesion);
        });

        let viewer = viewer_endpoint.connect(addr).await.expect("conectar");
        let mut input = viewer.open_input().await.expect("abrir input");

        // Alt hundido y despues Alt+Tab: la ventana pierde el foco con la tecla dentro.
        input
            .send(&Message::InputEvent(InputEvent::Key {
                scancode: 0x0007_00e2,
                pressed: true,
            }))
            .await
            .expect("enviar la pulsacion");
        input
            .send(&Message::ReleaseAll(vhdesk_proto::ReleaseAll))
            .await
            .expect("enviar el ReleaseAll");

        esperar_a(
            || espia.ver(|e| e.liberaciones >= 1),
            "la liberacion por perdida de foco",
        )
        .await;

        // Y la sesion sigue: se manda otra tecla y tiene que inyectarse igual.
        input
            .send(&Message::InputEvent(InputEvent::Key {
                scancode: 0x04,
                pressed: true,
            }))
            .await
            .expect("enviar despues del ReleaseAll");

        esperar_a(
            || espia.ver(|e| e.teclas.len() == 2),
            "la tecla posterior al ReleaseAll: el canal no deberia haberse cerrado",
        )
        .await;

        viewer.close();
        drop(viewer_endpoint);

        tokio::time::timeout(std::time::Duration::from_secs(5), servidor)
            .await
            .expect("el bucle no termino tras irse el viewer")
            .expect("tarea del host");

        espia.ver(|e| {
            assert_eq!(
                e.liberaciones, 2,
                "una liberacion por el ReleaseAll y otra al terminar el bucle"
            );
        });
    }

    const GEOMETRIA: MonitorFisico = MonitorFisico {
        x: 1920,
        y: 0,
        ancho: 1920,
        alto: 1080,
    };

    #[test]
    fn el_movimiento_normalizado_llega_en_pixeles_del_escritorio_virtual() {
        let mut espia = Espia::default();

        aplicar(
            InputEvent::MouseMoveAbsolute {
                monitor: 0,
                x: 1.0,
                y: 1.0,
            },
            0,
            &GEOMETRIA,
            &mut espia,
        )
        .expect("aplicar");

        assert_eq!(
            espia.movimientos,
            vec![(1920 + 1919, 1079)],
            "el origen del monitor tiene que sumarse: sin eso el raton se va al monitor de al lado"
        );
    }

    #[test]
    fn los_eventos_de_otro_monitor_se_ignoran() {
        let mut espia = Espia::default();

        aplicar(
            InputEvent::MouseMoveAbsolute {
                monitor: 3,
                x: 0.5,
                y: 0.5,
            },
            0,
            &GEOMETRIA,
            &mut espia,
        )
        .expect("aplicar");

        assert!(
            espia.movimientos.is_empty(),
            "un monitor que este host no sirve no debe mover nada"
        );
    }

    #[test]
    fn los_botones_y_teclas_pasan_tal_cual() {
        let mut espia = Espia::default();

        aplicar(
            InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true,
            },
            0,
            &GEOMETRIA,
            &mut espia,
        )
        .expect("aplicar");
        aplicar(
            InputEvent::Key {
                scancode: 0x04,
                pressed: true,
            },
            0,
            &GEOMETRIA,
            &mut espia,
        )
        .expect("aplicar");

        assert_eq!(espia.botones, vec![(MouseButton::Left, true)]);
        assert_eq!(espia.teclas, vec![(0x04, true)]);
    }
}

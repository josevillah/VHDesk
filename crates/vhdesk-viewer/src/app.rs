//! La ventana: pinta el video o, si aun no lo hay, dice por que, y manda la entrada.
//!
//! # El video no pasa por el teselador de egui
//!
//! Se pinta con un callback de `egui_wgpu`, que entrega el `RenderPass` en curso para
//! meterle ordenes de dibujo directamente. La alternativa —convertir el frame a una textura
//! de egui y dibujarla como una imagen mas— lo metería por el teselador y por el atlas, y
//! ahi se pierde justo la latencia que el ADR-0001 fue a buscar eligiendo wgpu.
//!
//! El **cursor remoto** si va por el teselador, y a proposito: son unos pocos miles de
//! pixeles una vez por frame, y no esta en el camino que se optimiza.
//!
//! # Estados visibles
//!
//! **Nunca se deja la ventana en negro sin explicacion.** Mientras no hay imagen se pinta
//! un texto centrado con lo que esta pasando: conectando, negociando, esperando el primer
//! frame, o el motivo por el que la sesion termino. Una ventana negra es indistinguible de
//! un cuelgue y no le dice nada a quien la mira.
//!
//! # El orden en que se manda la entrada
//!
//! En cada frame se drena **primero** el teclado y **despues** el raton. El porque esta en
//! [`crate::entrada`], junto con la limitacion que queda abierta.

use std::sync::Arc;
use std::sync::mpsc::Receiver;

use eframe::egui;
use eframe::wgpu;
use vhdesk_input::TeclaCapturada;
use vhdesk_proto::{InputEvent, Message, ReleaseAll};

use crate::cursor::CursorPintado;
use crate::encuadre::{Encuadre, encuadrar};
use crate::entrada::{Traductor, muestrear};
use crate::sesion::{Compartido, EmisorEntrada, Estado};

/// Aplicacion del viewer.
pub struct App {
    sesion: Arc<Compartido>,
    /// Cola por la que salen los eventos hacia el hilo de sesion.
    entrada: EmisorEntrada,
    /// Teclas que el enganche de Raw Input ha dejado desde el frame anterior.
    teclado: Receiver<TeclaCapturada>,
    traductor: Traductor,
    /// Si la ventana tenia el foco en el frame anterior.
    tenia_foco: bool,
    cursor: CursorPintado,
}

impl App {
    /// Crea la aplicacion sobre una sesion ya arrancada.
    pub const fn nueva(
        sesion: Arc<Compartido>,
        entrada: EmisorEntrada,
        teclado: Receiver<TeclaCapturada>,
    ) -> Self {
        Self {
            sesion,
            entrada,
            teclado,
            traductor: Traductor::nuevo(),
            // Arranca en `false`: si la ventana nace con el foco, la transicion a `true` no
            // hace nada, y si nace sin el, tampoco se manda un `ReleaseAll` de mentira.
            tenia_foco: false,
            cursor: CursorPintado::nuevo(),
        }
    }
}

impl eframe::App for App {
    /// El fondo de la ventana es negro opaco.
    ///
    /// Son las bandas del encuadre: el video se pinta solo dentro de su rectangulo y lo que
    /// queda alrededor es esto. Con el color por defecto de egui las bandas saldrian grises
    /// y el borde de la imagen quedaria mal definido.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let estado = self.sesion.estado();
        let video = self.sesion.dimensiones();

        // Antes de pintar: lo que el usuario acaba de hacer viaja cuanto antes.
        self.procesar_entrada(ctx, video);

        let marco = egui::Frame::NONE.fill(egui::Color32::BLACK);
        egui::CentralPanel::default()
            .frame(marco)
            .show(ctx, |ui| match (&estado, video) {
                (Estado::Activa, Some(video)) => self.pintar_video(ui, video),
                _ => pintar_estado(ui, &estado),
            });

        // No se pide repintado periodico a proposito. Cada cambio de estado, cada frame
        // nuevo y cada tecla capturada ya llaman a `request_repaint`, asi que despertar la
        // interfaz por reloj solo gastaria bateria mostrando lo mismo.
    }
}

impl App {
    /// Manda al host lo que el usuario ha hecho en este frame.
    ///
    /// **El teclado va primero.** Los dos caminos son distintos —el teclado entra por el
    /// enganche de Raw Input y el raton por los eventos de egui— y nada garantiza el orden
    /// relativo entre ellos. Drenando el teclado antes, el modificador precede al clic, que
    /// es lo que hace que Ctrl+clic y Mayus+clic funcionen. Ver [`crate::entrada`].
    fn procesar_entrada(&mut self, ctx: &egui::Context, video: Option<(u32, u32)>) {
        let foco = ctx.input(|i| i.focused);

        // El teclado se drena **tenga o no foco**. Puede quedar en la cola una liberacion
        // que ocurrio justo antes de perderlo, y tirarla seria dejar la tecla hundida.
        while let Ok(tecla) = self.teclado.try_recv() {
            self.enviar(InputEvent::Key {
                scancode: tecla.hid,
                pressed: tecla.pulsada,
            });
        }

        if let (true, Some(video)) = (foco, video) {
            let encuadre = encuadre_de(ctx, video);
            let muestra = ctx.input(muestrear);
            for evento in self.traductor.traducir(&muestra, &encuadre, video) {
                self.enviar(evento);
            }
        }

        // Y al final, la red de seguridad. Va **detras** del teclado de este frame para que
        // limpie lo que ese teclado haya dejado hundido, que es justo el caso de Alt+Tab: el
        // Alt viaja al host y la ventana pierde el foco inmediatamente despues.
        if self.tenia_foco && !foco {
            self.traductor.olvidar();
            if self.entrada.send(Message::ReleaseAll(ReleaseAll)).is_ok() {
                tracing::debug!("foco perdido: ReleaseAll enviado");
            }
        } else if !self.tenia_foco && foco {
            // Tambien se registra recuperar el foco. Es la unica forma de distinguir en un
            // log "no salio el ReleaseAll" de "nunca hubo foco que perder", que son dos
            // fallos distintos con el mismo sintoma.
            tracing::debug!("foco recuperado");
        }
        self.tenia_foco = foco;
    }

    fn enviar(&self, evento: InputEvent) {
        // Un envio fallido significa que el hilo de sesion ya termino. No se registra por
        // evento: seria una linea por pulsacion mientras la ventana ensena el motivo del
        // cierre.
        if self.entrada.send(Message::InputEvent(evento)).is_ok() {
            self.sesion.anotar_entrada();
        }
    }

    fn pintar_video(&mut self, ui: &mut egui::Ui, video: (u32, u32)) {
        let rect = ui.available_rect_before_wrap();

        // El rectangulo que se le pasa al callback es el del panel entero, en puntos. El
        // encuadre se calcula dentro, en pixeles fisicos, porque es lo que quiere el
        // viewport de wgpu y evita arrastrar el factor de escala hasta aqui.
        ui.painter()
            .add(eframe::egui_wgpu::Callback::new_paint_callback(
                rect,
                CallbackVideo {
                    sesion: Arc::clone(&self.sesion),
                    video,
                },
            ));

        self.cursor.pintar(ui, &self.sesion, video, rect);
    }
}

/// Encuadre del video dentro de la ventana, en pixeles fisicos.
///
/// Se calcula sobre `screen_rect` porque el panel central lleva `Frame::NONE` y ocupa la
/// ventana entera: es el mismo rectangulo que el callback de pintado recibe como viewport,
/// y tienen que coincidir o el raton apuntaria a un sitio distinto del que se ve.
fn encuadre_de(ctx: &egui::Context, video: (u32, u32)) -> Encuadre {
    let ppp = ctx.pixels_per_point();
    let rect = ctx.screen_rect();
    encuadrar(video, (rect.width() * ppp, rect.height() * ppp))
}

/// Texto centrado con el estado de la sesion.
fn pintar_estado(ui: &mut egui::Ui, estado: &Estado) {
    let color = match estado {
        Estado::Terminada { limpia: false, .. } => egui::Color32::from_rgb(255, 120, 120),
        _ => egui::Color32::from_gray(200),
    };

    ui.centered_and_justified(|ui| {
        ui.label(
            egui::RichText::new(estado.descripcion())
                .size(16.0)
                .color(color),
        );
    });
}

/// Dibuja el frame ya subido, dentro del encuadre.
struct CallbackVideo {
    sesion: Arc<Compartido>,
    video: (u32, u32),
}

impl eframe::egui_wgpu::CallbackTrait for CallbackVideo {
    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        paso: &mut wgpu::RenderPass<'static>,
        _recursos: &eframe::egui_wgpu::CallbackResources,
    ) {
        let vp = info.viewport_in_pixels();
        if vp.width_px <= 0 || vp.height_px <= 0 {
            return;
        }

        let encuadre = encuadrar(self.video, (vp.width_px as f32, vp.height_px as f32));
        if encuadre.esta_vacio() {
            return;
        }

        // El viewport de wgpu es absoluto respecto a la superficie, no relativo al panel,
        // asi que hay que sumarle el origen del callback.
        let Encuadre { x, y, ancho, alto } = encuadre;
        let izquierda = vp.left_px as f32 + x;
        let arriba = vp.top_px as f32 + y;

        self.sesion.con_renderer(|renderer| {
            paso.set_viewport(izquierda, arriba, ancho, alto, 0.0, 1.0);
            renderer.draw(paso);
        });
    }
}

//! La ventana: pinta el video o, si aun no lo hay, dice por que.
//!
//! # El video no pasa por el teselador de egui
//!
//! Se pinta con un callback de `egui_wgpu`, que entrega el `RenderPass` en curso para
//! meterle ordenes de dibujo directamente. La alternativa —convertir el frame a una textura
//! de egui y dibujarla como una imagen mas— lo metería por el teselador y por el atlas, y
//! ahi se pierde justo la latencia que el ADR-0001 fue a buscar eligiendo wgpu.
//!
//! # Estados visibles
//!
//! **Nunca se deja la ventana en negro sin explicacion.** Mientras no hay imagen se pinta
//! un texto centrado con lo que esta pasando: conectando, negociando, esperando el primer
//! frame, o el motivo por el que la sesion termino. Una ventana negra es indistinguible de
//! un cuelgue y no le dice nada a quien la mira.

use std::sync::Arc;

use eframe::egui;
use eframe::wgpu;

use crate::encuadre::{Encuadre, encuadrar};
use crate::sesion::{Compartido, Estado};

/// Aplicacion del viewer.
pub struct App {
    sesion: Arc<Compartido>,
}

impl App {
    /// Crea la aplicacion sobre una sesion ya arrancada.
    pub const fn nueva(sesion: Arc<Compartido>) -> Self {
        Self { sesion }
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

        let marco = egui::Frame::NONE.fill(egui::Color32::BLACK);
        egui::CentralPanel::default().frame(marco).show(ctx, |ui| {
            match (&estado, self.sesion.dimensiones()) {
                (Estado::Activa, Some(video)) => self.pintar_video(ui, video),
                _ => pintar_estado(ui, &estado),
            }
        });

        // No se pide repintado periodico a proposito. Cada cambio de estado y cada frame
        // nuevo ya llaman a `request_repaint` desde el hilo de sesion, asi que despertar la
        // interfaz por reloj solo gastaria bateria mostrando lo mismo.
    }
}

impl App {
    fn pintar_video(&self, ui: &mut egui::Ui, video: (u32, u32)) {
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
    }
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

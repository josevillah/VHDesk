//! El puntero remoto, dibujado encima del video.
//!
//! # Se dibuja donde dice el host, no donde esta el raton local
//!
//! La tentacion es pintarlo bajo el puntero local, que siempre esta perfectamente
//! sincronizado y nunca se ve raro. Seria mentir: el cursor que importa es el de la maquina
//! remota, y si por lo que sea no coinciden —el host recorto la posicion, una aplicacion
//! remota movio el puntero por su cuenta, el evento se perdio— eso es exactamente lo que
//! hay que ver. Un cursor que siempre parece correcto oculta el unico sintoma de que el
//! camino de entrada esta fallando.
//!
//! Por eso la posicion sale de los datagramas de `Cursor::Position` y la imagen de los
//! mensajes de control de `Cursor::Shape`.
//!
//! # Este si pasa por el teselador de egui
//!
//! El video lo evita por latencia; el cursor no tiene por que. Son unos pocos miles de
//! pixeles una vez por frame, ya esta en una textura, y montarle un pipeline de wgpu propio
//! para dibujar un cuadrado seria complicar el codigo para ahorrar algo que no se mide.

use std::sync::Arc;

use eframe::egui;

use crate::encuadre::encuadrar;
use crate::sesion::Compartido;

/// La textura del puntero y la version de la forma con la que se construyo.
pub struct CursorPintado {
    textura: Option<egui::TextureHandle>,
    /// Version de [`Compartido::version_forma`] que hay subida ahora mismo.
    version: u64,
}

impl CursorPintado {
    /// Sin ninguna forma todavia.
    pub const fn nuevo() -> Self {
        Self {
            textura: None,
            // Empieza en cero igual que el contador, asi que mientras no llegue ninguna
            // forma no hay nada que rehacer ni nada que pintar.
            version: 0,
        }
    }

    /// Dibuja el puntero remoto sobre el rectangulo donde se esta pintando el video.
    ///
    /// `panel` es el rectangulo del panel en **puntos**; el encuadre se calcula en pixeles
    /// fisicos, que es la unidad en la que trabaja [`crate::encuadre`], y el resultado se
    /// devuelve a puntos para egui.
    pub fn pintar(
        &mut self,
        ui: &egui::Ui,
        sesion: &Arc<Compartido>,
        video: (u32, u32),
        panel: egui::Rect,
    ) {
        let Some(cursor) = sesion.cursor().filter(|c| c.visible) else {
            return;
        };

        self.refrescar(ui.ctx(), sesion);
        let Some(textura) = self.textura.as_ref() else {
            // La posicion llega por datagrama y la forma por control, asi que es normal
            // conocer una sin la otra durante los primeros milisegundos de sesion. Dibujar
            // un cuadrado de relleno seria peor que no dibujar nada.
            return;
        };

        let ppp = ui.ctx().pixels_per_point();
        let encuadre = encuadrar(video, (panel.width() * ppp, panel.height() * ppp));
        if encuadre.esta_vacio() {
            return;
        }

        // Escala del video remoto a la ventana. Es la misma en los dos ejes porque el
        // encuadre conserva la proporcion, pero se calcula con el ancho para no dar por
        // supuesto lo que ya garantiza otro modulo.
        let escala = encuadre.ancho / video.0 as f32;
        let tamano = textura.size_vec2() * escala / ppp;

        // La posicion viene normalizada al monitor remoto, asi que basta interpolarla sobre
        // el encuadre. El punto activo se resta ya escalado: la punta de la flecha tiene que
        // caer donde esta el cursor, no la esquina de su imagen.
        let (ancho_forma, alto_forma) = sesion
            .con_forma(|f| (f.hotspot_x as f32, f.hotspot_y as f32))
            .unwrap_or((0.0, 0.0));

        let x =
            panel.left() + (encuadre.x + cursor.x * encuadre.ancho - ancho_forma * escala) / ppp;
        let y = panel.top() + (encuadre.y + cursor.y * encuadre.alto - alto_forma * escala) / ppp;

        let destino = egui::Rect::from_min_size(egui::pos2(x, y), tamano);

        ui.painter().image(
            textura.id(),
            destino,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    }

    /// Rehace la textura si ha llegado una forma nueva.
    fn refrescar(&mut self, ctx: &egui::Context, sesion: &Arc<Compartido>) {
        let version = sesion.version_forma();
        if version == self.version && self.textura.is_some() {
            return;
        }

        let Some(imagen) = sesion.con_forma(|forma| {
            let ancho = forma.width as usize;
            let alto = forma.height as usize;

            // El host manda `width * height * 4`, pero viene de la red: si no cuadra, no se
            // construye la imagen en vez de indexar fuera de rango.
            (forma.rgba.len() == ancho * alto * 4 && ancho > 0 && alto > 0).then(|| {
                // Alfa sin premultiplicar, que es lo que produce la conversion de
                // `vhdesk-capture`.
                egui::ColorImage::from_rgba_unmultiplied([ancho, alto], &forma.rgba)
            })
        }) else {
            return;
        };

        let Some(imagen) = imagen else {
            tracing::warn!("forma de cursor con dimensiones que no cuadran con sus pixeles");
            // Se marca como atendida igualmente: si no, se reintentaria en cada frame.
            self.version = version;
            return;
        };

        // El puntero se escala junto con el video, asi que casi nunca se dibuja a su tamano
        // nativo. `Nearest` conserva los bordes duros del cursor de Windows, que con
        // filtrado lineal se ven emborronados y delatan que es una imagen escalada.
        self.textura =
            Some(ctx.load_texture("cursor-remoto", imagen, egui::TextureOptions::NEAREST));
        self.version = version;
    }
}

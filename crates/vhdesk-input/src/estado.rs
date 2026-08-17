//! Registro de lo que esta pulsado ahora mismo.
//!
//! # El problema de las teclas pegadas
//!
//! Es el fallo mas grave que puede tener este crate. Si el viewer suelta la conexion,
//! pierde el foco o se cierra **con una tecla pulsada**, el host se queda con esa tecla
//! hundida para siempre: nadie va a mandar nunca el evento de soltarla.
//!
//! Con una letra es molesto. Con Ctrl, Alt o Mayus la maquina remota queda practicamente
//! inservible —cada clic es un clic con Ctrl, cada tecla un atajo— y el usuario no tiene
//! forma de relacionarlo con lo que paso, porque el sintoma aparece despues de que la
//! sesion se cerrara.
//!
//! Por eso el injector lleva la cuenta de todo lo que hunde, y [`RegistroPulsaciones`]
//! sabe soltarlo entero.
//!
//! Este modulo es puro: **devuelve** la lista de lo que hay que soltar en lugar de
//! soltarlo. El injector la ejecuta. Asi la parte delicada se testea sin tocar el sistema.

use std::collections::BTreeSet;

use vhdesk_proto::MouseButton;

/// Algo que hay que soltar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liberacion {
    /// Tecla, identificada por su usage ID de HID.
    Tecla(u32),
    /// Boton del raton.
    Boton(MouseButton),
}

/// Lo que el injector tiene hundido en este momento.
#[derive(Debug, Default)]
pub struct RegistroPulsaciones {
    // Ordenado para que `liberar_todo` produzca siempre la misma secuencia: un orden que
    // cambia entre ejecuciones convierte un fallo reproducible en uno intermitente.
    teclas: BTreeSet<u32>,
    botones: Vec<MouseButton>,
}

impl RegistroPulsaciones {
    /// Registro vacio.
    pub const fn nuevo() -> Self {
        Self {
            teclas: BTreeSet::new(),
            botones: Vec::new(),
        }
    }

    /// Anota que una tecla se pulso o se solto.
    pub fn anotar_tecla(&mut self, hid: u32, pulsada: bool) {
        if pulsada {
            self.teclas.insert(hid);
        } else {
            self.teclas.remove(&hid);
        }
    }

    /// Anota que un boton se pulso o se solto.
    pub fn anotar_boton(&mut self, boton: MouseButton, pulsado: bool) {
        if pulsado {
            if !self.botones.contains(&boton) {
                self.botones.push(boton);
            }
        } else {
            self.botones.retain(|b| *b != boton);
        }
    }

    /// Numero de teclas hundidas.
    pub fn teclas_pulsadas(&self) -> usize {
        self.teclas.len()
    }

    /// Numero de botones hundidos.
    pub fn botones_pulsados(&self) -> usize {
        self.botones.len()
    }

    /// Si no hay nada hundido.
    pub fn esta_vacio(&self) -> bool {
        self.teclas.is_empty() && self.botones.is_empty()
    }

    /// Vacia el registro y devuelve todo lo que habia que soltar.
    ///
    /// Los botones van primero: soltar un boton mientras hay un modificador hundido es
    /// menos danino que al reves, porque un clic con Ctrl residual puede seleccionar o
    /// abrir cosas.
    pub fn liberar_todo(&mut self) -> Vec<Liberacion> {
        let mut acciones = Vec::with_capacity(self.teclas.len() + self.botones.len());

        acciones.extend(self.botones.drain(..).map(Liberacion::Boton));
        acciones.extend(
            std::mem::take(&mut self.teclas)
                .into_iter()
                .map(Liberacion::Tecla),
        );

        acciones
    }
}

#[cfg(test)]
mod tests {
    use super::{Liberacion, RegistroPulsaciones};
    use vhdesk_proto::MouseButton;

    #[test]
    fn liberar_todo_vacia_el_registro_y_devuelve_lo_hundido() {
        let mut registro = RegistroPulsaciones::nuevo();

        // Ctrl izquierdo, Mayus izquierdo y la letra a.
        registro.anotar_tecla(0xE0, true);
        registro.anotar_tecla(0xE1, true);
        registro.anotar_tecla(0x04, true);
        registro.anotar_boton(MouseButton::Left, true);

        assert_eq!(registro.teclas_pulsadas(), 3);
        assert_eq!(registro.botones_pulsados(), 1);

        let acciones = registro.liberar_todo();

        assert!(
            registro.esta_vacio(),
            "tras liberar no puede quedar nada hundido"
        );
        assert_eq!(acciones.len(), 4);
        assert!(acciones.contains(&Liberacion::Boton(MouseButton::Left)));
        for hid in [0x04, 0xE0, 0xE1] {
            assert!(
                acciones.contains(&Liberacion::Tecla(hid)),
                "falta la liberacion de HID 0x{hid:02x}"
            );
        }
    }

    #[test]
    fn los_botones_se_sueltan_antes_que_los_modificadores() {
        let mut registro = RegistroPulsaciones::nuevo();
        registro.anotar_tecla(0xE0, true);
        registro.anotar_boton(MouseButton::Left, true);

        let acciones = registro.liberar_todo();

        assert!(
            matches!(acciones.first(), Some(Liberacion::Boton(_))),
            "soltar un boton con un modificador todavia hundido es menos danino que al reves"
        );
    }

    #[test]
    fn soltar_una_tecla_la_quita_del_registro() {
        let mut registro = RegistroPulsaciones::nuevo();

        registro.anotar_tecla(0x04, true);
        registro.anotar_tecla(0x04, false);

        assert!(registro.esta_vacio());
        assert!(registro.liberar_todo().is_empty());
    }

    #[test]
    fn pulsar_dos_veces_la_misma_tecla_solo_genera_una_liberacion() {
        // La repeticion automatica del teclado manda muchos `down` seguidos sin `up`.
        let mut registro = RegistroPulsaciones::nuevo();

        for _ in 0..10 {
            registro.anotar_tecla(0x04, true);
            registro.anotar_boton(MouseButton::Right, true);
        }

        assert_eq!(registro.teclas_pulsadas(), 1);
        assert_eq!(registro.botones_pulsados(), 1);
        assert_eq!(registro.liberar_todo().len(), 2);
    }

    #[test]
    fn un_up_sin_down_previo_no_ensucia_el_registro() {
        let mut registro = RegistroPulsaciones::nuevo();

        registro.anotar_tecla(0x04, false);
        registro.anotar_boton(MouseButton::Middle, false);

        assert!(registro.esta_vacio());
    }

    #[test]
    fn el_orden_de_liberacion_es_estable() {
        // Dos registros con las mismas teclas anotadas en orden distinto deben liberar en
        // el mismo orden: si no, un fallo reproducible se vuelve intermitente.
        let mut uno = RegistroPulsaciones::nuevo();
        let mut otro = RegistroPulsaciones::nuevo();

        for hid in [0xE0, 0x04, 0x1D, 0xE2] {
            uno.anotar_tecla(hid, true);
        }
        for hid in [0xE2, 0x1D, 0x04, 0xE0] {
            otro.anotar_tecla(hid, true);
        }

        assert_eq!(uno.liberar_todo(), otro.liberar_todo());
    }
}

//! Encaje del video en la ventana, y traduccion de coordenadas de vuelta.
//!
//! Dos problemas que son el mismo visto del derecho y del reves:
//!
//! - **Pintar**: la ventana casi nunca tiene la proporcion del escritorio remoto, asi que
//!   el video se escala conservando su relacion de aspecto y queda centrado, con bandas
//!   negras arriba y abajo o a los lados. Deformarlo para llenar la ventana seria peor:
//!   los circulos salen ovalados y el texto se lee mal.
//! - **Apuntar**: cuando el usuario mueve el raton sobre la ventana hay que saber a que
//!   pixel del escritorio remoto corresponde, y si el puntero esta sobre el video o sobre
//!   una banda negra. Un clic en la banda no debe mandarse: recortado al borde caeria justo
//!   donde estan la X de cerrar y las esquinas activas.
//!
//! Modulo puro y sin dependencias graficas: se testea entero en CI, incluso donde no hay
//! GPU. Es tambien el contrato con `vhdesk-input`, que recibe pixeles fisicos del escritorio
//! remoto y **no** coordenadas de esta ventana.

/// Rectangulo donde se pinta el video, en pixeles fisicos de la ventana.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Encuadre {
    /// Borde izquierdo.
    pub x: f32,
    /// Borde superior.
    pub y: f32,
    /// Anchura.
    pub ancho: f32,
    /// Altura.
    pub alto: f32,
}

impl Encuadre {
    /// Rectangulo vacio, para cuando aun no hay video o la ventana esta minimizada.
    pub const VACIO: Self = Self {
        x: 0.0,
        y: 0.0,
        ancho: 0.0,
        alto: 0.0,
    };

    /// Si el rectangulo no cubre ningun pixel.
    pub fn esta_vacio(&self) -> bool {
        self.ancho <= 0.0 || self.alto <= 0.0
    }

    /// Si un punto de la ventana cae dentro del video.
    ///
    /// El borde derecho e inferior quedan **excluidos**: con `ancho` de 800, las x validas
    /// van de 0 a 799,99. Incluirlos daria un pixel remoto fuera de rango justo en el
    /// borde, que es donde mas se hace clic.
    pub fn contiene(&self, x: f32, y: f32) -> bool {
        !self.esta_vacio()
            && x >= self.x
            && y >= self.y
            && x < self.x + self.ancho
            && y < self.y + self.alto
    }
}

/// Calcula donde pintar un video dentro de una ventana, conservando la proporcion.
///
/// Devuelve [`Encuadre::VACIO`] si alguna medida es cero o no es finita, que ocurre de
/// verdad: una ventana minimizada reporta tamano cero, y un `NaN` se cuela en cuanto
/// alguien divide antes de comprobar.
pub fn encuadrar(video: (u32, u32), ventana: (f32, f32)) -> Encuadre {
    let (ancho_video, alto_video) = (video.0 as f32, video.1 as f32);
    let (ancho_ventana, alto_ventana) = ventana;

    let medidas = [ancho_video, alto_video, ancho_ventana, alto_ventana];
    if medidas.iter().any(|m| !m.is_finite() || *m <= 0.0) {
        return Encuadre::VACIO;
    }

    // La escala es la menor de las dos: la que hace que quepa por el lado mas ajustado.
    let escala = (ancho_ventana / ancho_video).min(alto_ventana / alto_video);

    let ancho = ancho_video * escala;
    let alto = alto_video * escala;

    Encuadre {
        x: (ancho_ventana - ancho) / 2.0,
        y: (alto_ventana - alto) / 2.0,
        ancho,
        alto,
    }
}

/// Traduce un punto de la ventana al pixel del escritorio remoto que le corresponde.
///
/// Devuelve `None` si el punto cae fuera del video, sobre una banda negra. Quien llama debe
/// **no enviar nada** en ese caso, en lugar de recortar al borde: recortar convertiria un
/// movimiento sobre la banda en clics en el borde del escritorio remoto.
///
/// El resultado esta acotado a `0..=ancho-1` y `0..=alto-1`, asi que nunca senala un pixel
/// que no exista.
pub fn a_pixel_remoto(
    punto: (f32, f32),
    encuadre: &Encuadre,
    video: (u32, u32),
) -> Option<(i32, i32)> {
    if !encuadre.contiene(punto.0, punto.1) || video.0 == 0 || video.1 == 0 {
        return None;
    }

    let proporcion_x = (punto.0 - encuadre.x) / encuadre.ancho;
    let proporcion_y = (punto.1 - encuadre.y) / encuadre.alto;

    // Se multiplica por el numero de pixeles y se trunca, no por `ancho - 1`: asi cada
    // pixel remoto recibe una franja de la ventana del mismo tamano. Con `ancho - 1` el
    // primero y el ultimo recibirian media franja cada uno.
    let x = (proporcion_x * video.0 as f32) as i32;
    let y = (proporcion_y * video.1 as f32) as i32;

    Some((
        x.clamp(0, video.0 as i32 - 1),
        y.clamp(0, video.1 as i32 - 1),
    ))
}

#[cfg(test)]
mod tests {
    use super::{Encuadre, a_pixel_remoto, encuadrar};

    const VIDEO: (u32, u32) = (1920, 1080);

    /// Compara con tolerancia: hay division en coma flotante por el medio.
    fn casi(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn una_ventana_de_la_misma_proporcion_no_deja_bandas() {
        let e = encuadrar(VIDEO, (960.0, 540.0));

        assert!(casi(e.x, 0.0) && casi(e.y, 0.0), "no deberia haber bandas");
        assert!(casi(e.ancho, 960.0) && casi(e.alto, 540.0));
    }

    #[test]
    fn una_ventana_mas_alta_deja_bandas_arriba_y_abajo() {
        // 960x1000 para un video 16:9: sobra alto.
        let e = encuadrar(VIDEO, (960.0, 1000.0));

        assert!(casi(e.ancho, 960.0), "deberia llenar el ancho");
        assert!(casi(e.alto, 540.0), "el alto lo fija la proporcion");
        assert!(casi(e.x, 0.0), "sin bandas laterales");
        assert!(casi(e.y, 230.0), "centrado en vertical: (1000-540)/2");
    }

    #[test]
    fn una_ventana_mas_ancha_deja_bandas_a_los_lados() {
        let e = encuadrar(VIDEO, (2000.0, 540.0));

        assert!(casi(e.alto, 540.0), "deberia llenar el alto");
        assert!(casi(e.ancho, 960.0));
        assert!(casi(e.y, 0.0), "sin bandas arriba ni abajo");
        assert!(casi(e.x, 520.0), "centrado en horizontal: (2000-960)/2");
    }

    #[test]
    fn la_proporcion_se_conserva_siempre() {
        let proporcion_video = VIDEO.0 as f32 / VIDEO.1 as f32;

        for ventana in [(300.0, 900.0), (1600.0, 200.0), (1234.0, 567.0), (7.0, 5.0)] {
            let e = encuadrar(VIDEO, ventana);
            let proporcion = e.ancho / e.alto;
            assert!(
                (proporcion - proporcion_video).abs() < 0.001,
                "la ventana {ventana:?} deformo el video: {proporcion} contra {proporcion_video}"
            );
        }
    }

    #[test]
    fn el_video_nunca_se_sale_de_la_ventana() {
        for ventana in [(300.0, 900.0), (1600.0, 200.0), (1234.0, 567.0)] {
            let e = encuadrar(VIDEO, ventana);
            assert!(e.x >= -0.01 && e.y >= -0.01, "encuadre negativo: {e:?}");
            assert!(
                e.x + e.ancho <= ventana.0 + 0.01 && e.y + e.alto <= ventana.1 + 0.01,
                "el video se sale de la ventana: {e:?} en {ventana:?}"
            );
        }
    }

    #[test]
    fn una_ventana_degenerada_da_encuadre_vacio_sin_dividir_por_cero() {
        for ventana in [(0.0, 500.0), (500.0, 0.0), (f32::NAN, 500.0), (-10.0, 5.0)] {
            let e = encuadrar(VIDEO, ventana);
            assert!(e.esta_vacio(), "la ventana {ventana:?} dio {e:?}");
        }
        assert!(encuadrar((0, 1080), (800.0, 600.0)).esta_vacio());
    }

    #[test]
    fn las_esquinas_del_encuadre_mapean_a_las_esquinas_del_video() {
        let e = encuadrar(VIDEO, (960.0, 1000.0));

        assert_eq!(a_pixel_remoto((e.x, e.y), &e, VIDEO), Some((0, 0)));

        // El borde derecho e inferior estan excluidos, asi que se prueba justo dentro.
        let dentro = (e.x + e.ancho - 0.001, e.y + e.alto - 0.001);
        assert_eq!(
            a_pixel_remoto(dentro, &e, VIDEO),
            Some((1919, 1079)),
            "el ultimo pixel remoto tiene que ser alcanzable desde la ventana"
        );
    }

    #[test]
    fn el_centro_de_la_ventana_cae_en_el_centro_del_video() {
        let e = encuadrar(VIDEO, (960.0, 1000.0));
        let centro = (e.x + e.ancho / 2.0, e.y + e.alto / 2.0);

        let (x, y) = a_pixel_remoto(centro, &e, VIDEO).expect("el centro esta dentro");
        assert!((x - 960).abs() <= 1, "x del centro: {x}");
        assert!((y - 540).abs() <= 1, "y del centro: {y}");
    }

    #[test]
    fn un_punto_sobre_la_banda_negra_no_se_traduce() {
        // Ventana mas alta que el video: bandas arriba y abajo.
        let e = encuadrar(VIDEO, (960.0, 1000.0));

        for punto in [(480.0, 10.0), (480.0, 990.0), (480.0, e.y - 0.1)] {
            assert_eq!(
                a_pixel_remoto(punto, &e, VIDEO),
                None,
                "el punto {punto:?} esta sobre una banda y no debe traducirse: recortarlo al \
                 borde convertiria el movimiento en clics en la esquina del remoto"
            );
        }
    }

    #[test]
    fn contiene_excluye_el_borde_derecho_e_inferior() {
        let e = Encuadre {
            x: 10.0,
            y: 20.0,
            ancho: 100.0,
            alto: 50.0,
        };

        assert!(e.contiene(10.0, 20.0), "la esquina de inicio esta dentro");
        assert!(e.contiene(109.9, 69.9));
        assert!(!e.contiene(110.0, 45.0), "el borde derecho esta excluido");
        assert!(!e.contiene(50.0, 70.0), "el borde inferior esta excluido");
        assert!(!e.contiene(9.9, 45.0));
    }

    #[test]
    fn ningun_punto_de_dentro_produce_un_pixel_fuera_de_rango() {
        let e = encuadrar(VIDEO, (1234.0, 567.0));

        // Barrido por la diagonal del rectangulo. El `clamp` taparia un desbordamiento,
        // asi que lo que comprueba este test es que no haga falta taparlo.
        for i in 0..=100 {
            let t = i as f32 / 100.0;
            let punto = (e.x + t * (e.ancho - 0.001), e.y + t * (e.alto - 0.001));
            let (x, y) = a_pixel_remoto(punto, &e, VIDEO).expect("dentro del encuadre");
            assert!(
                (0..1920).contains(&x) && (0..1080).contains(&y),
                "({x}, {y})"
            );
        }
    }
}

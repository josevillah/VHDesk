//! Normalizacion de coordenadas al rango que espera `SendInput`.
//!
//! Con `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`, Windows interpreta las
//! coordenadas como un valor de 0 a 65535 repartido sobre el escritorio virtual completo,
//! no sobre un monitor. Este modulo hace esa conversion y **no toca ninguna API**, de forma
//! que se puede testear en cualquier plataforma.
//!
//! # La trampa del denominador
//!
//! La formula ingenua es `x * 65535 / ancho`, y esta mal. Con un escritorio de 1920 de
//! ancho, el ultimo pixel (el 1919) daria `1919 * 65535 / 1920 = 65500`, que Windows
//! traduce de vuelta a un pixel que **no es el ultimo**. El borde derecho se vuelve
//! inalcanzable.
//!
//! El denominador correcto es `ancho - 1`: hay 1920 posiciones pero 1919 intervalos entre
//! la primera y la ultima. Asi el pixel 0 da 0 y el 1919 da 65535 exactos.
//!
//! Importa mas de lo que parece porque justo en los bordes viven el boton de inicio, la X
//! de cerrar ventana y las esquinas activas, que es donde la gente hace clic todo el rato.

/// Geometria del escritorio virtual, en pixeles fisicos.
///
/// El origen **puede ser negativo**: si hay un monitor a la izquierda del principal, su
/// esquina superior izquierda tiene coordenadas negativas en el escritorio virtual.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EscritorioVirtual {
    /// Coordenada X de la esquina superior izquierda.
    pub x: i32,
    /// Coordenada Y de la esquina superior izquierda.
    pub y: i32,
    /// Anchura total en pixeles.
    pub ancho: i32,
    /// Altura total en pixeles.
    pub alto: i32,
}

/// Coordenada normalizada al rango 0..=65535 que espera `SendInput`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Normalizada {
    /// Componente horizontal.
    pub x: i32,
    /// Componente vertical.
    pub y: i32,
}

/// Convierte un punto en pixeles fisicos del escritorio virtual al rango 0..=65535.
///
/// Los puntos fuera del escritorio se recortan al borde en lugar de rechazarse: un viewer
/// puede mandar coordenadas de un pixel de mas por redondeo, y recortarlas es mas util que
/// perder el evento.
pub fn normalizar(x: i32, y: i32, escritorio: &EscritorioVirtual) -> Normalizada {
    Normalizada {
        x: normalizar_eje(x, escritorio.x, escritorio.ancho),
        y: normalizar_eje(y, escritorio.y, escritorio.alto),
    }
}

fn normalizar_eje(valor: i32, origen: i32, tamano: i32) -> i32 {
    // Un escritorio de un solo pixel no tiene intervalos sobre los que repartir; sin este
    // caso la division de abajo seria entre cero.
    if tamano <= 1 {
        return 0;
    }

    let intervalos = i64::from(tamano) - 1;
    let desplazado = i64::from(valor) - i64::from(origen);

    // En i64 porque en un escritorio multimonitor grande el producto se acerca al limite
    // de i32: 15359 * 65535 ya es mil millones.
    //
    // La suma de medio intervalo antes de dividir redondea al mas cercano en vez de
    // truncar, que es lo que reparte el error de forma simetrica en el centro.
    let escalado = (desplazado * 65535 + intervalos / 2) / intervalos;

    escalado.clamp(0, 65535) as i32
}

/// Geometria de un monitor concreto dentro del escritorio virtual, en pixeles fisicos.
///
/// Es lo que el host necesita para interpretar las coordenadas normalizadas que le llegan
/// del viewer: el protocolo las manda relativas **a un monitor**, no al escritorio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorFisico {
    /// Coordenada X de la esquina superior izquierda en el escritorio virtual.
    pub x: i32,
    /// Coordenada Y de la esquina superior izquierda en el escritorio virtual.
    pub y: i32,
    /// Anchura en pixeles.
    pub ancho: u32,
    /// Altura en pixeles.
    pub alto: u32,
}

/// Convierte una coordenada normalizada `0.0..=1.0` a pixel fisico del escritorio virtual.
///
/// Es el inverso de [`normalizar`] y comparte su trampa: el denominador es `ancho - 1`, no
/// `ancho`. Con el denominador ingenuo, `1.0` daria 1920 en un monitor de 1920 de ancho, o
/// sea un pixel **fuera** del monitor; el ultimo pixel real es el 1919. Ahi viven la X de
/// cerrar ventana y las esquinas activas.
///
/// Los valores vienen de la red, asi que se tratan como hostiles: fuera de rango se
/// recortan al borde y los no finitos (`NaN`, infinitos) se llevan al origen. Nunca entra
/// en panico ni devuelve un punto fuera del monitor.
pub fn a_pixeles(x: f32, y: f32, monitor: &MonitorFisico) -> (i32, i32) {
    (
        a_pixeles_eje(x, monitor.x, monitor.ancho),
        a_pixeles_eje(y, monitor.y, monitor.alto),
    )
}

fn a_pixeles_eje(valor: f32, origen: i32, tamano: u32) -> i32 {
    // `clamp` propaga NaN en vez de acotarlo, asi que hay que descartarlo antes. Un NaN que
    // llegara a la conversion a entero daria 0 en silencio, que es el mismo resultado, pero
    // por accidente y sin dejar constancia de que la entrada era basura.
    if !valor.is_finite() {
        return origen;
    }

    // Un monitor de un solo pixel no tiene intervalos sobre los que repartir.
    let Some(intervalos) = tamano.checked_sub(1).filter(|i| *i > 0) else {
        return origen;
    };

    let escalado = f64::from(valor.clamp(0.0, 1.0)) * f64::from(intervalos);
    // `round` antes de truncar: sin ella, 0,9999 caeria en el penultimo pixel y el borde
    // volveria a ser inalcanzable por otra via.
    let desplazamiento = escalado.round() as i64;

    origen.saturating_add(desplazamiento.clamp(0, i64::from(intervalos)) as i32)
}

#[cfg(test)]
mod tests {
    use super::{EscritorioVirtual, MonitorFisico, a_pixeles, normalizar};

    /// Un 1080p a la derecha del principal: el origen no es cero, que es donde se ve si
    /// alguien se olvido de sumarlo.
    const MONITOR: MonitorFisico = MonitorFisico {
        x: 1920,
        y: 0,
        ancho: 1920,
        alto: 1080,
    };

    #[test]
    fn los_extremos_normalizados_dan_los_pixeles_extremos() {
        assert_eq!(a_pixeles(0.0, 0.0, &MONITOR), (1920, 0));
        assert_eq!(
            a_pixeles(1.0, 1.0, &MONITOR),
            (1920 + 1919, 1079),
            "1.0 tiene que caer en el ultimo pixel del monitor, no un pixel mas alla"
        );
    }

    #[test]
    fn el_centro_normalizado_cae_en_el_centro_del_monitor() {
        let (x, y) = a_pixeles(0.5, 0.5, &MONITOR);
        assert_eq!((x, y), (1920 + 960, 540));
    }

    #[test]
    fn normalizar_y_desnormalizar_son_inversos_en_un_monitor_en_el_origen() {
        // La ida y vuelta completa: pixel -> normalizado -> pixel. Si los dos denominadores
        // no coincidieran, el error se acumularia hacia los bordes.
        let monitor = MonitorFisico {
            x: 0,
            y: 0,
            ancho: 1920,
            alto: 1080,
        };
        let escritorio = EscritorioVirtual {
            x: 0,
            y: 0,
            ancho: 1920,
            alto: 1080,
        };

        for pixel in [0, 1, 640, 959, 960, 1918, 1919] {
            let normalizado = normalizar(pixel, 0, &escritorio).x as f32 / 65535.0;
            let (vuelta, _) = a_pixeles(normalizado, 0.0, &monitor);
            assert_eq!(
                vuelta, pixel,
                "el pixel {pixel} no sobrevivio la ida y vuelta"
            );
        }
    }

    #[test]
    fn los_valores_fuera_de_rango_se_recortan_al_monitor() {
        assert_eq!(a_pixeles(-5.0, -5.0, &MONITOR), (1920, 0));
        assert_eq!(a_pixeles(9.0, 9.0, &MONITOR), (1920 + 1919, 1079));
    }

    #[test]
    fn los_valores_no_finitos_no_hacen_reventar_nada() {
        // Llegan de la red, asi que hay que suponer que alguien los mandara a proposito.
        for malo in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(a_pixeles(malo, malo, &MONITOR), (1920, 0), "con {malo}");
        }
    }

    #[test]
    fn un_monitor_degenerado_no_divide_por_cero() {
        for tamano in [0u32, 1] {
            let monitor = MonitorFisico {
                x: 7,
                y: 9,
                ancho: tamano,
                alto: tamano,
            };
            assert_eq!(a_pixeles(0.5, 0.5, &monitor), (7, 9));
        }
    }

    /// Un monitor 1080p solo, con origen en cero.
    const SIMPLE: EscritorioVirtual = EscritorioVirtual {
        x: 0,
        y: 0,
        ancho: 1920,
        alto: 1080,
    };

    /// Dos monitores con el secundario a la izquierda: el origen del escritorio virtual es
    /// negativo, que es el caso donde la resta del origen deja de ser decorativa.
    const DOBLE: EscritorioVirtual = EscritorioVirtual {
        x: -1920,
        y: 0,
        ancho: 3840,
        alto: 1080,
    };

    #[test]
    fn la_esquina_superior_izquierda_da_cero_exacto() {
        let n = normalizar(0, 0, &SIMPLE);
        assert_eq!((n.x, n.y), (0, 0));

        // Con origen negativo la esquina no es (0,0) sino (-1920, 0).
        let n = normalizar(-1920, 0, &DOBLE);
        assert_eq!((n.x, n.y), (0, 0));
    }

    #[test]
    fn la_esquina_inferior_derecha_da_65535_exacto() {
        // 1919 y 1079 son el ultimo pixel, no 1920 y 1080. Con el denominador ingenuo
        // (ancho en vez de ancho-1) aqui saldria 65500 y el borde seria inalcanzable.
        let n = normalizar(1919, 1079, &SIMPLE);
        assert_eq!(
            (n.x, n.y),
            (65535, 65535),
            "el ultimo pixel de cada borde tiene que ser alcanzable: ahi estan el boton de \
             inicio, la X de cerrar y las esquinas activas"
        );

        let n = normalizar(1919, 1079, &DOBLE);
        assert_eq!(
            n.x, 65535,
            "1919 es el ultimo pixel de un escritorio que va de -1920 a 1919"
        );
    }

    #[test]
    fn el_centro_del_escritorio_cae_en_el_centro_del_rango() {
        // Con 1920 pixeles el centro exacto es el 959,5, que no existe: los dos pixeles
        // centrales son el 959 y el 960, y el punto medio del rango (32767,5) tiene que
        // caer entre sus dos valores. Esperar que el 959 diera exactamente 32767 seria
        // ignorar que cada pixel vale 65535/1919 = 34,1 unidades.
        let izquierda = normalizar(959, 0, &SIMPLE).x;
        let derecha = normalizar(960, 0, &SIMPLE).x;

        assert!(
            izquierda < 32768 && derecha > 32767,
            "los dos pixeles centrales ({izquierda} y {derecha}) deberian rodear el medio \
             del rango"
        );

        // Y ninguno de los dos puede alejarse mas de un paso de pixel del centro.
        let paso = 65535 / 1919 + 1;
        assert!((32767 - izquierda) <= paso, "el pixel 959 dio {izquierda}");
        assert!((derecha - 32768) <= paso, "el pixel 960 dio {derecha}");
    }

    #[test]
    fn el_paso_entre_pixeles_contiguos_es_uniforme() {
        // Si el redondeo estuviera sesgado, los pasos serian irregulares y algunos pixeles
        // quedarian sin valor propio.
        let pasos: Vec<i32> = (500..520)
            .map(|x| normalizar(x, 0, &SIMPLE).x)
            .collect::<Vec<_>>()
            .windows(2)
            .map(|par| par[1] - par[0])
            .collect();

        // 65535/1919 = 34,15, asi que los pasos alternan entre 34 y 35.
        for paso in &pasos {
            assert!(
                (34..=35).contains(paso),
                "paso irregular de {paso} unidades: {pasos:?}"
            );
        }
    }

    #[test]
    fn el_reparto_es_monotono_y_no_se_salta_valores_en_los_extremos() {
        // Los dos primeros y los dos ultimos pixeles tienen que dar valores distintos y
        // crecientes: si el redondeo los colapsara, habria pixeles inalcanzables.
        let valores: Vec<i32> = [0, 1, 2, 1917, 1918, 1919]
            .iter()
            .map(|x| normalizar(*x, 0, &SIMPLE).x)
            .collect();

        for par in valores.windows(2) {
            assert!(par[0] < par[1], "valores no crecientes: {par:?}");
        }
    }

    #[test]
    fn los_puntos_fuera_del_escritorio_se_recortan() {
        assert_eq!(normalizar(-100, -100, &SIMPLE).x, 0);
        assert_eq!(normalizar(99_999, 99_999, &SIMPLE).x, 65535);
    }

    #[test]
    fn un_escritorio_degenerado_no_divide_por_cero() {
        for tamano in [0, 1] {
            let escritorio = EscritorioVirtual {
                x: 0,
                y: 0,
                ancho: tamano,
                alto: tamano,
            };
            let n = normalizar(0, 0, &escritorio);
            assert_eq!((n.x, n.y), (0, 0));
        }
    }
}

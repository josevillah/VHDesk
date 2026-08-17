//! El puntero del raton, que viaja **fuera** del frame.
//!
//! La duplicacion de escritorio no compone el puntero sobre los pixeles, y eso nos
//! conviene: mandandolo aparte, el viewer lo dibuja localmente y el cursor se mueve al
//! ritmo del raton del usuario en vez de al ritmo de los frames que llegan por la red.
//! Componerlo en el frame lo ataria a la latencia del video, que es justo lo que hace que
//! un escritorio remoto se sienta lento.
//!
//! Este modulo es puro: convierte las formas de puntero que da el sistema a RGBA y no
//! toca ninguna API. Es lo que permite testear la parte delicada sin una pantalla.

use crate::error::CaptureError;

/// Formato en el que el sistema entrega la imagen del puntero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerShapeKind {
    /// Dos mascaras de 1 bit por pixel, apiladas: primero AND y despues XOR.
    Monochrome,
    /// BGRA con canal alfa util.
    Color,
    /// BGRA donde el canal alfa no es transparencia sino una mascara: 0 significa copiar
    /// el color y 0xFF significa invertir lo que haya debajo.
    MaskedColor,
}

/// Posicion del puntero en coordenadas del monitor capturado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPosition {
    /// Coordenada horizontal.
    pub x: i32,
    /// Coordenada vertical.
    pub y: i32,
}

/// Imagen del puntero, ya convertida a RGBA sin relleno.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorShape {
    /// Anchura en pixeles.
    pub width: u32,
    /// Altura en pixeles.
    pub height: u32,
    /// Desplazamiento horizontal del punto activo dentro de la imagen.
    pub hotspot_x: u32,
    /// Desplazamiento vertical del punto activo dentro de la imagen.
    pub hotspot_y: u32,
    /// Pixeles RGBA, exactamente `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// Cambio en el puntero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorUpdate {
    /// Si el puntero se esta dibujando en el monitor capturado.
    pub visible: bool,
    /// Donde esta. Solo tiene sentido si `visible`.
    pub position: CursorPosition,
    /// Imagen nueva, si la hay.
    ///
    /// El sistema solo manda la forma cuando cambia, no en cada movimiento, asi que casi
    /// siempre es `None` y quien reciba estas actualizaciones debe cachear la ultima.
    pub shape: Option<CursorShape>,
}

/// Convierte la forma de puntero que da el sistema a RGBA.
///
/// `height` es la altura tal y como la reporta el sistema, que en el caso monocromo es el
/// doble de la altura real porque incluye las dos mascaras apiladas.
///
/// # Errores
///
/// Devuelve [`CaptureError::InvalidPointerShape`] si las dimensiones no son coherentes con
/// el tamano del buffer. Nunca entra en panico, por muy incoherentes que sean los datos.
pub fn decode_pointer_shape(
    kind: PointerShapeKind,
    raw: &[u8],
    pitch: usize,
    width: u32,
    height: u32,
    hotspot: (u32, u32),
) -> Result<CursorShape, CaptureError> {
    if width == 0 || height == 0 {
        return Err(CaptureError::InvalidPointerShape("dimensiones a cero"));
    }

    match kind {
        PointerShapeKind::Monochrome => decode_monochrome(raw, pitch, width, height, hotspot),
        PointerShapeKind::Color => decode_bgra(raw, pitch, width, height, hotspot, false),
        PointerShapeKind::MaskedColor => decode_bgra(raw, pitch, width, height, hotspot, true),
    }
}

fn decode_monochrome(
    raw: &[u8],
    pitch: usize,
    width: u32,
    reported_height: u32,
    hotspot: (u32, u32),
) -> Result<CursorShape, CaptureError> {
    if reported_height % 2 != 0 {
        return Err(CaptureError::InvalidPointerShape(
            "altura monocroma impar: deberia contener dos mascaras apiladas",
        ));
    }
    let height = reported_height / 2;
    if height == 0 {
        return Err(CaptureError::InvalidPointerShape("altura monocroma a cero"));
    }

    let bytes_por_fila = width.div_ceil(8) as usize;
    if pitch < bytes_por_fila {
        return Err(CaptureError::InvalidPointerShape(
            "pitch menor que una fila de bits",
        ));
    }
    let necesarios = pitch
        .checked_mul(reported_height as usize)
        .ok_or(CaptureError::InvalidPointerShape("tamano desbordado"))?;
    if raw.len() < necesarios {
        return Err(CaptureError::InvalidPointerShape(
            "buffer mas corto que las dos mascaras",
        ));
    }

    let mut rgba = vec![0u8; (width as usize) * (height as usize) * 4];

    for y in 0..height as usize {
        let fila_and = y * pitch;
        let fila_xor = (y + height as usize) * pitch;

        for x in 0..width as usize {
            let byte = x / 8;
            let desplazamiento = 7 - (x % 8);

            let and_bit = (raw[fila_and + byte] >> desplazamiento) & 1;
            let xor_bit = (raw[fila_xor + byte] >> desplazamiento) & 1;

            // La tabla clasica de cursores de Windows. El caso AND=1/XOR=1 pide invertir
            // lo que haya debajo, que no podemos hacer sin leer el fondo; lo aproximamos a
            // negro opaco, que es como se ve el cursor de texto sobre un fondo claro.
            let pixel: [u8; 4] = match (and_bit, xor_bit) {
                (0, 0) => [0, 0, 0, 255],
                (0, _) => [255, 255, 255, 255],
                (_, 0) => [0, 0, 0, 0],
                (_, _) => [0, 0, 0, 255],
            };

            let destino = (y * width as usize + x) * 4;
            rgba[destino..destino + 4].copy_from_slice(&pixel);
        }
    }

    Ok(CursorShape {
        width,
        height,
        hotspot_x: hotspot.0,
        hotspot_y: hotspot.1,
        rgba,
    })
}

fn decode_bgra(
    raw: &[u8],
    pitch: usize,
    width: u32,
    height: u32,
    hotspot: (u32, u32),
    enmascarado: bool,
) -> Result<CursorShape, CaptureError> {
    let bytes_por_fila = (width as usize)
        .checked_mul(4)
        .ok_or(CaptureError::InvalidPointerShape("anchura desbordada"))?;
    if pitch < bytes_por_fila {
        return Err(CaptureError::InvalidPointerShape(
            "pitch menor que una fila de pixeles",
        ));
    }
    let necesarios = pitch
        .checked_mul(height as usize)
        .ok_or(CaptureError::InvalidPointerShape("tamano desbordado"))?;
    if raw.len() < necesarios {
        return Err(CaptureError::InvalidPointerShape(
            "buffer mas corto que la imagen",
        ));
    }

    let mut rgba = vec![0u8; (width as usize) * (height as usize) * 4];

    for y in 0..height as usize {
        let origen = y * pitch;
        for x in 0..width as usize {
            let o = origen + x * 4;
            let (b, g, r, a) = (raw[o], raw[o + 1], raw[o + 2], raw[o + 3]);

            let pixel = if enmascarado {
                // Aqui el alfa es mascara, no transparencia: 0 significa "pinta este
                // color" y 0xFF significa "invierte el fondo". Como no tenemos el fondo,
                // el segundo caso se pinta opaco con su propio color.
                [r, g, b, 255]
            } else {
                [r, g, b, a]
            };

            let destino = (y * width as usize + x) * 4;
            rgba[destino..destino + 4].copy_from_slice(&pixel);
        }
    }

    Ok(CursorShape {
        width,
        height,
        hotspot_x: hotspot.0,
        hotspot_y: hotspot.1,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::{PointerShapeKind, decode_pointer_shape};
    use crate::error::CaptureError;

    #[test]
    fn la_mascara_monocroma_da_las_cuatro_combinaciones() {
        // Cursor de 8x1 real, o sea 8x2 reportado. Fila AND primero, fila XOR despues.
        // AND = 1100_0000, XOR = 1010_0000, lo que da, de izquierda a derecha:
        //   x=0: AND=1 XOR=1 -> invertir, aproximado a negro opaco
        //   x=1: AND=1 XOR=0 -> transparente
        //   x=2: AND=0 XOR=1 -> blanco
        //   x=3: AND=0 XOR=0 -> negro
        let raw = [0b1100_0000u8, 0b1010_0000];

        let forma = decode_pointer_shape(PointerShapeKind::Monochrome, &raw, 1, 8, 2, (0, 0))
            .expect("decodificar");

        assert_eq!(forma.width, 8);
        assert_eq!(
            forma.height, 1,
            "la altura real es la mitad de la reportada"
        );
        assert_eq!(&forma.rgba[0..4], &[0, 0, 0, 255]);
        assert_eq!(&forma.rgba[4..8], &[0, 0, 0, 0]);
        assert_eq!(&forma.rgba[8..12], &[255, 255, 255, 255]);
        assert_eq!(&forma.rgba[12..16], &[0, 0, 0, 255]);
    }

    #[test]
    fn el_color_respeta_el_pitch_y_reordena_a_rgba() {
        // 1x2 en BGRA con pitch de 8 bytes: 4 utiles y 4 de relleno por fila.
        let mut raw = vec![0u8; 16];
        raw[0..4].copy_from_slice(&[10, 20, 30, 40]); // B G R A
        raw[4..8].copy_from_slice(&[0xff; 4]); // relleno que no debe leerse
        raw[8..12].copy_from_slice(&[50, 60, 70, 80]);

        let forma = decode_pointer_shape(PointerShapeKind::Color, &raw, 8, 1, 2, (2, 3))
            .expect("decodificar");

        assert_eq!(forma.rgba, vec![30, 20, 10, 40, 70, 60, 50, 80]);
        assert_eq!((forma.hotspot_x, forma.hotspot_y), (2, 3));
    }

    #[test]
    fn el_color_enmascarado_sale_siempre_opaco() {
        let raw = vec![10u8, 20, 30, 0xff];

        let forma = decode_pointer_shape(PointerShapeKind::MaskedColor, &raw, 4, 1, 1, (0, 0))
            .expect("decodificar");

        assert_eq!(
            forma.rgba,
            vec![30, 20, 10, 255],
            "en MaskedColor el alfa es mascara, no transparencia"
        );
    }

    #[test]
    fn una_altura_monocroma_impar_se_rechaza() {
        let raw = vec![0u8; 8];
        assert!(matches!(
            decode_pointer_shape(PointerShapeKind::Monochrome, &raw, 1, 8, 3, (0, 0)),
            Err(CaptureError::InvalidPointerShape(_))
        ));
    }

    #[test]
    fn un_buffer_corto_se_rechaza_sin_panico() {
        // Los tres formatos, con el buffer siempre un byte por debajo de lo necesario.
        for (kind, pitch, width, height, len) in [
            (PointerShapeKind::Monochrome, 1usize, 8u32, 2u32, 1usize),
            (PointerShapeKind::Color, 4, 1, 2, 7),
            (PointerShapeKind::MaskedColor, 4, 1, 2, 7),
        ] {
            let raw = vec![0u8; len];
            assert!(
                matches!(
                    decode_pointer_shape(kind, &raw, pitch, width, height, (0, 0)),
                    Err(CaptureError::InvalidPointerShape(_))
                ),
                "{kind:?} con buffer corto deberia fallar limpiamente"
            );
        }
    }

    #[test]
    fn un_pitch_menor_que_la_fila_se_rechaza() {
        let raw = vec![0u8; 64];
        assert!(matches!(
            decode_pointer_shape(PointerShapeKind::Color, &raw, 2, 4, 2, (0, 0)),
            Err(CaptureError::InvalidPointerShape(_))
        ));
    }

    #[test]
    fn unas_dimensiones_a_cero_se_rechazan() {
        let raw = vec![0u8; 64];
        for (w, h) in [(0, 4), (4, 0)] {
            assert!(matches!(
                decode_pointer_shape(PointerShapeKind::Color, &raw, 16, w, h, (0, 0)),
                Err(CaptureError::InvalidPointerShape(_))
            ));
        }
    }
}

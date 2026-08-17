//! Copia de pixeles teniendo en cuenta el relleno de fila.
//!
//! Modulo puro y testeable en cualquier plataforma. El error que evita es siempre el
//! mismo: dar por hecho que una fila ocupa `width * 4` bytes. El sistema alinea las filas
//! de sus texturas, asi que en cuanto la resolucion no es multiplo de la alineacion, la
//! imagen sale cizallada en diagonal. Es un fallo que se ve raro y se depura mal.

use crate::error::CaptureError;

/// Copia `height` filas de `width` pixeles BGRA de `src` a `dst`.
///
/// El destino queda compactado: sus filas ocupan exactamente `width * 4` bytes, sin
/// relleno, que es lo que espera el convertidor a I420 del codificador.
///
/// # Errores
///
/// Devuelve [`CaptureError::BufferTooSmall`] si alguno de los dos buffers no da para las
/// dimensiones pedidas.
pub fn copy_frame(
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    width: u32,
    height: u32,
) -> Result<(), CaptureError> {
    let bytes_por_fila = (width as usize)
        .checked_mul(4)
        .ok_or(CaptureError::BufferTooSmall {
            needed: usize::MAX,
            available: dst.len(),
        })?;
    let alto = height as usize;

    if src_stride < bytes_por_fila {
        return Err(CaptureError::BufferTooSmall {
            needed: bytes_por_fila,
            available: src_stride,
        });
    }

    let destino_necesario =
        bytes_por_fila
            .checked_mul(alto)
            .ok_or(CaptureError::BufferTooSmall {
                needed: usize::MAX,
                available: dst.len(),
            })?;
    if dst.len() < destino_necesario {
        return Err(CaptureError::BufferTooSmall {
            needed: destino_necesario,
            available: dst.len(),
        });
    }

    // La ultima fila del origen no necesita arrastrar su relleno, asi que exigimos el
    // relleno solo en las filas intermedias. Si no, rechazariamos texturas validas cuyo
    // mapeo termina justo al acabar los pixeles utiles.
    let origen_necesario = src_stride
        .checked_mul(alto.saturating_sub(1))
        .and_then(|n| n.checked_add(bytes_por_fila))
        .ok_or(CaptureError::BufferTooSmall {
            needed: usize::MAX,
            available: src.len(),
        })?;
    if src.len() < origen_necesario {
        return Err(CaptureError::BufferTooSmall {
            needed: origen_necesario,
            available: src.len(),
        });
    }

    if src_stride == bytes_por_fila {
        // Sin relleno: una sola copia en lugar de `height` copias.
        dst[..destino_necesario].copy_from_slice(&src[..destino_necesario]);
        return Ok(());
    }

    for y in 0..alto {
        let origen = y * src_stride;
        let destino = y * bytes_por_fila;
        dst[destino..destino + bytes_por_fila]
            .copy_from_slice(&src[origen..origen + bytes_por_fila]);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::copy_frame;
    use crate::error::CaptureError;

    #[test]
    fn sin_relleno_copia_tal_cual() {
        let src: Vec<u8> = (0..32).collect();
        let mut dst = vec![0u8; 32];

        copy_frame(&src, 16, &mut dst, 4, 2).expect("copiar");

        assert_eq!(dst, src);
    }

    #[test]
    fn con_relleno_descarta_el_sobrante_de_cada_fila() {
        // 2 pixeles de ancho (8 bytes utiles) con stride de 12: 4 bytes de relleno.
        let mut src = vec![0xffu8; 24];
        src[0..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        src[12..20].copy_from_slice(&[9, 10, 11, 12, 13, 14, 15, 16]);

        let mut dst = vec![0u8; 16];
        copy_frame(&src, 12, &mut dst, 2, 2).expect("copiar");

        assert_eq!(
            dst,
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
            "el relleno de fila no debe aparecer en el destino"
        );
    }

    #[test]
    fn la_ultima_fila_no_necesita_arrastrar_su_relleno() {
        // Origen que termina justo al acabar los pixeles utiles de la ultima fila.
        let src = vec![7u8; 12 + 8];
        let mut dst = vec![0u8; 16];

        copy_frame(&src, 12, &mut dst, 2, 2).expect("copiar");

        assert!(dst.iter().all(|b| *b == 7));
    }

    #[test]
    fn un_destino_corto_se_rechaza() {
        let src = vec![0u8; 64];
        let mut dst = vec![0u8; 15];

        assert!(matches!(
            copy_frame(&src, 8, &mut dst, 2, 2),
            Err(CaptureError::BufferTooSmall { .. })
        ));
    }

    #[test]
    fn un_origen_corto_se_rechaza() {
        let src = vec![0u8; 15];
        let mut dst = vec![0u8; 16];

        assert!(matches!(
            copy_frame(&src, 8, &mut dst, 2, 2),
            Err(CaptureError::BufferTooSmall { .. })
        ));
    }

    #[test]
    fn un_stride_menor_que_la_fila_se_rechaza() {
        let src = vec![0u8; 64];
        let mut dst = vec![0u8; 64];

        assert!(matches!(
            copy_frame(&src, 4, &mut dst, 4, 2),
            Err(CaptureError::BufferTooSmall { .. })
        ));
    }
}

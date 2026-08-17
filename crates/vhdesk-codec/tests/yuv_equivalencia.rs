//! Comprueba que el crate `yuv` produce el mismo color que nuestra conversion escalar.
//!
//! Es la mitad del criterio de adopcion: por rapida que sea, una conversion que no
//! coincida en matriz y rango con la que espera libvpx da una imagen con los negros
//! lavados o aplastados, y ese fallo se diagnostica fatal porque no se parece a un error
//! de codigo sino a "los colores estan raros".

use vhdesk_codec::I420Frame;
use yuv::{
    YuvChromaSubsampling, YuvConversionMode, YuvPlanarImageMut, YuvRange, YuvStandardMatrix,
    bgra_to_yuv420,
};

/// Convierte con el crate `yuv` en la misma configuracion que usa la implementacion propia.
fn convertir_con_yuv(
    bgra: &[u8],
    width: u32,
    height: u32,
    stride: u32,
) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut destino = YuvPlanarImageMut::<u8>::alloc(width, height, YuvChromaSubsampling::Yuv420);

    bgra_to_yuv420(
        &mut destino,
        bgra,
        stride,
        YuvRange::Limited,
        YuvStandardMatrix::Bt601,
        YuvConversionMode::Balanced,
    )
    .expect("convertir con yuv");

    (
        destino.y_plane.borrow().to_vec(),
        destino.u_plane.borrow().to_vec(),
        destino.v_plane.borrow().to_vec(),
    )
}

fn liso(width: u32, height: u32, b: u8, g: u8, r: u8) -> Vec<u8> {
    let mut datos = Vec::with_capacity((width * height * 4) as usize);
    for _ in 0..width * height {
        datos.extend_from_slice(&[b, g, r, 255]);
    }
    datos
}

#[test]
fn yuv_da_los_mismos_valores_de_referencia_bt601() {
    // Los mismos que fija el test de la implementacion propia.
    //                       B    G    R      Y    U    V
    let referencias: &[(u8, u8, u8, u8, u8, u8)] = &[
        (0, 0, 0, 16, 128, 128),
        (255, 255, 255, 235, 128, 128),
        (0, 0, 255, 82, 90, 240),
        (0, 255, 0, 144, 54, 34),
        (255, 0, 0, 41, 240, 110),
    ];

    // BT.601 exacto en coma flotante, para saber cual de las dos implementaciones se
    // acerca mas cuando difieran:
    //   Y  = 16  + (65.481 R + 128.553 G +  24.966 B) / 255
    //   Cb = 128 + (-37.797 R -  74.203 G + 112.000 B) / 255
    //   Cr = 128 + (112.000 R -  93.786 G -  18.214 B) / 255
    let exacto = |r: u8, g: u8, b: u8| {
        let (r, g, b) = (f64::from(r), f64::from(g), f64::from(b));
        (
            16.0 + (65.481 * r + 128.553 * g + 24.966 * b) / 255.0,
            128.0 + (-37.797 * r - 74.203 * g + 112.0 * b) / 255.0,
            128.0 + (112.0 * r - 93.786 * g - 18.214 * b) / 255.0,
        )
    };

    let mut discrepancias = Vec::new();

    for (b, g, r, y_nuestro, u_nuestro, v_nuestro) in referencias.iter().copied() {
        let bgra = liso(2, 2, b, g, r);
        let (y, u, v) = convertir_con_yuv(&bgra, 2, 2, 8);
        let (ye, ue, ve) = exacto(r, g, b);

        println!(
            "BGR({b:>3},{g:>3},{r:>3})  nuestro Y={y_nuestro:>3} U={u_nuestro:>3} V={v_nuestro:>3}  \
             yuv Y={:>3} U={:>3} V={:>3}  exacto Y={ye:>7.3} U={ue:>7.3} V={ve:>7.3}",
            y[0], u[0], v[0]
        );

        if (y[0], u[0], v[0]) != (y_nuestro, u_nuestro, v_nuestro) {
            discrepancias.push((b, g, r));
        }

        // Lo que de verdad importa: que ambas esten dentro de una unidad del valor exacto.
        // Una diferencia mayor significaria matriz o rango distintos.
        for (obtenido, esperado, canal) in [(y[0], ye, 'Y'), (u[0], ue, 'U'), (v[0], ve, 'V')] {
            let error = (f64::from(obtenido) - esperado).abs();
            assert!(
                error <= 1.0,
                "yuv se aleja {error:.2} del BT.601 exacto en el canal {canal} para \
                 BGR({b},{g},{r}): matriz o rango incorrectos"
            );
        }
    }

    println!("\ndiscrepancias con nuestra tabla: {discrepancias:?}");
}

#[test]
fn yuv_y_la_implementacion_propia_coinciden_en_una_imagen_completa() {
    // Patron con bordes duros y gradientes, que es donde mas se notaria una diferencia de
    // redondeo o de submuestreo de croma.
    let (ancho, alto) = (64u32, 48u32);
    let mut bgra = vec![0u8; (ancho * alto * 4) as usize];
    for y in 0..alto {
        for x in 0..ancho {
            let p = ((y * ancho + x) * 4) as usize;
            let borde = (x / 8 + y / 8) % 2 == 0;
            bgra[p] = if borde { 20 } else { (x * 4) as u8 };
            bgra[p + 1] = if borde { 200 } else { (y * 5) as u8 };
            bgra[p + 2] = if borde { 90 } else { 255 - (x * 3) as u8 };
            bgra[p + 3] = 255;
        }
    }

    let mut propia = I420Frame::new(ancho, alto).expect("crear");
    propia
        .fill_from_bgra(&bgra, (ancho * 4) as usize)
        .expect("convertir");

    let (y, u, v) = convertir_con_yuv(&bgra, ancho, alto, ancho * 4);

    let diferencia = |a: &[u8], b: &[u8]| -> (u8, f64) {
        let mut maxima = 0u8;
        let mut suma = 0u64;
        for (x, y) in a.iter().zip(b) {
            let d = x.abs_diff(*y);
            maxima = maxima.max(d);
            suma += u64::from(d);
        }
        (maxima, suma as f64 / a.len() as f64)
    };

    let (max_y, media_y) = diferencia(propia.y(), &y);
    let (max_u, media_u) = diferencia(propia.u(), &u);
    let (max_v, media_v) = diferencia(propia.v(), &v);

    println!("diferencia maxima  Y={max_y} U={max_u} V={max_v}");
    println!("diferencia media   Y={media_y:.3} U={media_u:.3} V={media_v:.3}");

    // Se permite una unidad de diferencia por redondeo: las dos implementaciones usan la
    // misma matriz pero no tienen por que redondear igual en punto fijo. Mas de eso
    // significaria matriz o rango distintos, que es el fallo que este test busca.
    assert!(
        max_y <= 1 && max_u <= 1 && max_v <= 1,
        "diferencia mayor que el redondeo: Y={max_y} U={max_u} V={max_v}"
    );
}

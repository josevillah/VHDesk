//! Conversion de BGRA a I420.
//!
//! Los codecs de video no trabajan en RGB: quieren luminancia y crominancia separadas, con
//! la crominancia a la mitad de resolucion en cada eje (I420), porque el ojo distingue
//! mucho peor los cambios de color que los de brillo. La captura entrega BGRA, asi que esta
//! conversion esta en el camino de **todos** los frames.
//!
//! Esta en el camino de todos los frames y recorre 8,3 MiB por frame de 1080p, asi que
//! era el cuello de botella del pipeline: la implementacion escalar propia costaba 5,54 ms
//! de media y 7,71 ms de p99 a 1080p. Se sustituyo por el crate `yuv`, que hace lo mismo
//! con SIMD despachado en tiempo de ejecucion (SSE4.1, AVX2, NEON) y cuesta **0,51 ms de
//! media y 0,62 ms de p99**: doce veces mas rapido. Ver `examples/bench-yuv-simd.rs`.
//!
//! El despacho en tiempo de ejecucion importa mas de lo que parece: si la seleccion de
//! instrucciones fuese en tiempo de compilacion, un binario compilado en una maquina con
//! AVX-512 se caeria con instruccion ilegal en otra que no lo tenga.
//!
//! # Espacio de color
//!
//! BT.601 con rango limitado (Y en 16..=235, croma en 16..=240), que es lo que asumen por
//! defecto libvpx y practicamente todo el ecosistema de video de consumo. Usar rango
//! completo aqui y limitado al decodificar, o al reves, produce una imagen con los negros
//! lavados o aplastados: es un fallo que se ve como "los colores estan raros" y se depura
//! fatal, asi que el convenio queda escrito aqui y en el decodificador.
//!
//! La implementacion anterior usaba coeficientes en punto fijo de 8 bits con truncamiento,
//! y se desviaba una unidad del valor exacto en algunos colores (rojo puro daba Y=82
//! cuando el valor exacto es 81,481). `yuv` redondea al mas cercano y acierta. Los tests
//! de referencia fijan los valores exactos.

use yuv::{
    BufferStoreMut, YuvConversionMode, YuvPlanarImageMut, YuvRange, YuvStandardMatrix,
    bgra_to_yuv420,
};

use crate::error::CodecError;

/// Un frame en I420, con sus tres planos en buffers reutilizables.
///
/// Se crea una vez por sesion y se rellena en cada frame con [`I420Frame::fill_from_bgra`].
/// Igual que en la captura, el objetivo es no asignar por frame: a 60 fps, reservar y
/// liberar los tres planos de 1080p seria ~180 MB/s de trasiego inutil.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct I420Frame {
    width: u32,
    height: u32,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl I420Frame {
    /// Crea los planos para un frame de las dimensiones dadas.
    ///
    /// # Errores
    ///
    /// Devuelve [`CodecError::InvalidDimensions`] si alguna dimension es cero o el tamano
    /// resultante desborda.
    pub fn new(width: u32, height: u32) -> Result<Self, CodecError> {
        let invalidas = || CodecError::InvalidDimensions { width, height };

        if width == 0 || height == 0 {
            return Err(invalidas());
        }

        let luma = (width as usize)
            .checked_mul(height as usize)
            .ok_or_else(invalidas)?;
        let croma = (chroma_width(width) as usize)
            .checked_mul(chroma_height(height) as usize)
            .ok_or_else(invalidas)?;

        Ok(Self {
            width,
            height,
            y: vec![0; luma],
            u: vec![0; croma],
            v: vec![0; croma],
        })
    }

    /// Anchura en pixeles.
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Altura en pixeles.
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Plano de luminancia, con `width` bytes por fila.
    pub fn y(&self) -> &[u8] {
        &self.y
    }

    /// Plano U, con [`I420Frame::chroma_width`] bytes por fila.
    pub fn u(&self) -> &[u8] {
        &self.u
    }

    /// Plano V, con [`I420Frame::chroma_width`] bytes por fila.
    pub fn v(&self) -> &[u8] {
        &self.v
    }

    /// Bytes por fila de los planos de crominancia.
    ///
    /// Con anchura impar se redondea hacia arriba: el ultimo pixel de la fila forma un
    /// bloque de croma el solo.
    pub const fn chroma_width(&self) -> u32 {
        chroma_width(self.width)
    }

    /// Filas de los planos de crominancia.
    pub const fn chroma_height(&self) -> u32 {
        chroma_height(self.height)
    }

    /// Rellena los tres planos a partir de un frame BGRA.
    ///
    /// `stride` son los bytes por fila del origen, que pueden ser mas que `width * 4`.
    ///
    /// # Errores
    ///
    /// Devuelve [`CodecError::BufferSize`] si el origen no da para las dimensiones de este
    /// frame.
    pub fn fill_from_bgra(&mut self, bgra: &[u8], stride: usize) -> Result<(), CodecError> {
        let ancho = self.width as usize;
        let alto = self.height as usize;
        let bytes_por_fila = ancho * 4;

        if stride < bytes_por_fila {
            return Err(CodecError::BufferSize {
                width: self.width,
                height: self.height,
                needed: bytes_por_fila,
                actual: stride,
            });
        }

        // La ultima fila no necesita arrastrar su relleno.
        let necesarios = stride * (alto - 1) + bytes_por_fila;
        if bgra.len() < necesarios {
            return Err(CodecError::BufferSize {
                width: self.width,
                height: self.height,
                needed: necesarios,
                actual: bgra.len(),
            });
        }

        let stride_u32 = u32::try_from(stride).map_err(|_| CodecError::BufferSize {
            width: self.width,
            height: self.height,
            needed: stride,
            actual: stride,
        })?;

        // Se construye una vista sobre nuestros propios planos en lugar de dejar que `yuv`
        // asigne los suyos: asi la conversion sigue sin reservar memoria por frame, que es
        // la propiedad que tiene este tipo desde el principio.
        let mut vista = YuvPlanarImageMut {
            y_plane: BufferStoreMut::Borrowed(&mut self.y),
            y_stride: self.width,
            u_plane: BufferStoreMut::Borrowed(&mut self.u),
            u_stride: chroma_width(self.width),
            v_plane: BufferStoreMut::Borrowed(&mut self.v),
            v_stride: chroma_width(self.width),
            width: self.width,
            height: self.height,
        };

        bgra_to_yuv420(
            &mut vista,
            bgra,
            stride_u32,
            YuvRange::Limited,
            YuvStandardMatrix::Bt601,
            YuvConversionMode::Balanced,
        )
        .map_err(|error| CodecError::Backend {
            operation: "bgra_to_yuv420",
            detail: error.to_string(),
        })?;

        Ok(())
    }

    /// Copia dentro de este frame los planos de otro, que pueden venir con relleno.
    ///
    /// Existe para sacar un frame decodificado del buffer interno del decodificador, que
    /// deja de ser valido en la siguiente llamada a `decode`. El destino es reutilizable,
    /// asi que el consumidor puede rotar entre dos o tres y no asignar por frame.
    ///
    /// # Errores
    ///
    /// Devuelve [`CodecError::BufferSize`] si los planos de origen no dan para las
    /// dimensiones de este frame.
    pub fn copy_from_planes(
        &mut self,
        y: &[u8],
        u: &[u8],
        v: &[u8],
        y_stride: usize,
        uv_stride: usize,
    ) -> Result<(), CodecError> {
        let ancho = self.width as usize;
        let alto = self.height as usize;
        let croma_ancho = self.chroma_width() as usize;
        let croma_alto = self.chroma_height() as usize;

        copiar_plano(
            y,
            y_stride,
            &mut self.y,
            ancho,
            alto,
            self.width,
            self.height,
        )?;
        copiar_plano(
            u,
            uv_stride,
            &mut self.u,
            croma_ancho,
            croma_alto,
            self.width,
            self.height,
        )?;
        copiar_plano(
            v,
            uv_stride,
            &mut self.v,
            croma_ancho,
            croma_alto,
            self.width,
            self.height,
        )
    }
}

/// Copia un plano fila a fila, descartando el relleno del origen.
fn copiar_plano(
    origen: &[u8],
    origen_stride: usize,
    destino: &mut [u8],
    ancho: usize,
    alto: usize,
    width: u32,
    height: u32,
) -> Result<(), CodecError> {
    let error = |needed: usize, actual: usize| CodecError::BufferSize {
        width,
        height,
        needed,
        actual,
    };

    if origen_stride < ancho {
        return Err(error(ancho, origen_stride));
    }
    // La ultima fila del origen no necesita arrastrar su relleno.
    let necesarios = origen_stride * alto.saturating_sub(1) + ancho;
    if origen.len() < necesarios {
        return Err(error(necesarios, origen.len()));
    }
    if destino.len() < ancho * alto {
        return Err(error(ancho * alto, destino.len()));
    }

    if origen_stride == ancho {
        destino[..ancho * alto].copy_from_slice(&origen[..ancho * alto]);
        return Ok(());
    }

    for fila in 0..alto {
        let desde = fila * origen_stride;
        let hasta = fila * ancho;
        destino[hasta..hasta + ancho].copy_from_slice(&origen[desde..desde + ancho]);
    }

    Ok(())
}

const fn chroma_width(width: u32) -> u32 {
    width.div_ceil(2)
}

const fn chroma_height(height: u32) -> u32 {
    height.div_ceil(2)
}

#[cfg(test)]
mod tests {
    use super::I420Frame;
    use crate::error::CodecError;

    /// Construye un frame BGRA de un solo color.
    fn liso(width: u32, height: u32, b: u8, g: u8, r: u8) -> Vec<u8> {
        let mut datos = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            datos.extend_from_slice(&[b, g, r, 255]);
        }
        datos
    }

    #[test]
    fn el_negro_y_el_blanco_caen_en_los_extremos_del_rango_limitado() {
        let mut frame = I420Frame::new(2, 2).expect("crear");

        frame
            .fill_from_bgra(&liso(2, 2, 0, 0, 0), 8)
            .expect("convertir negro");
        assert!(
            frame.y().iter().all(|y| *y == 16),
            "el negro debe ser Y=16 en rango limitado, no Y=0"
        );
        assert!(frame.u().iter().all(|u| *u == 128));
        assert!(frame.v().iter().all(|v| *v == 128));

        frame
            .fill_from_bgra(&liso(2, 2, 255, 255, 255), 8)
            .expect("convertir blanco");
        assert!(
            frame.y().iter().all(|y| *y == 235),
            "el blanco debe ser Y=235 en rango limitado, no Y=255"
        );
        assert!(frame.u().iter().all(|u| *u == 128));
        assert!(frame.v().iter().all(|v| *v == 128));
    }

    #[test]
    fn los_primarios_dan_los_valores_de_referencia_de_bt601() {
        // Valores de la tabla de BT.601 en rango limitado con la aproximacion entera de 8
        // bits. Si este test se rompe, o se ha cambiado un coeficiente o se ha cambiado el
        // redondeo, y en ambos casos la imagen saldra con los colores desviados.
        //
        //                        B    G    R      Y   U(Cb) V(Cr)
        let referencias: &[(u8, u8, u8, u8, u8, u8)] = &[
            (0, 0, 0, 16, 128, 128),        // negro
            (255, 255, 255, 235, 128, 128), // blanco
            (0, 0, 255, 81, 90, 240),       // rojo   (exacto 81,481)
            (0, 255, 0, 145, 54, 34),       // verde  (exacto 144,553)
            (255, 0, 0, 41, 240, 110),      // azul
        ];

        let mut frame = I420Frame::new(2, 2).expect("crear");

        for (b, g, r, y, u, v) in referencias.iter().copied() {
            frame
                .fill_from_bgra(&liso(2, 2, b, g, r), 8)
                .expect("convertir");

            assert_eq!(
                (frame.y()[0], frame.u()[0], frame.v()[0]),
                (y, u, v),
                "BGR ({b}, {g}, {r})"
            );
        }
    }

    #[test]
    fn cada_primario_desvia_la_crominancia_en_su_direccion() {
        let mut frame = I420Frame::new(2, 2).expect("crear");
        let mut croma_de = |b, g, r| {
            frame
                .fill_from_bgra(&liso(2, 2, b, g, r), 8)
                .expect("color");
            (frame.u()[0], frame.v()[0])
        };

        // U es la diferencia con el azul y V la diferencia con el rojo; 128 es neutro.
        let (u_rojo, v_rojo) = croma_de(0, 0, 255);
        assert!(v_rojo > 200 && u_rojo < 128, "rojo: U={u_rojo} V={v_rojo}");

        let (u_azul, v_azul) = croma_de(255, 0, 0);
        assert!(u_azul > 200 && v_azul < 128, "azul: U={u_azul} V={v_azul}");

        // El verde no tiene eje propio: se aleja del neutro por los dos lados a la vez.
        let (u_verde, v_verde) = croma_de(0, 255, 0);
        assert!(
            u_verde < 128 && v_verde < 128,
            "verde: U={u_verde} V={v_verde}"
        );
    }

    #[test]
    fn el_verde_es_mas_luminoso_que_el_rojo_y_este_mas_que_el_azul() {
        let mut frame = I420Frame::new(2, 2).expect("crear");
        let mut luma_de = |b, g, r| {
            frame
                .fill_from_bgra(&liso(2, 2, b, g, r), 8)
                .expect("color");
            frame.y()[0]
        };

        let verde = luma_de(0, 255, 0);
        let rojo = luma_de(0, 0, 255);
        let azul = luma_de(255, 0, 0);

        assert!(
            verde > rojo && rojo > azul,
            "orden de luminancia incorrecto: verde={verde} rojo={rojo} azul={azul}"
        );
    }

    #[test]
    fn el_croma_promedia_el_bloque_de_dos_por_dos() {
        // Tablero de 2x2: blanco, negro / negro, blanco. La luminancia debe conservar el
        // detalle y la crominancia debe salir de la media, que es gris neutro.
        let mut bgra = Vec::new();
        for (b, g, r) in [(255u8, 255u8, 255u8), (0, 0, 0), (0, 0, 0), (255, 255, 255)] {
            bgra.extend_from_slice(&[b, g, r, 255]);
        }

        let mut frame = I420Frame::new(2, 2).expect("crear");
        frame.fill_from_bgra(&bgra, 8).expect("convertir");

        assert_eq!(
            frame.y(),
            &[235, 16, 16, 235],
            "la luma conserva el detalle"
        );
        assert_eq!(frame.u().len(), 1, "un solo bloque de croma");
        assert_eq!(frame.u()[0], 128);
        assert_eq!(frame.v()[0], 128);
    }

    #[test]
    fn se_respeta_el_stride_del_origen() {
        // 2x2 con 8 bytes de relleno por fila. El relleno es rojo puro: si se colara en la
        // conversion, la crominancia lo delataria.
        let mut bgra = Vec::new();
        for _ in 0..2 {
            bgra.extend_from_slice(&[255, 255, 255, 255, 255, 255, 255, 255]);
            bgra.extend_from_slice(&[0, 0, 255, 255, 0, 0, 255, 255]);
        }

        let mut frame = I420Frame::new(2, 2).expect("crear");
        frame.fill_from_bgra(&bgra, 16).expect("convertir");

        assert_eq!(frame.y(), &[235, 235, 235, 235]);
        assert_eq!(
            (frame.u()[0], frame.v()[0]),
            (128, 128),
            "el relleno de fila se ha colado en la crominancia"
        );
    }

    #[test]
    fn las_dimensiones_impares_redondean_el_croma_hacia_arriba() {
        let frame = I420Frame::new(3, 3).expect("crear");

        assert_eq!((frame.chroma_width(), frame.chroma_height()), (2, 2));
        assert_eq!(frame.y().len(), 9);
        assert_eq!(frame.u().len(), 4);
    }

    #[test]
    fn un_frame_impar_se_convierte_sin_leer_fuera_del_buffer() {
        let mut frame = I420Frame::new(3, 3).expect("crear");
        // Exactamente 3x3 pixeles, ni un byte mas: si el promediado del bloque de borde se
        // saliera del ultimo pixel, esto entraria en panico o daria BufferSize.
        frame
            .fill_from_bgra(&liso(3, 3, 10, 20, 30), 12)
            .expect("convertir");

        assert!(frame.y().iter().all(|y| *y > 16));
    }

    #[test]
    fn unas_dimensiones_a_cero_se_rechazan() {
        for (w, h) in [(0, 4), (4, 0)] {
            assert!(matches!(
                I420Frame::new(w, h),
                Err(CodecError::InvalidDimensions { .. })
            ));
        }
    }

    #[test]
    fn un_origen_corto_se_rechaza_sin_panico() {
        let mut frame = I420Frame::new(4, 4).expect("crear");

        assert!(matches!(
            frame.fill_from_bgra(&[0u8; 60], 16),
            Err(CodecError::BufferSize { .. })
        ));
    }

    #[test]
    fn un_stride_menor_que_la_fila_se_rechaza() {
        let mut frame = I420Frame::new(4, 4).expect("crear");

        assert!(matches!(
            frame.fill_from_bgra(&[0u8; 1024], 8),
            Err(CodecError::BufferSize { .. })
        ));
    }

    #[test]
    fn rellenar_dos_veces_reutiliza_los_mismos_planos() {
        let mut frame = I420Frame::new(64, 64).expect("crear");
        let direccion = frame.y().as_ptr();

        for color in [(0u8, 0u8, 0u8), (255, 255, 255)] {
            frame
                .fill_from_bgra(&liso(64, 64, color.0, color.1, color.2), 256)
                .expect("convertir");
        }

        assert_eq!(
            frame.y().as_ptr(),
            direccion,
            "la conversion no debe reasignar los planos entre frames"
        );
    }
}

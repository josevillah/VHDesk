//! Backend VP8 sobre libvpx.
//!
//! Configurado para tiempo real, que es un modo bastante distinto del que usaria un
//! codificador de archivos:
//!
//! - `deadline = VPX_DL_REALTIME`: el codificador tiene un presupuesto de tiempo y sacrifica
//!   calidad antes que pasarse. Un frame perfecto que llega tarde no vale nada.
//! - `g_lag_in_frames = 0`: sin mirar hacia adelante. Cualquier valor mayor haria que el
//!   codificador retuviera frames para decidir mejor, anadiendo latencia de golpe.
//! - `g_error_resilient`: cada frame se codifica de forma que perder uno no arrastre a los
//!   siguientes indefinidamente. Cuesta algo de bitrate y lo vale en cuanto hay red de por
//!   medio.
//! - `rc_end_usage = VPX_CBR` con buffers de rate control cortos: preferimos un bitrate
//!   estable a picos que llenen la cola de la red.
//! - `kf_mode = VPX_KF_DISABLED`: los keyframes los pide la aplicacion, uno por uno. Ver
//!   la seccion de keyframes bajo demanda en la documentacion del crate.
//!
//! # Seguridad de memoria
//!
//! Este modulo hace FFI a una biblioteca en C que ademas procesa datos venidos de la red en
//! el decodificador. Dos reglas que se siguen aqui: ningun puntero que se le pasa a libvpx
//! sobrevive a la llamada en la que se usa, y toda entrada del decodificador se trata como
//! hostil, de forma que un flujo corrupto salga por `Err` y jamas por un panico.

use std::ffi::CStr;
use std::ptr;

use bytes::Bytes;
use vhdesk_libvpx_sys as vpx;
use vhdesk_proto::VideoCodec;

use crate::error::CodecError;
use crate::yuv::I420Frame;
use crate::{DecodedFrame, EncodedFrame, EncoderConfig, VideoDecoder, VideoEncoder};

/// Velocidad del codificador VP8, de 0 (lento y bueno) a 16 (rapido y peor).
///
/// En tiempo real interesa el extremo rapido: a 60 fps hay 16 ms para todo el pipeline, y
/// el codificador es solo una parte. Es el primer parametro que tocar si la fase 4 mide que
/// el encode se pasa de presupuesto.
const CPU_USED: i32 = 8;

/// Umbral por debajo del cual un bloque se considera estatico y no se recodifica.
///
/// En un escritorio la mayor parte de la pantalla no cambia entre frames, asi que este
/// parametro ahorra mucho trabajo. Es distinto de los rectangulos sucios de la captura: eso
/// lo decide el sistema de ventanas y esto lo decide el codificador comparando pixeles.
const STATIC_THRESHOLD: i32 = 100;

/// Convierte un codigo de error de libvpx en un `Result`.
fn comprobar(codigo: vpx::vpx_codec_err_t, operacion: &'static str) -> Result<(), CodecError> {
    if codigo == vpx::vpx_codec_err_t::VPX_CODEC_OK {
        return Ok(());
    }

    // SAFETY: `vpx_codec_err_to_string` devuelve una cadena estatica de la biblioteca para
    // cualquier codigo, valido o no, y nunca devuelve null.
    let detalle = unsafe {
        let texto = vpx::vpx_codec_err_to_string(codigo);
        if texto.is_null() {
            String::from("(sin descripcion)")
        } else {
            CStr::from_ptr(texto).to_string_lossy().into_owned()
        }
    };

    Err(CodecError::Backend {
        operation: operacion,
        detail: detalle,
    })
}

/// Codificador VP8.
pub struct Vp8Encoder {
    /// En caja para que la direccion del contexto no cambie: libvpx guarda punteros a sus
    /// propias estructuras internas y mover esto despues de inicializarlo seria un desastre
    /// silencioso.
    ctx: Box<vpx::vpx_codec_ctx_t>,
    width: u32,
    height: u32,
    /// Duracion nominal de un frame, en la base de tiempos del codificador.
    frame_duration_us: u64,
    force_keyframe: bool,
}

impl Vp8Encoder {
    /// Crea un codificador VP8 con los ajustes dados.
    ///
    /// # Errores
    ///
    /// Devuelve [`CodecError::InvalidDimensions`] si las dimensiones no sirven, y
    /// [`CodecError::Backend`] si libvpx rechaza la configuracion.
    pub fn new(config: EncoderConfig) -> Result<Self, CodecError> {
        if config.width == 0 || config.height == 0 {
            return Err(CodecError::InvalidDimensions {
                width: config.width,
                height: config.height,
            });
        }

        // SAFETY: devuelve la interfaz estatica del codificador VP8, siempre valida.
        let interfaz = unsafe { vpx::vpx_codec_vp8_cx() };

        let mut cfg: vpx::vpx_codec_enc_cfg_t = unsafe { std::mem::zeroed() };
        // SAFETY: `cfg` es una estructura del llamante, viva, y del tipo exacto que espera
        // la biblioteca porque los enlaces se generaron de sus propias cabeceras.
        comprobar(
            unsafe { vpx::vpx_codec_enc_config_default(interfaz, &mut cfg, 0) },
            "vpx_codec_enc_config_default",
        )?;

        let fps = config.max_framerate.max(1);

        cfg.g_w = config.width;
        cfg.g_h = config.height;
        // Base de tiempos en microsegundos: es la unidad en la que el resto del pipeline
        // mide, y asi no hay conversiones que redondeen por el camino.
        cfg.g_timebase.num = 1;
        cfg.g_timebase.den = 1_000_000;
        cfg.rc_target_bitrate = config.target_bitrate_kbps;
        cfg.rc_end_usage = vpx::vpx_rc_mode::VPX_CBR;
        cfg.g_pass = vpx::vpx_enc_pass::VPX_RC_ONE_PASS;
        cfg.g_lag_in_frames = 0;
        cfg.g_error_resilient = vpx::VPX_ERROR_RESILIENT_DEFAULT;
        // Keyframes SOLO bajo demanda. `VPX_KF_DISABLED` no es un detalle de ajuste: con
        // `VPX_KF_AUTO`, libvpx inserta keyframes por su cuenta al detectar cambio de
        // escena, y en un escritorio eso ocurre cada vez que el usuario cambia de ventana.
        // Con el modo automatico, "bajo demanda" no significaria nada.
        cfg.kf_mode = vpx::vpx_kf_mode::VPX_KF_DISABLED;
        cfg.g_threads = 1;

        // Buffers de rate control cortos, en milisegundos. Los valores por defecto estan
        // pensados para archivos y permiten picos de varios segundos; aqui un pico llena la
        // cola de la red y se convierte en latencia.
        cfg.rc_buf_sz = 1_000;
        cfg.rc_buf_initial_sz = 500;
        cfg.rc_buf_optimal_sz = 600;

        let mut ctx: Box<vpx::vpx_codec_ctx_t> = Box::new(unsafe { std::mem::zeroed() });

        // SAFETY: contexto y configuracion vivos; la version de ABI es la de las cabeceras
        // con las que se generaron estos enlaces, que son las de la biblioteca enlazada.
        comprobar(
            unsafe {
                vpx::vpx_codec_enc_init_ver(
                    &mut *ctx,
                    interfaz,
                    &cfg,
                    0,
                    vpx::VPX_ENCODER_ABI_VERSION as i32,
                )
            },
            "vpx_codec_enc_init_ver",
        )?;

        let mut encoder = Self {
            ctx,
            width: config.width,
            height: config.height,
            frame_duration_us: 1_000_000 / u64::from(fps),
            // El primer frame tiene que ser keyframe: sin el, el viewer no tiene por donde
            // engancharse.
            force_keyframe: true,
        };

        encoder.control(vpx::vp8e_enc_control_id::VP8E_SET_CPUUSED, CPU_USED)?;
        encoder.control(
            vpx::vp8e_enc_control_id::VP8E_SET_STATIC_THRESHOLD,
            STATIC_THRESHOLD,
        )?;

        Ok(encoder)
    }

    fn control(&mut self, id: vpx::vp8e_enc_control_id, valor: i32) -> Result<(), CodecError> {
        // SAFETY: `vpx_codec_control_` es variadica y el tipo del argumento depende del
        // identificador. Los dos que usamos aqui, CPUUSED y STATIC_THRESHOLD, esperan un
        // entero, que es lo que se les pasa.
        comprobar(
            unsafe { vpx::vpx_codec_control_(&mut *self.ctx, id as i32, valor) },
            "vpx_codec_control_",
        )
    }

    /// Monta una `vpx_image_t` que apunta a los planos del frame, sin copiarlos.
    ///
    /// La imagen no sobrevive a la llamada de codificacion en la que se usa, asi que los
    /// punteros que contiene siguen siendo validos durante toda su vida util.
    fn envolver(frame: &I420Frame) -> vpx::vpx_image_t {
        let mut imagen: vpx::vpx_image_t = unsafe { std::mem::zeroed() };

        imagen.fmt = vpx::vpx_img_fmt::VPX_IMG_FMT_I420;
        imagen.bit_depth = 8;
        imagen.w = frame.width();
        imagen.h = frame.height();
        imagen.d_w = frame.width();
        imagen.d_h = frame.height();
        imagen.r_w = frame.width();
        imagen.r_h = frame.height();
        // I420 tiene la crominancia a la mitad en los dos ejes.
        imagen.x_chroma_shift = 1;
        imagen.y_chroma_shift = 1;
        imagen.bps = 12;

        imagen.planes[0] = frame.y().as_ptr().cast_mut();
        imagen.planes[1] = frame.u().as_ptr().cast_mut();
        imagen.planes[2] = frame.v().as_ptr().cast_mut();
        imagen.stride[0] = frame.width() as i32;
        imagen.stride[1] = frame.chroma_width() as i32;
        imagen.stride[2] = frame.chroma_width() as i32;

        imagen
    }

    /// Recoge el primer paquete de frame comprimido que haya producido el codificador.
    fn recoger(&mut self, timestamp_us: u64) -> Option<EncodedFrame> {
        let mut iterador: vpx::vpx_codec_iter_t = ptr::null();

        loop {
            // SAFETY: el iterador es una variable local que libvpx actualiza en cada
            // llamada; devuelve null cuando no quedan paquetes.
            let paquete = unsafe { vpx::vpx_codec_get_cx_data(&mut *self.ctx, &mut iterador) };
            if paquete.is_null() {
                return None;
            }

            // SAFETY: no es null, y libvpx garantiza que apunta a un paquete valido cuya
            // vida dura hasta la siguiente llamada de codificacion.
            let paquete = unsafe { &*paquete };
            if paquete.kind != vpx::vpx_codec_cx_pkt_kind::VPX_CODEC_CX_FRAME_PKT {
                continue;
            }

            // SAFETY: el discriminante `kind` acaba de confirmar que la union contiene un
            // paquete de frame.
            let datos = unsafe { paquete.data.frame };
            if datos.buf.is_null() || datos.sz == 0 {
                continue;
            }

            // SAFETY: libvpx acaba de declarar `sz` bytes legibles en `buf`. Se copian aqui
            // porque el buffer es suyo y lo reutiliza en el siguiente frame.
            let bytes = unsafe { std::slice::from_raw_parts(datos.buf.cast::<u8>(), datos.sz) };

            return Some(EncodedFrame {
                data: Bytes::copy_from_slice(bytes),
                keyframe: datos.flags & vpx::VPX_FRAME_IS_KEY != 0,
                timestamp_us,
            });
        }
    }
}

impl VideoEncoder for Vp8Encoder {
    fn codec(&self) -> VideoCodec {
        VideoCodec::Vp8
    }

    fn request_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    fn encode(
        &mut self,
        frame: &I420Frame,
        timestamp_us: u64,
    ) -> Result<Option<EncodedFrame>, CodecError> {
        if frame.width() != self.width || frame.height() != self.height {
            return Err(CodecError::DimensionsChanged {
                expected_width: self.width,
                expected_height: self.height,
                width: frame.width(),
                height: frame.height(),
            });
        }

        let imagen = Self::envolver(frame);
        let banderas = if self.force_keyframe {
            vpx::VPX_EFLAG_FORCE_KF
        } else {
            0
        };

        // Los tres `as _` no son pereza: `duration` y `deadline` son `unsigned long` en C,
        // que mide 32 bits en Windows y 64 en Linux, y las banderas son `long`. Dejar que
        // el compilador deduzca el tipo de la firma generada mantiene esto correcto en las
        // dos plataformas sin un `#[cfg]`.
        //
        // SAFETY: la imagen y los planos a los que apunta siguen vivos durante toda la
        // llamada, que es el unico momento en que libvpx los lee.
        let resultado = unsafe {
            vpx::vpx_codec_encode(
                &mut *self.ctx,
                &imagen,
                timestamp_us as i64,
                self.frame_duration_us as _,
                banderas as _,
                vpx::VPX_DL_REALTIME as _,
            )
        };
        comprobar(resultado, "vpx_codec_encode")?;

        // Solo se limpia despues de que la codificacion haya salido bien: si fallo, el
        // keyframe sigue pendiente.
        self.force_keyframe = false;

        Ok(self.recoger(timestamp_us))
    }
}

impl Drop for Vp8Encoder {
    fn drop(&mut self) {
        // SAFETY: el contexto se inicializo correctamente (si no, `new` habria fallado
        // antes de construir el `Self`) y no se ha destruido todavia.
        unsafe {
            vpx::vpx_codec_destroy(&mut *self.ctx);
        }
    }
}

/// Decodificador VP8.
pub struct Vp8Decoder {
    ctx: Box<vpx::vpx_codec_ctx_t>,
}

impl Vp8Decoder {
    /// Crea un decodificador VP8.
    ///
    /// # Errores
    ///
    /// Devuelve [`CodecError::Backend`] si libvpx no puede inicializarse.
    pub fn new() -> Result<Self, CodecError> {
        // SAFETY: interfaz estatica del decodificador VP8, siempre valida.
        let interfaz = unsafe { vpx::vpx_codec_vp8_dx() };
        let mut ctx: Box<vpx::vpx_codec_ctx_t> = Box::new(unsafe { std::mem::zeroed() });

        // SAFETY: contexto vivo; se pasa null como configuracion, que es lo que la API pide
        // para usar los valores por defecto.
        comprobar(
            unsafe {
                vpx::vpx_codec_dec_init_ver(
                    &mut *ctx,
                    interfaz,
                    ptr::null(),
                    0,
                    vpx::VPX_DECODER_ABI_VERSION as i32,
                )
            },
            "vpx_codec_dec_init_ver",
        )?;

        Ok(Self { ctx })
    }
}

impl VideoDecoder for Vp8Decoder {
    fn codec(&self) -> VideoCodec {
        VideoCodec::Vp8
    }

    fn decode(&mut self, data: &[u8]) -> Result<Option<DecodedFrame<'_>>, CodecError> {
        if data.is_empty() {
            // Para libvpx un buffer vacio significa "fin del flujo", no "frame vacio". Si
            // se lo pasaramos tal cual, un frame perdido en la red cerraria el decodificador.
            return Err(CodecError::InvalidBitstream("frame de video vacio"));
        }

        let longitud = u32::try_from(data.len())
            .map_err(|_| CodecError::InvalidBitstream("frame de video mayor de 4 GiB"))?;

        // SAFETY: se declaran exactamente los bytes que tiene el slice, que sigue vivo
        // durante toda la llamada. libvpx no conserva el puntero despues de volver.
        let resultado = unsafe {
            vpx::vpx_codec_decode(&mut *self.ctx, data.as_ptr(), longitud, ptr::null_mut(), 0)
        };
        comprobar(resultado, "vpx_codec_decode")?;

        let mut iterador: vpx::vpx_codec_iter_t = ptr::null();
        // SAFETY: iterador local que libvpx actualiza; devuelve null si no hay imagen.
        let imagen = unsafe { vpx::vpx_codec_get_frame(&mut *self.ctx, &mut iterador) };
        if imagen.is_null() {
            return Ok(None);
        }

        // SAFETY: no es null y libvpx garantiza que la imagen vive hasta la siguiente
        // llamada de decodificacion, que es exactamente lo que expresa el prestamo de
        // `DecodedFrame<'_>` sobre `&mut self`.
        let imagen = unsafe { &*imagen };

        let y_stride = imagen.stride[0].max(0) as usize;
        let uv_stride = imagen.stride[1].max(0) as usize;
        let alto = imagen.d_h as usize;
        let alto_croma = imagen.d_h.div_ceil(2) as usize;

        if imagen.planes[0].is_null() || imagen.planes[1].is_null() || imagen.planes[2].is_null() {
            return Err(CodecError::InvalidBitstream(
                "el decodificador devolvio una imagen sin planos",
            ));
        }

        // SAFETY: los tres planos no son null y libvpx los dimensiona segun el stride y la
        // altura que el mismo acaba de declarar en la imagen.
        let (y, u, v) = unsafe {
            (
                std::slice::from_raw_parts(imagen.planes[0], y_stride * alto),
                std::slice::from_raw_parts(imagen.planes[1], uv_stride * alto_croma),
                std::slice::from_raw_parts(imagen.planes[2], uv_stride * alto_croma),
            )
        };

        Ok(Some(DecodedFrame {
            width: imagen.d_w,
            height: imagen.d_h,
            y,
            u,
            v,
            y_stride,
            uv_stride,
        }))
    }
}

impl Drop for Vp8Decoder {
    fn drop(&mut self) {
        // SAFETY: mismo razonamiento que en el codificador.
        unsafe {
            vpx::vpx_codec_destroy(&mut *self.ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Vp8Decoder;
    use vhdesk_libvpx_sys as vpx;

    unsafe extern "C" fn obtener(
        _priv_: *mut std::ffi::c_void,
        _min: usize,
        _fb: *mut vpx::vpx_codec_frame_buffer_t,
    ) -> i32 {
        -1
    }

    unsafe extern "C" fn liberar(
        _priv_: *mut std::ffi::c_void,
        _fb: *mut vpx::vpx_codec_frame_buffer_t,
    ) -> i32 {
        0
    }

    /// Fija por escrito el hecho que decide como viaja un frame decodificado.
    ///
    /// libvpx permite que la aplicacion suministre los buffers de frame, lo que daria
    /// control sobre su ciclo de vida y permitiria pasarlos a otro hilo sin copiar, igual
    /// que hace el pool de la captura. **Pero solo con VP9.** Este test existe para que el
    /// dia que alguien proponga esa optimizacion se encuentre con la respuesta ya medida,
    /// y para que si algun dia libvpx la habilita en VP8, el test falle y nos entere.
    #[test]
    fn vp8_no_admite_buffers_de_frame_externos() {
        let mut decoder = Vp8Decoder::new().expect("crear decodificador");

        // SAFETY: el contexto esta inicializado y las dos funciones son `extern "C"` con
        // las firmas que declara libvpx.
        let resultado = unsafe {
            vpx::vpx_codec_set_frame_buffer_functions(
                &mut *decoder.ctx,
                Some(obtener),
                Some(liberar),
                std::ptr::null_mut(),
            )
        };

        assert_eq!(
            resultado,
            vpx::vpx_codec_err_t::VPX_CODEC_INCAPABLE,
            "VP8 admite buffers externos: se puede reconsiderar copiar el frame decodificado"
        );
    }
}

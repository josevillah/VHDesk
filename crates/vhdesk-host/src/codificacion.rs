//! La etapa de conversion de color y codificacion, en su propio hilo.
//!
//! # Por que conversion y encode van juntas
//!
//! El pipeline tiene dos hilos, no cuatro: captura por un lado, y conversion mas encode por
//! otro. Separar la captura es imprescindible porque paga 3,14 ms de media (9,18 de p99)
//! **bloqueada esperando a la GPU**, y ese bloqueo no debe frenar al codificador que esta
//! trabajando en el frame anterior. Ese es el motivo de todo el reparto en hilos.
//!
//! Separar la conversion del encode, en cambio, no compensa: la conversion cuesta 1,32 ms y
//! el encode 13,2 ms, asi que partirlas sube el techo de throughput un 10% a cambio de un
//! salto de cola mas. Y aqui **se optimiza para latencia, no para throughput**: sumar
//! etapas mejora la grafica de FPS y empeora la sesion.
//!
//! Si el bloque F mide que hace falta, partirlas es meter una ranura en medio de
//! [`convertir_y_codificar`], no rehacer nada.
//!
//! El transporte tampoco es una etapa: `send_frame` no espera a la red, deja la escritura
//! en una tarea y vuelve.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::runtime::Handle;
use vhdesk_codec::{EncodedFrame, EncoderConfig, I420Frame, VideoEncoder, open_encoder};
use vhdesk_proto::VideoCodec;
use vhdesk_transport::{FrameSaliente, SenalKeyframe, VideoSender};

use crate::ranura::{FrameAcumulado, Metadatos, Ranura};

/// Que hacer con un frame recien recogido de la ranura.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    /// Si hay que forzar keyframe.
    pub keyframe: bool,
    /// Si hay que codificar algo.
    pub codificar: bool,
}

/// Decide que hacer con un frame.
///
/// # El orden de las dos comprobaciones no es intercambiable
///
/// La de keyframe va **primero** y el cortocircuito de "no hay nada que codificar"
/// **despues**. Al reves se produce un fallo que ademas cae en el caso mas probable de
/// todos: un viewer que se reengancha a una maquina inactiva manda su `KeyframeRequest`,
/// la pantalla no tiene ni un pixel distinto, el cortocircuito se dispara antes de mirar la
/// peticion, y el keyframe no sale nunca. El usuario ve una ventana en blanco y nada en los
/// registros: el host esta funcionando perfectamente, solo que decidiendo que no hay nada
/// que mandar.
///
/// Por eso un keyframe pedido **siempre** manda sobre el cortocircuito.
pub fn planificar(keyframe_pedido: bool, meta: &Metadatos) -> Plan {
    let keyframe = keyframe_pedido || meta.full_refresh;

    Plan {
        keyframe,
        // El orden esta aqui: `keyframe ||` va antes que la consulta de cambios.
        codificar: keyframe || meta.hay_cambios(),
    }
}

/// Separacion minima entre frames codificados para respetar la cota de fps.
fn intervalo_minimo(fps: u32) -> Duration {
    Duration::from_secs_f64(1.0 / f64::from(fps.max(1)))
}

/// Si todavia no toca codificar segun la cota de fps.
///
/// # Por que la cota se aplica aqui y no en la captura
///
/// La captura entrega a la velocidad de la pantalla, y esta bien que lo haga: es lo que
/// mantiene fresco el frame que hay en la ranura. Frenarla alli significaria quedarse con
/// una imagen mas vieja de la que se podia tener.
///
/// Y **no** significa dejar frames esperando: el frame que no se codifica se tira en el
/// acto, con sus metadatos absorbidos por el siguiente. La cota baja el consumo de CPU y de
/// ancho de banda sin anadir ni un milisegundo de latencia, que es la unica forma aceptable
/// de limitar el ritmo en este pipeline.
fn demasiado_pronto(ultimo: Option<Instant>, ahora: Instant, intervalo: Duration) -> bool {
    match ultimo {
        Some(anterior) => ahora.saturating_duration_since(anterior) < intervalo,
        // El primer frame de la sesion nunca espera.
        None => false,
    }
}

/// Ajustes del hilo de codificacion.
pub struct Ajustes {
    /// Codec negociado con el viewer.
    pub codec: VideoCodec,
    /// Indice del monitor que se esta sirviendo.
    pub monitor: u8,
    /// Bitrate objetivo en kbps.
    pub bitrate_kbps: u32,
    /// Cota superior de frames por segundo.
    pub fps: u32,
    /// Origen del reloj de la sesion, del que cuelgan todas las marcas de tiempo.
    pub inicio: Instant,
}

/// Bucle de conversion y codificacion. Termina cuando la ranura se cierra.
///
/// # Errores
///
/// Devuelve error si el codificador no se puede construir o si falla de forma que no tenga
/// sentido seguir.
pub fn bucle(
    ranura: &Arc<Ranura>,
    senal: &SenalKeyframe,
    mut emisor: VideoSender,
    ajustes: &Ajustes,
    handle: &Handle,
) -> Result<()> {
    // `send_frame` deja la escritura en una tarea de tokio, asi que este hilo necesita
    // estar dentro del contexto del runtime aunque no sea asincrono. Sin esto, el primer
    // frame entra en panico con "there is no reactor running".
    let _contexto = handle.enter();

    let mut estado: Option<EstadoCodificador> = None;
    let mut frames = 0u64;

    let intervalo = intervalo_minimo(ajustes.fps);
    let mut ultimo_encode: Option<Instant> = None;
    // Metadatos de los frames que este hilo decide no codificar. **No se pueden tirar**: son
    // acumulativos desde el ultimo frame codificado, igual que en la ranura, y perderlos deja
    // sin repintar lo que cambio mientras tanto. La ranura acumula los frames que descarta
    // ella; esto acumula los que descarta el bucle.
    let mut arrastrados: Option<Metadatos> = None;

    while let Some(mut frame) = ranura.recoger() {
        if let Some(anteriores) = arrastrados.take() {
            frame.meta.absorber_descartado(&anteriores);
        }

        let plan = planificar(senal.pendiente(), &frame.meta);

        // Un keyframe pedido no espera al reloj: el viewer que lo pidio no tiene imagen.
        let pronto = !plan.keyframe && demasiado_pronto(ultimo_encode, Instant::now(), intervalo);

        if !plan.codificar || pronto {
            // Pantalla quieta (cero trabajo y cero ancho de banda) o cota de fps alcanzada.
            // En los dos casos los metadatos siguen vivos hacia el frame siguiente.
            arrastrados = Some(frame.meta);
            continue;
        }

        let codificador = match estado.as_mut() {
            // Un cambio de resolucion a mitad de sesion obliga a rehacer el codificador:
            // libvpx rechaza un frame que no coincida con su configuracion.
            Some(actual) if actual.width == frame.width && actual.height == frame.height => actual,
            _ => {
                tracing::info!(
                    ancho = frame.width,
                    alto = frame.height,
                    "(re)creando el codificador"
                );
                estado.insert(EstadoCodificador::nuevo(&frame, ajustes)?)
            }
        };

        if plan.keyframe {
            codificador.encoder.request_keyframe();
        }

        let timestamp_us = frame
            .captured_at
            .saturating_duration_since(ajustes.inicio)
            .as_micros() as u64;

        ultimo_encode = Some(Instant::now());

        let Some(comprimido) = convertir_y_codificar(codificador, &frame, timestamp_us)? else {
            continue;
        };

        // Punto de medida del bloque F: aqui estan los dos costes del mismo frame y la edad
        // que ya arrastra el frame cuando sale hacia la red. El informe se construye alli;
        // esto solo lo deja anotado.
        tracing::trace!(
            conversion_us = codificador.ultima_conversion.as_micros(),
            encode_us = codificador.ultimo_encode.as_micros(),
            edad_us = frame.captured_at.elapsed().as_micros(),
            keyframe = comprimido.keyframe,
            bytes = comprimido.data.len(),
            descartados = frame.meta.descartados,
            "frame codificado"
        );

        emisor
            .send_frame(FrameSaliente {
                monitor: ajustes.monitor,
                codec: ajustes.codec,
                keyframe: comprimido.keyframe,
                timestamp_us: comprimido.timestamp_us,
                width: frame.width as u16,
                height: frame.height as u16,
                data: comprimido.data,
            })
            .context("encolar el frame en el transporte")?;

        frames += 1;
    }

    tracing::debug!(
        frames,
        descartados_por_el_emisor = emisor.descartados(),
        "el hilo de codificacion termina"
    );
    Ok(())
}

/// Codificador vivo con sus buffers reutilizables.
struct EstadoCodificador {
    encoder: Box<dyn VideoEncoder>,
    i420: I420Frame,
    width: u32,
    height: u32,
    ultima_conversion: Duration,
    ultimo_encode: Duration,
}

impl EstadoCodificador {
    fn nuevo(frame: &FrameAcumulado, ajustes: &Ajustes) -> Result<Self> {
        let config = EncoderConfig {
            width: frame.width,
            height: frame.height,
            target_bitrate_kbps: ajustes.bitrate_kbps,
            max_framerate: ajustes.fps,
        };

        Ok(Self {
            encoder: open_encoder(ajustes.codec, config).context("abrir el codificador")?,
            // Se crea una vez y se rellena en cada frame: a 30 fps, asignar los tres planos
            // de 1080p por frame serian ~90 MB/s de trasiego inutil.
            i420: I420Frame::new(frame.width, frame.height).context("reservar el I420")?,
            width: frame.width,
            height: frame.height,
            ultima_conversion: Duration::ZERO,
            ultimo_encode: Duration::ZERO,
        })
    }
}

/// Convierte a I420 y codifica. Las dos etapas fusionadas, en una sola funcion.
fn convertir_y_codificar(
    estado: &mut EstadoCodificador,
    frame: &FrameAcumulado,
    timestamp_us: u64,
) -> Result<Option<EncodedFrame>> {
    let t0 = Instant::now();
    estado
        .i420
        .fill_from_bgra(&frame.buffer, frame.stride)
        .context("convertir BGRA a I420")?;
    estado.ultima_conversion = t0.elapsed();

    let t1 = Instant::now();
    let salida = estado
        .encoder
        .encode(&estado.i420, timestamp_us)
        .context("codificar el frame")?;
    estado.ultimo_encode = t1.elapsed();

    Ok(salida)
}

#[cfg(test)]
mod tests {
    use super::{demasiado_pronto, intervalo_minimo, planificar};
    use crate::ranura::Metadatos;
    use std::time::{Duration, Instant};
    use vhdesk_capture::Rect;

    #[test]
    fn la_cota_de_fps_se_traduce_al_intervalo_correcto() {
        assert_eq!(intervalo_minimo(30).as_micros(), 33_333);
        assert_eq!(intervalo_minimo(60).as_micros(), 16_666);
        // Cero fps no llega aqui (lo rechaza el parseo), pero dividir por cero seria peor
        // que cualquier valor.
        assert_eq!(intervalo_minimo(0), Duration::from_secs(1));
    }

    #[test]
    fn el_primer_frame_de_la_sesion_no_espera_al_reloj() {
        assert!(!demasiado_pronto(
            None,
            Instant::now(),
            intervalo_minimo(30)
        ));
    }

    #[test]
    fn la_cota_deja_pasar_solo_cuando_toca() {
        let intervalo = intervalo_minimo(30);
        let base = Instant::now();

        assert!(
            demasiado_pronto(Some(base), base + Duration::from_millis(16), intervalo),
            "a 16 ms de haber codificado, a 30 fps todavia no toca"
        );
        assert!(!demasiado_pronto(
            Some(base),
            base + Duration::from_millis(34),
            intervalo
        ));
    }

    #[test]
    fn el_arrastre_de_metadatos_conserva_lo_que_cambio_mientras_se_saltaban_frames() {
        // Es la misma invariante de la ranura aplicada al segundo sitio donde se descartan
        // frames: la cota de fps. Si el bucle tirara los metadatos de los frames que se
        // salta, todo lo que cambio entre dos frames codificados se quedaria sin repintar.
        let mut vivo = Metadatos {
            dirty: vec![Rect {
                left: 0,
                top: 0,
                right: 10,
                bottom: 10,
            }],
            ..Metadatos::default()
        };

        // Dos frames saltados por la cota, cada uno con su region.
        for indice in 1..3 {
            let mut siguiente = Metadatos {
                dirty: vec![Rect {
                    left: indice * 100,
                    top: 0,
                    right: indice * 100 + 10,
                    bottom: 10,
                }],
                ..Metadatos::default()
            };
            siguiente.absorber_descartado(&vivo);
            vivo = siguiente;
        }

        assert_eq!(vivo.dirty.len(), 3, "se perdio alguna region por el camino");
        assert!(planificar(false, &vivo).codificar);
    }

    fn quieta() -> Metadatos {
        Metadatos::default()
    }

    fn con_cambios() -> Metadatos {
        Metadatos {
            dirty: vec![Rect {
                left: 0,
                top: 0,
                right: 10,
                bottom: 10,
            }],
            ..Metadatos::default()
        }
    }

    #[test]
    fn con_la_pantalla_quieta_y_un_keyframe_pedido_se_codifica() {
        // El escenario de un viewer que se reengancha a una maquina inactiva, que es el
        // caso mas probable de todos. Si el cortocircuito de "no hay nada que codificar" se
        // evaluara antes que la peticion, el keyframe no saldria nunca y el viewer se
        // quedaria con la pantalla congelada esperandolo.
        let plan = planificar(true, &quieta());

        assert!(plan.keyframe, "la peticion se perdio");
        assert!(
            plan.codificar,
            "un keyframe pedido tiene que ganar al cortocircuito de pantalla quieta"
        );
    }

    #[test]
    fn con_la_pantalla_quieta_y_sin_peticion_no_se_codifica() {
        let plan = planificar(false, &quieta());

        assert!(
            !plan.codificar,
            "codificar una pantalla quieta es trabajo y ancho de banda tirados"
        );
        assert!(!plan.keyframe);
    }

    #[test]
    fn un_full_refresh_fuerza_keyframe_aunque_nadie_lo_pidiera() {
        // Los rectangulos sucios de un `full_refresh` no describen todo lo que cambio, asi
        // que tratarlo como delta deja basura en pantalla.
        let meta = Metadatos {
            full_refresh: true,
            ..Metadatos::default()
        };
        let plan = planificar(false, &meta);

        assert!(plan.keyframe);
        assert!(
            plan.codificar,
            "un full_refresh sin rectangulos sucios sigue teniendo que emitirse"
        );
    }

    #[test]
    fn con_cambios_normales_se_codifica_sin_forzar_keyframe() {
        let plan = planificar(false, &con_cambios());

        assert!(plan.codificar);
        assert!(
            !plan.keyframe,
            "un frame normal no debe gastar los ~100 KB de un keyframe"
        );
    }
}

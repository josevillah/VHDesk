//! Captura en Windows con DXGI Desktop Duplication.

pub mod dpi;

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HMODULE, POINT, RECT};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1,
    D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_ACCESS_DENIED, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_MORE_DATA,
    DXGI_ERROR_NOT_FOUND, DXGI_ERROR_SESSION_DISCONNECTED, DXGI_ERROR_UNSUPPORTED,
    DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, DXGI_OUTDUPL_MOVE_RECT,
    DXGI_OUTDUPL_POINTER_SHAPE_INFO, DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR,
    DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR, DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME,
    DXGI_OUTPUT_DESC, IDXGIAdapter, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1,
    IDXGIOutputDuplication, IDXGIResource,
};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::core::Interface;

use crate::cursor::{CursorPosition, CursorShape, CursorUpdate, PointerShapeKind};
use crate::error::CaptureError;
use crate::frame::{CaptureEvent, Frame, MonitorId, MonitorInfo, MoveRect, Rect};
use crate::pixels::copy_frame;
use crate::pool::BufferPool;
use crate::{ScreenCapturer, cursor};

/// Buffers que conserva el pool. Tres bastan para el patron de esta fase (uno en captura,
/// uno en vuelo hacia el encoder y uno de holgura) sin retener memoria de mas.
const BUFFERS_EN_POOL: usize = 3;

/// Rectangulos que caben en el scratch antes de tener que agrandarlo.
const RECTS_INICIALES: usize = 64;

/// Espera cuando el escritorio seguro nos deniega el acceso, para no girar en vacio.
const ESPERA_ACCESO_DENEGADO: Duration = Duration::from_millis(100);

/// Bandera de `MONITORINFO::dwFlags` que marca el monitor principal.
///
/// Se declara aqui porque las proyecciones de `windows` no la exponen; su valor es parte
/// de la ABI publica de Win32 y no cambia.
const MONITORINFOF_PRIMARY: u32 = 0x0000_0001;

/// Enumera los monitores conectados, emparejando cada uno con el adaptador que lo posee.
///
/// El emparejamiento importa: en portatiles con grafica hibrida los monitores se reparten
/// entre la integrada y la dedicada, y pedir la duplicacion de un output a un adaptador que
/// no es su dueno falla con `DXGI_ERROR_UNSUPPORTED`, un error que no orienta nada sobre
/// la causa real.
pub fn enumerate_monitors() -> Result<Vec<MonitorInfo>, CaptureError> {
    // SAFETY: `CreateDXGIFactory1` solo escribe la interfaz que se le pide, cuyo tipo se
    // fija en el parametro generico.
    let factory: IDXGIFactory1 =
        unsafe { CreateDXGIFactory1() }.map_err(|e| envolver(e, "CreateDXGIFactory1"))?;

    let mut monitores = Vec::new();

    for indice_adaptador in 0u32.. {
        // SAFETY: enumerar por indice es la forma prevista de recorrer adaptadores; el
        // final se senala con DXGI_ERROR_NOT_FOUND, no con un puntero invalido.
        let adaptador: IDXGIAdapter1 = match unsafe { factory.EnumAdapters1(indice_adaptador) } {
            Ok(adaptador) => adaptador,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(envolver(e, "EnumAdapters1")),
        };

        for indice_output in 0u32.. {
            // SAFETY: mismo contrato de enumeracion por indice.
            let output: IDXGIOutput = match unsafe { adaptador.EnumOutputs(indice_output) } {
                Ok(output) => output,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(envolver(e, "EnumOutputs")),
            };

            // SAFETY: `GetDesc` rellena una estructura propiedad del llamante.
            let desc = unsafe { output.GetDesc() }.map_err(|e| envolver(e, "GetDesc"))?;
            if !desc.AttachedToDesktop.as_bool() {
                continue;
            }

            monitores.push(describir_monitor(
                MonitorId {
                    adapter: indice_adaptador,
                    output: indice_output,
                },
                &desc,
                &nombre_de_adaptador(&adaptador),
            ));
        }
    }

    if monitores.is_empty() {
        return Err(CaptureError::NoMonitors);
    }
    Ok(monitores)
}

/// Convierte una cadena UTF-16 terminada en NUL y de longitud fija a `String`.
fn cadena_fija(bruta: &[u16]) -> String {
    let fin = bruta.iter().position(|c| *c == 0).unwrap_or(bruta.len());
    String::from_utf16_lossy(&bruta[..fin])
}

fn nombre_de_adaptador(adaptador: &IDXGIAdapter1) -> String {
    // SAFETY: `GetDesc1` rellena una estructura propiedad del llamante.
    match unsafe { adaptador.GetDesc1() } {
        Ok(desc) => cadena_fija(&desc.Description),
        Err(_) => String::from("(desconocido)"),
    }
}

fn describir_monitor(id: MonitorId, desc: &DXGI_OUTPUT_DESC, adaptador: &str) -> MonitorInfo {
    let coordenadas = desc.DesktopCoordinates;

    MonitorInfo {
        id,
        name: cadena_fija(&desc.DeviceName),
        adapter_name: adaptador.to_owned(),
        width: (coordenadas.right - coordenadas.left).max(0) as u32,
        height: (coordenadas.bottom - coordenadas.top).max(0) as u32,
        position: (coordenadas.left, coordenadas.top),
        scale: escala_de(desc.Monitor),
        primary: es_principal(desc.Monitor),
    }
}

fn escala_de(monitor: HMONITOR) -> f32 {
    let (mut ppp_x, mut ppp_y) = (0u32, 0u32);
    // SAFETY: los dos punteros apuntan a variables locales vivas durante la llamada.
    let resultado = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut ppp_x, &mut ppp_y) };

    match resultado {
        Ok(()) if ppp_x > 0 => ppp_x as f32 / 96.0,
        _ => 1.0,
    }
}

fn es_principal(monitor: HMONITOR) -> bool {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `info` esta viva y su `cbSize` declara correctamente su tamano, que es lo
    // que la API usa para saber cuanto puede escribir.
    let ok = unsafe { GetMonitorInfoW(monitor, &mut info) };
    ok.as_bool() && (info.dwFlags & MONITORINFOF_PRIMARY) != 0
}

/// Textura intermedia en memoria accesible por CPU.
///
/// La textura del escritorio vive en memoria de la GPU y no se puede mapear, asi que hay
/// que copiarla a una textura de tipo *staging*. Se crea una sola vez y se conserva: la
/// creacion es cara y hacerla por frame seria el mayor coste del pipeline.
struct Staging {
    texture: ID3D11Texture2D,
    width: u32,
    height: u32,
}

/// Capturador de un monitor con DXGI Desktop Duplication.
///
/// **No es `Send`.** Los objetos COM que guarda estan atados al hilo que los creo, asi que
/// el capturador se construye y se usa en el mismo hilo. Lo que si cruza hilos es el
/// [`Frame`], cuyo buffer es un handle del pool.
///
/// **Solo puede existir un capturador por monitor y proceso.** DXGI permite una unica
/// duplicacion activa sobre cada output; abrir una segunda falla. Importa para la fase 5,
/// cuando haya multi-monitor: seran capturadores distintos sobre outputs distintos, nunca
/// dos sobre el mismo.
pub struct DxgiCapturer {
    monitor: MonitorInfo,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    output: IDXGIOutput1,
    /// `None` mientras la duplicacion no esta disponible, tipicamente porque el escritorio
    /// seguro esta delante. Se reconstruye sola en el siguiente ciclo.
    duplication: Option<IDXGIOutputDuplication>,
    staging: Option<Staging>,
    pool: BufferPool,
    dirty_scratch: Vec<RECT>,
    move_scratch: Vec<DXGI_OUTDUPL_MOVE_RECT>,
    shape_scratch: Vec<u8>,
    sequence: u64,
    /// El proximo frame describe la pantalla entera, no un delta.
    full_refresh_pending: bool,
    /// Ultimo estado de cursor que se reporto hacia fuera, para no repetirlo.
    ultimo_cursor: Option<(bool, CursorPosition)>,
    aviso_dpi_emitido: bool,
}

impl DxgiCapturer {
    /// Abre la captura del monitor indicado.
    ///
    /// # Errores
    ///
    /// Devuelve [`CaptureError::UnknownMonitor`] si el identificador no corresponde a
    /// ningun monitor conectado, y [`CaptureError::Unsupported`] si el sistema no admite
    /// duplicacion en ese monitor.
    pub fn new(id: MonitorId) -> Result<Self, CaptureError> {
        if !dpi::is_per_monitor_aware() {
            tracing::warn!(
                "el proceso no tiene conciencia de DPI por monitor v2: con escalado activo \
                 las resoluciones y las coordenadas del raton no cuadraran. Llama a \
                 vhdesk_capture::ensure_dpi_awareness() al principio de main()"
            );
        }

        // SAFETY: ver `enumerate_monitors`.
        let factory: IDXGIFactory1 =
            unsafe { CreateDXGIFactory1() }.map_err(|e| envolver(e, "CreateDXGIFactory1"))?;

        // SAFETY: enumeracion por indice; el fallo se senala por codigo de error.
        let adaptador: IDXGIAdapter1 =
            unsafe { factory.EnumAdapters1(id.adapter) }.map_err(|_| {
                CaptureError::UnknownMonitor {
                    adapter: id.adapter,
                    output: id.output,
                }
            })?;

        // SAFETY: idem.
        let output: IDXGIOutput = unsafe { adaptador.EnumOutputs(id.output) }.map_err(|_| {
            CaptureError::UnknownMonitor {
                adapter: id.adapter,
                output: id.output,
            }
        })?;

        // SAFETY: `GetDesc` rellena una estructura del llamante.
        let desc = unsafe { output.GetDesc() }.map_err(|e| envolver(e, "GetDesc"))?;
        let monitor = describir_monitor(id, &desc, &nombre_de_adaptador(&adaptador));

        let output1: IDXGIOutput1 = output
            .cast()
            .map_err(|e| envolver(e, "cast IDXGIOutput1"))?;
        let (device, context) = crear_dispositivo(&adaptador)?;

        let mut capturador = Self {
            monitor,
            device,
            context,
            output: output1,
            duplication: None,
            staging: None,
            pool: BufferPool::new(BUFFERS_EN_POOL),
            dirty_scratch: vec![RECT::default(); RECTS_INICIALES],
            move_scratch: vec![DXGI_OUTDUPL_MOVE_RECT::default(); RECTS_INICIALES],
            shape_scratch: Vec::new(),
            sequence: 0,
            full_refresh_pending: true,
            ultimo_cursor: None,
            aviso_dpi_emitido: false,
        };

        // Se abre aqui para que un monitor que no admite duplicacion falle al construir y
        // no en mitad de la sesion.
        capturador.abrir_duplicacion()?;
        Ok(capturador)
    }

    fn abrir_duplicacion(&mut self) -> Result<(), CaptureError> {
        // SAFETY: `DuplicateOutput` recibe el dispositivo D3D11 que creamos sobre el mismo
        // adaptador que posee este output, que es la condicion que la API exige.
        let duplicacion = unsafe { self.output.DuplicateOutput(&self.device) }
            .map_err(|e| clasificar(e, "DuplicateOutput"))?;

        self.duplication = Some(duplicacion);
        // La duplicacion recien abierta no tiene historia: su primer frame es la pantalla
        // entera y sus rectangulos sucios no describen todo lo que hay en ella.
        self.full_refresh_pending = true;
        self.staging = None;
        Ok(())
    }

    /// Reconstruye la duplicacion tras perderla.
    ///
    /// Devuelve `Ok(false)` si el sistema sigue denegando el acceso, que es lo que ocurre
    /// mientras el escritorio seguro esta delante y no es un fallo del que haya que
    /// recuperarse: simplemente todavia no se puede capturar.
    fn recuperar(&mut self) -> Result<bool, CaptureError> {
        self.duplication = None;

        match self.abrir_duplicacion() {
            Ok(()) => {
                tracing::info!(monitor = %self.monitor.id, "duplicacion reinicializada");
                Ok(true)
            }
            Err(CaptureError::AccessDenied) => {
                tracing::debug!(
                    monitor = %self.monitor.id,
                    "acceso a la duplicacion denegado; probablemente el escritorio seguro"
                );
                std::thread::sleep(ESPERA_ACCESO_DENEGADO);
                Ok(false)
            }
            Err(otro) => Err(otro),
        }
    }

    fn asegurar_staging(
        &mut self,
        width: u32,
        height: u32,
    ) -> Result<&ID3D11Texture2D, CaptureError> {
        let hay_que_recrear = match &self.staging {
            Some(staging) => staging.width != width || staging.height != height,
            None => true,
        };

        if hay_que_recrear {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };

            let mut textura: Option<ID3D11Texture2D> = None;
            // SAFETY: `desc` esta viva durante la llamada y el destino es una variable
            // local que la API rellena con una referencia contada.
            unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut textura)) }
                .map_err(|e| envolver(e, "CreateTexture2D"))?;

            let texture = textura.ok_or(CaptureError::Unsupported)?;
            self.staging = Some(Staging {
                texture,
                width,
                height,
            });
        }

        self.staging
            .as_ref()
            .map(|staging| &staging.texture)
            .ok_or(CaptureError::Unsupported)
    }

    fn duplicacion(&self) -> Result<&IDXGIOutputDuplication, CaptureError> {
        self.duplication.as_ref().ok_or(CaptureError::AccessLost)
    }

    fn leer_cursor(
        &mut self,
        info: &DXGI_OUTDUPL_FRAME_INFO,
    ) -> Result<Option<CursorUpdate>, CaptureError> {
        let hay_forma = info.PointerShapeBufferSize > 0;
        if info.LastMouseUpdateTime == 0 && !hay_forma {
            return Ok(None);
        }

        let visible = info.PointerPosition.Visible.as_bool();
        let position = punto_a_posicion(info.PointerPosition.Position);

        // DXGI marca "el puntero se actualizo" a partir de su propia marca de tiempo, que
        // no garantiza que nada haya cambiado de verdad. Aqui se convierte esa senal en un
        // cambio real: sin este filtro, un aviso repetido se traduciria en un mensaje de
        // cursor por la red y en un repintado del viewer para dibujar algo identico.
        let sin_cambios = !hay_forma && self.ultimo_cursor == Some((visible, position));
        if sin_cambios {
            return Ok(None);
        }

        let forma = if hay_forma {
            Some(self.leer_forma_de_cursor(info.PointerShapeBufferSize)?)
        } else {
            None
        };

        self.ultimo_cursor = Some((visible, position));

        Ok(Some(CursorUpdate {
            visible,
            position,
            shape: forma,
        }))
    }

    fn leer_forma_de_cursor(&mut self, tamano: u32) -> Result<CursorShape, CaptureError> {
        if self.shape_scratch.len() < tamano as usize {
            self.shape_scratch.resize(tamano as usize, 0);
        }

        let mut info = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
        let mut requerido = 0u32;

        // Se clona la interfaz (un incremento de refcount) para soltar el prestamo sobre
        // `self` y poder pasar el scratch como mutable en la misma llamada.
        let duplicacion = self.duplicacion()?.clone();

        // SAFETY: el buffer tiene al menos `tamano` bytes, que es lo que declaramos, y los
        // dos punteros de salida apuntan a variables locales vivas.
        unsafe {
            duplicacion.GetFramePointerShape(
                tamano,
                self.shape_scratch.as_mut_ptr().cast(),
                &mut requerido,
                &mut info,
            )
        }
        .map_err(|e| clasificar(e, "GetFramePointerShape"))?;

        let kind = if info.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0 as u32 {
            PointerShapeKind::Monochrome
        } else if info.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0 as u32 {
            PointerShapeKind::Color
        } else if info.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR.0 as u32 {
            PointerShapeKind::MaskedColor
        } else {
            return Err(CaptureError::InvalidPointerShape(
                "tipo de forma de puntero desconocido",
            ));
        };

        cursor::decode_pointer_shape(
            kind,
            &self.shape_scratch[..tamano as usize],
            info.Pitch as usize,
            info.Width,
            info.Height,
            (info.HotSpot.x.max(0) as u32, info.HotSpot.y.max(0) as u32),
        )
    }

    fn leer_dirty_rects(&mut self) -> Result<Vec<Rect>, CaptureError> {
        let tamano_rect = size_of::<RECT>();
        let duplicacion = self.duplicacion()?.clone();

        loop {
            let capacidad = (self.dirty_scratch.len() * tamano_rect) as u32;
            let mut requerido = 0u32;

            // SAFETY: el puntero apunta a un `Vec` cuya longitud, en bytes, es exactamente
            // la que declaramos en `capacidad`.
            let resultado = unsafe {
                duplicacion.GetFrameDirtyRects(
                    capacidad,
                    self.dirty_scratch.as_mut_ptr(),
                    &mut requerido,
                )
            };

            match resultado {
                Ok(()) => {
                    let cuantos = (requerido as usize) / tamano_rect;
                    return Ok(self
                        .dirty_scratch
                        .iter()
                        .take(cuantos)
                        .map(rect_de_win32)
                        .collect());
                }
                Err(e) if e.code() == DXGI_ERROR_MORE_DATA => {
                    let necesarios = (requerido as usize).div_ceil(tamano_rect) + 8;
                    self.dirty_scratch.resize(necesarios, RECT::default());
                }
                Err(e) => return Err(clasificar(e, "GetFrameDirtyRects")),
            }
        }
    }

    fn leer_move_rects(&mut self) -> Result<Vec<MoveRect>, CaptureError> {
        let tamano_rect = size_of::<DXGI_OUTDUPL_MOVE_RECT>();
        let duplicacion = self.duplicacion()?.clone();

        loop {
            let capacidad = (self.move_scratch.len() * tamano_rect) as u32;
            let mut requerido = 0u32;

            // SAFETY: igual que en `leer_dirty_rects`.
            let resultado = unsafe {
                duplicacion.GetFrameMoveRects(
                    capacidad,
                    self.move_scratch.as_mut_ptr(),
                    &mut requerido,
                )
            };

            match resultado {
                Ok(()) => {
                    let cuantos = (requerido as usize) / tamano_rect;
                    return Ok(self
                        .move_scratch
                        .iter()
                        .take(cuantos)
                        .map(|m| MoveRect {
                            source: (m.SourcePoint.x, m.SourcePoint.y),
                            destination: rect_de_win32(&m.DestinationRect),
                        })
                        .collect());
                }
                Err(e) if e.code() == DXGI_ERROR_MORE_DATA => {
                    let necesarios = (requerido as usize).div_ceil(tamano_rect) + 8;
                    self.move_scratch
                        .resize(necesarios, DXGI_OUTDUPL_MOVE_RECT::default());
                }
                Err(e) => return Err(clasificar(e, "GetFrameMoveRects")),
            }
        }
    }

    fn copiar_pixeles(
        &mut self,
        recurso: &IDXGIResource,
        cursor: Option<CursorUpdate>,
        info: &DXGI_OUTDUPL_FRAME_INFO,
        capturado_en: Instant,
    ) -> Result<CaptureEvent, CaptureError> {
        let origen: ID3D11Texture2D = recurso
            .cast()
            .map_err(|e| envolver(e, "cast ID3D11Texture2D"))?;

        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `GetDesc` rellena una estructura del llamante.
        unsafe { origen.GetDesc(&mut desc) };
        let (width, height) = (desc.Width, desc.Height);

        let dirty = self.leer_dirty_rects()?;
        let moves = self.leer_move_rects()?;

        // FASE 4: con los dirty rects en la mano se puede copiar solo las regiones sucias
        // con CopySubresourceRegion en vez del frame entero. En la fase 1 la copia completa
        // esta bien y evita una fuente de bugs sutiles mientras el pipeline no funciona.
        let staging = self.asegurar_staging(width, height)?.clone();
        // SAFETY: las dos texturas tienen el mismo formato y dimensiones (la de staging se
        // creo a partir de las del origen), que es lo que `CopyResource` exige.
        unsafe { self.context.CopyResource(&staging, &origen) };

        let mut mapeado = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: la textura es de tipo staging con acceso de lectura por CPU, y el
        // destino del mapeo es una variable local viva.
        unsafe {
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapeado))
        }
        .map_err(|e| envolver(e, "Map"))?;

        let resultado = self.volcar_mapeado(&mapeado, width, height);

        // SAFETY: `Unmap` cierra el mapeo abierto justo arriba sobre la misma textura y
        // subrecurso. Se llama siempre, tambien si el volcado fallo.
        unsafe { self.context.Unmap(&staging, 0) };

        let buffer = resultado?;

        self.sequence += 1;
        let full_refresh = std::mem::take(&mut self.full_refresh_pending);

        Ok(CaptureEvent::Frame(Frame {
            buffer,
            width,
            height,
            stride: width as usize * 4,
            sequence: self.sequence,
            captured_at: capturado_en,
            presented_at_qpc: info.LastPresentTime,
            full_refresh,
            accumulated_frames: info.AccumulatedFrames,
            dirty,
            moves,
            cursor,
        }))
    }

    fn volcar_mapeado(
        &self,
        mapeado: &D3D11_MAPPED_SUBRESOURCE,
        width: u32,
        height: u32,
    ) -> Result<crate::pool::PooledBuffer, CaptureError> {
        let stride = mapeado.RowPitch as usize;
        let disponibles =
            stride
                .checked_mul(height as usize)
                .ok_or(CaptureError::BufferTooSmall {
                    needed: usize::MAX,
                    available: 0,
                })?;

        // SAFETY: `Map` acaba de devolver un puntero valido a `RowPitch * height` bytes
        // legibles, y el slice no sobrevive al `Unmap` que hace el llamante: se consume
        // dentro de `copy_frame`, antes de volver.
        let origen = unsafe { std::slice::from_raw_parts(mapeado.pData.cast::<u8>(), disponibles) };

        let mut destino = self.pool.take(width as usize * 4 * height as usize);
        copy_frame(origen, stride, &mut destino, width, height)?;
        Ok(destino)
    }
}

impl ScreenCapturer for DxgiCapturer {
    fn monitor(&self) -> &MonitorInfo {
        &self.monitor
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<CaptureEvent, CaptureError> {
        if !self.aviso_dpi_emitido && !dpi::is_per_monitor_aware() {
            self.aviso_dpi_emitido = true;
            tracing::warn!(
                monitor = %self.monitor.id,
                "capturando sin conciencia de DPI por monitor: las coordenadas pueden no \
                 cuadrar con las del input"
            );
        }

        if self.duplication.is_none() && !self.recuperar()? {
            return Ok(CaptureEvent::Timeout);
        }

        match self.intentar_capturar(timeout) {
            Err(CaptureError::AccessLost | CaptureError::AccessDenied) => {
                self.recuperar()?;
                // El ciclo siguiente ya traera frame; devolvemos Timeout en vez de un error
                // para que el bucle del host no interprete esto como una sesion caida.
                Ok(CaptureEvent::Timeout)
            }
            otro => otro,
        }
    }
}

impl DxgiCapturer {
    fn intentar_capturar(&mut self, timeout: Duration) -> Result<CaptureEvent, CaptureError> {
        let milisegundos = timeout.as_millis().min(u128::from(u32::MAX)) as u32;

        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut recurso: Option<IDXGIResource> = None;

        // SAFETY: los tres punteros de salida apuntan a variables locales vivas durante
        // toda la llamada.
        let adquirido = unsafe {
            self.duplicacion()?
                .AcquireNextFrame(milisegundos, &mut info, &mut recurso)
        };

        match adquirido {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(CaptureEvent::Timeout),
            Err(e) => return Err(clasificar(e, "AcquireNextFrame")),
        }

        let capturado_en = Instant::now();
        let resultado = self.procesar(&info, recurso.as_ref(), capturado_en);

        // ReleaseFrame debe llamarse antes del proximo AcquireNextFrame, pase lo que pase
        // con el procesado. Por eso se guarda el resultado y se libera antes de devolverlo.
        if let Some(duplicacion) = self.duplication.as_ref() {
            // SAFETY: hay exactamente un frame adquirido, que es el que se libera.
            let _ = unsafe { duplicacion.ReleaseFrame() };
        }

        resultado
    }

    fn procesar(
        &mut self,
        info: &DXGI_OUTDUPL_FRAME_INFO,
        recurso: Option<&IDXGIResource>,
        capturado_en: Instant,
    ) -> Result<CaptureEvent, CaptureError> {
        let cursor = self.leer_cursor(info)?;

        // LastPresentTime a cero significa que no hay pixeles nuevos: el sistema solo nos
        // esta contando que el puntero se movio. Recodificar la pantalla aqui seria tirar
        // ancho de banda y CPU en una imagen identica a la anterior.
        if info.LastPresentTime == 0 {
            return Ok(match cursor {
                Some(actualizacion) => CaptureEvent::CursorOnly(actualizacion),
                None => CaptureEvent::Timeout,
            });
        }

        let Some(recurso) = recurso else {
            return Err(CaptureError::AccessLost);
        };

        self.copiar_pixeles(recurso, cursor, info, capturado_en)
    }
}

fn crear_dispositivo(
    adaptador: &IDXGIAdapter1,
) -> Result<(ID3D11Device, ID3D11DeviceContext), CaptureError> {
    let niveles = [
        D3D_FEATURE_LEVEL_11_1,
        D3D_FEATURE_LEVEL_11_0,
        D3D_FEATURE_LEVEL_10_1,
        D3D_FEATURE_LEVEL_10_0,
    ];

    let adaptador_base: IDXGIAdapter = adaptador
        .cast()
        .map_err(|e| envolver(e, "cast IDXGIAdapter"))?;

    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;

    // SAFETY: al pasar un adaptador concreto, la API exige `D3D_DRIVER_TYPE_UNKNOWN`; con
    // cualquier otro valor devolveria E_INVALIDARG. Los destinos son variables locales.
    unsafe {
        D3D11CreateDevice(
            Some(&adaptador_base),
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&niveles),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(|e| envolver(e, "D3D11CreateDevice"))?;

    match (device, context) {
        (Some(device), Some(context)) => Ok((device, context)),
        _ => Err(CaptureError::Unsupported),
    }
}

fn rect_de_win32(rect: &RECT) -> Rect {
    Rect {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    }
}

fn punto_a_posicion(punto: POINT) -> CursorPosition {
    CursorPosition {
        x: punto.x,
        y: punto.y,
    }
}

/// Traduce los errores de DXGI que tienen un significado propio en el dominio de la
/// captura, para que quien los reciba no tenga que conocer los codigos de DXGI.
fn clasificar(error: windows::core::Error, operacion: &'static str) -> CaptureError {
    let codigo = error.code();

    if codigo == DXGI_ERROR_ACCESS_LOST || codigo == DXGI_ERROR_SESSION_DISCONNECTED {
        CaptureError::AccessLost
    } else if codigo == DXGI_ERROR_ACCESS_DENIED {
        CaptureError::AccessDenied
    } else if codigo == DXGI_ERROR_UNSUPPORTED {
        CaptureError::Unsupported
    } else {
        envolver(error, operacion)
    }
}

fn envolver(error: windows::core::Error, operacion: &'static str) -> CaptureError {
    CaptureError::Windows {
        operation: operacion,
        source: error,
    }
}

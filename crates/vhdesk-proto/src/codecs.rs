//! Registro de codecs y negociacion por sesion.
//!
//! El codec se negocia por sesion en lugar de fijarse en tiempo de compilacion. La razon
//! es que el camino software y el camino hardware no coinciden: VP8 es la linea base
//! libre de regalias para el encoder software, pero la decodificacion VP8 por hardware es
//! rara, mientras que H.264 la soporta practicamente cualquier GPU. Sin negociacion, el
//! dia que se active el encoder por hardware habria que romper el formato del protocolo.

use serde::{Deserialize, Serialize};

use crate::error::ProtoError;

/// Codec de video de un flujo de sesion.
///
/// Los valores del wire son estables: una vez asignado, un numero no se reutiliza nunca
/// para otro codec, aunque el codec se retire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum VideoCodec {
    /// VP8 por software (libvpx). Linea base obligatoria: todo peer debe soportarla.
    ///
    /// Implementado en la fase 1.
    Vp8,
    /// H.264 / AVC. Camino preferente cuando hay encoder y decoder por hardware.
    ///
    /// Implementado en la fase 4.
    H264,
    /// H.265 / HEVC.
    ///
    /// Implementado en la fase 4.
    H265,
    /// AV1. Solo por hardware; el encode AV1 por software no da tiempo real a 1080p.
    ///
    /// Implementado en la fase 4.
    Av1,
}

impl VideoCodec {
    /// Codec que todo peer esta obligado a implementar, y por tanto el que se elige
    /// cuando la interseccion de capacidades no deja nada mejor.
    pub const BASELINE: Self = Self::Vp8;

    /// Valor de este codec en el wire.
    pub const fn to_wire(self) -> u8 {
        match self {
            Self::Vp8 => 1,
            Self::H264 => 2,
            Self::H265 => 3,
            Self::Av1 => 4,
        }
    }

    /// Interpreta un valor del wire.
    ///
    /// # Errores
    ///
    /// Devuelve [`ProtoError::UnknownDiscriminant`] si el valor no esta asignado.
    pub const fn from_wire(value: u8) -> Result<Self, ProtoError> {
        match value {
            1 => Ok(Self::Vp8),
            2 => Ok(Self::H264),
            3 => Ok(Self::H265),
            4 => Ok(Self::Av1),
            other => Err(ProtoError::UnknownDiscriminant {
                field: "VideoCodec",
                value: other,
            }),
        }
    }
}

/// Codec de audio de un flujo de sesion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum AudioCodec {
    /// Opus. Linea base obligatoria.
    ///
    /// Implementado en la fase 5.
    Opus,
    /// PCM entero de 16 bits sin comprimir. Solo para diagnostico en red local: consume
    /// ancho de banda desproporcionado y nunca debe elegirse por defecto.
    ///
    /// Implementado en la fase 5.
    PcmS16,
}

impl AudioCodec {
    /// Codec que todo peer esta obligado a implementar.
    pub const BASELINE: Self = Self::Opus;

    /// Valor de este codec en el wire.
    pub const fn to_wire(self) -> u8 {
        match self {
            Self::Opus => 1,
            Self::PcmS16 => 2,
        }
    }

    /// Interpreta un valor del wire.
    ///
    /// # Errores
    ///
    /// Devuelve [`ProtoError::UnknownDiscriminant`] si el valor no esta asignado.
    pub const fn from_wire(value: u8) -> Result<Self, ProtoError> {
        match value {
            1 => Ok(Self::Opus),
            2 => Ok(Self::PcmS16),
            other => Err(ProtoError::UnknownDiscriminant {
                field: "AudioCodec",
                value: other,
            }),
        }
    }
}

/// Elige el primer codec de `preferidos` que aparezca tambien en `soportados`.
///
/// El orden de `preferidos` es la preferencia de quien decide (el host, que es quien
/// codifica), no la del viewer. Si no hay interseccion se devuelve `None` y el llamante
/// debe caer a [`VideoCodec::BASELINE`] o rechazar la sesion.
pub fn negotiate<T: Copy + PartialEq>(preferidos: &[T], soportados: &[T]) -> Option<T> {
    preferidos
        .iter()
        .copied()
        .find(|candidato| soportados.contains(candidato))
}

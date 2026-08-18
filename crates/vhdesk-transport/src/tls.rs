//! Identidad TLS de la sesion.
//!
//! # ATENCION: esto NO autentica nada todavia
//!
//! En la fase 1 el objetivo es ver imagen en una LAN, sin autenticacion. El certificado se
//! genera al vuelo en cada arranque y el cliente **acepta cualquier certificado**, asi que
//! el cifrado protege frente a un observador pasivo pero **no frente a un atacante activo
//! en el camino**. Es exactamente el modelo de amenazas de "cable cruzado entre dos
//! maquinas mias", y nada mas.
//!
//! La fase 2 sustituye esto por lo que dice el ADR-0001: certificado autofirmado cuyo SPKI
//! **es** la identidad, pinneado en el primer contacto (TOFU), y verificador de certificado
//! de **cliente** en el host para que la autenticacion sea mutua. Los puntos exactos donde
//! hay que tocar estan marcados con `// FASE 2:` en este archivo.

use std::sync::Arc;
use std::time::Duration;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

use crate::error::TransportError;

/// Protocolo de aplicacion que se negocia en el handshake TLS.
///
/// Sirve para que un QUIC ajeno que llegue a este puerto se rechace en el handshake en
/// lugar de avanzar hasta el protocolo y fallar alli con un error confuso.
pub const ALPN: &[u8] = b"vhdesk/1";

/// Cuanto se aguanta sin recibir nada del peer antes de dar la conexion por muerta.
///
/// Mas corto que los 30 s por defecto de quinn a proposito. Aqui detectar rapido un peer
/// muerto no es una optimizacion: mientras el host no se entera de que el viewer ya no
/// esta, cualquier tecla que quedara hundida sigue hundida. Ver la seccion de teclas
/// pegadas en `vhdesk-input`.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(15);

/// Cada cuanto se manda un PING cuando no hay nada que enviar.
///
/// **Sin esto la sesion se cae sola con la pantalla quieta.** No es un problema teorico:
/// los keyframes son bajo demanda y un escritorio inmovil no genera frames, asi que un
/// usuario que se levante a por un cafe deja de producir trafico de video por completo. Con
/// el `max_idle_timeout` por defecto de quinn (30 s) y `keep_alive_interval` en `None`, que
/// son los valores que traia esta configuracion, la conexion moria en medio minuto.
///
/// # Por que el keepalive de QUIC y no un datagrama de la aplicacion
///
/// Un `Message::Ping` periodico por datagrama haria lo mismo peor: QUIC ya tiene el frame
/// PING para exactamente esto, quinn lo emite solo cuando de verdad no hubo trafico (un
/// temporizador propio lo mandaria tambien en mitad de una sesion activa), y al vivir por
/// debajo de la aplicacion sigue funcionando aunque las tareas de arriba esten ocupadas.
/// Ademas es lo que mantendra vivo el mapeo NAT en la fase 3, que es un problema de la capa
/// de transporte y no del protocolo.
///
/// El intervalo tiene que quedar **bien por debajo** de [`IDLE_TIMEOUT`]: con 5 s contra 15
/// caben dos PING perdidos antes de que la conexion se de por muerta.
pub const KEEPALIVE: Duration = Duration::from_secs(5);

/// Ajustes de transporte comunes a los dos extremos.
///
/// El timeout de inactividad efectivo es el **minimo** de lo que anuncian los dos peers, asi
/// que fijarlo en un solo lado no basta y por eso esto se aplica al cliente y al servidor.
fn transport_config() -> Arc<quinn::TransportConfig> {
    let mut config = quinn::TransportConfig::default();

    config.keep_alive_interval(Some(KEEPALIVE));
    // `IdleTimeout` no admite cualquier duracion (esta acotado por el maximo que cabe en un
    // `VarInt`), pero 15 s entran de sobra; si algun dia alguien pone aqui algo absurdo,
    // preferimos el valor por defecto de quinn a un panico en el arranque.
    match quinn::IdleTimeout::try_from(IDLE_TIMEOUT) {
        Ok(timeout) => {
            config.max_idle_timeout(Some(timeout));
        }
        Err(error) => tracing::warn!(%error, "IDLE_TIMEOUT invalido; se usa el de quinn"),
    }

    Arc::new(config)
}

/// Identidad TLS de esta instalacion.
///
/// FASE 2: dejara de generarse en cada arranque para pasar a ser persistente, y su SPKI
/// sera el identificador con el que el peer nos reconoce entre sesiones.
pub struct Identity {
    /// Certificado en formato DER.
    pub certificate: CertificateDer<'static>,
    /// Clave privada correspondiente.
    pub key: PrivateKeyDer<'static>,
}

/// Instala el proveedor criptografico de rustls para todo el proceso.
///
/// **Llamala explicitamente al principio de `main`.** rustls 0.23 exige un proveedor por
/// defecto y no lo elige solo; si falta, el fallo aparece mucho despues, en mitad del
/// handshake, con un mensaje que no orienta hacia aqui. Es idempotente: si ya habia uno
/// instalado, no hace nada.
pub fn install_crypto_provider() {
    // El resultado se ignora a proposito: el `Err` solo significa "ya habia uno", que es
    // exactamente lo que queremos garantizar.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Genera un certificado autofirmado nuevo.
///
/// # Errores
///
/// Devuelve [`TransportError::Certificate`] si la generacion falla.
pub fn generate_self_signed() -> Result<Identity, TransportError> {
    // El nombre no se valida contra nada: en la fase 2 la identidad sera la clave publica,
    // no el nombre, porque un nombre en un certificado autofirmado no dice nada.
    let generado = rcgen::generate_simple_self_signed(vec!["vhdesk".to_owned()])
        .map_err(|e| TransportError::Certificate(e.to_string()))?;

    let key = PrivatePkcs8KeyDer::from(generado.signing_key.serialize_der());

    Ok(Identity {
        certificate: generado.cert.der().clone(),
        key: PrivateKeyDer::Pkcs8(key),
    })
}

/// Configuracion de servidor para quinn.
///
/// # Errores
///
/// Devuelve [`TransportError::Tls`] si rustls rechaza el certificado o la clave.
pub fn server_config(identity: Identity) -> Result<quinn::ServerConfig, TransportError> {
    // FASE 2: aqui hay que instalar un verificador de certificado de **cliente**, para que
    // el host valide la identidad del viewer igual que el viewer valida la del host. TLS
    // por defecto solo autentica un lado, y con `with_no_client_auth` cualquiera que
    // alcance este puerto completa el handshake.
    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![identity.certificate], identity.key)?;
    tls.alpn_protocols = vec![ALPN.to_vec()];

    let quic = QuicServerConfig::try_from(tls)?;
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(quic));
    config.transport_config(transport_config());
    Ok(config)
}

/// Configuracion de cliente que **acepta cualquier certificado**.
///
/// # Errores
///
/// Devuelve [`TransportError::Tls`] si la configuracion resultante no es valida para QUIC.
pub fn client_config_insecure() -> Result<quinn::ClientConfig, TransportError> {
    // FASE 2: sustituir `AcceptAnyServerCert` por el verificador de pinning SPKI, y anadir
    // el certificado de cliente de esta instalacion para la autenticacion mutua.
    let mut tls = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert))
        .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN.to_vec()];

    let quic = QuicClientConfig::try_from(tls)?;
    let mut config = quinn::ClientConfig::new(Arc::new(quic));
    config.transport_config(transport_config());
    Ok(config)
}

/// Verificador que da por bueno cualquier certificado de servidor.
///
/// FASE 2: este tipo entero desaparece. Se sustituye por un verificador que compara el
/// SPKI del certificado contra el que se pinneo en el primer contacto y avisa de forma
/// prominente si cambia.
#[derive(Debug)]
struct AcceptAnyServerCert;

impl ServerCertVerifier for AcceptAnyServerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        // FASE 2: comparar el SPKI con el pinneado.
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        // QUIC solo usa TLS 1.3, asi que este camino no se recorre nunca.
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        // FASE 2: verificar de verdad contra la clave publica pinneada. Aceptar la firma
        // sin comprobarla es lo que hace que esto no proteja frente a un atacante activo.
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::{generate_self_signed, install_crypto_provider};

    #[test]
    fn cada_arranque_genera_una_identidad_distinta() {
        install_crypto_provider();

        let primera = generate_self_signed().expect("generar");
        let segunda = generate_self_signed().expect("generar");

        assert!(!primera.certificate.is_empty());
        assert_ne!(
            primera.certificate, segunda.certificate,
            "en la fase 1 la identidad es efimera; cuando deje de serlo en la fase 2 este \
             test tiene que cambiar de sentido, no borrarse"
        );
    }
}

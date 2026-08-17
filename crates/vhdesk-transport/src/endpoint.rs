//! El endpoint QUIC.

use std::net::SocketAddr;

use crate::error::TransportError;
use crate::session::Session;
use crate::tls;

/// Un endpoint QUIC que sirve **a la vez** para escuchar y para conectar.
///
/// Que sea uno solo no es comodidad, es un requisito del ADR-0001 que hay que respetar
/// desde ahora aunque no se note hasta la fase 3: el hole punching solo funciona
/// perforando desde el mismo socket UDP cuya direccion reflexiva observo el servidor de
/// rendezvous. Con un endpoint para hablar con el servidor y otro para hablar con el peer,
/// el mapeo NAT que aprendio el servidor no vale para nada, y descubrirlo entonces
/// obligaria a rehacer esta capa entera.
pub struct Endpoint {
    inner: quinn::Endpoint,
}

impl Endpoint {
    /// Abre el endpoint en la direccion dada.
    ///
    /// Con `0` como puerto, el sistema elige uno libre; se consulta con
    /// [`Endpoint::local_addr`].
    ///
    /// # Errores
    ///
    /// Devuelve [`TransportError::Bind`] si el puerto esta ocupado o la direccion no es
    /// valida.
    pub fn bind(addr: SocketAddr) -> Result<Self, TransportError> {
        let identidad = tls::generate_self_signed()?;
        let servidor = tls::server_config(identidad)?;

        let mut inner = quinn::Endpoint::server(servidor, addr)
            .map_err(|source| TransportError::Bind { addr, source })?;

        // El mismo endpoint queda configurado tambien como cliente.
        inner.set_default_client_config(tls::client_config_insecure()?);

        Ok(Self { inner })
    }

    /// Direccion local real, util cuando se pidio el puerto 0.
    ///
    /// # Errores
    ///
    /// Devuelve [`TransportError::Bind`] si el socket ya no esta disponible.
    pub fn local_addr(&self) -> Result<SocketAddr, TransportError> {
        self.inner
            .local_addr()
            .map_err(|source| TransportError::Bind {
                addr: SocketAddr::from(([0, 0, 0, 0], 0)),
                source,
            })
    }

    /// Espera a que un peer se conecte.
    ///
    /// # Errores
    ///
    /// Devuelve [`TransportError::EndpointClosed`] si el endpoint se cerro.
    pub async fn accept(&self) -> Result<Session, TransportError> {
        let entrante = self
            .inner
            .accept()
            .await
            .ok_or(TransportError::EndpointClosed)?;

        let conn = entrante.await?;
        Ok(Session::new(conn))
    }

    /// Conecta con un peer.
    ///
    /// # Errores
    ///
    /// Devuelve [`TransportError::Connect`] si el peer no responde o rechaza la conexion.
    pub async fn connect(&self, addr: SocketAddr) -> Result<Session, TransportError> {
        // El nombre no se verifica contra nada en la fase 1; ver `tls`.
        let conn = self
            .inner
            .connect(addr, "vhdesk")
            .map_err(|e| TransportError::Certificate(e.to_string()))?
            .await
            .map_err(|source| TransportError::Connect { addr, source })?;

        Ok(Session::new(conn))
    }

    /// Espera a que todas las conexiones terminen de cerrarse.
    ///
    /// Hay que llamarlo antes de salir del proceso: si no, el ultimo paquete de cierre
    /// puede quedarse sin enviar y el peer tarda en enterarse.
    pub async fn wait_idle(&self) {
        self.inner.wait_idle().await;
    }
}

//! Argumentos de linea de ordenes.

use std::net::SocketAddr;

use anyhow::{Context, Result, bail};

/// Puerto por defecto del host.
const PUERTO: u16 = 21118;

/// Configuracion con la que arranca el daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cli {
    /// Direccion en la que escuchar.
    pub listen: SocketAddr,
    /// Indice del monitor a servir, dentro de la lista que devuelve la enumeracion.
    pub monitor: u8,
    /// Bitrate objetivo en kbps.
    pub bitrate_kbps: u32,
    /// Cota superior de frames por segundo.
    pub fps: u32,
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([0, 0, 0, 0], PUERTO)),
            monitor: 0,
            // 8 Mbps y 30 fps son los valores comprometidos de la fase 1. Los 60 fps siguen
            // abiertos, pendientes de la medicion extremo a extremo del bloque F.
            bitrate_kbps: 8_000,
            fps: 30,
        }
    }
}

/// Texto de ayuda.
pub const AYUDA: &str = "\
vhdesk-host: sirve esta maquina a un viewer de VHDesk.

  --listen <ip:puerto>   direccion de escucha (por defecto 0.0.0.0:21118)
  --monitor <indice>     monitor a servir (por defecto 0)
  --bitrate <kbps>       bitrate objetivo (por defecto 8000)
  --fps <n>              cota real de frames por segundo (por defecto 30)
  --help                 esta ayuda

El nivel de trazas se controla con la variable de entorno VHDESK_LOG.
";

/// Interpreta los argumentos.
///
/// # Errores
///
/// Devuelve error si falta el valor de una opcion, si no se puede interpretar, o si la
/// opcion no existe.
pub fn parsear<I: IntoIterator<Item = String>>(argumentos: I) -> Result<Option<Cli>> {
    let mut cli = Cli::default();
    let mut iterador = argumentos.into_iter();

    while let Some(bandera) = iterador.next() {
        let mut valor = || -> Result<String> {
            iterador
                .next()
                .with_context(|| format!("a {bandera} le falta el valor"))
        };

        match bandera.as_str() {
            "--help" | "-h" => return Ok(None),
            "--listen" => cli.listen = valor()?.parse().context("direccion invalida")?,
            "--monitor" => cli.monitor = valor()?.parse().context("monitor invalido")?,
            "--bitrate" => cli.bitrate_kbps = valor()?.parse().context("bitrate invalido")?,
            "--fps" => cli.fps = valor()?.parse().context("fps invalido")?,
            otro => bail!("argumento desconocido: {otro}\n\n{AYUDA}"),
        }
    }

    if cli.bitrate_kbps == 0 {
        bail!("el bitrate no puede ser cero");
    }
    if cli.fps == 0 {
        bail!("los fps no pueden ser cero");
    }

    Ok(Some(cli))
}

#[cfg(test)]
mod tests {
    use super::{Cli, parsear};

    fn args(lista: &[&str]) -> Vec<String> {
        lista.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn sin_argumentos_valen_los_valores_por_defecto() {
        let cli = parsear(args(&[])).expect("parsear").expect("no es ayuda");
        assert_eq!(cli, Cli::default());
        assert_eq!(cli.listen.port(), 21118);
    }

    #[test]
    fn las_opciones_se_interpretan() {
        let cli = parsear(args(&[
            "--listen",
            "127.0.0.1:9000",
            "--monitor",
            "1",
            "--bitrate",
            "4000",
            "--fps",
            "60",
        ]))
        .expect("parsear")
        .expect("no es ayuda");

        assert_eq!(cli.listen.to_string(), "127.0.0.1:9000");
        assert_eq!(cli.monitor, 1);
        assert_eq!(cli.bitrate_kbps, 4_000);
        assert_eq!(cli.fps, 60);
    }

    #[test]
    fn help_pide_ayuda_en_vez_de_arrancar() {
        assert!(parsear(args(&["--help"])).expect("parsear").is_none());
    }

    #[test]
    fn un_valor_que_falta_o_no_vale_se_rechaza() {
        assert!(parsear(args(&["--listen"])).is_err(), "falta el valor");
        assert!(parsear(args(&["--listen", "no-una-ip"])).is_err());
        assert!(
            parsear(args(&["--fps", "0"])).is_err(),
            "cero fps no tiene sentido"
        );
        assert!(parsear(args(&["--bitrate", "0"])).is_err());
        assert!(parsear(args(&["--inventada"])).is_err());
    }
}

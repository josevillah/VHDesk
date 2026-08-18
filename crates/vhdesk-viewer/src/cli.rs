//! Argumentos de linea de ordenes del viewer.

use std::net::SocketAddr;

use anyhow::{Context, Result, bail};

/// Configuracion con la que arranca el viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cli {
    /// Host al que conectarse.
    pub connect: SocketAddr,
    /// Si se sincroniza la presentacion con el refresco del monitor.
    pub vsync: bool,
}

/// Texto de ayuda.
pub const AYUDA: &str = "\
vhdesk-viewer: ve y controla otra maquina con VHDesk.

  --connect <ip:puerto>  host al que conectarse (obligatorio)
  --vsync                sincroniza con el refresco del monitor
  --help                 esta ayuda

Sin --vsync la ventana presenta en cuanto tiene un frame listo. Es lo correcto para
medir latencia: el vsync anade hasta un periodo de refresco (16,7 ms a 60 Hz) que no
tiene nada que ver con el pipeline y falsearia la medida del bloque F. Con --vsync se
evita el desgarro, que es lo que se quiere para usarlo de verdad.

El nivel de trazas se controla con la variable de entorno VHDESK_LOG.
";

/// Interpreta los argumentos.
///
/// Devuelve `Ok(None)` si se pidio la ayuda.
///
/// # Errores
///
/// Devuelve error si falta `--connect`, si a una opcion le falta el valor, o si la opcion
/// no existe.
pub fn parsear<I: IntoIterator<Item = String>>(argumentos: I) -> Result<Option<Cli>> {
    let mut connect: Option<SocketAddr> = None;
    let mut vsync = false;
    let mut iterador = argumentos.into_iter();

    while let Some(bandera) = iterador.next() {
        match bandera.as_str() {
            "--help" | "-h" => return Ok(None),
            "--vsync" => vsync = true,
            "--connect" => {
                let valor = iterador
                    .next()
                    .context("a --connect le falta la direccion")?;
                connect = Some(valor.parse().context("direccion invalida")?);
            }
            otro => bail!("argumento desconocido: {otro}\n\n{AYUDA}"),
        }
    }

    // Sin destino no hay nada que hacer, y abrir una ventana vacia para luego decirlo seria
    // peor que fallar aqui con la ayuda delante.
    let connect = connect.context("falta --connect <ip:puerto>")?;

    Ok(Some(Cli { connect, vsync }))
}

#[cfg(test)]
mod tests {
    use super::parsear;

    fn args(lista: &[&str]) -> Vec<String> {
        lista.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn connect_es_obligatorio() {
        assert!(
            parsear(args(&[])).is_err(),
            "sin destino no hay nada que hacer"
        );
        assert!(parsear(args(&["--vsync"])).is_err());
    }

    #[test]
    fn se_interpreta_el_destino_y_el_vsync() {
        let cli = parsear(args(&["--connect", "192.168.1.50:21118"]))
            .expect("parsear")
            .expect("no es ayuda");
        assert_eq!(cli.connect.to_string(), "192.168.1.50:21118");
        assert!(!cli.vsync, "el vsync esta apagado por defecto");

        let cli = parsear(args(&["--connect", "127.0.0.1:21118", "--vsync"]))
            .expect("parsear")
            .expect("no es ayuda");
        assert!(cli.vsync);
    }

    #[test]
    fn help_pide_ayuda_en_vez_de_arrancar() {
        assert!(parsear(args(&["--help"])).expect("parsear").is_none());
        // La ayuda gana aunque falte el destino: pedirla no deberia dar un error de uso.
        assert!(parsear(args(&["-h"])).expect("parsear").is_none());
    }

    #[test]
    fn un_valor_que_falta_o_no_vale_se_rechaza() {
        assert!(parsear(args(&["--connect"])).is_err(), "falta el valor");
        assert!(parsear(args(&["--connect", "no-una-ip"])).is_err());
        assert!(
            parsear(args(&["--connect", "1.2.3.4"])).is_err(),
            "falta el puerto"
        );
        assert!(parsear(args(&["--inventada"])).is_err());
    }
}

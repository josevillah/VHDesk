//! Errores de inyeccion de entrada.

/// Fallo al inyectar un evento.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InputError {
    /// El sistema acepto menos eventos de los que se le pasaron.
    ///
    /// Casi siempre es UIPI: la ventana en primer plano pertenece a un proceso con mayor
    /// nivel de integridad (algo ejecutado como administrador) y Windows bloquea la
    /// entrada sintetica hacia ella. **No devuelve error de la API**, simplemente inserta
    /// menos eventos, asi que ignorar el valor de retorno produce el sintoma "a veces no
    /// responde el teclado" sin ninguna pista de por que.
    #[error("el sistema solo acepto {insertados} de {esperados} eventos; probablemente UIPI")]
    Bloqueado {
        /// Eventos que el sistema acepto.
        insertados: u32,
        /// Eventos que se le pasaron.
        esperados: u32,
    },

    /// El scancode HID recibido no tiene equivalente en esta plataforma.
    #[error("la tecla HID 0x{hid:04x} no tiene equivalente en esta plataforma")]
    TeclaNoSoportada {
        /// Usage ID de la tabla HID que llego.
        hid: u32,
    },

    /// No se pudieron obtener las dimensiones del escritorio virtual.
    #[error("no se pudo consultar la geometria del escritorio virtual")]
    EscritorioDesconocido,

    /// La inyeccion de entrada no esta implementada en esta plataforma.
    #[error("la inyeccion de entrada todavia no esta implementada en esta plataforma")]
    UnsupportedPlatform,
}

//! Conciencia de DPI del proceso.
//!
//! Sin conciencia de DPI por monitor, Windows miente a la aplicacion en cuanto hay
//! escalado: reporta resoluciones virtualizadas y las coordenadas que devuelve la captura
//! dejan de cuadrar con las que acepta la inyeccion de input. El sintoma aparece lejos de
//! la causa, en forma de "el raton no va donde apunto".
//!
//! Ajustar esto es un efecto **global del proceso**, asi que la libreria no lo hace por su
//! cuenta: [`ensure_dpi_awareness`] existe para que la llame el `main` del binario, y el
//! capturador se limita a comprobarlo y avisar si no esta puesto.

use windows::Win32::UI::HiDpi::{
    AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    GetThreadDpiAwarenessContext, SetProcessDpiAwarenessContext,
};

/// Si el proceso ya tiene conciencia de DPI por monitor v2.
pub fn is_per_monitor_aware() -> bool {
    // SAFETY: ninguna de las dos llamadas recibe punteros ni asigna nada.
    // `GetThreadDpiAwarenessContext` devuelve un valor opaco valido siempre, y
    // `AreDpiAwarenessContextsEqual` solo lo compara con una constante del sistema.
    unsafe {
        let actual = GetThreadDpiAwarenessContext();
        AreDpiAwarenessContextsEqual(actual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).as_bool()
    }
}

/// Declara el proceso como consciente del DPI por monitor v2.
///
/// **Afecta al proceso entero y solo puede hacerse una vez**, antes de crear ninguna
/// ventana. Llamala desde el `main` de tu binario, no desde una libreria.
///
/// Devuelve `true` si al terminar el proceso tiene la conciencia correcta, tanto si la
/// puso esta llamada como si ya venia puesta (por ejemplo desde el manifiesto del
/// ejecutable, que es lo que ocurrira cuando haya instalador en la fase 6).
pub fn ensure_dpi_awareness() -> bool {
    if is_per_monitor_aware() {
        return true;
    }

    // SAFETY: la funcion solo recibe una constante del sistema. Falla de forma limpia
    // devolviendo un error si el contexto ya estaba fijado, caso que cubre la
    // comprobacion posterior.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };

    is_per_monitor_aware()
}

//! Inyeccion de entrada de verdad, comprobada contra el propio sistema.
//!
//! Todos van marcados `#[ignore]` por dos razones: no pueden correr en CI, y **secuestran
//! el raton y el teclado** de quien ejecute la suite. Nadie deberia perder el control de su
//! maquina por lanzar `cargo test`.
//!
//! ```text
//! cargo test -p vhdesk-input --test inyeccion -- --ignored --test-threads=1
//! ```
//!
//! `--test-threads=1` no es opcional: hay un solo puntero y un solo teclado, y dos tests
//! moviendolos a la vez se pisan.
//!
//! Estos tests verifican lo que los tests puros **no pueden**: que la aritmetica de
//! normalizacion, al pasar por Windows y volver, deja el puntero en el pixel correcto.

#![cfg(windows)]

use std::time::Duration;

use vhdesk_input::{EscritorioVirtual, InputInjector, open_injector};
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};

fn escritorio() -> EscritorioVirtual {
    // SAFETY: `GetSystemMetrics` solo lee un valor a partir de un indice constante.
    unsafe {
        EscritorioVirtual {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
            ancho: GetSystemMetrics(SM_CXVIRTUALSCREEN),
            alto: GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    }
}

fn posicion_del_cursor() -> (i32, i32) {
    let mut punto = POINT::default();
    // SAFETY: el destino es una variable local viva durante la llamada.
    unsafe { GetCursorPos(&mut punto) }.expect("consultar la posicion del cursor");
    (punto.x, punto.y)
}

/// Mueve el puntero y devuelve donde acabo de verdad.
fn mover_y_leer(injector: &mut dyn InputInjector, x: i32, y: i32) -> (i32, i32) {
    injector.mouse_move_absolute(x, y).expect("mover el raton");
    // El movimiento no es instantaneo: entra en la cola de entrada del sistema.
    std::thread::sleep(Duration::from_millis(60));
    posicion_del_cursor()
}

#[test]
#[ignore = "mueve el raton de verdad; ejecutalo a mano con --ignored --test-threads=1"]
fn las_cuatro_esquinas_del_escritorio_son_alcanzables() {
    let mut injector = open_injector().expect("abrir el injector");
    let e = escritorio();

    // El ultimo pixel es `origen + tamano - 1`, no `origen + tamano`. Con el denominador
    // ingenuo en la normalizacion, las esquinas de la derecha y de abajo quedarian a unos
    // pocos pixeles del borde y ahi es donde estan el boton de inicio y la X de cerrar.
    let esquinas = [
        (e.x, e.y),
        (e.x + e.ancho - 1, e.y),
        (e.x, e.y + e.alto - 1),
        (e.x + e.ancho - 1, e.y + e.alto - 1),
    ];

    for (x, y) in esquinas {
        let (real_x, real_y) = mover_y_leer(injector.as_mut(), x, y);
        assert_eq!(
            (real_x, real_y),
            (x, y),
            "se pidio la esquina ({x}, {y}) y el puntero acabo en ({real_x}, {real_y})"
        );
    }
}

#[test]
#[ignore = "mueve el raton de verdad; ejecutalo a mano con --ignored --test-threads=1"]
fn un_punto_del_centro_cae_dentro_de_un_pixel() {
    let mut injector = open_injector().expect("abrir el injector");
    let e = escritorio();

    let objetivo = (e.x + e.ancho / 3, e.y + e.alto / 3);
    let (real_x, real_y) = mover_y_leer(injector.as_mut(), objetivo.0, objetivo.1);

    // Un pixel de tolerancia: el viaje de ida y vuelta por el rango de 65535 no siempre es
    // exacto en el interior, y no importa. En las esquinas si se exige exactitud.
    assert!(
        (real_x - objetivo.0).abs() <= 1 && (real_y - objetivo.1).abs() <= 1,
        "se pidio {objetivo:?} y el puntero acabo en ({real_x}, {real_y})"
    );
}

#[test]
#[ignore = "pulsa teclas de verdad; ejecutalo a mano con --ignored --test-threads=1"]
fn liberar_todo_suelta_un_modificador_hundido() {
    /// Usage ID de HID de Mayus izquierdo.
    const MAYUS_IZQUIERDO: u32 = 0xE1;

    let mut injector = open_injector().expect("abrir el injector");

    injector
        .key(MAYUS_IZQUIERDO, true)
        .expect("hundir el modificador");
    std::thread::sleep(Duration::from_millis(60));

    assert!(
        esta_hundida_mayus(),
        "el modificador deberia estar hundido tras inyectar su pulsacion"
    );

    // Y aqui es donde se comprueba lo que de verdad importa: que no se queda pegado.
    injector.liberar_todo().expect("liberar");
    std::thread::sleep(Duration::from_millis(60));

    assert!(
        !esta_hundida_mayus(),
        "el modificador quedo pegado: con Ctrl o Alt esto deja la maquina remota inservible \
         y el usuario no puede relacionarlo con la causa"
    );
}

fn esta_hundida_mayus() -> bool {
    // SAFETY: `GetAsyncKeyState` solo consulta el estado de una tecla por su codigo
    // virtual; no recibe punteros.
    let estado = unsafe { GetAsyncKeyState(VK_SHIFT.0 as i32) };
    // El bit mas significativo indica que la tecla esta hundida ahora mismo.
    (estado as u16 & 0x8000) != 0
}

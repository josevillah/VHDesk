# ADR-0002: Cómo obtenemos libvpx y sus enlaces FFI

- **Estado**: aceptado
- **Fecha**: 2026-08-17
- **Fase**: 1, bloque B

## Contexto

El [ADR-0001](0001-stack-inicial.md) fijó VP8 con libvpx como línea base del camino
software. libvpx es una biblioteca en C: hay que conseguirla, enlazarla y generar los
enlaces FFI de Rust. En Linux y macOS es un paquete del sistema y `pkg-config`; en Windows
con MSVC no hay ninguna de las dos cosas.

Se intentaron dos caminos con el crate `env-libvpx-sys` antes de llegar aquí, y los dos
fallaron. Merece la pena dejar constancia porque el segundo fallo es sutil y peligroso.

## La decisión: un crate `-sys` propio

`crates/vhdesk-libvpx-sys` es un crate mínimo del workspace: un `ffi.h` con las siete
cabeceras que usamos y un `build.rs` de unas cincuenta líneas que localiza la biblioteca y
ejecuta bindgen. La envoltura segura sigue estando en `vhdesk-codec`.

**Lo esencial: bindgen se ejecuta contra las cabeceras de la biblioteca que realmente se va
a enlazar, y para el objetivo real de compilación.** De ahí salen las dos propiedades que
buscábamos, y ninguna depende de que nadie acierte con una versión.

La biblioteca se obtiene con vcpkg en Windows (modo manifiesto, `vcpkg.json` en la raíz con
`builtin-baseline`, triplete `x64-windows-static-md`) y con el paquete del sistema en Linux
y macOS, localizado por `pkg-config`. Como los enlaces se generan de las cabeceras que haya,
que la versión difiera entre plataformas ya no es un problema de corrección.

El triplete importa: `x64-windows` produce DLL que habría que distribuir, y
`x64-windows-static` enlaza también el runtime de C, que entonces no cuadra con el de Rust.
`x64-windows-static-md` es la combinación correcta. No hace falta `VPX_STATIC`: esa variable
solo cambia el nombre que se le pide al enlazador, y como el `vpx.lib` de vcpkg ya es un
archivo estático, `-l vpx` lo incorpora entero igual.

## Por qué no `env-libvpx-sys`

### Intento 1: enlaces generados con bindgen (feature `generate`)

El crate fija bindgen 0.68.1, de agosto de 2023. Con libclang 22 bindgen degrada
`vpx_codec_enc_cfg` y `vpx_codec_dec_cfg` a tipos opacos sin campos, y no falla: emite
enlaces inutilizables. Las cabeceras estaban bien (`clang -fsyntax-only` las parsea sin una
queja) y los enlaces pregenerados del propio crate contienen esas estructuras completas.

### Intento 2: enlaces pregenerados

Peor, y esta es la razón de fondo de todo el ADR. Los enlaces pregenerados declaran:

```rust
pub type size_t = ::std::os::raw::c_ulong;
```

Eso es cierto en Linux, donde `unsigned long` mide 64 bits. **En Windows x64 es falso**:
`unsigned long` mide 32 bits y `size_t` mide 64. Se generaron en Linux y se distribuyen
como si fueran independientes de la plataforma.

La consecuencia no es que un campo se lea mal: es que **todos los campos que van detrás de
un `size_t` dentro de una estructura quedan en el desplazamiento equivocado**. En
`vpx_codec_cx_pkt`, el paquete que devuelve el codificador con cada frame, eso desplaza
`pts`, `duration`, `flags` y `partition_id`.

Aquí lo destapó el compilador por casualidad, porque `slice::from_raw_parts` pide `usize` y
se encontró un `u32`. Si el campo hubiera coincidido por accidente, habría compilado y
leído basura en tiempo de ejecución, en el camino por el que pasan todos los frames.

Generar los enlaces para el objetivo real elimina esta clase de error entera. Además se
dejan activas las comprobaciones de layout de bindgen, que verifican en tiempo de
compilación el tamaño y el desplazamiento de cada campo contra lo que dijo el compilador de
C: si alguna vez vuelve a haber un desajuste, no compila.

**Descartado también**: declarar una `VPX_VERSION` distinta de la instalada.
`vpx_codec_enc_config_default()` escribe dentro de una estructura que reserva quien llama;
si creció entre versiones, la biblioteca escribe más allá del final del buffer.

## Consecuencias

- Compilar VHDesk exige **libvpx y LLVM** (bindgen necesita libclang) en las tres
  plataformas. En Windows, además, vcpkg. Hay que documentarlo en el README.
- `vcpkg_installed/` es salida de compilación y no debe versionarse.
- En Windows hacen falta `VPX_LIB_DIR` y `VPX_INCLUDE_DIR`; en Linux y macOS basta
  `pkg-config`. El `build.rs` da un mensaje de error que explica ambas cosas si no
  encuentra la biblioteca.
- Mantenemos ~50 líneas de `build.rs` propias. A cambio dejamos de depender de un crate sin
  mantener cuyos enlaces son incorrectos en nuestra plataforma principal.
- Estamos en libvpx 1.16.0, la del baseline de vcpkg. Subir de versión ya no requiere nada
  especial.
- **Fase 6**: al enlazar libvpx estáticamente no hay DLL que distribuir, pero libvpx es
  BSD-3-Clause y hay que incluir su aviso de copyright. No entra en conflicto con GPL-3.0.

## Qué nos haría cambiar de opinión

- **Volver a un crate de terceros** si aparece uno mantenido que genere los enlaces para el
  objetivo real. Nuestro `build.rs` es pequeño, pero es código que no querríamos mantener
  si alguien lo hace mejor.
- **Vendorizar libvpx y compilarlo desde `build.rs`**, si vcpkg resulta ser una barrera
  real para contribuir. Se descartó porque el sistema de compilación de libvpx en MSVC es
  configure/make con yasm.
- **Abandonar VP8 como línea base**, con las condiciones que ya recoge el ADR-0001. Esta
  decisión desaparecería con él.

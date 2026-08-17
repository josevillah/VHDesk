# Compilar VHDesk

Además del toolchain de Rust, VHDesk necesita dos cosas nativas: **libvpx** (el códec de
vídeo) y **LLVM** (porque los enlaces FFI a libvpx se generan con bindgen, que usa
libclang). El porqué de esa segunda dependencia está en
[ADR-0002](adr/0002-libvpx-en-windows.md).

## Requisitos comunes

- Rust estable reciente. El `rust-toolchain.toml` de la raíz fija el canal y los
  componentes, así que `rustup` lo instalará solo la primera vez.
- LLVM 16 o posterior, para `libclang`.

## Windows

```powershell
winget install LLVM.LLVM
git clone https://github.com/microsoft/vcpkg C:/vcpkg
C:/vcpkg/bootstrap-vcpkg.bat
```

Y desde la raíz del repositorio:

```powershell
C:/vcpkg/vcpkg.exe install --triplet x64-windows-static-md
```

Eso lee el [`vcpkg.json`](../vcpkg.json) de la raíz, que fija la versión de libvpx con un
`builtin-baseline`, y deja el resultado en `vcpkg_installed/`. El `build.rs` lo encuentra
solo: sube directorios hasta dar con `vcpkg.json` y busca ahí. No hay que definir ninguna
variable de entorno.

El triplete no es opcional. `x64-windows` genera DLL que habría que distribuir junto al
ejecutable, y `x64-windows-static` enlaza también el runtime de C, que entonces no cuadra
con el que usa Rust en MSVC. `x64-windows-static-md` es la combinación correcta.

Si prefieres una instalación global de vcpkg en lugar del manifiesto, define `VCPKG_ROOT` y
el `build.rs` también la encontrará.

## Linux

```bash
sudo apt install libvpx-dev pkg-config clang
```

O el equivalente de tu distribución. `build.rs` localiza libvpx con `pkg-config`.

## macOS

```bash
brew install libvpx pkg-config llvm
```

Las herramientas de línea de comandos de Xcode ya traen una `libclang` que suele bastar;
`llvm` de Homebrew es el respaldo si bindgen no la encuentra.

## Compilar y comprobar

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

Las cuatro tienen que pasar limpias.

## Si libvpx no aparece

`build.rs` busca en este orden:

1. `VPX_LIB_DIR` y `VPX_INCLUDE_DIR`, si están las dos definidas.
2. `vcpkg_installed/<triplete>` en la raíz del repositorio.
3. `VCPKG_ROOT/installed/<triplete>` y `VCPKG_INSTALLATION_ROOT/installed/<triplete>`.
4. `pkg-config`.

Las dos primeras variables son un **override opcional** para instalaciones en sitios raros,
no el camino normal. Si las defines, defínelas juntas: con una sola no hace nada.

```powershell
$env:VPX_LIB_DIR     = 'D:\libvpx\lib'
$env:VPX_INCLUDE_DIR = 'D:\libvpx\include'
```

Si aun así falla, el mensaje de error del `build.rs` dice qué instalar en cada sistema.

## Tests que no corren solos

Los tests de captura que necesitan una sesión de escritorio están marcados `#[ignore]`,
porque los runners de CI no tienen una pantalla con la que DXGI Desktop Duplication
funcione. Para ejecutarlos a mano:

```bash
cargo test -p vhdesk-capture --test dxgi -- --ignored --nocapture
```

## Bancos de pruebas

Miden sobre capturas reales del escritorio y **hay que ejecutarlos en release**; en debug
los números no significan nada.

```bash
cargo run -p vhdesk-codec --example bench-pipeline --release
cargo run -p vhdesk-codec --example bench-yuv --release
cargo run -p vhdesk-capture --example dump-frames --release -- --idle 20
```

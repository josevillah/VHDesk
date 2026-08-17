//! Localiza libvpx y genera sus enlaces FFI con bindgen.
//!
//! Los enlaces se generan **para el objetivo real de compilacion**, y esa es la razon de
//! que este crate exista en lugar de usar uno de terceros. Los enlaces pregenerados que
//! circulan por ahi suelen estar hechos en Linux, donde `size_t` es `unsigned long`; en
//! Windows x64 eso es falso (`unsigned long` mide 32 bits y `size_t` 64), asi que todos
//! los campos que van detras de un `size_t` en una estructura quedan desplazados. Es un
//! error que no da fallo de compilacion y corrompe memoria en tiempo de ejecucion.
//!
//! Ver `docs/adr/0002-libvpx-en-windows.md` y `docs/BUILDING.md`.

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=ffi.h");
    println!("cargo:rerun-if-env-changed=VPX_LIB_DIR");
    println!("cargo:rerun-if-env-changed=VPX_INCLUDE_DIR");
    println!("cargo:rerun-if-env-changed=VCPKG_ROOT");
    println!("cargo:rerun-if-env-changed=VCPKG_INSTALLATION_ROOT");

    let include_dir = localizar();
    generar_enlaces(&include_dir);
}

/// Encuentra libvpx y emite las directivas de enlazado.
///
/// Devuelve el directorio de cabeceras que hay que pasarle a bindgen. El orden de busqueda
/// va de lo mas explicito a lo mas automatico, de forma que quien necesite apuntar a una
/// instalacion concreta pueda hacerlo sin pelearse con la deteccion.
fn localizar() -> PathBuf {
    if let Some(include_dir) = desde_variables() {
        return include_dir;
    }

    if let Some(include_dir) = desde_vcpkg() {
        return include_dir;
    }

    desde_pkg_config()
}

/// Ruta indicada a mano. Es un *override* opcional, no el camino habitual.
fn desde_variables() -> Option<PathBuf> {
    let lib_dir = env::var("VPX_LIB_DIR").ok()?;
    let include_dir = env::var("VPX_INCLUDE_DIR").ok()?;

    enlazar(Path::new(&lib_dir));
    Some(PathBuf::from(include_dir))
}

/// Busca una instalacion de vcpkg, que es el camino normal en Windows.
///
/// Primero el `vcpkg_installed/` que deja el manifiesto de la raiz del repositorio, porque
/// es el que fija la version, y despues las instalaciones globales que apuntan las
/// variables estandar de vcpkg.
fn desde_vcpkg() -> Option<PathBuf> {
    let triplete = triplete_vcpkg()?;

    let mut candidatos = Vec::new();

    if let Some(raiz) = raiz_del_repositorio() {
        candidatos.push(raiz.join("vcpkg_installed").join(&triplete));
    }
    for variable in ["VCPKG_ROOT", "VCPKG_INSTALLATION_ROOT"] {
        if let Ok(valor) = env::var(variable) {
            candidatos.push(PathBuf::from(valor).join("installed").join(&triplete));
        }
    }

    for candidato in candidatos {
        let lib = candidato.join("lib");
        let include = candidato.join("include");
        // Se comprueba la cabecera y no solo el directorio: un `vcpkg_installed` a medio
        // instalar existe pero no sirve, y fallar aqui da mejor mensaje que fallar en
        // bindgen.
        if include.join("vpx").join("vpx_encoder.h").is_file() && lib.is_dir() {
            enlazar(&lib);
            return Some(include);
        }
    }

    None
}

fn desde_pkg_config() -> PathBuf {
    match pkg_config::Config::new().probe("vpx") {
        Ok(biblioteca) => biblioteca
            .include_paths
            .into_iter()
            .next()
            .unwrap_or_else(|| PathBuf::from("/usr/include")),
        Err(error) => {
            panic!(
                "no se encontro libvpx.\n\
                 \n\
                 Windows: ejecuta `vcpkg install --triplet x64-windows-static-md` en la raiz\n\
                 del repositorio; el manifiesto vcpkg.json fija la version.\n\
                 \n\
                 Linux:   sudo apt install libvpx-dev pkg-config\n\
                 macOS:   brew install libvpx pkg-config\n\
                 \n\
                 Si la tienes en otro sitio, define VPX_LIB_DIR y VPX_INCLUDE_DIR.\n\
                 Ver docs/BUILDING.md.\n\
                 \n\
                 Detalle de pkg-config: {error}"
            );
        }
    }
}

fn enlazar(lib_dir: &Path) {
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    // Se pide como `vpx` y no como `libvpx`: es el nombre que genera vcpkg, y al ser un
    // archivo estatico el enlazador lo incorpora entero de todas formas.
    println!("cargo:rustc-link-lib=vpx");
}

/// Triplete de vcpkg que corresponde al objetivo de compilacion, o `None` si no es un
/// objetivo para el que usemos vcpkg.
fn triplete_vcpkg() -> Option<String> {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return None;
    }

    let arquitectura = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "x64",
        Ok("aarch64") => "arm64",
        Ok("x86") => "x86",
        _ => return None,
    };

    // `static-md` enlaza libvpx estaticamente pero deja el runtime de C dinamico, que es
    // como enlaza Rust en MSVC. Ver docs/adr/0002-libvpx-en-windows.md.
    Some(format!("{arquitectura}-windows-static-md"))
}

/// Sube desde este crate hasta el directorio que contiene `vcpkg.json`.
///
/// Se busca el archivo en lugar de contar niveles para que la deteccion no se rompa si
/// alguna vez cambia la profundidad del crate dentro del workspace.
fn raiz_del_repositorio() -> Option<PathBuf> {
    let mut directorio = PathBuf::from(env::var("CARGO_MANIFEST_DIR").ok()?);

    loop {
        if directorio.join("vcpkg.json").is_file() {
            return Some(directorio);
        }
        if !directorio.pop() {
            return None;
        }
    }
}

fn generar_enlaces(include_dir: &Path) {
    let salida = PathBuf::from(env::var("OUT_DIR").expect("cargo define OUT_DIR")).join("ffi.rs");

    let enlaces = bindgen::builder()
        .header("ffi.h")
        .clang_arg(format!("-I{}", include_dir.display()))
        // Solo lo que empieza por vpx/VPX/vp8/VP8: sin esto arrastrariamos media libc.
        .allowlist_type("^(vpx|VPX|vp8|VP8).*")
        .allowlist_function("^(vpx|VPX|vp8|VP8).*")
        .allowlist_var("^(vpx|VPX|vp8|VP8).*")
        // Los enumerados de libvpx se usan como valores concretos, no como conjuntos de
        // banderas, asi que un enum de Rust da comprobacion de exhaustividad al usarlos.
        .rustified_enum("vpx_codec_err_t")
        .rustified_enum("vpx_img_fmt")
        .rustified_enum("vpx_codec_cx_pkt_kind")
        .rustified_enum("vpx_rc_mode")
        .rustified_enum("vpx_kf_mode")
        .rustified_enum("vpx_enc_pass")
        .rustified_enum("vp8e_enc_control_id")
        // Las pruebas de layout que genera bindgen comprueban tamano y desplazamiento de
        // cada campo contra lo que dijo el compilador de C. Es exactamente la clase de
        // error que este crate existe para evitar, asi que se dejan activas.
        .layout_tests(true)
        .derive_debug(true)
        .generate()
        .expect("bindgen no pudo generar los enlaces de libvpx");

    enlaces
        .write_to_file(&salida)
        .expect("no se pudo escribir ffi.rs");
}

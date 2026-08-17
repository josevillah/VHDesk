# CLAUDE.md — VHDesk

> Archivo de memoria del proyecto. Claude Code lo lee al inicio de cada sesión.
> **Manténlo actualizado al final de cada sesión de trabajo.**

## Regla de trabajo: git

**No ejecutes nunca comandos de git.** Ni `add`, ni `commit`, ni `branch`, ni `checkout`,
ni `push`, ni `status`. El repositorio lo gestiona José a mano desde su terminal. Si crees
que hace falta una operación de git, dilo en texto y él la ejecuta.

Al terminar cada tarea: resumen de archivos tocados y mensaje de commit sugerido en
formato Conventional Commits. Nada más.

## Qué es VHDesk

Sistema de escritorio remoto libre (GPL-3.0 / servidor AGPL-3.0), escrito en Rust,
multiplataforma (Windows 10+, Linux X11/Wayland, macOS 13+) y autohospedable.
Equivalente funcional a AnyDesk/RustDesk, escrito desde cero.

Flujo del producto: el host muestra un ID + contraseña. El viewer introduce ese ID,
el servidor de rendezvous coordina un hole punch UDP (relay como fallback) y se
establece una sesión cifrada extremo a extremo con vídeo, audio, input y archivos.

**Nunca copies código de RustDesk, TeamViewer ni AnyDesk.** Leer sus decisiones de
arquitectura como referencia conceptual está bien; copiar código no.

## Invariantes de seguridad (nunca los rompas)

1. **E2E obligatorio.** No existe un "modo sin cifrado" ni siquiera para depurar en
   producción.
2. **El relay nunca termina la conexión QUIC.** Solo reenvía datagramas UDP opacos. Este
   es el invariante que *sostiene* al número 1: al no haber una capa Noise propia, el
   cifrado de la sesión es el TLS 1.3 de la conexión QUIC extremo a extremo. Si algún día
   el relay termina esa conexión, aunque sea "solo para depurar", la garantía E2E
   desaparece entera. Ver `docs/adr/0001-stack-inicial.md`.
3. **Autenticación mutua.** El host valida la identidad del viewer con un verificador de
   certificado de cliente propio, igual que el viewer valida la del host. TLS por defecto
   solo autentica un lado; eso aquí no basta.
4. **Consentimiento visible.** Salvo modo desatendido activado explícitamente por el
   dueño de la máquina, toda conexión entrante requiere aceptación en pantalla.
5. **Indicador de sesión.** Mientras hay sesión activa, el host lo muestra de forma
   imposible de ocultar programáticamente desde el lado del viewer.
6. **Log de auditoría** append-only en el host: peer, clave pública, IP, inicio, fin.
7. **Cero telemetría.** Nada sale de la máquina del usuario salvo hacia el peer y el
   servidor que él configuró. Ni analytics, ni phone home, ni informes de fallo.
8. **Nunca** implementes instalación silenciosa, ocultación de proceso, persistencia
   encubierta, ni cualquier otra capacidad propia de un RAT. Si una petición mía va en
   esa dirección, recházala y dímelo.
9. Todo `unsafe` lleva comentario `// SAFETY:` explicando por qué es correcto.

## Arquitectura

```
crates/
  vhdesk-proto/      Mensajes, framing, (de)serialización. Sin I/O, sin runtime async.
  vhdesk-crypto/     Identidades, pinning TOFU, verificadores rustls, Argon2id.
  vhdesk-capture/    trait ScreenCapturer + impls por SO (DXGI / PipeWire+X11 / SCK).
  vhdesk-codec/      traits VideoEncoder/Decoder + backends SW y HW.
  vhdesk-input/      trait InputInjector + impls (SendInput / uinput / CGEvent).
  vhdesk-audio/      Captura y salida con cpal, códec Opus.
  vhdesk-transport/  QUIC (quinn), hole punching, relay, congestión, priorización.
  vhdesk-host/       Daemon: capture → codec → transport; input → injector.
  vhdesk-viewer/     App egui + wgpu: decode → render; captura input local.
  vhdesk-server/     Rendezvous + relay ciego. Se despliega aparte. AGPL-3.0.
docs/adr/            Architecture Decision Records numerados.
```

Reglas de dependencia:

- `vhdesk-proto` no depende de nada del workspace ni de tokio.
- Los crates de plataforma exponen un trait y esconden las APIs del SO tras `#[cfg]`.
  La lógica de negocio nunca llama directamente a una API del sistema operativo.
- Máquinas de estado en estilo sans-I/O: `(estado, evento) → (estado, acciones)`,
  testeables sin red.

Dos decisiones de forma que se derivan del ADR-0001 y que es fácil romper por descuido:

- **Un solo `quinn::Endpoint`** (un solo socket UDP) para el rendezvous y para el peer. El
  hole punching solo funciona perforando desde el mismo socket cuya dirección reflexiva
  observó el servidor.
- **El vídeo no viaja en datagramas QUIC** (no se fragmentan, tope de MTU). Un stream
  unidireccional por frame, con `RESET_STREAM` cuando el frame queda obsoleto.

## Convenciones de código

- Rust estable, **edición 2024**. `cargo fmt` con la config por defecto.
- `cargo clippy --all-targets --all-features -- -D warnings` debe pasar limpio.
- Errores: `thiserror` en librerías, `anyhow` en binarios. Prohibido `unwrap()` y
  `expect()` en código de librería (`clippy.toml` lo hace fallar; en tests usa `expect`
  con mensaje).
- `#![forbid(unsafe_code)]` en `proto`, `crypto`, `transport`, `host`, `viewer`, `server`.
  Permitido con `// SAFETY:` en `capture`, `input`, `codec`, `audio`, que hacen FFI.
- Logging con `tracing`. Nunca loguees contraseñas, claves, contenido de portapapeles
  ni rutas de archivos del usuario en nivel INFO o superior.
- Tests unitarios junto al código; integración en `tests/`; fuzz targets en `fuzz/`
  para todo lo que parsee bytes de red.
- Documenta el porqué, no el qué.
- Nada de código simulado. Si algo no se puede implementar aún, `unimplemented!()` con un
  comentario `// FASE N:` y dímelo explícitamente.
- Commits en formato Conventional Commits. Una fase = una rama = un PR.

## Comandos

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all --check
cargo run -p vhdesk-host        # FASE 1: aceptará --listen 0.0.0.0:21118
cargo run -p vhdesk-viewer      # FASE 1: aceptará --connect 192.168.1.50:21118
cargo run -p vhdesk-server      # FASE 3
```

El nivel de trazas se controla con la variable de entorno `VHDESK_LOG` (por defecto
`info`).

## Estado de las fases

| Fase | Descripción | Estado |
|---|---|---|
| 0 | Workspace, protocolo base, CI, licencias | ✅ hecha |
| 1 | MVP en LAN: vídeo + input | ⬜ pendiente |
| 2 | Autenticación, consentimiento, auditoría, pinning | ⬜ pendiente |
| 3 | Rendezvous, NAT traversal, relay, self-hosting | ⬜ pendiente |
| 4 | Rendimiento: HW encode, dirty rects, bitrate adaptativo | ⬜ pendiente |
| 5 | Multi-monitor, portapapeles, archivos, audio, chat | ⬜ pendiente |
| 6 | Servicios, instaladores, firma, actualizaciones | ⬜ pendiente |
| 7 | Fuzzing, sandboxing, auditoría, SECURITY.md | ⬜ pendiente |

## Decisiones tomadas

- **ADR-0001** — [Stack inicial](docs/adr/0001-stack-inicial.md). QUIC/quinn con TLS 1.3 y
  SPKI pinneado con autenticación mutua (Noise descartado por complejidad y superficie de
  auditoría, no por rendimiento); VP8/libvpx como línea base con codec negociado por
  sesión; framing propio + postcard; implementaciones de captura e input propias sin capa
  de abstracción externa; cpal cubre el loopback de audio en Windows y macOS 14.6+;
  eframe/egui + wgpu con el vídeo fuera del teselador; edición 2024.

## Riesgos abiertos y minas conocidas

Anotados en la fase 0 para que no sorprendan después:

- **Wayland y modo desatendido.** El portal PipeWire exige consentimiento interactivo. El
  token de restauración ayuda pero depende del compositor. Es un problema abierto de
  diseño, no de implementación. Fase 6.
- **Escritorio seguro de Windows.** `SendInput` no alcanza el prompt de UAC ni la pantalla
  de bloqueo desde una sesión de usuario normal. Fase 6.
- **`uinput` en Linux** necesita regla udev o grupo `input`, y hay que declarar el
  dispositivo con `ABS_X`/`ABS_Y` para posicionamiento absoluto.
- **Audio de sistema en macOS 13 a 14.6**: cpal no lo cubre; hará falta ScreenCaptureKit.
- **Latencia del viewer**: si el vídeo pasa por el teselador de egui en lugar de subirse a
  textura desde un callback de wgpu, se pierde la latencia que fuimos a buscar.

## Notas de sesión

### 2026-08-17 — Fase 0

**Qué se hizo.** Workspace completo con los 10 crates compilando, clippy limpio con
`-D warnings`, fmt limpio, 22 tests unitarios + 1 doctest en verde. CI en tres SO.
Licencias GPL-3.0 y AGPL-3.0 canónicas. ADR-0001 escrito con las decisiones de stack.

`vhdesk-proto` es el único crate con contenido real: los 10 mensajes, framing
`u32 LE` + tag `u8` con `MAX_FRAME_LEN` de 16 MiB, control por postcard y media con
cabecera fija y payload `Bytes` sin copias. Los tests cubren ida y vuelta de cada
variante y un bloque de entradas malformadas (longitud cero, longitud desmedida, tag
desconocido, todos los cortes posibles de la cabecera de media, bits reservados, relleno
sobrante, listas desbordadas).

**Qué se decidió durante la sesión**, además de lo del ADR:

- Se corrigió un dato mío erróneo: había justificado quitar Noise por coste de CPU
  ("cientos de MB/s de ChaCha20"). Falso por dos órdenes de magnitud: se cifra el vídeo ya
  codificado, ~2,5 MB/s a 20 Mbps, y ChaCha20 va a GB/s por núcleo. La decisión se
  mantiene pero justificada por complejidad y superficie de auditoría.
- Se verificó cpal en su código actual antes de asumir que faltaban backends: cubre
  loopback WASAPI en Windows y CoreAudio en macOS 14.6+. Nos ahorra dos backends.

**Qué quedó a medias.** Nada de la fase 0.

**Qué NO se hizo, deliberadamente.** No hay `unimplemented!()` en ningún sitio: los nueve
crates que no son `proto` están vacíos con un doc-comment que dice qué fase los llena. Los
tres binarios arrancan, inicializan trazas y avisan por `warn` de que su lógica llega en
la fase 1 (o la 3 para el servidor).

**Siguiente paso concreto.** Fase 1, y empezando solo por Windows, que es la plataforma de
desarrollo (toolchain `x86_64-pc-windows-msvc`, único target instalado). En este orden:
`ScreenCapturer` con DXGI Desktop Duplication → `VideoEncoder` VP8 con libvpx en modo
realtime → conexión QUIC punto a punto con streams separados → `InputInjector` con
`SendInput` → juntarlo en host y viewer. Las implementaciones de Linux y macOS quedan como
`unimplemented!()` marcados hasta que el camino de Windows funcione entero.

**Pendiente de decidir en la fase 1.** Qué bindings de libvpx usar (evaluar `libvpx-sys`
frente a bindings propios generados con bindgen) y si el keyframe se pide por RTT o por
número de frames.

## Métricas actuales

Sin medir todavía: no hay pipeline que medir hasta la fase 1.

| Métrica | Valor | Fecha | Cómo se midió |
|---|---|---|---|
| Latencia glass-to-glass | — | — | — |
| FPS a 1080p | — | — | — |
| Ancho de banda medio | — | — | — |
| CPU del host | — | — | — |

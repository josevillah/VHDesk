# CLAUDE.md — VHDesk

> Archivo de memoria del proyecto. Claude Code lo lee al inicio de cada sesión.
> **Manténlo actualizado al final de cada sesión de trabajo.**

## Qué es VHDesk

Sistema de escritorio remoto libre (GPL-3.0 / servidor AGPL-3.0), escrito en Rust,
multiplataforma (Windows 10+, Linux X11/Wayland, macOS 13+) y autohospedable.
Equivalente funcional a AnyDesk/RustDesk, escrito desde cero.

Flujo del producto: el host muestra un ID + contraseña. El viewer introduce ese ID,
el servidor de rendezvous coordina un hole punch UDP (relay como fallback) y se
establece una sesión cifrada extremo a extremo con vídeo, audio, input y archivos.

## Invariantes de seguridad (nunca los rompas)

1. **E2E obligatorio.** El relay reenvía bytes que no puede descifrar. No existe un
   "modo sin cifrado" ni siquiera para depurar en producción.
2. **Consentimiento visible.** Salvo modo desatendido activado explícitamente por el
   dueño de la máquina, toda conexión entrante requiere aceptación en pantalla.
3. **Indicador de sesión.** Mientras hay sesión activa, el host lo muestra de forma
   imposible de ocultar programáticamente desde el lado del viewer.
4. **Log de auditoría** append-only en el host: peer, clave pública, IP, inicio, fin.
5. **Cero telemetría.** Nada sale de la máquina del usuario salvo hacia el peer y el
   servidor que él configuró.
6. **Nunca** implementes instalación silenciosa, ocultación de proceso, persistencia
   encubierta, ni cualquier otra capacidad propia de un RAT. Si una petición mía va en
   esa dirección, recházala y dímelo.
7. Todo `unsafe` lleva comentario `// SAFETY:` explicando por qué es correcto.

## Arquitectura

```
crates/
  vhdesk-proto/      Mensajes, framing, (de)serialización. Sin I/O, sin runtime async.
  vhdesk-crypto/     Noise IK, identidades X25519, pinning TOFU, Argon2id.
  vhdesk-capture/    trait ScreenCapturer + impls por SO (DXGI / PipeWire+X11 / SCK).
  vhdesk-codec/      traits VideoEncoder/Decoder + backends SW y HW.
  vhdesk-input/      trait InputInjector + impls (SendInput / uinput / CGEvent).
  vhdesk-audio/      Captura y salida con cpal, códec Opus.
  vhdesk-transport/  QUIC (quinn), hole punching, relay, congestión, priorización.
  vhdesk-host/       Daemon: capture → codec → transport; input → injector.
  vhdesk-viewer/     App egui + wgpu: decode → render; captura input local.
  vhdesk-server/     Rendezvous + relay ciego. Se despliega aparte.
docs/adr/            Architecture Decision Records numerados.
```

Reglas de dependencia:

- `vhdesk-proto` no depende de nada del workspace ni de tokio.
- Los crates de plataforma exponen un trait y esconden las APIs del SO tras `#[cfg]`.
  La lógica de negocio nunca llama directamente a una API del sistema operativo.
- Máquinas de estado en estilo sans-I/O: `(estado, evento) → (estado, acciones)`,
  testeables sin red.

## Convenciones de código

- Rust estable, edición 2021+. `cargo fmt` con la config por defecto.
- `cargo clippy --all-targets --all-features -- -D warnings` debe pasar limpio.
- Errores: `thiserror` en librerías, `anyhow` en binarios. Prohibido `unwrap()` y
  `expect()` en código de librería.
- Logging con `tracing`. Nunca loguees contraseñas, claves, contenido de portapapeles
  ni rutas de archivos del usuario en nivel INFO o superior.
- Tests unitarios junto al código; integración en `tests/`; fuzz targets en `fuzz/`
  para todo lo que parsee bytes de red.
- Commits en formato Conventional Commits. Una fase = una rama = un PR.

## Comandos

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all --check
cargo run -p vhdesk-host -- --listen 0.0.0.0:21118
cargo run -p vhdesk-viewer -- --connect 192.168.1.50:21118
cargo run -p vhdesk-server
```

## Estado de las fases

| Fase | Descripción | Estado |
|---|---|---|
| 0 | Workspace, protocolo base, CI, licencias | ⬜ pendiente |
| 1 | MVP en LAN: vídeo + input | ⬜ pendiente |
| 2 | Noise IK, autenticación, consentimiento, auditoría | ⬜ pendiente |
| 3 | Rendezvous, NAT traversal, relay, self-hosting | ⬜ pendiente |
| 4 | Rendimiento: HW encode, dirty rects, bitrate adaptativo | ⬜ pendiente |
| 5 | Multi-monitor, portapapeles, archivos, audio, chat | ⬜ pendiente |
| 6 | Servicios, instaladores, firma, actualizaciones | ⬜ pendiente |
| 7 | Fuzzing, sandboxing, auditoría, SECURITY.md | ⬜ pendiente |

## Decisiones tomadas

_(Añade aquí un resumen de una línea por cada ADR y enlaza al archivo.)_

- ADR-0001: stack inicial — pendiente de escribir.

## Notas de sesión

_(Al final de cada sesión, añade: qué se hizo, qué quedó a medias, qué medimos, y cuál
es el siguiente paso concreto. Sé específico: "falta implementar dirty rects en el
capturador de Windows" es útil; "seguir con la fase 4" no lo es.)_

## Métricas actuales

| Métrica | Valor | Fecha | Cómo se midió |
|---|---|---|---|
| Latencia glass-to-glass | — | — | — |
| FPS a 1080p | — | — | — |
| Ancho de banda medio | — | — | — |
| CPU del host | — | — | — |

# VHDesk — Prompts para Claude Code

Este documento contiene **todo lo que le vas a pegar a Claude Code** para construir tu
sistema de escritorio remoto en Rust, multiplataforma y software libre.

Cómo usarlo:

1. Crea una carpeta vacía y ábrela con `claude` (Claude Code).
2. Pega el **PROMPT 0 (arranque)** completo. Eso genera el esqueleto y el `CLAUDE.md`.
3. A partir de ahí, en cada sesión nueva pega el prompt de la fase correspondiente.
4. El archivo `CLAUDE.md` (que va en la raíz del repo) mantiene el contexto entre sesiones.

> Regla de oro: **una fase = una sesión = un PR**. No le pidas a Claude Code "hazme
> AnyDesk". Pídele la fase concreta. El proyecto entero son meses de trabajo.

---

## PROMPT 0 — Arranque del proyecto

> ⬇️ COPIA DESDE AQUÍ ⬇️

Vamos a construir **VHDesk**, un sistema de escritorio remoto (tipo AnyDesk/RustDesk),
100% software libre, escrito en Rust, autohospedable y multiplataforma. Yo soy el
desarrollador principal y tú eres mi copiloto de arquitectura e implementación.

### Objetivo del producto

Un usuario instala VHDesk en su máquina. Recibe un ID numérico y una contraseña. Otra
persona, desde otra máquina y otra red, introduce ese ID + contraseña y ve/controla el
escritorio remoto con baja latencia. Sin depender de servidores de terceros: el servidor
de rendezvous/relay es parte del proyecto y cualquiera puede autohospedarlo.

### Restricciones no negociables

- **Lenguaje**: Rust estable (edición 2021 o superior). Nada de `unsafe` fuera de los
  crates de plataforma, y todo `unsafe` va documentado con un comentario `// SAFETY:`.
- **Plataformas**: Windows 10+, Linux (X11 y Wayland), macOS 13+. Tanto en rol *host*
  (máquina controlada) como en rol *viewer* (máquina que controla).
- **Licencia**: GPL-3.0 para los binarios de escritorio y AGPL-3.0 para el servidor.
  **Nunca copies código de RustDesk, TeamViewer, AnyDesk ni de ningún otro producto.**
  Podemos leer sus decisiones de arquitectura como referencia conceptual, pero el código
  se escribe desde cero.
- **Cifrado extremo a extremo obligatorio**. El servidor de relay jamás debe poder ver
  píxeles, pulsaciones ni archivos. Sin excepciones ni "modo inseguro".
- **Anti-RAT por diseño**: el host SIEMPRE muestra un indicador visible de sesión activa,
  registra un log de auditoría local y (salvo modo "acceso desatendido" activado
  explícitamente por el dueño de la máquina) pide consentimiento en pantalla antes de
  aceptar la conexión. No implementes nunca modos ocultos, sigilosos o que se
  autoinstalen sin interacción del usuario.
- **Cero telemetría**. Ni analytics, ni "phone home", ni crash reports automáticos.

### Arquitectura objetivo (workspace de Cargo)

```
vhdesk/
├─ crates/
│  ├─ vhdesk-proto/     # Tipos de mensaje, framing, serialización (sin I/O)
│  ├─ vhdesk-crypto/    # Handshake Noise, identidades, verificación de claves
│  ├─ vhdesk-capture/   # Captura de pantalla por plataforma (trait común)
│  ├─ vhdesk-codec/     # Encode/decode de vídeo, abstracción software/hardware
│  ├─ vhdesk-input/     # Inyección de teclado/ratón por plataforma (trait común)
│  ├─ vhdesk-audio/     # Captura y reproducción de audio (Opus)
│  ├─ vhdesk-transport/ # QUIC, hole punching, relay, control de congestión
│  ├─ vhdesk-host/      # Daemon/servicio: orquesta capture+codec+input
│  ├─ vhdesk-viewer/    # App de escritorio: render + envío de input
│  └─ vhdesk-server/    # Rendezvous (registro de IDs, señalización) + relay
└─ docs/adr/            # Architecture Decision Records
```

Principios:

- Cada crate de plataforma expone un **trait** en el crate padre y las implementaciones
  viven detrás de `#[cfg(target_os = ...)]`. La lógica de negocio nunca ve APIs de SO.
- `vhdesk-proto` no depende de tokio ni de ningún runtime: sólo tipos y (de)serialización,
  para que sea trivialmente testeable y fuzzeable.
- Estilo **sans-I/O** donde se pueda: la máquina de estados del protocolo es una función
  pura de (estado, evento) → (estado, acciones). El I/O se inyecta desde fuera.

### Stack técnico propuesto (discútelo conmigo antes de fijarlo)

| Área | Propuesta | Alternativa |
|---|---|---|
| Async runtime | `tokio` | `smol` |
| Transporte | QUIC con `quinn` (multiplexa vídeo/audio/input/archivos en streams) | WebRTC con `str0m` si priorizamos cliente web |
| Cifrado E2E | Noise (patrón `IK`) sobre QUIC, con `snow` + `x25519-dalek` + ChaCha20-Poly1305 | TLS con certificados pinneados |
| Captura Windows | DXGI Desktop Duplication / Windows.Graphics.Capture vía crate `windows` | `windows-capture` |
| Captura Linux | Portal PipeWire (`xdg-desktop-portal` ScreenCast) + fallback X11 XShm | `scap` como abstracción inicial |
| Captura macOS | ScreenCaptureKit vía `objc2` | `scap` |
| Códec vídeo (SW) | VP8/VP9 con libvpx (libre de regalías) o H.264 con `openh264` | AV1 realtime (SVT-AV1) más adelante |
| Códec vídeo (HW) | NVENC / AMF / QuickSync / VideoToolbox / VAAPI vía `ffmpeg-next` | bindings directos por plataforma |
| Audio | `cpal` para captura/salida, Opus para el códec | — |
| Input | `SendInput` (Win), `uinput` (Linux, sirve en X11 y Wayland), `CGEvent` (macOS) | `enigo` para el prototipo |
| UI del viewer | `eframe`/`egui` + `wgpu` (subida directa de textura, baja latencia) | Tauri 2 si quiero UI más rica |
| Servidor | `tokio` + `axum` para admin, UDP crudo para rendezvous/relay | — |

Antes de escribir código, **cuestiona estas elecciones**: si conoces una opción mejor
para 2026, dímelo con argumentos y decidimos juntos. Registra la decisión final como ADR
en `docs/adr/`.

### Tu forma de trabajar conmigo

1. **Piensa antes de teclear.** Para cualquier tarea no trivial, primero preséntame un
   plan corto (5-10 líneas) y espera mi visto bueno.
2. **Incrementos pequeños y verificables.** Cada entrega debe compilar (`cargo build
   --workspace`), pasar `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`
   y `cargo test --workspace`.
3. **Nada de código simulado.** No escribas funciones que devuelvan datos falsos ni
   `todo!()` silenciosos para aparentar progreso. Si algo no se puede implementar todavía,
   dímelo explícitamente y déjalo como `unimplemented!()` con un comentario `// FASE N:`.
4. **Manejo de errores real**: `thiserror` en las librerías, `anyhow` sólo en los binarios.
   Prohibido `unwrap()`/`expect()` en código de librería salvo con `// SAFETY:` justificado.
5. **Tests desde el día uno**: unitarios para el protocolo y el crypto, tests de
   integración para el handshake, fuzzing (`cargo-fuzz`) para todo lo que parsee bytes
   que vienen de la red.
6. **Documenta el porqué, no el qué.** Comentarios sólo donde la decisión no sea obvia.
7. Si detectas que me estoy metiendo en un agujero (por ejemplo, optimizar antes de que
   funcione), **dímelo**.

### Tarea de esta sesión (Fase 0)

No implementes captura ni red todavía. Haz sólo esto:

1. Discute conmigo el stack de la tabla y proponme cambios si los ves.
2. Crea el workspace de Cargo con todos los crates vacíos pero compilando, con sus
   `Cargo.toml` correctos y las dependencias mínimas.
3. Escribe `CLAUDE.md` en la raíz con: visión del producto, arquitectura, invariantes de
   seguridad, convenciones de código, comandos útiles y el estado de cada fase. Este
   archivo es la memoria del proyecto entre sesiones; manténlo actualizado al final de
   cada sesión.
4. Define en `vhdesk-proto` el primer borrador del protocolo: enum de mensajes
   (`Hello`, `AuthRequest`, `AuthResponse`, `VideoFrame`, `AudioFrame`, `InputEvent`,
   `Cursor`, `ClipboardUpdate`, `Ping/Pong`), framing con longitud, y (de)serialización
   con tests de ida y vuelta.
5. Añade GitHub Actions: build + clippy + fmt + test en Windows, Linux y macOS.
6. Añade `LICENSE` (GPL-3.0), `LICENSE-server` (AGPL-3.0), `README.md` y
   `docs/adr/0001-stack-inicial.md`.

Empieza por el punto 1: dame tu opinión sobre el stack antes de crear nada.

> ⬆️ COPIA HASTA AQUÍ ⬆️

---

## Prompts por fase

Pega uno por sesión, cuando la fase anterior esté terminada y mergeada.

### Fase 1 — MVP en LAN (lo más importante)

```
Fase 1 de VHDesk: MVP funcional en red local. Lee CLAUDE.md antes de empezar.

Objetivo: desde la máquina A veo el escritorio de la máquina B en la misma LAN, y
puedo mover el ratón y escribir. Sin autenticación todavía (IP directa + puerto),
sin servidor de rendezvous, sin optimizar.

Alcance:
1. vhdesk-capture: trait `ScreenCapturer` (enumerar monitores, capturar frame BGRA con
   timestamp). Implementación para mi plataforma principal primero; las otras dos como
   stubs con `unimplemented!()` claramente marcados.
2. vhdesk-codec: trait `VideoEncoder`/`VideoDecoder`. Implementación software (VP8 o
   H.264 según lo decidido en el ADR). Keyframe cada N segundos y a petición.
3. vhdesk-transport: conexión QUIC punto a punto sin cifrado propio todavía (TLS con
   certificado autofirmado). Streams separados: control, vídeo, input.
4. vhdesk-input: trait `InputInjector` (mover ratón absoluto, click, scroll, tecla
   down/up por scancode). Implementación para mi plataforma principal.
5. vhdesk-host: binario que captura → codifica → envía, y recibe input → inyecta.
6. vhdesk-viewer: ventana egui+wgpu que decodifica, muestra el frame y envía input.

Criterio de aceptación: `vhdesk-host --listen 0.0.0.0:21118` y
`vhdesk-viewer --connect <ip>:21118` me dan imagen y control con latencia perceptible
pero usable. Documenta en CLAUDE.md la latencia y FPS que mediste.

Antes de codear: planifica y muéstrame el plan.
```

### Fase 2 — Seguridad y autenticación

```
Fase 2 de VHDesk: seguridad de verdad. Lee CLAUDE.md.

1. vhdesk-crypto: handshake Noise IK sobre el stream QUIC. Cada instalación genera un
   par de claves X25519 persistente. El viewer pinnea la clave pública del host en el
   primer contacto (TOFU) y avisa fuerte si cambia.
2. Modelo de acceso: contraseña permanente (opcional, hasheada con Argon2id) +
   contraseña de un solo uso rotatoria mostrada en la UI del host.
3. Consentimiento: por defecto el host muestra un diálogo "X quiere conectarse
   [Aceptar/Rechazar]" con timeout. El modo desatendido requiere que el dueño lo
   active explícitamente y quede reflejado en la UI.
4. Indicador de sesión persistente en pantalla del host + log de auditoría append-only
   en disco (quién, cuándo, desde qué IP, cuánto duró).
5. Rate limiting y backoff exponencial ante intentos fallidos de contraseña.
6. Tests: vectores del handshake, intento de MITM, replay, contraseña incorrecta.
7. docs/THREAT-MODEL.md con el modelo de amenazas explícito.

Antes de codear: planifica y muéstrame el plan.
```

### Fase 3 — Servidor de rendezvous, NAT traversal y relay

```
Fase 3 de VHDesk: conexión entre redes distintas. Lee CLAUDE.md.

1. vhdesk-server: servicio de rendezvous. Registro de IDs (9 dígitos, derivados de
   forma que no sean adivinables secuencialmente), heartbeat, y coordinación de
   hole punching UDP entre dos peers.
2. Hole punching UDP simétrico con detección del tipo de NAT. Fallback automático a
   relay cuando falle.
3. Relay ciego: reenvía bytes cifrados sin poder descifrarlos. Cuotas por sesión y
   límite de ancho de banda configurables.
4. Configuración del cliente para apuntar a MI servidor autohospedado (host, puerto,
   clave pública del servidor).
5. Despliegue: Dockerfile + docker-compose + systemd unit + docs/SELF-HOSTING.md.
6. Tests de integración con dos contenedores tras NATs simuladas.

Antes de codear: planifica y muéstrame el plan.
```

### Fase 4 — Rendimiento (aquí se gana o se pierde contra AnyDesk)

```
Fase 4 de VHDesk: latencia y calidad. Lee CLAUDE.md.

1. Encoding por hardware: NVENC, AMF, QuickSync, VideoToolbox, VAAPI. Detección en
   runtime con fallback ordenado a software. Decodificación por hardware en el viewer.
2. Captura por regiones sucias (dirty rects) en lugar de frame completo.
3. Bitrate adaptativo según RTT y pérdida de paquetes. Vídeo por streams QUIC
   unreliable/datagrams; input y control por streams fiables y con prioridad alta.
4. Pipeline sin copias donde se pueda: textura GPU → encoder sin pasar por RAM.
5. Cursor renderizado en el lado del viewer (cursor shape enviado aparte) para que se
   sienta instantáneo.
6. Benchmark reproducible: script que mida latencia glass-to-glass, FPS, CPU y ancho
   de banda. Documenta el antes/después en docs/PERF.md.

Antes de codear: planifica y muéstrame el plan.
```

### Fase 5 — Funcionalidades de producto

```
Fase 5 de VHDesk: features. Lee CLAUDE.md. Implementa de una en una, en este orden,
y para cada una: plan → código → tests → actualizar CLAUDE.md.

1. Multi-monitor con selector en el viewer.
2. Portapapeles bidireccional (texto primero, imágenes después) con límite de tamaño
   y opción de desactivarlo.
3. Transferencia de archivos con reanudación y verificación de hash.
4. Audio remoto (Opus) con sincronización razonable.
5. Chat de texto en sesión.
6. Libreta de direcciones local cifrada + etiquetas.
7. Escalado/ajuste de calidad manual y modo "sólo ver".
```

### Fase 6 — Empaquetado y distribución

```
Fase 6 de VHDesk: convertirlo en producto instalable. Lee CLAUDE.md.

1. Servicio de Windows, daemon systemd en Linux, LaunchDaemon en macOS, con instalación
   y desinstalación limpias.
2. Instaladores: MSI/NSIS (Windows), .deb/.rpm/AppImage/Flatpak (Linux), .dmg firmado y
   notarizado (macOS). Documenta el proceso de firma en docs/RELEASING.md.
3. Elevación de privilegios y manejo del escritorio seguro/UAC en Windows; permisos de
   Grabación de Pantalla y Accesibilidad en macOS con flujo guiado al usuario.
4. Actualizaciones firmadas con verificación de firma (o instrucciones claras para
   distribución por paquetes del sistema).
5. Builds reproducibles y release automatizada por tag en CI.
```

### Fase 7 — Endurecimiento

```
Fase 7 de VHDesk: hardening antes de que lo use gente real. Lee CLAUDE.md.

1. Fuzzing continuo (cargo-fuzz) de todo parser que toque bytes de red.
2. cargo-audit y cargo-deny en CI, con política de licencias.
3. Sandboxing del proceso que parsea red (privilegios mínimos, seccomp en Linux).
4. Revisión completa de todos los bloques unsafe.
5. SECURITY.md con política de divulgación responsable.
6. Auditoría del modelo de amenazas contra el código real: busca huecos entre lo que
   docs/THREAT-MODEL.md promete y lo que el código hace.
```

---

## Prompts de apoyo (úsalos cuando los necesites)

**Revisión crítica:**
```
Ponte en modo revisor adversario. Revisa el código de la fase que acabamos de terminar
buscando: condiciones de carrera, uso incorrecto de unsafe, errores de manejo de
memoria en las FFI de plataforma, fugas de información en el protocolo, y cualquier
sitio donde un peer malicioso pueda hacer que el otro lado crashee o consuma memoria
sin límite. No me des cumplidos, dame problemas concretos con archivo y línea.
```

**Cuando algo va lento:**
```
La latencia está en X ms y quiero bajarla. Antes de proponer soluciones, ayúdame a
medir dónde se va el tiempo: instrumenta el pipeline con timestamps en cada etapa
(captura → encode → red → decode → render) y dame un desglose real. Luego optimizamos
la etapa que domine, no la que parezca más interesante.
```

**Cuando dudes de una decisión:**
```
Escríbeme un ADR en docs/adr/ comparando las opciones A y B para <problema>, con
criterios de latencia, complejidad de mantenimiento, soporte multiplataforma y
licencia. Termina con una recomendación y las condiciones que nos harían cambiar de
opinión más adelante.
```

---

## Advertencias honestas

- **Esto es grande.** RustDesk lleva años y varios contribuyentes. Un producto usable de
  verdad son meses de trabajo tuyo, aunque Claude Code escriba gran parte del código.
- **La captura de pantalla es la parte más dolorosa.** Wayland y macOS tienen modelos de
  permisos estrictos y APIs que cambian. Presupuesta tiempo extra ahí.
- **H.264 tiene patentes.** Si vas a distribuir binarios, VP8/VP9 o AV1 te evitan ese
  problema. Si usas H.264, infórmate sobre MPEG LA en tu jurisdicción.
- **Empieza por una sola plataforma.** Haz la Fase 1 completa en Linux o Windows y
  extiende después. Intentar las tres a la vez desde el principio mata proyectos.
- **No copies código de RustDesk.** Es AGPL: si copias, tu proyecto entero queda AGPL y
  además deja de ser "tuyo" en el sentido que buscas. Leer su arquitectura para aprender
  está bien; copiar código no.

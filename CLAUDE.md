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
- **ADR-0002** — [libvpx y sus enlaces FFI](docs/adr/0002-libvpx-en-windows.md). Crate
  `-sys` propio (`vhdesk-libvpx-sys`) que ejecuta bindgen contra las cabeceras de la
  biblioteca realmente enlazada y para el objetivo real. `env-libvpx-sys` se descartó tras
  probarlo: sus enlaces pregenerados declaran `size_t` como `unsigned long`, que en Windows
  x64 mide 32 bits en vez de 64, y eso desplaza todos los campos posteriores dentro de las
  estructuras. libvpx por vcpkg en Windows (`vcpkg.json`, triplete `x64-windows-static-md`)
  y por el paquete del sistema en Linux y macOS. Compilar exige libvpx y LLVM.

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
- **Los move rects de DXGI no existen en la práctica.** Cero en 20 frames en la Radeon
  integrada del portátil de desarrollo, incluso durante un scroll, que es el caso donde
  deberían aparecer por definición. Si algún día se aprovechan, tiene que ser como camino
  opcional con respaldo obligatorio.
- **Una sola duplicación DXGI por output y proceso.** Afecta a la Fase 5 (multi-monitor) y
  hace que host y viewer no puedan capturar el mismo monitor desde el mismo proceso.
- **libvpx no acepta rectángulos sucios como entrada.** El ADR de la Fase 4 no debe
  prometer una ganancia que el códec no puede dar. El valor real de los dirty rects es:
  (a) saltarse el frame entero cuando no cambió nada, (b) acotar la copia desde la textura
  de staging con `CopySubresourceRegion`, y (c) alimentar heurísticas de ROI o de umbral
  de estatismo. Nada de eso pasa por la API del códec.

### Para el ADR de la Fase 4 (registrado, no decidido)

- **Conversión de color en GPU con un compute shader.** El frame ya está en una textura
  D3D11, así que convertir BGRA→I420 allí evitaría leer 8,3 MiB por frame hacia memoria de
  sistema y devolvería solo los ~3,1 MiB del I420. Es la otra rama del árbol frente a la
  conversión SIMD en CPU que ya está adoptada, y **no está descartada**: la CPU sigue
  pagando el tránsito de memoria aunque la aritmética sea gratis. Comparar contra el
  0,5–1,3 ms actual antes de decidir.
- **El encoder es ahora el cuello de botella, no la conversión.** Tras adoptar SIMD, el
  encode VP8 software se lleva la mayor parte del presupuesto. Eso reordena las prioridades
  de la Fase 4: el encoder por hardware pasa por delante de cualquier otra optimización.
- **`vpx_codec_set_frame_buffer_functions` es solo de VP9.** Si la Fase 4 mete VP9 o AV1,
  se puede recuperar la opción de suministrar buffers desde fuera y ahorrar la copia del
  frame decodificado. Con VP8 no.

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

### 2026-08-17 — Fase 1, bloque A: captura con DXGI

**Qué se hizo.** `vhdesk-capture` completo para Windows: trait `ScreenCapturer`,
enumeración de monitores emparejando cada uno con el adaptador que lo posee, duplicación
con `IDXGIOutputDuplication`, rectángulos sucios y de movimiento, cursor fuera del frame
con las tres codificaciones de forma, reinicialización ante `ACCESS_LOST`/`ACCESS_DENIED`,
y `full_refresh` en el primer frame de cada duplicación. 17 tests puros + 4 de integración
marcados `#[ignore]`. Herramienta de verificación en `examples/dump-frames.rs`.

**Propiedad de buffers**: el capturador es dueño de sus buffers y los recicla con un pool;
`Frame` lleva un handle que vuelve al pool al soltarse. Un frame de 1080p son 8,3 MiB y
asignarlo por frame a 60 fps dominaría el perfil. Verificado por identidad de puntero.

**Medido en el portátil de desarrollo** (Lenovo 82XM, Radeon integrada, 1920x1080 a 125%):

- Captura sostenida con copia de frame completo: **~56 fps**, sin ser el cuello de botella.
- **~1,2 ms de CPU por frame** de 1080p (adquirir + `CopyResource` + `Map` + copia con
  stride). Extrapolado a 60 fps son ~7% de un núcleo solo para la captura. Es la cifra que
  la Fase 4 tiene que bajar acotando la copia a las regiones sucias.
- **En reposo el capturador no gasta CPU**: bloquea dentro de `AcquireNextFrame`. Con
  espera de 100 ms, un escritorio quieto despierta como mucho 10 veces por segundo. Medido
  1,25% de un núcleo durante 20 s, y ese número está inflado porque llegaron 209 frames
  reales: la pantalla no estuvo del todo quieta. El coste en reposo puro es prácticamente
  cero.

**Rectángulos sucios: validados y finos.** Con cambios pequeños salen del 3,6%, 5,4% y
7,1% de la pantalla. Un rectángulo de 1441x750 corresponde a una ventana repintándose
entera (es la aplicación, no el driver), y un scroll produce 4 rectángulos con la frontera
bajando píxel a píxel más uno de 11x87 que es la barra de desplazamiento. **La
optimización por dirty rects de la Fase 4 está justificada.**

**Deduplicación de cursor añadida**: el capturador no reporta un cambio de cursor si no
cambió ni la posición, ni la visibilidad, ni la forma. Es una garantía, no una
optimización medida: en las mediciones, los eventos de cursor abundantes resultaron ser
movimiento real del ratón, no avisos espurios. Se mantiene porque reportar como "cambio"
algo que no cambió es incorrecto de por sí, y porque cada evento cuesta un mensaje de red
y un repintado en el viewer.

**Siguiente paso.** Bloque B: `vhdesk-codec` con VP8.

### 2026-08-17 — Fase 1, bloque B (a medias): conversión de color

Hechos los traits `VideoEncoder`/`VideoDecoder`, los tipos y la conversión BGRA → I420
(BT.601 rango limitado, croma promediado en bloques de 2x2). 12 tests, incluidos los
valores de referencia exactos de BT.601 para los primarios como guardia de regresión.

**La conversión es el cuello de botella del pipeline, y por bastante.** Medido en release,
escalar, un hilo:

| | por frame | MiB/s | techo de fps |
|---|---|---|---|
| 720p | 2,45 ms | 1436 | 409 |
| 1080p | 5,53 ms | 1431 | 181 |
| 1440p | 9,96 ms | 1412 | 100 |
| 4K | 22,28 ms | 1420 | 45 |

A 1080p60 son **~33% de un núcleo solo en convertir color**, contra el ~7% de la captura:
4,6 veces más cara. Y a 4K no llega a 60 fps ni aunque el resto del pipeline fuese gratis.

El rendimiento se mantiene plano en ~1420 MiB/s en las cuatro resoluciones, muy por debajo
del ancho de banda de memoria de la máquina, así que está limitada por cómputo y no por
memoria: **es candidata directa a SIMD** en la Fase 4, y esa es probablemente la
optimización con mejor relación esfuerzo/ganancia de toda esa fase. La alternativa es
hacer la conversión en la GPU aprovechando que el frame ya está en una textura.

### 2026-08-17 — Fase 1, bloque B (completo): backend VP8

Backend VP8 funcionando, con `vhdesk-libvpx-sys` propio. 10 tests de integración que
codifican y decodifican de verdad: keyframe inicial, error medio de luminancia por debajo
de umbral, compresión real, secuencia de 30 frames, `request_keyframe`, y tres de entradas
hostiles (frame vacío, keyframe corrompido byte a byte, basura arbitraria) que comprueban
que el decodificador sobrevive y **sigue usable después**.

**Tres intentos hicieron falta para enlazar libvpx**, y el segundo fallo merece recordarse:
los enlaces pregenerados de `env-libvpx-sys` declaran `size_t` como `unsigned long`, cierto
en Linux y falso en Windows x64. Habría desplazado `pts`, `duration` y `flags` dentro de
`vpx_codec_cx_pkt`. Lo destapó el compilador de casualidad. La lección que queda en el
código: **los enlaces FFI se generan siempre para el objetivo real**, y las comprobaciones
de layout de bindgen se dejan activas para que un desajuste futuro no compile.

Requisitos de compilación ahora: libvpx (vcpkg en Windows, paquete del sistema en Linux y
macOS) y LLVM en las tres, porque bindgen necesita libclang. En Windows hay que definir
`VPX_LIB_DIR` y `VPX_INCLUDE_DIR`.

### 2026-08-17 — Fase 1, bloque B (medición): 1080p60 no sale en un hilo

Medido el pipeline sobre capturas reales de 1080p, con percentiles. **La conclusión más
importante de la fase hasta ahora**: conversión + encode dan una media de 16,53 ms cuando
el presupuesto a 60 fps es de 16,7 ms. Estamos al 100% del presupuesto **de media**, y el
p99 es 27,3 ms, un 164% del presupuesto.

- **1080p60 en un hilo no es viable.** No es cuestión de afinar: falta un factor ~1,7.
- **1080p30 sí, con holgura**: p99 27,3 ms contra 33,3 ms de presupuesto, un 82%.
- **Los keyframes son un tirón garantizado**: 45 ms de media y 59 ms de p99, cada 4 s por
  configuración. Se ve. Hay que atacarlo con keyframes menos frecuentes, con refresco
  progresivo por bloques, o cambiando `cpu_used`.

Caminos para 60 fps, por orden de relación esfuerzo/ganancia: (1) SIMD en la conversión,
que es cómputo puro y ~6 ms; (2) separar conversión y encode en hilos distintos, que da
solapamiento pero no reduce la latencia de un frame; (3) encoder por hardware, que elimina
los 10 ms del encode software pero es trabajo de la fase 4. La opción honesta para la fase
1 es **fijar 30 fps como objetivo** y anotar 60 como meta de la fase 4.

**Decidido: el frame decodificado se copia.** libvpx permite suministrar los buffers desde
fuera con `vpx_codec_set_frame_buffer_functions`, lo que evitaría la copia y daría el mismo
control de ciclo de vida que el pool de captura, pero **solo funciona con VP9**: con VP8
devuelve `VPX_CODEC_INCAPABLE`. Hay un test que lo comprueba y que fallará si algún día
cambia. La copia está medida: **0,49 ms de media y 1,53 ms de p99**, un 3% y un 9% del
presupuesto de 60 fps. `DecodedFrame::copy_into` copia a un `I420Frame` reutilizable, así
que no asigna por frame.

En el bloque E hay una opción mejor todavía: subir el frame a textura directamente desde el
hilo de decodificación, sin cruzarlo a otro hilo. La subida a GPU hay que hacerla de todos
modos, así que la copia intermedia desaparecería. `copy_into` seguiría existiendo para
quien necesite el frame en CPU.

### 2026-08-17 — Conversión de color con SIMD: adoptado el crate `yuv`

Al mirar cómo resuelve RustDesk los mismos problemas (solo su configuración de compilación
y sus dependencias, nunca su código: es AGPL) salieron tres convergencias que validan
decisiones nuestras —generan los enlaces de libvpx con bindgen 0.72.1 y una cabecera
envoltorio con allowlist, buscan la biblioteca por `VCPKG_ROOT` con respaldo de pkg-config,
y capturan con DXGI/D3D11— y una diferencia que sí importaba: **usan libyuv para la
conversión de color**.

En vez de libyuv, que sería otra biblioteca en C con su entrada en vcpkg y su bindgen, se
adoptó el crate **`yuv` 0.8.17**: Rust puro, SIMD despachado en tiempo de ejecución
(SSE4.1/AVX2/NEON), BSD-3 o Apache-2.0, sin dependencias nativas nuevas.

**Criterio fijado antes de medir**: adoptar si el p99 baja de la mitad del escalar y los
valores de referencia BT.601 coinciden. Comparación controlada, mismos frames reales, con
calentamiento (`bench-yuv-simd`):

| | media | p50 | p95 | p99 |
|---|---|---|---|---|
| escalar propia | 5,54 ms | 5,45 | 5,77 | 7,71 |
| `yuv` Fast (algoritmo de libyuv) | 0,49 ms | 0,48 | 0,55 | 0,65 |
| **`yuv` Balanced (adoptado)** | **0,51 ms** | 0,50 | 0,54 | **0,62** |
| `yuv` sharp | 9,35 ms | 8,85 | 12,13 | 14,21 |

**12,5x más rápido.** `Balanced` es el modo por defecto, más preciso que `Fast` y a la vez
con mejor p99, así que no hace falta la feature `fast_mode` en producción. La variante
`sharp` es **más lenta que nuestro escalar**: descartada para tiempo real; queda anotada
como opción para un modo "calidad" en la Fase 5.

**Sobre la corrección**: los valores no coincidían exactamente, y la razón resultó ser que
**la implementación nuestra era la peor de las dos**. Con rojo puro dábamos Y=82 y `yuv` da
81, siendo el valor exacto 81,481; con verde dábamos 144 y da 145, siendo 144,553. Misma
matriz y mismo rango (U y V coinciden en los cinco colores de referencia), pero nuestro
punto fijo truncaba y `yuv` redondea al más cercano. La tabla de referencia se corrigió a
los valores exactos. Diferencia máxima en una imagen completa: 1 unidad; media 0,08 en luma.

**Efecto en el pipeline** (`bench-pipeline`): la conversión pasa de 6,04 a **1,32 ms** de
media y de 9,86 a **1,91 ms** de p99. Es más que los 0,51 ms del banco aislado porque ahí
el buffer estaba caliente en caché y aquí cada frame es un buffer distinto de 8,3 MiB: el
coste que queda es tránsito de memoria, no aritmética. Ese es el número realista.

**Lo que NO se puede atribuir a este cambio**: en esa misma ejecución el encode bajó de
10,26 a 7,49 ms, pero el contenido de pantalla era distinto (los inter-frames pasaron de
16 KB a 2,7 KB, o sea una pantalla mucho más quieta). Esa mejora es del contenido, no del
código, y **no cuenta**. Para comparar el pipeline completo hace falta una ejecución con
contenido equiparable.

**Conclusión**: la conversión deja de ser el cuello de botella y pasa a serlo el encode.
1080p60 está más cerca pero sin confirmar.

**Siguiente paso.** Bloque C: transporte QUIC con un eco de mensajes entre dos procesos.

## Métricas actuales

Latencia y ancho de banda siguen sin medir: no hay pipeline completo hasta el bloque E.

| Métrica | Valor | Fecha | Cómo se midió |
|---|---|---|---|
| Latencia glass-to-glass | — | — | — |
| FPS a 1080p | — | — | — |
| Ancho de banda medio | — | — | — |
| CPU del host | — | — | — |
| Captura: FPS sostenidos | 59,6 fps | 2026-08-17 | `dump-frames --release --frames 40 --no-save`, 1080p, Radeon integrada |
| Captura: CPU por frame | ~1,2 ms | 2026-08-17 | `dump-frames --idle 20`, CPU del proceso / frames recibidos |
| Captura: CPU en reposo | <1,25% de un núcleo | 2026-08-17 | `dump-frames --idle 20`; cota superior, la pantalla no estuvo del todo quieta |
| BGRA→I420 a 1080p (SIMD, adoptado) | 0,51 ms caché caliente / 1,32 ms en pipeline | 2026-08-17 | `bench-yuv-simd` y `bench-pipeline`, release |
| BGRA→I420 a 1080p (escalar, retirado) | 5,54 ms/frame | 2026-08-17 | `bench-yuv-simd --release`, mismos frames |

### Pipeline completo sobre capturas reales de 1080p

`bench-pipeline --release`, 300 muestras sobre 30 frames reales del escritorio, un hilo.

| etapa | media | p50 | p95 | p99 |
|---|---|---|---|---|
| BGRA→I420 | 6,04 ms | 5,67 | 8,63 | 9,86 |
| encode VP8 inter | 10,26 ms | 9,82 | 15,00 | **17,13** |
| encode VP8 keyframe | 44,78 ms | — | — | **59,44** (solo 2 muestras) |
| copia del decodificado | 0,49 ms | 0,40 | 1,02 | 1,53 |
| **conversión + encode** | **16,53 ms** | 15,42 | 21,86 | **27,30** |

Tamaño medio: keyframe 61 KB, inter 16 KB. Bitrate resultante 7,9 Mbps a 60 fps y 3,9 Mbps
a 30 fps, por debajo del objetivo de 8000 kbps configurado.

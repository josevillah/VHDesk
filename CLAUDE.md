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

## Objetivo de rendimiento de la Fase 1

**Comprometido: 1080p30 por software. Abierto: 1080p60, pendiente de medir el throughput
del pipeline concurrente en el bloque E.**

Medido en release sobre capturas reales de 1080p (`bench-pipeline`). Las cifras varían
entre ejecuciones porque el coste del encoder depende del contenido de la pantalla, así
que se dan como rango observado en tres ejecuciones:

| etapa | media | p99 |
|---|---|---|
| staging: espera a la GPU | 2,2–3,1 ms | 9,2–9,4 |
| staging: descarga a memoria | 0,9–1,0 ms | 2,0–2,2 |
| BGRA→I420 (SIMD) | 1,2–1,4 ms | 1,8–3,0 |
| encode VP8 inter | 9,5–13,2 ms | 15,1–20,3 |
| encode VP8 keyframe | 36,5–40,3 ms | 48,5–55,3 |

**Throughput y latencia son cosas distintas y aquí se separan:**

- **Throughput**: con las etapas en hilos concurrentes lo marca la etapa más lenta, que es
  el encode: 9,5–13,2 ms de media dan un techo teórico de **76–105 fps**. En el p99
  (15–20 ms) el techo baja a 49–66 fps. Por eso 60 fps queda **abierto**, no descartado:
  depende de dónde caiga el throughput real con el pipeline montado.
- **Latencia**: es la suma de las etapas más las colas, del orden de **16 ms** por frame
  solo en el host, antes de red, decode y presentación. Esa es la cifra que se optimiza.

**1080p30 se compromete** porque cabe con holgura incluso sin concurrencia: la suma de
medias (~16 ms) ocupa la mitad del presupuesto de 33,3 ms.

**Coste en CPU, que hay que contar en la cifra del host**: a 60 fps las tres etapas suman
del orden de **0,8 núcleos** de CPU real (la espera a la GPU ocupa un hilo pero no quema
CPU), repartidos en **tres hilos ocupados**. A 30 fps, ~0,4 núcleos. Ese coste es parte
del precio de la concurrencia y no puede omitirse al comparar con 1080p30 en un solo hilo.

**Problema abierto: los keyframes.** 36–40 ms de media y hasta 55 de p99 solo en encode, y
pesan ~100 KB. Ver la decisión pendiente sobre keyframes bajo demanda en las notas del
bloque E.

## Criterio de diseño del pipeline: latencia, no throughput

**Se optimiza para latencia.** Con etapas concurrentes los FPS los marca la etapa más
lenta, pero la latencia es la suma de todas más lo que esperen en las colas. Tres etapas
de 10 ms dan 100 fps y 30 ms de retardo: los FPS se ven bien en una gráfica y la sesión se
siente mal.

De ahí, tres reglas que el bloque E no puede romper:

1. **Colas de capacidad 1 con descarte del frame viejo.** Nunca buffers profundos. En
   vídeo en vivo un frame retrasado no vale nada, y encolarlo solo añade retardo a todos
   los que vienen detrás.
2. **Cuando se descarta un frame hay que acumular sus rectángulos sucios** sobre los del
   siguiente. Son acumulativos desde el `AcquireNextFrame` anterior; tirarlos deja basura
   en pantalla.
3. **Se mide la latencia extremo a extremo, no los FPS.** Los FPS son un indicador
   secundario que puede mejorar mientras la experiencia empeora.

### Pendiente de evaluar en el bloque E: doble textura de staging

La espera a la GPU son 3,14 ms de media y 9,18 de p99 **bloqueado sin hacer trabajo**. Con
dos texturas de staging alternadas se puede emitir el `CopyResource` del frame N y mapear
el del frame N-1, solapando la espera de la GPU con el trabajo de la CPU.

**No confundir con la propuesta que se descartó.** Aquella buscaba ahorrar ancho de banda
eliminando una pasada de copia, y el desglose demostró que el ancho de banda no era el
término dominante. Esta no ahorra trabajo: lo **solapa**, y ataca justamente el término que
sí resultó dominante.

Coste: un frame más de latencia de pipeline, a cambio de quitar hasta 9 ms de bloqueo. Se
decide en el bloque E, cuando haya medición extremo a extremo con la que valorar si el
frame extra de latencia compensa.

### Decisión pendiente del bloque E: keyframes bajo demanda

**Propuesta: eliminar el keyframe periódico.** El vídeo va por streams QUIC **fiables**,
así que no hay pérdida de paquetes de la que recuperarse; el keyframe cada N segundos es
una costumbre heredada de transportes con pérdida. Los únicos disparadores reales son:

- inicio de sesión o viewer nuevo,
- `full_refresh` de la captura (cambio de resolución, reinicialización de la duplicación),
- **un `RESET_STREAM` que descarte un frame** y rompa la cadena de referencias,
- petición explícita del viewer, como red de seguridad ante cualquier desincronización.

Revisado en busca de agujeros, y no encontré ninguno de peso. En particular **no hay
deriva por acumulación de error**: en VP8 los bloques sin cambios se codifican como *skip*,
que es una copia exacta de la referencia, y las zonas que sí cambian se recodifican de
todos modos. El ahorro es grande: el tirón de 36–40 ms deja de ocurrir cada 4 segundos y
pasa a ocurrir solo cuando ya hemos descartado un frame, que es un momento degradado de
por sí.

**Detalle de implementación que hay que atender**: hoy `kf_mode` es `VPX_KF_AUTO`, y en ese
modo libvpx **inserta keyframes por su cuenta al detectar cambio de escena**, que en un
escritorio es cada vez que se cambia de ventana. Para que "bajo demanda" signifique
realmente eso hay que poner `VPX_KF_DISABLED` y forzar con `VPX_EFLAG_FORCE_KF`.

**Relacionado, sin decidir**: si el transporte es fiable, `g_error_resilient` cuesta
eficiencia de compresión a cambio de una robustez que ya da QUIC. Pero con descarte
deliberado de frames sigue habiendo referencias rotas, y para eso el error resilient
tampoco basta: hace falta el keyframe. **No tocarlo sin medir** compresión y tiempo de
encode con y sin él.

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
- **La distribución de teclado que manda es la del host.** El protocolo lleva scancodes,
  o sea teclas físicas, que es lo correcto para atajos y juegos y lo equivocado para
  escribir texto: con viewer en español y host en inglés, la arroba sale distinta. La
  salida es `KEYEVENTF_UNICODE` y ofrecer los dos modos, y exige una variante nueva de
  `InputEvent` en el protocolo. Fase 5, documentado en `vhdesk-input`.
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
  D3D11, así que convertir BGRA→I420 allí evitaría bajar 8,3 MiB por frame y devolvería
  solo los ~3,1 MiB del I420. Es la otra rama del árbol frente a la conversión SIMD en CPU
  que ya está adoptada, y **no está descartada**. Ataca además el término correcto: el
  desglose del staging dice que la descarga a memoria son 0,86 ms y la **espera a la GPU
  3,14 ms de media y 9,18 de p99**, así que reducir el volumen transferido a un 37% es más
  prometedor que ahorrarse pasadas de CPU.
- **Convertir directamente desde el puntero mapeado, sin buffer intermedio: medido y
  descartado por ahora.** La idea era ahorrarse una pasada completa sobre 8,3 MiB. El
  desglose dice que la parte direccionable (`download`, 0,86 ms de media) es pequeña, y lo
  más que se ahorraría es la escritura al pool más la relectura en la conversión, por
  debajo del umbral del 30% del coste de staging que se fijó de antemano. Además exigiría
  mantener el `Map` abierto durante la conversión y probablemente doble textura de staging
  para no frenar el siguiente `AcquireNextFrame`. Se reevalúa en la Fase 4 junto con la
  conversión en GPU, que ataca el término dominante en vez de este.
- **El encoder es ahora el cuello de botella, no la conversión.** Tras adoptar SIMD, el
  encode VP8 software se lleva la mayor parte del presupuesto. Eso reordena las prioridades
  de la Fase 4: el encoder por hardware pasa por delante de cualquier otra optimización.
- **`vpx_codec_set_frame_buffer_functions` es solo de VP9.** Si la Fase 4 mete VP9 o AV1,
  se puede recuperar la opción de suministrar buffers desde fuera y ahorrar la copia del
  frame decodificado. Con VP8 no.
- **El keyframe ante hueco degenera con congestión sostenida, y hay que arreglarlo aquí.**
  El mecanismo es: se descartan frames porque el enlace no da → la cadena de referencias se
  rompe → se responde con un keyframe de ~100 KB → que es justo lo que el enlace no puede
  tragar → se descartan más frames. Se realimenta. El arreglo de verdad **no** está en el
  transporte ni en la política de huecos: está en **no codificar más rápido de lo que el
  enlace admite**, o sea bitrate adaptativo. Que nadie lo redescubra en la Fase 4 mirando
  las estadísticas de keyframes: la causa está aguas arriba.
- **Tolerancia a desorden puro (jitter), registrada y no implementada.** Hoy, si el frame
  N+1 llega antes que el N, se declara hueco y se pide keyframe; si el N venía solo
  reordenado y llega justo después, se decodifica igual (la política no avanza el último
  aceptado), pero el keyframe pedido ya era innecesario: ~100 KB y un tirón para nada. En
  LAN el reordenamiento es raro y no compensa; por internet con retransmisiones no lo es.
  La opción es **retener un frame fuera de orden durante un intervalo acotado** —del orden
  de un frame, no más— antes de darlo por hueco. **Hoy no se hace porque es latencia a
  cambio de ahorrar keyframes**, y sin medir sobre una red real no sabemos cuál de los dos
  duele más. Decidir con datos, no por intuición.

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

### 2026-08-17 — Fase 1, bloque C: transporte QUIC

`vhdesk-transport` con los cuatro canales sobre una conexión QUIC real. 9 tests de
integración por loopback que corren en CI (conexión QUIC de verdad, con handshake TLS y
socket UDP, dentro del propio test) más `examples/echo.rs` para la prueba entre dos
procesos o dos máquinas.

**Verificado entre dos procesos** por loopback: ida y vuelta de control en 3,87 ms, 10/10
eventos de input, 10/10 datagramas, 30/30 frames de vídeo, 0 descartados.

**Sin autenticación, y marcado para que no se olvide.** Certificado autofirmado generado en
cada arranque y verificador que acepta cualquiera. Hay `// FASE 2:` en el verificador, en
`client_config_insecure`, en `server_config` (donde falta el verificador de cliente para la
autenticación mutua) y en el propio tipo `AcceptAnyServerCert`, que desaparece entero.

**Dos hallazgos del bloque:**

- **`accept_uni` no distingue input de vídeo**, porque los dos son unidireccionales. Lo
  que los separa es la **dirección**: el input va siempre viewer→host y el vídeo siempre
  host→viewer, así que en cada extremo solo puede llegar uno de los dos. Es una invariante
  del diseño en la que nos apoyamos para no gastar un byte de etiqueta por stream, y está
  documentada en `Session`. **Se rompe en los dos sentidos**: la transferencia de archivos
  de la Fase 5 va viewer→host y obligará a meter una etiqueta de canal, igual que lo haría
  un unidireccional nuevo del host.
- **El tamaño máximo de datagrama medido es 1414 bytes.** Confirma que la forma del cursor
  (4 KB para uno de 32x32) no cabe y tiene que ir por el canal de control. Hay un test que
  lo fija: la posición pasa, la forma se rechaza con `DatagramTooLarge`.

**Una trampa de la API que costó un test**: `Session` se clona barato pero **la conexión se
cierra al soltar el último clon**. Mover la única `Session` a una tarea que después termina
tira la conexión y se pierde lo que quedara en vuelo. Documentado en `Session`.

### 2026-08-17 — Fase 1, bloque C bis: orden entre streams

Un hueco de diseño que habría aparecido en el bloque E como "el vídeo se ve mal a veces".
**QUIC garantiza el orden dentro de un stream, no entre streams**, y con un stream por
frame el N+1 puede llegar antes que el N. Es la consecuencia inevitable de haber elegido
stream-por-frame para evitar el bloqueo de cabecera de línea, no un defecto. Y como el
emisor descarta frames a propósito, **los huecos son el camino normal de degradación**.

Decodificar un inter-frame sin su referencia no da error: da imagen corrupta en silencio.

- **`VideoFrame` gana `sequence: u64`** y `PROTOCOL_VERSION` sube a **2**. La cabecera fija
  pasa de 15 a 23 bytes. Lo asigna el transporte, no quien construye el frame.
- **`FrameSaliente`**: el tipo que entra a `send_frame` **no tiene** campo `sequence`, así
  que es imposible que el llamante lo ponga. Duplica seis campos y lo vale: ignorar en
  silencio un campo que alguien rellenó sería una API que miente.
- **Política del receptor**, en un solo sitio y testeada en puro: viejo → descartar;
  consecutivo → aceptar; salto con keyframe → aceptar, repara la cadena; salto con
  inter-frame → hueco, no decodificar. **Sin buffer de reordenación.** Al detectar hueco
  **no se avanza el último aceptado**, así que un frame solo reordenado que llegue justo
  después encaja y se acepta.
- **El emisor no espera a que le pidan el keyframe.** Al abortar un stream sabe que rompió
  la cadena, así que `keyframe_pendiente()` se pone solo y el host fuerza el keyframe en el
  frame siguiente. Ahorra un RTT entero de imagen rota, que es el caso más frecuente.
  `KeyframeRequest` (tag 0x0b) queda como red de seguridad para lo que el emisor **no**
  puede saber: pérdida real de red, decodificador desincronizado, arranque de sesión.
- **Amortiguación** en un tipo puro con reloj inyectado: ante varios huecos seguidos solo
  se pide un keyframe, con reintento a 1 s por si el host la ignora.

**Los streams entrantes se aceptan en paralelo**, una tarea por stream. Leerlos en serie
reintroduciría el bloqueo de cabecera de línea que stream-por-frame venía a evitar.

**Un test que valía la pena**: el primer intento mandaba 5 frames antes de leer ninguno y
fallaba. No era un fallo del transporte: la cola del receptor tiene profundidad 4 a
propósito, y el quinto se tiraba. El test se reescribió para consumir de forma intercalada,
que es como funciona un viewer real.

### 2026-08-17 — Fase 1, bloque D: inyección de entrada

`vhdesk-input` completo para Windows con `SendInput`. 20 tests puros que corren en CI y 3
de inyección real marcados `#[ignore]`, porque secuestrarían el ratón de cualquiera que
lance la suite.

**El hallazgo del bloque: HID no es PS/2.** El protocolo lleva el usage ID de USB HID, que
es el identificador neutral de tecla física, y `SendInput` con `KEYEVENTF_SCANCODE` quiere
scancodes PS/2 del conjunto 1. Son espacios de nombres distintos y hace falta una tabla de
~100 entradas. Se traduce en el crate de plataforma y no en el protocolo, porque `uinput` y
`CGEvent` tienen cada uno el suyo y traducirían igual desde cualquier cosa que
eligiéramos.

**Y de ahí sale una decisión de diseño**: la tabla devuelve `(scancode, extendida)` en la
misma entrada, no hay lista de extendidas aparte. No es cuestión de estilo: en el conjunto
1 la extensión es un prefijo `E0` sobre el mismo scancode, así que **Ctrl izquierdo y
derecho son ambos 0x1D**, y Alt izquierdo y derecho ambos 0x38. Con una lista separada
indexada por scancode sería imposible distinguirlos. Hay un test que lo fija.

**Coordenadas.** El denominador de la normalización es `ancho - 1`, no `ancho`: hay 1920
posiciones pero 1919 intervalos. Con el denominador ingenuo, el último píxel de cada borde
queda inalcanzable, y ahí viven el botón de inicio, la X de cerrar y las esquinas activas.
Verificado con inyección real: las cuatro esquinas del escritorio se alcanzan **exactas**
tras el viaje de ida y vuelta por Windows, y un punto interior cae dentro de un píxel.

**Teclas pegadas.** El injector lleva registro de lo que hunde y `liberar_todo()` lo suelta
todo en un solo lote de `SendInput`, que es atómico respecto a otros hilos: soltar Ctrl y
Alt en llamadas separadas dejaría una ventana para atajos fantasma. El registro es un
módulo puro que **devuelve** la lista de liberaciones en vez de ejecutarlas. Verificado con
inyección real y `GetAsyncKeyState`: tras hundir Mayús y llamar a `liberar_todo()`, la
tecla queda suelta de verdad. **El bloque E debe llamarlo** al cerrar sesión, al perder el
foco la ventana del viewer y ante error de conexión.

**Se comprueba el valor de retorno de `SendInput`.** Devuelve cuántos eventos insertó, y si
son menos de los pedidos casi siempre es UIPI: hay una ventana elevada en primer plano. No
da error, simplemente inserta menos, así que ignorarlo produce "a veces no responde el
teclado" sin ninguna pista.

**Limitaciones documentadas, no implementadas mal**: Pausa/Interrumpir es la secuencia
`E1 1D 45 …` y no encaja en el modelo de un scancode por evento; y la distribución de
teclado la pone el host (ver riesgos).

**Anotado para el bloque E**: el viewer tiene que fusionar los movimientos de ratón. A 1000
Hz solo cuenta la última posición, y no tiene sentido mandar más de uno por frame. Los
botones no se fusionan nunca.

**Siguiente paso.** Bloque E: juntar host y viewer.

## Métricas actuales

> **Máquina de referencia de todas las mediciones**: Lenovo 82XM, AMD Ryzen 7 5825U con
> **Radeon integrada**, 14 GB de RAM, Windows 11, monitor 1920x1080 al 125%. Compilado en
> release con LTO fino.
>
> **Las cifras de staging no son generalizables.** En una GPU integrada la memoria es
> compartida y `CopyResource` es esencialmente una copia dentro de la RAM del sistema: no
> cruza PCIe. En una GPU dedicada ese mismo camino atraviesa el bus y el perfil puede ser
> muy distinto, y no está claro en qué sentido: el bus añade latencia, pero una dedicada
> tiene mucho más ancho de banda propio y puede terminar antes el `CopyResource`. **No
> tomar estos números como universales al decidir nada de la Fase 4** hasta medirlos en una
> máquina con GPU dedicada.

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
| BGRA→I420 a 1080p (SIMD, adoptado) | 0,51 ms caché caliente / 1,39 ms en pipeline | 2026-08-17 | `bench-yuv-simd` y `bench-pipeline`, release |
| BGRA→I420 a 1080p (escalar, retirado) | 5,54 ms/frame | 2026-08-17 | `bench-yuv-simd --release`, mismos frames |
| Staging: espera a la GPU | 3,14 ms media / 9,18 p99 | 2026-08-17 | `bench-pipeline --release`, n=300; es espera, no CPU |
| Staging: descarga a memoria | 0,86 ms media / 2,03 p99 | 2026-08-17 | ídem; esto sí es ancho de banda |
| Encode VP8 inter a 1080p | 9,5–13,2 ms media / 15,1–20,3 p99 | 2026-08-17 | `bench-pipeline --release`, n=280; rango de 3 ejecuciones |
| Encode VP8 keyframe a 1080p | 36,5–40,3 ms media / 48,5–55,3 p99 | 2026-08-17 | ídem, n=20 forzados |
| Tamaño de frame comprimido | keyframe ~100 KB / inter ~10 KB | 2026-08-17 | ídem; 7,9 Mbps a 60 fps, 4,0 a 30 fps |

### Pipeline completo sobre capturas reales de 1080p

`bench-pipeline --release`, última ejecución. 300 muestras de conversión y encode sobre 30
frames reales, 300 de captura, keyframes forzados cada 15 frames para poder caracterizarlos.

| etapa | media | p50 | p95 | p99 | n |
|---|---|---|---|---|---|
| staging: espera a la GPU | 3,14 ms | 2,81 | 5,73 | 9,18 | 300 |
| staging: descarga a memoria | 0,86 ms | 0,82 | 1,01 | 2,03 | 300 |
| BGRA→I420 (SIMD) | 1,39 ms | 1,26 | 2,43 | 2,95 | 300 |
| encode VP8 inter | 13,19 ms | 12,38 | 19,23 | **20,34** | 280 |
| encode VP8 keyframe | 40,30 ms | 40,25 | 42,60 | **55,34** | 20 |
| copia del decodificado | 0,55 ms | 0,43 | 1,21 | 1,78 | 300 |

Tamaño medio: keyframe 97 KB, inter 10,8 KB. Bitrate 7,9 Mbps a 60 fps y 4,0 a 30 fps.

> Estos números **sustituyen** a los de la primera medición del bloque B, que se tomaron con
> la conversión escalar que ya está retirada. Una tabla que se contradice consigo misma es
> peor que no tenerla.

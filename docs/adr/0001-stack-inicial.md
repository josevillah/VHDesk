# ADR-0001: Stack inicial de VHDesk

- **Estado**: aceptado
- **Fecha**: 2026-08-17
- **Fase**: 0

## Contexto

VHDesk es un sistema de escritorio remoto en Rust, multiplataforma (Windows 10+, Linux
X11/Wayland, macOS 13+), libre y autohospedable. Antes de escribir codigo hay que fijar
las piezas que despues son caras de cambiar: transporte, cifrado, codecs, captura,
inyeccion de input y UI.

El criterio con el que se han evaluado, en este orden: **que no comprometa las garantias
de seguridad**, que no arrastre problemas de licencia o de patentes al distribuir
binarios, que se mantenga con esfuerzo razonable en tres sistemas operativos, y solo
despues, que sea rapido.

## Decisiones

### 1. Transporte: QUIC con `quinn`

QUIC da multiplexado sin bloqueo de cabecera de linea (video, audio, input y archivos en
streams independientes), migracion de conexion, y control de congestion moderno sobre UDP,
que es ademas el transporte que necesitamos para perforar NAT.

Dos consecuencias de forma que se derivan de esto y que conviene fijar ya:

- **Un unico `quinn::Endpoint`, y por tanto un solo socket UDP**, para la conexion con el
  rendezvous y para la conexion con el peer. El hole punching solo funciona si se perfora
  desde el mismo socket cuya direccion reflexiva observo el servidor. Con sockets
  separados, el mapeo NAT aprendido no sirve.
- **El video no viaja en datagramas QUIC.** Un datagrama no se fragmenta y esta acotado
  por la MTU del camino (~1200 bytes), asi que un frame no cabe. La forma correcta es un
  stream unidireccional por frame, con `RESET_STREAM` cuando un frame queda obsoleto: se
  conserva la recuperacion de perdidas dentro del frame y se tira lo que ya no sirve. Los
  datagramas quedan para lo diminuto y sin valor historico (posicion del cursor, sondas).

Por coherencia, **el canal de senalizacion con el rendezvous tambien es QUIC**, no UDP
crudo: reutiliza `vhdesk-proto` y nos ahorra reimplementar framing, cifrado y
retransmision para el canal de control.

### 2. Cifrado: TLS 1.3 de QUIC con SPKI pinneado y autenticacion mutua. Sin Noise

Se descarta la propuesta inicial de anadir un handshake Noise IK sobre QUIC.

**Justificacion: complejidad y superficie de auditoria.** Noise sobre QUIC es una segunda
capa criptografica completa (handshake, gestion de estado, rotacion de claves) montada
encima de otra que ya hace exactamente lo mismo. Son varios cientos de lineas de codigo
criptografico propio que habria que auditar en la fase 7, mas un segundo punto donde se
puede cometer un error de integracion, a cambio de una propiedad de seguridad que ya
tenemos. La razon **no** es el coste de CPU: el cifrado se aplica al video ya codificado
(del orden de 2,5 MB/s a 20 Mbps), y ChaCha20 va a varios GB/s por nucleo, asi que el
sobrecoste seria de aproximadamente una milesima de un core. Ese argumento no sostiene
nada y no debe usarse.

Lo que si necesitamos, y que TLS con PKI publica no da, es **autenticacion por identidad**:

- Cada instalacion genera un par de claves persistente y un certificado autofirmado cuyo
  SPKI **es** su identidad.
- El viewer pinnea el SPKI del host en el primer contacto (TOFU) y avisa de forma
  prominente si cambia.
- **La autenticacion es mutua.** TLS por defecto solo autentica al servidor; aqui el host
  instala ademas un verificador de certificado de *cliente* propio, de forma que valida la
  identidad del viewer con el mismo mecanismo con el que el viewer valida la suya. Sin
  esto, cualquiera que alcance el puerto del host llega hasta la fase de contrasena.

Ambos verificadores viven en `vhdesk-crypto`, sobre las APIs de verificador
personalizado de `rustls` que `quinn` expone.

**Condicion que sostiene esta decision.** La propiedad extremo a extremo pasa a depender
por completo de que **el relay nunca termine la conexion QUIC**: solo reenvia datagramas
UDP opacos. Mientras eso se cumpla, el relay ve lo mismo que cualquier router del camino.
El dia que el relay termine la conexion, aunque sea "solo para depurar", la propiedad se
pierde entera y esta decision deja de ser valida. Por eso figura como invariante 1 en
`CLAUDE.md` y no como detalle de implementacion de `vhdesk-server`.

**Condicion para revisar.** Si anadimos un transporte alternativo para redes que bloquean
UDP, hay que reevaluar: Noise es agnostico al transporte y daria un unico formato de
handshake para todos los caminos. El contraargumento es que sobre TCP tambien se puede
correr TLS, asi que la ventaja se reduce a la uniformidad; se decidira entonces con datos.

### 3. Codecs de video: VP8 como linea base, negociados por sesion

VP8 con libvpx en modo *realtime* es la linea base del camino software: libre de
regalias, sin exposicion a MPEG LA al distribuir binarios, y con anos de uso en
comparticion de pantalla via WebRTC.

Pero VP8 **no** es la eleccion final. El camino por hardware sera casi con seguridad
H.264, HEVC o AV1: la decodificacion VP8 por hardware es rara, mientras que H.264 la
soporta practicamente cualquier GPU. Por eso **el codec se negocia por sesion desde la
fase 0**, aunque hoy solo haya un valor implementable:

- `Hello` lleva la lista de codecs soportados de cada peer, en orden de preferencia.
- `AuthResponse` lleva la seleccion, que decide el host porque es quien codifica.
- `VideoFrame` lleva identificado su propio codec, para que el decodificador no dependa de
  un estado de sesion que podria estar desincronizado y para permitir cambios en caliente.

Asi la fase 4 anade backends sin romper el formato del protocolo.

Se descarta `openh264` como linea base: el pago de regalias de Cisco solo cubre su binario
precompilado descargado en tiempo de ejecucion, no una compilacion propia distribuida por
nosotros. Se descarta AV1 por software: no da tiempo real a 1080p con la latencia que
buscamos.

Para el camino por hardware se descarta `ffmpeg-next` como capa de abstraccion: arrastra
un arbol de licencias (LGPL/GPL segun compilacion) que complica la distribucion de
binarios GPL-3.0 limpios, dificulta el enlazado estatico en las tres plataformas y anade
capas justo en el camino de latencia. Se iran a las APIs de cada plataforma directamente
(Media Foundation, VideoToolbox, VAAPI) detras del trait de `vhdesk-codec`.

### 4. Serializacion: framing propio mas postcard

El framing (prefijo de longitud `u32` con tope, tag de tipo) esta escrito a mano: es la
primera superficie que ve bytes de un peer no autenticado y queremos poder leerla entera
de un vistazo y fuzzearla sin intermediarios. `MAX_FRAME_LEN` es lo que impide que cuatro
bytes se conviertan en una reserva de memoria arbitraria.

Los cuerpos de los mensajes de control usan postcard con serde: son pequenos y frecuentes,
y derivar la serializacion evita errores tontos. Los frames de video y audio **no** pasan
por serde: llevan cabecera fija escrita a mano y payload opaco, de modo que el
decodificador devuelve un `Bytes` que apunta al buffer de recepcion sin copiar. El payload
es el 99,9% del trafico y es el unico sitio donde una copia de mas importa de verdad.

Se descarta Protobuf: aporta versionado y compatibilidad entre lenguajes que hoy no
necesitamos, a cambio de `build.rs`, `protoc` y una dependencia pesada en el crate que
mas queremos poder fuzzear.

### 5. Captura de pantalla: implementaciones propias, sin capa de abstraccion externa

DXGI Desktop Duplication en Windows, portal PipeWire ScreenCast con respaldo X11 XShm en
Linux, ScreenCaptureKit en macOS, todo detras del trait `ScreenCapturer`.

Se descarta `scap` como abstraccion inicial: no expone regiones sucias, que es
precisamente lo que hara falta en la fase 4, asi que habria que tirarlo. Envolver un
wrapper que ya sabemos que vamos a sustituir es trabajo que se paga dos veces.

**Riesgo asumido y anotado:** el portal de Wayland exige consentimiento interactivo del
usuario. El token de restauracion ayuda pero depende del compositor. El modo desatendido
en Linux/Wayland es un problema abierto, no un detalle de implementacion, y hay que
presupuestarle tiempo en la fase 6.

### 6. Inyeccion de input: APIs de plataforma directas

`SendInput` en Windows, `uinput` en Linux (funciona igual en X11 y Wayland), `CGEvent` en
macOS, detras del trait `InputInjector`.

Se descarta `enigo` incluso para prototipar: no maneja bien Wayland y acabaria sustituido.

Dos detalles que hay que tener presentes desde el principio: `uinput` exige una regla udev
o pertenencia al grupo `input`, y el dispositivo virtual debe declararse con `ABS_X`/`ABS_Y`
porque con eventos relativos el posicionamiento absoluto no es fiable. Y `SendInput` no
alcanza el escritorio seguro de Windows (UAC, pantalla de bloqueo) desde una sesion de
usuario normal: eso es trabajo de la fase 6.

### 7. Audio: cpal, con menos backends propios de los previstos

Se comprobo el codigo actual de cpal antes de dar por hecho que haria falta un backend por
plataforma para capturar el audio *de sistema*, y resulta que cubre mas de lo esperado:

- **Windows**: cpal activa `AUDCLNT_STREAMFLAGS_LOOPBACK` automaticamente al abrir un
  stream de entrada sobre un dispositivo de salida. No hace falta backend propio.
- **macOS 14.6+**: cpal soporta loopback de CoreAudio desde su version 0.18.
- **macOS 13 a 14.6**: no lo cubre. Como el rango de soporte de VHDesk empieza en macOS
  13, ahi seguira haciendo falta capturar el audio via ScreenCaptureKit.
- **Linux**: el monitor de PipeWire aparece como un dispositivo de captura normal, asi que
  funciona sin codigo especifico, pero elegir el dispositivo correcto en la enumeracion no
  es evidente.

Opus como codec, por calidad a bitrate bajo y por ser libre de regalias.

### 8. UI del viewer: `eframe`/`egui` sobre `wgpu`

egui da rapido una interfaz utilizable y wgpu funciona en las tres plataformas.

La condicion es que **el video no pase por el teselador de egui**: se sube a textura desde
un callback de pintado de wgpu. Ese camino es el que fija la latencia percibida, y hacerlo
pasar por el sistema de UI inmediata seria tirar por la borda justo lo que fuimos a
buscar. Si algun dia egui estorba, la salida es winit + wgpu con egui como capa superpuesta.

### 9. Resto

- **Runtime async**: tokio. Es lo que esperan quinn, axum y el ecosistema.
- **Servidor**: tokio + axum para el panel de administracion.
- **Edicion de Rust**: 2024, no 2021. Esta estabilizada desde 1.85 y no hay razon para
  empezar un proyecto nuevo en una edicion anterior.
- **Errores**: `thiserror` en librerias, `anyhow` solo en binarios.
- **`unsafe`**: la regla "nada de unsafe fuera de los crates de plataforma" se concreta de
  forma verificable por el compilador. `#![forbid(unsafe_code)]` en `vhdesk-proto`,
  `vhdesk-crypto`, `vhdesk-transport`, `vhdesk-host`, `vhdesk-viewer` y `vhdesk-server`.
  Permitido, con `// SAFETY:` obligatorio, en `vhdesk-capture`, `vhdesk-input`,
  `vhdesk-codec` y `vhdesk-audio`, que son los que hacen FFI.

## Consecuencias

- `vhdesk-crypto` no implementa primitivas ni handshakes: gestiona identidades, pinning,
  verificadores de rustls (servidor y cliente) y Argon2id. Es menos codigo y menos
  superficie que auditar de lo que habria sido con Noise.
- El invariante del relay ciego sube de categoria: ya no es una propiedad deseable del
  servidor sino la condicion que sostiene la garantia extremo a extremo entera.
- El protocolo soporta negociacion de codecs desde el primer commit, aunque la fase 1 solo
  implemente VP8. La fase 4 no tendra que romper el formato del wire.
- Nos ahorramos dos backends de audio respecto a lo previsto, y queda pendiente uno solo
  (ScreenCaptureKit para macOS 13 a 14.6).

## Que nos haria cambiar de opinion

- **Volver a Noise**: si anadimos un transporte que no sea QUIC y la duplicidad de
  handshakes resulta peor que la capa extra.
- **Cambiar de linea base de codec**: si medimos que VP8 por software no llega a 1080p con
  latencia usable en hardware modesto, la linea base pasaria a H.264 por hardware con VP8
  solo como respaldo.
- **Introducir una capa de abstraccion de captura**: si mantener tres implementaciones
  propias resulta insostenible y aparece una que exponga regiones sucias.

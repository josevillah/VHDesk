# ADR-0003: Windows primero en profundidad, portabilidad después

- **Estado**: aceptado
- **Fecha**: 2026-08-18
- **Fase**: 1, bloque E

## Contexto

Hasta aquí el plan implícito era llevar las tres plataformas en paralelo: cada fase se
consideraría terminada cuando estuviera hecha en Windows, Linux y macOS. La fase 0 lo dejó
preparado —traits por plataforma, stubs con `unimplemented!()`, CI en tres sistemas— y la
fase 1 empezó por Windows solo porque es la máquina de desarrollo, no por una decisión de
alcance.

El bloque E obliga a tomar esa decisión de verdad, porque a partir del viewer todo lo que
viene detrás (consentimiento en pantalla, indicador de sesión, servicio del sistema,
instalador, firma) es trabajo que se multiplica por tres o no se multiplica.

## La decisión

**Las fases 1 a 7 se completan solo para Windows**, hasta tener un producto instalable y
usable a diario. La portabilidad a Linux y macOS pasa a una **fase 8** posterior.

Concretamente:

- La **fase 6** (servicios, instaladores, firma, actualizaciones) se acota a Windows:
  servicio de Windows, MSI o NSIS, firma Authenticode y actualizaciones.
- Se añade una **fase 8** de portabilidad: captura, input y audio en Linux (X11 y Wayland)
  y macOS, más el empaquetado y la firma de cada uno.
- **Los stubs de Linux y macOS siguen compilando** con `unimplemented!()`. No se borran.

## Por qué

**Windows primero aplaza menos trabajo del que parece.** Las fases 2 y 3 —identidades,
pinning TOFU, autenticación mutua, contraseñas con Argon2id, consentimiento, auditoría,
rendezvous, hole punching, relay— son casi independientes de plataforma: viven en
`vhdesk-crypto`, `vhdesk-transport`, `vhdesk-proto` y `vhdesk-server`, ninguno de los
cuales tiene un `#[cfg(windows)]`. Lo que de verdad se aplaza es captura, input, audio y
empaquetado.

**La parte cara de la multiplataforma es justo la que no queremos pagar todavía.** No es
escribir un backend de captura más: es el modelo de permisos de los portales de Wayland (con
el problema abierto del modo desatendido, que es de diseño y no de implementación), el
permiso de Accesibilidad y la captura de pantalla de macOS, y la firma y notarización de
cada plataforma. Pagar eso mientras el diseño todavía se mueve significa pagarlo dos veces.

**Llegar antes a algo usable a diario es lo que mantiene vivo un proyecto de un solo
desarrollador.** Un escritorio remoto que funciona en una plataforma y se usa todos los días
enseña más sobre qué hay que arreglar que tres implementaciones a medias que no se usan.

## El riesgo que crea esta decisión, y la salvaguarda

El riesgo es concreto y tiene nombre: que los traits de plataforma **se conviertan en
envoltorios de DXGI y `SendInput` con nombre genérico**. Con una sola implementación detrás,
nada impide que un concepto del sistema operativo se filtre al trait, y el coste no aparece
hasta que hay que escribir la segunda implementación, con las fases 5 y 6 ya construidas
encima.

**No es hipotético: ya hay al menos una filtración en el código de hoy.**
`vhdesk_capture::Frame` expone `presented_at_qpc: i64`, que son literalmente unidades del
contador de alta resolución de Windows dentro de un tipo que se presenta como neutral. Y
`MonitorId { adapter, output }` tiene la forma de DXGI —índice de adaptador más índice de
salida—, que en X11 no significa nada. Ninguna de las dos es grave hoy; las dos son
exactamente el tipo de cosa que se acumula sin que nadie lo note.

### Salvaguarda: spike de X11, acotado a un día, al final de la fase 4

Al terminar la fase 4 se implementan `ScreenCapturer` e `InputInjector` sobre **X11**, con
un límite duro de un día.

**No como plataforma soportada.** El objetivo no es que funcione bien ni que se mantenga: es
comprobar que los dos traits admiten una segunda implementación sin cambiar de forma.

X11 es la elección correcta para esto precisamente porque es la más simple de las que
quedan: XShm y XTest, sin portales, sin diálogos de permiso, sin notarización. Todo lo que
falle ahí es un fallo del **diseño del trait**, no del entorno, que es justo lo que se quiere
medir.

El spike se considera superado si los traits sobreviven sin cambios de firma. Si hay que
cambiarlos, es mejor saberlo con dos implementaciones encima que con las fases 5 y 6
construidas sobre una sola. Lo que salga se registra aunque el código del spike se tire.

Preguntas concretas que el spike tiene que responder, además de la general:

- ¿Qué se hace con `presented_at_qpc` cuando no hay QPC? ¿Se convierte a un instante
  neutral, se hace opcional, o se saca del tipo público?
- ¿Sobrevive `MonitorId { adapter, output }`, o hace falta un identificador opaco?
- ¿Basta `CaptureEvent` tal cual, o el modelo de un buffer BGRA contiguo con `stride` se
  queda corto?
- ¿La tabla HID → scancode del crate de input está de verdad en el sitio correcto, o el
  segundo backend revela que hay parte compartible?

## Qué NO cambia

- **Los stubs siguen compilando.** Son la presión que mantiene honestos a los traits: si
  `open_capturer` deja de existir para Linux, nada obliga a que `ScreenCapturer` siga
  siendo un trait y no un alias de la implementación de Windows.
- **La CI sigue en los tres sistemas.** Compilar en Linux y macOS y ejecutar allí los tests
  independientes de plataforma es barato y detecta el día que algo específico de Windows se
  cuela en un crate que no debía tenerlo.
- **Las reglas de dependencia del CLAUDE.md siguen igual.** La lógica de negocio no llama a
  APIs del sistema operativo, y los crates de plataforma esconden las suyas tras `#[cfg]`.
- **`vhdesk-server` no se ve afectado.** Es un servicio de red y se despliega donde sea.

## Consecuencias

- **El README tiene que decirlo con claridad**: la v1.0 es solo Windows, y Linux y macOS
  llegan después. Prometer multiplataforma y no entregarla quema más que no prometerla.
- Los avisos de plataforma que ya están anotados como riesgos abiertos (Wayland y modo
  desatendido, `uinput` y sus reglas udev, el audio de sistema en macOS 13 a 14.6) pasan de
  la fase 6 a la fase 8. **Siguen escritos**: aplazar no es olvidar.
- El escritorio seguro de Windows (UAC y pantalla de bloqueo) sigue en la fase 6, porque es
  de la plataforma que sí se completa.
- La fase 7 (fuzzing, sandboxing, auditoría) se hace sobre Windows. El fuzzing de todo lo
  que parsea bytes de red es independiente de plataforma y se aprovecha entero después.
- La fase 8 hereda un diseño ya validado por el uso diario, que es mejor punto de partida
  para un port que un diseño validado solo por tests.

## Qué nos haría cambiar de opinión

- **Que el spike de X11 obligue a cambiar los traits de forma profunda.** Sería la señal de
  que la abstracción se está construyendo sobre una sola implementación y que conviene
  adelantar un segundo backend real antes de la fase 5.
- **Que aparezca un colaborador con Linux o macOS como plataforma principal.** El argumento
  central de este ADR es el ancho de banda de un solo desarrollador, y deja de aplicar en
  cuanto hay dos.
- **Que una decisión de fase 2 o 3 resulte no ser independiente de plataforma** después de
  todo. El almacén de secretos es el candidato: DPAPI, Secret Service y Keychain no se
  parecen. Si el diseño de la fase 2 se ata a DPAPI, habría que reconsiderar el orden.

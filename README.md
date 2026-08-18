# VHDesk

Sistema de escritorio remoto libre, en Rust y autohospedable.

El host muestra un ID y una contraseña. El viewer introduce ese ID, un servidor de
rendezvous coordina la conexión directa entre las dos máquinas (con relay como respaldo) y
se establece una sesión cifrada extremo a extremo con vídeo, audio, entrada y archivos.

## Estado: en desarrollo, y **la v1.0 será solo para Windows**

Esto es importante y va antes que nada:

| Plataforma | Estado |
|---|---|
| **Windows 10+** | plataforma principal, en desarrollo activo |
| **Linux** (X11 y Wayland) | **después de la v1.0** |
| **macOS 13+** | **después de la v1.0** |

El proyecto se diseña multiplataforma desde el primer día —la captura, la entrada y el audio
viven detrás de traits, y el código de Linux y macOS compila aunque todavía no haga nada—,
pero **se completa primero en Windows y en profundidad**, hasta tener algo instalable y
usable a diario. Linux y macOS llegan después, como fase propia.

El razonamiento está en [ADR-0003](docs/adr/0003-windows-primero.md). En resumen: la parte
cara de la multiplataforma no es escribir otro backend de captura, sino el modelo de
permisos de Wayland, el de macOS y la firma de cada sistema, y no queremos pagar eso
mientras el diseño todavía se mueve.

Se dice aquí y no en una nota al pie porque **prometer multiplataforma y no entregarla quema
más que no prometerla**. Si necesitas escritorio remoto en Linux o macOS hoy, VHDesk todavía
no es para ti.

Nada de esto está terminado: no hay versión publicada, ni instalador, ni autenticación.
Ahora mismo VHDesk sirve para desarrollarlo, no para usarlo.

## Qué lo diferencia

- **Cifrado extremo a extremo obligatorio.** No hay modo sin cifrado, ni siquiera para
  depurar. El relay reenvía datagramas UDP opacos y **nunca** termina la conexión: no puede
  leer la sesión aunque quiera.
- **Autenticación mutua.** El host valida la identidad del viewer igual que el viewer valida
  la del host.
- **Cero telemetría.** Nada sale de tu máquina salvo hacia el peer y el servidor que tú
  configures. Ni analytics, ni informes de fallo.
- **Autohospedable.** El servidor de rendezvous y relay se despliega aparte y es AGPL-3.0.
- **Consentimiento visible e indicador de sesión** que el lado que controla no puede ocultar,
  y log de auditoría en el host.

VHDesk **no** implementa, y no va a implementar, instalación silenciosa, ocultación de
proceso ni persistencia encubierta. Es una herramienta de asistencia remota, no un RAT.

## Compilar

Ver [docs/BUILDING.md](docs/BUILDING.md). Hacen falta Rust estable, libvpx y LLVM.

```bash
cargo build --workspace
cargo test --workspace
```

## Licencia

Los binarios de escritorio (host y viewer) son **GPL-3.0-only**. El servidor de rendezvous y
relay es **AGPL-3.0-only**, porque es un servicio en red y la cláusula de red importa.

VHDesk está escrito desde cero. No contiene código de RustDesk, TeamViewer ni AnyDesk.

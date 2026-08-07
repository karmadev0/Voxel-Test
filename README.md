# Voxel Engine — Fase 4

Motor voxel propio en Rust. **Fase 4**: streaming dinámico de chunks —
el mundo ahora se carga/descarga según por dónde camina el jugador, en vez
de quedar fijo al área generada al arrancar.

## Qué incluye esta entrega

De Fases anteriores:
- Terreno procedural, greedy meshing, backend OpenGL, generación paralela.
- Contador de FPS en el título.
- Romper/colocar bloques con raycasting DDA, hotbar simple (1-5).

Nuevo en Fase 3:
- **Guardado/carga a disco** (`world.rs` + `chunk.rs` con `serde`/`bincode`):
  al modificar un bloque, ese chunk queda marcado como "sucio". `F5` guarda
  todos los chunks sucios en `world_save/chunk_X_Z.bin`. Al iniciar, cada
  chunk primero intenta cargarse desde ahí antes de generarse de nuevo. El
  mundo también se guarda automáticamente al cerrar la ventana.
- **Física real** (`player.rs`, nuevo): gravedad + colisión AABB contra el
  mundo, resuelta eje por eje (mover en X → corregir, mover en Y →
  corregir, mover en Z → corregir). Ya no se atraviesan bloques.
- **Modo Caminar / Vuelo** (tecla `F`): Caminar tiene gravedad, colisión y
  salto; Vuelo es el modo libre de las fases anteriores, útil para
  construir rápido o inspeccionar el mundo desde arriba. Arranca en modo
  Caminar.

Nuevo en Fase 4:
- **Streaming dinámico de chunks** (`world.rs`: `load_chunk` /
  `unload_chunk`, `main.rs`: `update_chunk_streaming`): cada frame se
  chequea en qué chunk está la cámara; si cambió de chunk desde el último
  chequeo, se cargan (generan o leen de disco) los chunks que entraron al
  radio `RENDER_RADIUS`, y se descargan los que quedaron afuera —
  guardando primero si tenían cambios sin guardar, para no perder
  ediciones al alejarte caminando.
- Los chunks nuevos se generan y mallean **en paralelo con rayon**, igual
  que en la carga inicial, para minimizar el hitch al cruzar de chunk.

Nuevo en Fase 5:
- **Streaming asincrónico** (`world.rs`: `ChunkLoader`, `lib.rs`:
  `update_chunk_streaming` + `finalize_ready_chunks`): cruzar de chunk ya
  no bloquea el frame. En vez de generar+mallear los chunks nuevos y
  esperar a que terminen antes de seguir dibujando, cada chunk se manda a
  un hilo de fondo (`rayon::spawn`) y el resultado vuelve por un
  `mpsc::channel`. El hilo principal lo recoge de a lo sumo
  `MAX_FINALIZED_CHUNKS_PER_FRAME` (2) por frame y recién ahí sube la
  malla a la GPU — que es la única parte que tiene que pasar sí o sí por
  el hilo principal (`wgpu::Device` no es seguro de usar desde cualquier
  hilo para crear buffers en este setup). El resultado: cruzar de chunk
  caminando ya no se siente como un microtrabón, aunque tarde uno o dos
  frames más en terminar de aparecer el chunk nuevo (a cambio de no
  trabar nada).

## Manejo de crashes (Fase 6)

Antes, cualquier panic interno (bug de lógica, índice fuera de rango,
`.unwrap()` sobre `None`, etc.) tiraba abajo toda la app sin dejar ningún
rastro útil para depurar. Ahora (`src/crash.rs` + `lib.rs`):

- **El proceso ya no se cierra solo.** Cada callback de winit
  (`resumed`, `window_event`, `device_event`) está envuelto en
  `std::panic::catch_unwind`. Si algo panickea ahí adentro, se atrapa y
  la app queda abierta en una **pantalla roja de emergencia** en vez de
  cerrarse — se deja de correr toda la lógica normal del juego a
  propósito (el mundo puede haber quedado en un estado a medio
  actualizar), pero la ventana sigue viva.
- **Siempre queda un archivo con el reporte completo** (mensaje,
  ubicación exacta en el código, backtrace):
  - Desktop: carpeta `crash_logs/` al lado del ejecutable —
    `crash_<timestamp>.txt` por cada crash, más `last_crash.txt` con el
    más reciente.
  - Android: `Android/data/com.voxelengine.fase4/files/crash_logs/` en
    el almacenamiento del dispositivo (mismo esquema de archivos; se
    puede sacar con un explorador de archivos o `adb pull`). Si por lo
    que sea esa ruta no está disponible, el reporte igual queda en
    `adb logcat` (buscar `PANIC:`).
- **Desktop además muestra un cuadro de diálogo nativo** con el mensaje y
  la ruta del log apenas pasa el crash, y se puede volver a copiar el
  reporte completo al portapapeles apretando **C** mientras se ve la
  pantalla roja.
- **En Android, tocar en cualquier parte de la pantalla roja** copia el
  log completo al portapapeles del sistema (vía JNI contra
  `ClipboardManager`, no requiere ningún permiso especial). Como el
  engine todavía no tiene un pipeline de texto para mostrar un mensaje de
  confirmación en pantalla, la señal de "listo, se copió" es que la
  pantalla cambia brevemente a verde y vuelve a rojo. No hay diálogo
  nativo en Android (no tiene un equivalente simple sin agregar una
  Activity/layout de Java aparte).
- **Pendiente / próximo paso natural:** la pantalla de crash hoy es solo
  un color sólido (no hay texto en pantalla porque el engine todavía no
  tiene un pipeline de fuentes/texto — la hotbar de Android tiene la
  misma limitación). Si en algún momento se agrega un pase de texto, lo
  primero que lo va a aprovechar es justamente esta pantalla, para
  mostrar el mensaje del crash directamente en Android sin depender de
  logcat.

## Controles

| Acción | Tecla |
|---|---|
| Moverse | W A S D |
| Saltar (modo Caminar) / Subir (modo Vuelo) | Espacio |
| Bajar (solo modo Vuelo) | Shift izquierdo |
| Mirar alrededor | Mouse (click primero para capturarlo) |
| Romper bloque | Click izquierdo |
| Colocar bloque | Click derecho |
| Elegir bloque a colocar | 1 (pasto) / 2 (tierra) / 3 (piedra) |
| Alternar Caminar / Vuelo | F |
| Guardar mundo a disco | F5 |
| Liberar el mouse | Esc |

## Limitaciones conocidas (quedan para más adelante)

- El mesh de cada chunk todavía no consulta los bloques reales del chunk
  vecino en los bordes (los trata como aire) — puede haber alguna cara de
  más dibujada en el límite entre chunks. No rompe nada, es una
  optimización pendiente (Fase 5, todavía no implementada — ver más
  abajo).
- Colores planos por tipo de bloque, sin texturas todavía (Fase 5).
- ~~El streaming carga/mallea los chunks nuevos de forma síncrona~~ →
  resuelto: streaming asincrónico, ver sección Fase 5 más arriba.

## Cómo compilar

```bash
sudo apt-get install -y libx11-dev libxi-dev libxrandr-dev libxcursor-dev \
  libxinerama-dev libgl1-mesa-dev libegl1-mesa-dev pkg-config

cargo build --release
./target/release/voxel-engine
```

O compilalo en GitHub Actions (`.github/workflows/desktop.yml`) y descargá
el binario desde la pestaña Actions → Artifacts (hay una versión por
plataforma: `voxel-engine-linux`, `voxel-engine-windows`,
`voxel-engine-macos`).

**Nota:** la carpeta `world_save/` (donde se guarda tu mundo) y `target/`
no se suben al repo (ver `.gitignore`) — son datos locales de cada partida
y archivos de compilación, no código fuente.

## Android

El motor corre en Android como app nativa (misma lógica que desktop,
compartida en `lib.rs`), usando el backend Vulkan de wgpu. Diferencias
respecto a desktop:

- **No hay teclado ni mouse**, así que el input se maneja por zonas
  táctiles fijas, dibujadas en pantalla como overlay 2D semitransparente
  (`src/ui_overlay.rs`; el hit-test en sí vive en `src/touch.rs`, que es
  la fuente de verdad de las posiciones — el dibujo las reutiliza para no
  desincronizarse nunca):
  - Mitad izquierda de la pantalla: joystick de movimiento (aro + nub,
    "flotante" — aparece donde tocás, no en un punto fijo).
  - Mitad derecha, arrastrar: mirar (equivalente al mouse). Sin
    indicador visual propio.
  - Círculo entre ambos joysticks, abajo: saltar (se ilumina mientras se
    mantiene apretado).
  - Esquina inferior derecha: dos círculos — verde coloca bloque, rojo
    rompe.
  - Esquina superior derecha: cinco cuadrados = hotbar (1-5, igual que
    las teclas Digit1-5 en desktop), pintados con el mismo color que el
    bloque que seleccionan; el activo tiene borde blanco. No hay texto
    (todavía no hay pase de fuentes en el engine).
- Vulkan es obligatorio (sin fallback a GLES por ahora).
- El guardado/carga a disco (`world_save/`) usa el almacenamiento interno
  de la app en vez de una ruta relativa arbitraria — gestionado por
  Android automáticamente, no requiere permisos de almacenamiento.

### Compilar el APK localmente

```bash
rustup target add aarch64-linux-android
cargo install cargo-apk --locked
# Necesitás el NDK instalado y la variable ANDROID_NDK_HOME apuntando a él.
cargo apk build --release --target aarch64-linux-android
# El APK queda bajo target/release/apk/ (o target/<target-triple>/release/apk/)
```

Para probarlo con un dispositivo conectado por USB (con depuración USB
activada): `cargo apk run --release --target aarch64-linux-android`.

### Compilar el APK en GitHub Actions (sin instalar nada localmente)

El workflow `.github/workflows/android.yml` compila el APK en cada push a
`main` (o a mano desde la pestaña **Actions → Build Android APK → Run
workflow**) y lo deja descargable como artifact durante 14 días.

**Importante sobre las rutas:** el workflow asume que la raíz del repo de
GitHub contiene una carpeta `voxel-engine/` (igual que este zip). Si en tu
repo `Cargo.toml` está directamente en la raíz (sin esa carpeta
intermedia), sacá `voxel-engine` de `working-directory` en el workflow y
de la ruta del `path:` en el paso de `upload-artifact`.

El APK se firma con la clave de debug de Android (se genera sola la
primera vez) — instalable y jugable en cualquier dispositivo/emulador,
pero no apta para publicar en Play Store sin volver a firmarla con una
clave de release propia.

## Fase 5

## Próximos pasos (Fase 5, en progreso)

- [x] Streaming asincrónico (generar en hilo de fondo, sin microtrabones).
- [x] Overlay 2D en Android: joystick, botones de romper/colocar/saltar y
      hotbar, ahora visibles en pantalla (antes eran zonas invisibles).
      Sin texto (no hay pase de fuentes todavía): la hotbar usa el mismo
      color que el bloque que selecciona, y el bloque activo se marca con
      un borde blanco. Ver `src/ui_overlay.rs`.
- [ ] Culling consciente de chunks vecinos en el greedy meshing.
- [ ] Cuevas 3D e iluminación por propagación.
- [ ] Atlas de texturas real en vez de colores planos.



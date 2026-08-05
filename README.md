# Voxel Engine — Fase 1

Motor voxel propio en Rust. Esta es la **Fase 1**: mundo navegable con
terreno generado proceduralmente, renderizado con `wgpu` sobre backend
**OpenGL** (pensado para la Intel UHD Graphics 600 / Celeron N4000), usando
**greedy meshing** desde el arranque para mantener el número de triángulos
bajo control en hardware modesto.

## Qué incluye esta entrega

- Generación de terreno con ruido Perlin (colinas suaves, una capa de bioma:
  pasto / tierra / piedra).
- 81 chunks generados en paralelo con `rayon` (grilla de 9×9 alrededor del
  origen, radio configurable en `main.rs` vía `RENDER_RADIUS`).
- Greedy meshing real: cada chunk se malla fusionando caras adyacentes del
  mismo tipo de bloque en rectángulos, no cubo por cubo.
- Cámara de vuelo libre (WASD + mouse) sin colisión todavía.
- CI en GitHub Actions que verifica que el proyecto compila en Linux en
  cada push (no ejecuta el juego — un runner de Actions no tiene GPU con
  salida de video, solo confirma que el código compila limpio).

## Qué NO incluye todavía (a propósito, es la siguiente fase)

- Colocar / romper bloques (Fase 2).
- Colisión de la cámara con el terreno / gravedad (Fase 2).
- Texturas (por ahora cada tipo de bloque es un color plano).
- Guardado en disco / streaming de chunks (Fase 3).

## Cómo compilar

Necesitás Rust instalado (`rustup.rs`). En Linux también necesitás las
librerías de sistema para ventana + OpenGL:

```bash
sudo apt-get install -y libx11-dev libxi-dev libxrandr-dev libxcursor-dev \
  libxinerama-dev libgl1-mesa-dev libegl1-mesa-dev pkg-config

cargo build --release
./target/release/voxel-engine
```

Si preferís compilar en una máquina más potente (o en GitHub Actions) y
correrlo después en tu laptop: el CI de este repo ya sube el binario
compilado como *artifact* descargable en cada push (pestaña **Actions** →
el run correspondiente → **Artifacts** → `voxel-engine-linux-x86_64`).
Como es cross-compiling x86_64 → x86_64 Linux, el binario debería correr
directo en tu Celeron N4000 sin recompilar ahí, siempre que la libc del
runner de Actions (Ubuntu) sea compatible con la tuya — si tu distro es muy
distinta, puede hacer falta compilar directo en tu máquina o usar `musl`
como target estático (lo dejamos pendiente si hace falta).

## Controles

| Acción | Tecla |
|---|---|
| Moverse | W A S D |
| Subir / bajar | Espacio / Shift izquierdo |
| Mirar alrededor | Mouse (click primero para capturarlo) |
| Liberar el mouse | Esc |

## Nota honesta sobre esta primera entrega

El ecosistema de `wgpu` cambia de API con cierta frecuencia entre
versiones. Este código está escrito contra `wgpu = "0.19"`; si al compilar
`cargo` resuelve una versión más nueva y aparecen errores de tipos o de
métodos renombrados, decime el mensaje de error exacto y lo ajusto — es
normal en el primer intento con un ecosistema que evoluciona rápido, como
mencioné antes de escribir el código.

## Próximos pasos (Fase 2)

- Raycasting (DDA) para seleccionar el bloque apuntado.
- Colocar / romper bloques con regeneración incremental del mesh del chunk
  afectado.
- Atlas de texturas real en vez de colores planos.
- Colisión AABB de la cámara contra el terreno + gravedad.

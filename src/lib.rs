/// lib.rs
/// Núcleo del engine, compartido entre el binario de escritorio (main.rs,
/// que solo llama a `run()`) y la entrada nativa de Android
/// (`android_main`, más abajo). Arma la ventana (winit), inicializa wgpu
/// (OpenGL en desktop por la GPU integrada del Celeron N4000, Vulkan en
/// Android), genera un área de chunks en paralelo con rayon, construye sus
/// mallas con greedy meshing, y corre el loop de render con una cámara de
/// vuelo libre. En Android, además de teclado/mouse, el loop reacciona a
/// eventos táctiles (ver `touch.rs`) para mover, mirar, romper/colocar y
/// elegir bloque de la hotbar.

mod engine;
mod environment;
mod logic;
mod physics;
mod platform;
mod textures;

use engine::camera::{projection_matrix, Camera};
use engine::highlight;
use engine::render_state::Uniforms;
use environment::chunk::{BlockType, Chunk, CHUNK_SIZE_X, CHUNK_SIZE_Z};
use environment::mesher::Vertex;
use environment::sky::{FOG_START_FRACTION, SKY_COLOR};
use environment::save_manager::WorldMeta;
use environment::world::{raycast, World};
use logic::immersive;
use logic::touch::{TouchAction, TouchController};
use logic::ui_overlay;
use physics::player::Player;
use platform::crash;
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use wgpu::util::DeviceExt;
use winit::{
    application::ApplicationHandler,
    event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::PhysicalKey,
    window::{CursorGrabMode, Window, WindowId},
};

#[cfg(target_os = "android")]
use winit::platform::android::activity::AndroidApp;

/// Pantalla activa del juego. Controla qué se dibuja y si la lógica
/// del juego (física, cámara) se actualiza o se pausa. Cualquier
/// variante distinta de `Playing` pausa el juego (ver `update`).
///
/// Jerarquía de menús (botón "< VOLVER" y Esc suben un nivel, ver
/// `TouchAction::Back` y el manejo de Escape en `window_event`):
///
///   MainMenu                ("JUGAR" / "CONFIGURACIÓN" / "SALIR", raíz)
///     ├─ Settings           (alcanzable también desde acá, ver
///     │                       `State::settings_return`)
///     ├─ WorldList          ("+ CREAR MUNDO NUEVO" + mundos guardados)
///     │    ├─ NameWorld          (teclado en pantalla, al crear)
///     │    └─ ConfirmDeleteWorld (al tocar el ícono de borrar de una fila)
///     └─ Playing
///          └─ Pause              ("MODO DE JUEGO" / "AJUSTES" / "SALIR")
///               ├─ GameMode      (selector Supervivencia/Creativo/Espectador)
///               └─ Settings      (FPS, radio de chunks, nubes, niebla + botón
///                    │            "AJUSTES ADICIONALES")
///                    └─ SettingsMore  (info de build, panel de debug)
///
/// `Settings` es la única pantalla alcanzable desde dos padres distintos
/// (`MainMenu` y `Pause`) — de ahí `State::settings_return`, que guarda
/// a cuál de los dos hay que volver con "< VOLVER".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GameScreen {
    /// Menú principal: primera pantalla al abrir la app, y a la que se
    /// vuelve al salir de una partida (ver `TouchAction::ExitGame`).
    MainMenu,
    /// Lista de mundos guardados + "CREAR MUNDO NUEVO", alcanzable desde
    /// "JUGAR" en el menú principal (ver `TouchAction::PlayGame`).
    WorldList,
    /// Teclado en pantalla para elegir el nombre del mundo nuevo, entre
    /// `WorldList` y crear el mundo de verdad (ver
    /// `TouchAction::OpenNameWorld`/`ConfirmNameWorld`).
    NameWorld,
    /// Confirmación antes de borrar un mundo de la lista (ver
    /// `TouchAction::RequestDeleteWorld`/`ConfirmDeleteWorld`), para que
    /// tocar el ícono de borrar por error no se lleve puesto el mundo.
    ConfirmDeleteWorld,
    /// Juego corriendo normalmente.
    Playing,
    /// Menú de pausa (antes esto era la única pantalla de
    /// "Configuración"): 3 botones — modo de juego, ajustes, salir.
    Pause,
    /// Selector de modo de juego, fullscreen, abierto desde Pause.
    GameMode,
    /// Ajustes de uso más frecuente (FPS, radio de chunks, nubes,
    /// niebla), fullscreen, abierto desde Pause o desde MainMenu.
    Settings,
    /// Ajustes menos frecuentes (info de build, panel de debug),
    /// fullscreen, abierto desde Settings vía "AJUSTES ADICIONALES".
    SettingsMore,
    /// Pantalla no interactiva de "GUARDANDO...", intercalada entre
    /// `Playing`/`Pause` y `MainMenu` al tocar "SALIR" (ver
    /// `TouchAction::ExitGame`). Dura exactamente 2 frames: el primero
    /// solo pinta esta pantalla (ver `State::saving_started`), el
    /// segundo hace el guardado real — así el guardado, que es
    /// síncrono y puede tardar con mundos grandes, no bloquea el frame
    /// sin que el jugador vea ningún feedback antes.
    Saving,
}

/// Modo de juego, igual que en Minecraft:
///
/// - `Survival` (antes "modo caminar"): gravedad + colisión normal
///   contra el mundo, vía `Player::update`. Se puede construir encima o
///   debajo de uno mismo sin restricción — si te encierras, te
///   encierras: hay que cavar para salir. Sin auto-rescate.
/// - `Creative`: vuelo libre (sin gravedad) pero CON colisión contra el
///   mundo, vía `Player::fly_update` — no se atraviesan bloques
///   caminando/volando. No se puede colocar un bloque que se solape con
///   el propio jugador (a diferencia de Survival), y si de todos modos
///   queda atrapado (por streaming de chunks, un bug, etc.) el
///   auto-rescate lo saca al aire libre más cercano.
/// - `Spectator`: vuelo libre sin colisión alguna (noclip), vía
///   `Camera::update` — atraviesa cualquier bloque. Nunca puede quedar
///   atrapado, así que ni el bloqueo de colocación ni el auto-rescate
///   aplican acá.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GameMode {
    Survival,
    Creative,
    Spectator,
}

impl GameMode {
    /// Nombre para mostrar en overlays de texto (panel de debug, mensaje
    /// al cambiar de modo) — ya en mayúsculas, para la fuente bitmap.
    fn label(self) -> &'static str {
        match self {
            GameMode::Survival => "SUPERVIVENCIA",
            GameMode::Creative => "CREATIVO",
            GameMode::Spectator => "ESPECTADOR",
        }
    }

    /// Si en este modo el jugador tiene colisión contra el mundo
    /// (Survival y Creative sí, Spectator no).
    #[allow(dead_code)] // se usará en el paso 5 (auto-rescate)
    fn has_collision(self) -> bool {
        !matches!(self, GameMode::Spectator)
    }

    /// Si en este modo NO se puede colocar un bloque sólido que se
    /// solape con la propia caja de colisión del jugador (ver
    /// `handle_click`). Ahora aplica a Survival Y Creative: colocar
    /// bloque(s) sólido(s) encima de uno mismo mirando hacia abajo ya
    /// no encierra al jugador sin salida — ese "encerrarse con dos
    /// bloques y listo" era un exploit, no una mecánica deseada.
    /// Espectador no tiene colisión, así que la pregunta ni aplica.
    ///
    /// Ojo: esto SOLO bloquea la colocación manual vía `handle_click`.
    /// A propósito no toca nada relacionado con asfixia/ahogo por
    /// bloques con gravedad (arena, agua) que puedan agregarse a
    /// futuro — esos bloques van a caer sobre el jugador por su propia
    /// simulación de física, no por esta ruta de "colocar con el
    /// mouse/dedo", así que van a poder seguir atrapando/ahogando al
    /// jugador como mecánica real del juego sin que este chequeo se
    /// interponga.
    fn blocks_self_placement(self) -> bool {
        !matches!(self, GameMode::Spectator)
    }

    /// Si en este modo aplica el auto-rescate al aire libre más cercano
    /// cuando el jugador queda atrapado (paso 5). Solo Creative.
    fn auto_rescue(self) -> bool {
        matches!(self, GameMode::Creative)
    }

    /// Índice 0/1/2 (Survival/Creative/Spectator) para el selector de 3
    /// opciones del panel de ajustes (ver logic/ui_overlay.rs y
    /// logic/touch.rs, que no conocen el tipo `GameMode` en sí, solo
    /// índices — así ese código no depende de un tipo interno de lib.rs).
    fn index(self) -> usize {
        match self {
            GameMode::Survival => 0,
            GameMode::Creative => 1,
            GameMode::Spectator => 2,
        }
    }

    /// Inverso de `index()`: arma el `GameMode` a partir del índice que
    /// llega desde `TouchAction::SetGameMode`. Cualquier índice fuera de
    /// 0..3 (no debería pasar nunca, ya que `rect_mode_option` solo
    /// genera 0/1/2) cae en Survival por seguridad.
    fn from_index(index: usize) -> Self {
        match index {
            1 => GameMode::Creative,
            2 => GameMode::Spectator,
            _ => GameMode::Survival,
        }
    }
}

/// Lo que un hilo de fondo manda de vuelta al terminar de generar/cargar
/// un chunk y mallearlo: coordenadas, el chunk (para insertarlo en
/// `World`) y su malla ya calculada en CPU (`MeshData`) — lo único que
/// falta es subirla a la GPU, y eso se hace en el hilo principal porque
/// `wgpu::Device` se usa ahí.
type ChunkResult = ((i32, i32), environment::chunk::Chunk, environment::mesher::MeshData);

// Radio de chunks a generar alrededor del origen (bajo por defecto: en
// el Celeron N4000 preferimos ver menos mundo a buen framerate antes
// que un mundo enorme que trabe el frame). Ahora es ajustable en vivo
// desde la pantalla de configuración (ver `State::render_radius`); esta
// constante es solo el valor inicial.
const DEFAULT_RENDER_RADIUS: i32 = 4;

// Límites del slider de distancia de chunks en la pantalla de config.
// El mínimo (1) es el radio más chico posible: el jugador solo ve el
// chunk en el que está parado más el anillo que lo rodea. Con radios
// tan cortos la niebla (ver `fog_start`/`fog_end` en `update`) es la que
// evita que el borde del mundo cargado se vea como un corte abrupto.
// El máximo (128) ya son 257x257 = 66049 chunks si se llegara a cargar
// todo a la vez — en el hardware objetivo de este proyecto (Celeron
// N4000) eso no da un framerate jugable, así que aunque la UI lo
// permita, es responsabilidad de quien juega no dejarlo tan alto ahí.
const MIN_RENDER_RADIUS: i32 = 1;
const MAX_RENDER_RADIUS: i32 = 128;

/// Opciones cíclicas del intervalo de autoguardado, en segundos, fila
/// "AUTOGUARDADO" de la pantalla de ajustes adicionales (ver
/// `TouchAction::CycleAutosaveInterval`). 60s es el default pedido
/// originalmente; el resto da margen para partidas más tranquilas (5
/// min) o dispositivos donde preferís perder como mucho 15s de progreso.
const AUTOSAVE_OPTIONS_SECS: [u32; 5] = [15, 30, 60, 120, 300];
const DEFAULT_AUTOSAVE_SECS: u32 = 60;

/// Cuántos chunks recién llegados del hilo de fondo se convierten a
/// buffers de GPU por frame como máximo. Crear un `wgpu::Buffer` no es
/// gratis (aunque sea rápido comparado con generar+mallear el chunk), así
/// que ponerle un tope evita un frame largo si de golpe terminan muchos
/// chunks a la vez (por ejemplo al aparecer, o si el jugador corre rápido
/// en modo Vuelo y cruza varios chunks de un saque).
const MAX_FINALIZED_CHUNKS_PER_FRAME: usize = 2;

// Distancia máxima (en bloques) a la que se puede romper/colocar.
const REACH: f32 = 6.0;

struct ChunkMesh {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    num_indices: u32,
}

struct State {
    // Device/queue/surface, pipelines y buffers de wgpu (ver
    // engine/render_state.rs). Todo lo puramente gráfico vive ahí.
    render: engine::render_state::RenderState,

    // Instante en el que arrancó la app: `time.x` en los uniforms (ver
    // arriba) se calcula como el tiempo transcurrido desde acá, para
    // animar el desplazamiento de las nubes con el viento.
    start_time: Instant,

    chunk_meshes: HashMap<(i32, i32), ChunkMesh>,
    world: World,
    camera: Camera,
    player: Player,
    game_mode: GameMode,
    selected_block: BlockType,

    mouse_captured: bool,
    last_frame: Instant,

    // Controles táctiles (Android). En desktop queda inicializado pero
    // sin uso, ya que ahí no llegan WindowEvent::Touch.
    touch: TouchController,

    // Streaming de chunks: recordamos en qué chunk está parado el jugador
    // para no recalcular qué cargar/descargar en cada frame — solo cuando
    // realmente cruza a un chunk distinto (o cambia `render_radius` desde
    // la pantalla de config, ver `last_streamed_render_radius`).
    current_player_chunk: (i32, i32),

    // Radio de chunks actual (ajustable en vivo, ver `MIN_RENDER_RADIUS`/
    // `MAX_RENDER_RADIUS` y la fila "Distancia de chunks" en la pantalla
    // de configuración). `last_streamed_render_radius` es el valor que
    // tenía la última vez que `update_chunk_streaming` cargó/descargó
    // chunks; cuando difiere de `render_radius`, forzamos un recálculo
    // aunque el jugador siga parado en el mismo chunk.
    render_radius: i32,
    last_streamed_render_radius: i32,

    // --- Streaming asincrónico de chunks ---
    // `chunk_loader` es el handle liviano que se clona y se manda a
    // `rayon::spawn` para generar/cargar un chunk en un hilo de fondo.
    // `pending_chunks` evita pedir dos veces el mismo chunk si todavía no
    // volvió del hilo de fondo. El resultado (chunk + su malla ya
    // calculada, todo trabajo de CPU) vuelve por `chunk_result_rx`; recién
    // ahí, en el hilo principal, se sube a la GPU (`finalize_ready_chunks`).
    chunk_loader: environment::world::ChunkLoader,
    pending_chunks: std::collections::HashSet<(i32, i32)>,
    chunk_result_tx: std::sync::mpsc::Sender<ChunkResult>,
    chunk_result_rx: std::sync::mpsc::Receiver<ChunkResult>,

    // Contador de FPS: contamos frames y cada 1 segundo calculamos el
    // promedio, en vez de medir frame a frame (eso saltaría demasiado
    // para ser legible).
    fps_frame_count: u32,
    fps_timer: Instant,
    pub current_fps: f32,
    // Si se dibuja o no el contador de FPS en pantalla. Se puede
    // prender/apagar desde el panel de configuración.
    show_fps: bool,
    // Si se dibuja o no la capa de nubes (draw call de `clouds_pipeline`
    // en `render`). Prendido/apagado desde el panel de configuración,
    // fila "NUBES".
    show_clouds: bool,
    // Si la niebla de distancia está activa. Cuando es `false`, `update`
    // manda un `fog_start`/`fog_end` centinela enorme en vez del
    // calculado a partir de `render_radius`, así el mix hacia
    // `SKY_COLOR` nunca llega a activarse, ni en el terreno ni en el
    // borde de la capa de nubes. Prendido/apagado desde el panel de
    // configuración, fila "NIEBLA".
    show_fog: bool,
    // Si se dibuja el overlay de información de build (etiqueta de build
    // + plataforma) en la esquina superior izquierda. Prendido/apagado
    // desde el panel de configuración, fila "INFO DE BUILD".
    show_build_info: bool,
    // Si se dibuja el panel de debug (posición, chunk, bloque apuntado,
    // modo, fps, ruta de log). Toggleable con F3 (desktop) o desde el
    // panel de configuración, fila "PANEL DE DEBUG (F3)".
    show_debug_panel: bool,
    // Hasta cuándo mostrar "COPIADO!" en el botón del panel de debug en
    // vez de "COPIAR", tras un copiado exitoso. `None` = sin flash activo.
    debug_copy_flash_until: Option<Instant>,
    // Cooldown del auto-rescate (paso 5): evita re-disparar un
    // teletransporte en frames consecutivos mientras la posición
    // rescatada termina de asentarse (por ejemplo si el punto elegido
    // resulta estar muy pegado a otro sólido y el jugador vuelve a
    // "tocar" algo un par de frames después). `None` = sin cooldown
    // activo, se puede rescatar de nuevo apenas haga falta.
    rescue_cooldown_until: Option<Instant>,
    // Rescate en curso: posición de aire libre hacia la que se está
    // arrastrando al jugador de a poco (ver `RESCUE_SPEED` y
    // `step_rescue_drag`), en vez de teletransportarlo de un frame a
    // otro. `None` = no hay rescate activo ahora mismo.
    rescue_target: Option<glam::Vec3>,

    /// Pantalla activa (ver `GameScreen`).
    game_screen: GameScreen,
    /// A qué pantalla volver al tocar "< VOLVER" desde `Settings`, ya
    /// que esa pantalla es alcanzable tanto desde `MainMenu` como desde
    /// `Pause` (ver el diagrama en `GameScreen`). Se actualiza cada vez
    /// que se abre `Settings` (`TouchAction::OpenSettingsScreen`), a
    /// partir de la pantalla en la que estábamos parados en ese momento.
    settings_return: GameScreen,
    /// Última posición conocida del cursor del mouse, en píxeles
    /// físicos (ver `WindowEvent::CursorMoved`). Solo se usa en
    /// desktop, para poder hacer hit-test de los botones de las
    /// pantallas de menú con un click — a diferencia de Android, un
    /// `WindowEvent::MouseInput` no trae su propia coordenada.
    cursor_pos: (f64, f64),

    /// Mundos guardados disponibles, refrescado cada vez que se entra a
    /// `GameScreen::WorldList` (ver `TouchAction::PlayGame`). La lista
    /// en sí vive en disco (`save_manager::list_worlds`); esto es sólo
    /// una copia para que `render`/hit-testing no tengan que leer el
    /// filesystem en cada frame.
    available_worlds: Vec<WorldMeta>,
    /// Nombre del mundo actualmente cargado, si hay uno. `None` antes de
    /// entrar a jugar por primera vez (arranque en `MainMenu`).
    current_world_name: Option<String>,

    /// Nombre que se está escribiendo en `GameScreen::NameWorld`, tecla
    /// a tecla (ver `TouchAction::KeyboardChar`/`KeyboardBackspace`).
    /// Se prellena con `save_manager::next_free_name()` al abrir esa
    /// pantalla (`TouchAction::OpenNameWorld`) y se usa tal cual al
    /// confirmar (`ConfirmNameWorld`) — si queda vacío, se vuelve a
    /// generar un nombre por defecto en vez de crear un mundo sin
    /// nombre.
    name_input: String,
    /// Texto en composición del IME (`Ime::Preedit`) mientras se está
    /// tipeando en `GameScreen::NameWorld` — por ejemplo, sílabas que
    /// todavía no se confirmaron en un IME de predicción/CJK, o el
    /// candidato resaltado antes de tocar "espacio"/aceptar. Se muestra
    /// pegado a `name_input` con otro color (ver
    /// `ui_overlay::build_nameworld_screen`) pero NO se agrega a
    /// `name_input` hasta que llega `Ime::Commit` — eso es justamente lo
    /// que distingue "todavía escribiendo" de "ya confirmado". Se limpia
    /// al entrar/salir de `NameWorld` y en cada `Ime::Commit`/`Disabled`.
    name_preedit: String,
    /// Índice en `available_worlds` del mundo que se va a borrar,
    /// mientras `GameScreen::ConfirmDeleteWorld` está en pantalla (ver
    /// `TouchAction::RequestDeleteWorld`). `None` el resto del tiempo.
    pending_delete_index: Option<usize>,

    /// Cada cuántos segundos se autoguarda mientras se juega (ver
    /// `update`, chequeo contra `last_autosave`). Ajustable en vivo desde
    /// "AJUSTES ADICIONALES" → fila "AUTOGUARDADO" (ver
    /// `AUTOSAVE_OPTIONS_SECS`).
    autosave_interval_secs: u32,
    /// Último instante en que corrió el autoguardado (o en que arrancó/
    /// se cargó la partida, como punto de partida). Comparado contra
    /// `autosave_interval_secs` en cada `update()` mientras se está
    /// jugando.
    last_autosave: Instant,

    /// Distingue el primer frame en `GameScreen::Saving` (todavía en
    /// `false`: `update()` no hace nada más que dejar pasar el frame
    /// para que `render()` pinte "GUARDANDO...") del segundo frame en
    /// adelante (ya en `true`: recién ahí `update()` ejecuta el
    /// guardado síncrono de verdad y vuelve a `MainMenu`). Sin este
    /// diferimiento de un frame, el guardado se ejecutaría en el mismo
    /// frame en que se entra a `Saving` y la pantalla nunca llegaría a
    /// pintarse antes del bloqueo.
    saving_started: bool,
}

impl State {
    async fn new(window: Arc<winit::window::Window>) -> Self {
        let render = engine::render_state::RenderState::new(window).await;

        // Ya no se genera terreno acá: la app arranca en `MainMenu` y el
        // mundo de verdad recién se crea/carga cuando el jugador elige
        // uno en la lista de mundos (ver `State::start_world`, disparado
        // desde `TouchAction::SelectWorld`/`CreateNewWorld`). Acá solo
        // dejamos un `World` vacío de relleno para no tener que hacer
        // `Option<World>` en todos lados.
        let world = World::new(0, std::path::PathBuf::new());
        let chunk_meshes: HashMap<(i32, i32), ChunkMesh> = HashMap::new();

        let camera = Camera::new(glam::Vec3::new(8.0, 40.0, 8.0));
        let player = Player::new(glam::Vec3::new(8.0, 40.0, 8.0));

        let chunk_loader = world.loader();
        let (chunk_result_tx, chunk_result_rx) = std::sync::mpsc::channel();

        Self {
            render,
            start_time: Instant::now(),
            chunk_meshes,
            world,
            camera,
            player,
            game_mode: GameMode::Survival,
            selected_block: BlockType::Stone,
            mouse_captured: false,
            last_frame: Instant::now(),
            touch: TouchController::new(),
            fps_frame_count: 0,
            fps_timer: Instant::now(),
            current_fps: 0.0,
            show_fps: true,
            show_clouds: true,
            show_fog: true,
            show_build_info: true,
            show_debug_panel: false,
            debug_copy_flash_until: None,
            rescue_cooldown_until: None,
            rescue_target: None,
            game_screen: GameScreen::MainMenu,
            settings_return: GameScreen::MainMenu,
            cursor_pos: (0.0, 0.0),
            current_player_chunk: (0, 0),
            render_radius: DEFAULT_RENDER_RADIUS,
            last_streamed_render_radius: DEFAULT_RENDER_RADIUS,
            chunk_loader,
            pending_chunks: std::collections::HashSet::new(),
            chunk_result_tx,
            chunk_result_rx,
            available_worlds: Vec::new(),
            current_world_name: None,
            name_input: String::new(),
            name_preedit: String::new(),
            pending_delete_index: None,
            autosave_interval_secs: DEFAULT_AUTOSAVE_SECS,
            last_autosave: Instant::now(),
            saving_started: false,
        }
    }

    /// Carga (genera el terreno inicial de) el mundo descripto por
    /// `meta` y entra a jugar. Es el único punto donde se construye un
    /// `World` "de verdad" con contenido — reemplaza lo que antes hacía
    /// `State::new` de una: generar el área inicial, mallear todos esos
    /// chunks en paralelo con rayon, y subirlos a la GPU. Sirve tanto
    /// para `SelectWorld` (mundo ya existente) como para `CreateNewWorld`
    /// (mundo recién creado, sin chunks guardados todavía — ahí
    /// `generate_area` genera en vez de leer de disco, ver `ChunkLoader`).
    fn start_world(&mut self, meta: WorldMeta) {
        log::info!("Cargando mundo '{}' (semilla {})...", meta.name, meta.seed);
        let save_dir = environment::save_manager::world_dir(&meta.name);
        let mut world = World::new(meta.seed, save_dir);
        world.generate_area(self.render_radius);

        let coords: Vec<(i32, i32)> = world.chunks.keys().copied().collect();
        let mesh_data: Vec<((i32, i32), environment::mesher::MeshData)> = coords
            .par_iter()
            .map(|&(cx, cz)| {
                let mesh = world.generate_chunk_mesh(cx, cz).unwrap_or(environment::mesher::MeshData {
                    vertices: Vec::new(),
                    indices: Vec::new(),
                });
                ((cx, cz), mesh)
            })
            .collect();

        let mut chunk_meshes: HashMap<(i32, i32), ChunkMesh> = HashMap::new();
        for ((cx, cz), mesh) in mesh_data {
            if mesh.indices.is_empty() {
                continue;
            }
            chunk_meshes.insert((cx, cz), build_chunk_mesh(&self.render.device, cx, cz, &mesh));
        }

        self.chunk_loader = world.loader();
        self.world = world;
        self.chunk_meshes = chunk_meshes;
        self.pending_chunks.clear();
        // Por si quedó algún resultado pendiente de un mundo anterior
        // dando vueltas en el canal (no debería, pero así no se filtra
        // un chunk del mundo viejo al nuevo si justo llegó tarde).
        while self.chunk_result_rx.try_recv().is_ok() {}

        // Punto de spawn: si el mundo ya tenía una partida guardada,
        // reanudamos justo donde se dejó (posición + rotación de
        // cámara); si es la primera vez (mundo recién creado, o un
        // `meta.bin` de antes de que existiera `player_state`), usamos
        // el spawn de siempre.
        let (mut spawn_pos, spawn_yaw, spawn_pitch) = match meta.player_state {
            Some(ps) => (
                glam::Vec3::new(ps.feet_x, ps.feet_y, ps.feet_z),
                ps.yaw,
                ps.pitch,
            ),
            None => (glam::Vec3::new(8.0, 40.0, 8.0), -90f32.to_radians(), 0.0),
        };

        // Chequeo de spawn seguro: esto es responsabilidad del código,
        // no del jugador (a diferencia del auto-rescate en Creativo, que
        // solo actúa si el jugador se encerró jugando), así que corre
        // siempre acá, en los dos casos — mundo nuevo generado o mundo
        // viejo cargado — sin importar el modo de juego. Se resuelve
        // ANTES de crear `Player`/`Camera` definitivos, así nunca llega
        // a verse ni un frame atascado dentro de un bloque.
        {
            let probe = Player::new(spawn_pos);
            if probe.is_trapped(&self.world) {
                if let Some(free_pos) = probe.find_nearest_free_position(&self.world, 8) {
                    log::warn!(
                        "Spawn ({:.1}, {:.1}, {:.1}) estaba dentro de un bloque, reubicando a ({:.1}, {:.1}, {:.1}).",
                        spawn_pos.x, spawn_pos.y, spawn_pos.z,
                        free_pos.x, free_pos.y, free_pos.z,
                    );
                    spawn_pos = free_pos;
                } else {
                    log::warn!(
                        "Spawn ({:.1}, {:.1}, {:.1}) estaba dentro de un bloque y no se encontró aire libre en 8 bloques a la redonda.",
                        spawn_pos.x, spawn_pos.y, spawn_pos.z,
                    );
                }
            }
        }

        self.camera = Camera::new(spawn_pos);
        self.camera.yaw = spawn_yaw;
        self.camera.pitch = spawn_pitch;
        self.player = Player::new(spawn_pos);
        self.game_mode = GameMode::Survival;
        self.current_player_chunk = (0, 0);
        self.last_streamed_render_radius = self.render_radius;
        self.current_world_name = Some(meta.name);
        self.last_autosave = Instant::now();

        self.game_screen = GameScreen::Playing;
        self.last_frame = Instant::now();
    }

    /// Guarda `player_state` en el `meta.bin` del mundo activo, si hay
    /// uno cargado (`current_world_name`). Se llama SIEMPRE junto con
    /// `World::save_dirty_chunks()` — mismo caller, mismo momento — para
    /// que la posición nunca quede desincronizada de los chunks: no
    /// tiene sentido guardar el terreno sin guardar dónde estabas parado
    /// en él. Barato (un solo archivo chico), así que no hace falta
    /// gatearlo por "solo si cambió" como sí hace `save_dirty_chunks`
    /// con los chunks.
    fn save_player_state_now(&self) {
        if let Some(name) = &self.current_world_name {
            environment::save_manager::save_player_state(
                name,
                environment::save_manager::PlayerState {
                    feet_x: self.player.feet_position.x,
                    feet_y: self.player.feet_position.y,
                    feet_z: self.player.feet_position.z,
                    yaw: self.camera.yaw,
                    pitch: self.camera.pitch,
                },
            );
        }
    }

    /// Contraparte de `start_world`: descarga el mundo actual de memoria
    /// (mallas de GPU, chunks pendientes de generar/mallear, nombre del
    /// mundo activo) al volver al menú principal desde `ExitGame`. No
    /// toca disco — el guardado ya lo hizo el caller (`save_dirty_chunks`)
    /// antes de llamar a esto. Sin esto, `chunk_meshes` seguía apuntando
    /// al mundo abandonado y el loop de render lo dibujaba de fondo hasta
    /// la próxima vez que se cargara un mundo.
    fn unload_world(&mut self) {
        self.chunk_meshes.clear();
        self.pending_chunks.clear();
        while self.chunk_result_rx.try_recv().is_ok() {}
        self.current_world_name = None;
    }

    /// Arma un `SavedSession` con todo lo necesario para restaurar la
    /// partida actual en otra superficie/dispositivo wgpu (ver comentario
    /// de `SavedSession`). Solo tiene sentido llamarla con un mundo
    /// cargado (`current_world_name.is_some()`) — la llamamos desde
    /// `App::suspended` justo después de chequear eso.
    fn to_session(&self) -> SavedSession {
        SavedSession {
            world_name: self.current_world_name.clone(),
            chunks: self.world.chunks.clone(),
            seed: self.world.seed(),
            save_dir: self.world.save_dir().to_path_buf(),
            camera: self.camera.clone(),
            player: self.player.clone(),
            game_mode: self.game_mode,
            selected_block: self.selected_block,
            // Si justo estábamos en un menú (pausa, ajustes, etc.)
            // cuando Android mandó la app a segundo plano, restauramos
            // en ese mismo menú en vez de forzar `Playing` — pero si
            // estábamos jugando de verdad, entrar de nuevo directo a
            // `Playing` (sin pasar por pausa) sería una sorpresa
            // desagradable después de haber estado en otra app, así que
            // ahí sí forzamos `Pause`.
            game_screen: if self.game_screen == GameScreen::Playing || self.game_screen == GameScreen::Saving {
                // `Saving` es transitorio (2 frames) y no interactivo —
                // restaurarlo tal cual dejaría la app trabada en una
                // pantalla sin salida si Android suspende justo en ese
                // instante minúsculo. `Pause` es la reanudación segura,
                // igual que para `Playing`.
                GameScreen::Pause
            } else {
                self.game_screen
            },
            settings_return: self.settings_return,
            render_radius: self.render_radius,
            show_fps: self.show_fps,
            show_clouds: self.show_clouds,
            show_fog: self.show_fog,
            show_build_info: self.show_build_info,
            show_debug_panel: self.show_debug_panel,
            autosave_interval_secs: self.autosave_interval_secs,
        }
    }

    /// Reconstruye un `State` a partir de un `SavedSession` (ver
    /// `App::resumed`): recrea el dispositivo/superficie wgpu (lo único
    /// que de verdad hacía falta tirar en `suspended()`) y los buffers
    /// de GPU de cada chunk, pero reusa los chunks, la cámara, el
    /// jugador y los ajustes tal cual estaban — no vuelve a generar ni a
    /// leer nada de disco que no hiciera falta.
    async fn resume(window: Arc<winit::window::Window>, session: SavedSession) -> Self {
        let render = engine::render_state::RenderState::new(window).await;

        let mut world = World::new(session.seed, session.save_dir);
        for (&(cx, cz), chunk) in session.chunks.iter() {
            world.insert_loaded_chunk(cx, cz, chunk.clone());
        }

        let coords: Vec<(i32, i32)> = world.chunks.keys().copied().collect();
        let mesh_data: Vec<((i32, i32), environment::mesher::MeshData)> = coords
            .par_iter()
            .map(|&(cx, cz)| {
                let mesh = world.generate_chunk_mesh(cx, cz).unwrap_or(environment::mesher::MeshData {
                    vertices: Vec::new(),
                    indices: Vec::new(),
                });
                ((cx, cz), mesh)
            })
            .collect();

        let mut chunk_meshes: HashMap<(i32, i32), ChunkMesh> = HashMap::new();
        for ((cx, cz), mesh) in mesh_data {
            if mesh.indices.is_empty() {
                continue;
            }
            chunk_meshes.insert((cx, cz), build_chunk_mesh(&render.device, cx, cz, &mesh));
        }

        let chunk_loader = world.loader();
        let (chunk_result_tx, chunk_result_rx) = std::sync::mpsc::channel();

        // Hay que leer esto ANTES del `Self { ... }` de abajo: el campo
        // `player: session.player` mueve `session.player` a la struct
        // (no implementa `Copy`), así que si se calculara adentro del
        // literal, después de ese campo, sería un use-after-move — el
        // orden de los campos de un struct literal se evalúa de arriba
        // a abajo, pero el valor ya movido no vuelve a estar disponible
        // más abajo en el mismo literal.
        let current_player_chunk =
            World::world_pos_to_chunk(session.player.feet_position.x, session.player.feet_position.z);

        Self {
            render,
            start_time: Instant::now(),
            chunk_meshes,
            world,
            camera: session.camera,
            player: session.player,
            game_mode: session.game_mode,
            selected_block: session.selected_block,
            mouse_captured: false,
            last_frame: Instant::now(),
            touch: TouchController::new(),
            fps_frame_count: 0,
            fps_timer: Instant::now(),
            current_fps: 0.0,
            show_fps: session.show_fps,
            show_clouds: session.show_clouds,
            show_fog: session.show_fog,
            show_build_info: session.show_build_info,
            show_debug_panel: session.show_debug_panel,
            debug_copy_flash_until: None,
            rescue_cooldown_until: None,
            rescue_target: None,
            game_screen: session.game_screen,
            settings_return: session.settings_return,
            cursor_pos: (0.0, 0.0),
            current_player_chunk,
            render_radius: session.render_radius,
            last_streamed_render_radius: session.render_radius,
            chunk_loader,
            pending_chunks: std::collections::HashSet::new(),
            chunk_result_tx,
            chunk_result_rx,
            available_worlds: Vec::new(),
            current_world_name: session.world_name,
            name_input: String::new(),
            name_preedit: String::new(),
            pending_delete_index: None,
            autosave_interval_secs: session.autosave_interval_secs,
            last_autosave: Instant::now(),
            saving_started: false,
        }
    }

    /// Cuenta un frame renderizado y, si pasó 1 segundo, recalcula el FPS
    /// promedio y devuelve Some(fps) para que el caller actualice el título
    /// de la ventana. Devuelve None si todavía no pasó el segundo.
    fn tick_fps(&mut self) -> Option<f32> {
        self.fps_frame_count += 1;
        let elapsed = self.fps_timer.elapsed().as_secs_f32();
        if elapsed >= 1.0 {
            self.current_fps = self.fps_frame_count as f32 / elapsed;
            self.fps_frame_count = 0;
            self.fps_timer = Instant::now();
            Some(self.current_fps)
        } else {
            None
        }
    }

    /// Re-genera el mesh de un chunk (o lo borra del mapa si quedó vacío)
    /// y actualiza `chunk_meshes`. Se llama sobre los chunks que
    /// realmente cambiaron tras romper/colocar un bloque, y también sobre
    /// vecinos que necesitan actualizar su culling de borde tras el
    /// streaming (ver `finalize_ready_chunks`) — no todo el mundo.
    fn remesh_chunk(&mut self, cx: i32, cz: i32) {
        match self.world.generate_chunk_mesh(cx, cz) {
            Some(mesh) if !mesh.indices.is_empty() => {
                let gpu_mesh = build_chunk_mesh(&self.render.device, cx, cz, &mesh);
                self.chunk_meshes.insert((cx, cz), gpu_mesh);
            }
            _ => {
                self.chunk_meshes.remove(&(cx, cz));
            }
        }
    }

    /// Lanza un rayo desde la cámara y rompe (click izquierdo) o coloca
    /// (click derecho) un bloque, usando DDA para encontrar el bloque
    /// exacto apuntado (ver environment::world::raycast).
    fn handle_click(&mut self, button: MouseButton) {
        let origin = self.camera.position;
        let direction = self.camera.view_direction();

        let Some(hit) = raycast(&self.world, origin, direction, REACH) else {
            return;
        };

        let dirty = match button {
            MouseButton::Left => {
                let (x, y, z) = hit.block_pos;
                self.world.set_block(x, y, z, BlockType::Air)
            }
            MouseButton::Right => {
                let (x, y, z) = hit.place_pos;
                // Se impide construir un bloque sólido donde está parado
                // el jugador mismo (evita el exploit de encerrarse sin
                // salida colocando bloques encima/delante mirando hacia
                // abajo). Aplica en Supervivencia y Creativo por igual.
                // En Espectador no hay colisión, así que la pregunta ni
                // aplica. Ver el comentario en
                // `GameMode::blocks_self_placement` sobre por qué esto
                // no afecta a futuros bloques con gravedad (arena, agua).
                if self.game_mode.blocks_self_placement() && self.player.occupies_block(x, y, z) {
                    log::info!(
                        "Colocación de bloque bloqueada en ({}, {}, {}): se solapa con el jugador.",
                        x, y, z
                    );
                    return;
                }
                self.world.set_block(x, y, z, self.selected_block)
            }
            _ => return,
        };

        for (cx, cz) in dirty {
            self.remesh_chunk(cx, cz);
        }
    }

    /// Cambia el modo de juego activo, sincronizando cámara/jugador para
    /// que la transición no teletransporte ni haga caer bruscamente a
    /// nadie. Se llama solo desde el panel de ajustes (selector de 3
    /// modos, ver `TouchAction::SetGameMode`) — no hay atajo de teclado
    /// para ciclar modos en el juego, a propósito: cambiar de modo es
    /// una decisión de "pausa y pienso qué quiero hacer", no algo para
    /// alternar sin querer en medio del juego.
    fn set_game_mode(&mut self, mode: GameMode) {
        if self.game_mode == mode {
            return;
        }
        // Tanto Survival como Creative usan `Player` (con o sin
        // gravedad) y necesitan que `feet_position` arranque en la
        // posición actual de la cámara — si no, al entrar a Survival
        // desde Spectator el jugador "saltaría" a donde estaba parado
        // la última vez que usó Player, potencialmente muy lejos de
        // donde está la cámara ahora.
        if matches!(mode, GameMode::Survival | GameMode::Creative) {
            self.player.feet_position = Player::feet_from_eye_position(self.camera.position);
            self.player.velocity = glam::Vec3::ZERO;
        }
        self.game_mode = mode;
        log::info!("Modo de juego cambiado a {}", mode.label());
    }

    /// Aplica un `TouchAction` disparado desde cualquier pantalla de menú
    /// (MainMenu, Pause, GameMode, Settings, SettingsMore). Punto de
    /// entrada único tanto para eventos táctiles (Android) como para
    /// clicks de mouse (desktop, ver `WindowEvent::MouseInput`) — así las
    /// dos entradas de input nunca pueden desincronizarse en qué hace
    /// cada botón.
    fn apply_menu_action(&mut self, action: TouchAction, event_loop: &ActiveEventLoop, window: &Window) {
        // Activamos el IME nativo (teclado del sistema) solo mientras
        // `GameScreen::NameWorld` está en pantalla, y lo apagamos apenas
        // se sale — así no queda un teclado fantasma habilitado
        // mientras se juega. Comparamos antes/después en vez de mirar
        // la acción en sí porque hay más de una forma de entrar/salir de
        // `NameWorld` (`OpenNameWorld`, `ConfirmNameWorld`, `Back`).
        let was_nameworld = self.game_screen == GameScreen::NameWorld;
        self.apply_menu_action_inner(action, event_loop);
        let is_nameworld = self.game_screen == GameScreen::NameWorld;
        if was_nameworld != is_nameworld {
            window.set_ime_allowed(is_nameworld);
            if is_nameworld {
                // Ancla el popup del IME (candidatos/predicción, o el
                // recuadro que algunos teclados dibujan alrededor del
                // campo activo) cerca del cuadro de texto en vez de en
                // la esquina de la pantalla por defecto.
                let field = TouchController::rect_nameworld_textfield(self.render.size);
                window.set_ime_cursor_area(
                    winit::dpi::PhysicalPosition::new(field.0 as i32, field.1 as i32),
                    winit::dpi::PhysicalSize::new(field.2.max(1.0) as u32, field.3.max(1.0) as u32),
                );
            } else {
                // Por las dudas: si algún día se agrega una forma de
                // salir de `NameWorld` que no pase por `Back`/
                // `ConfirmNameWorld` (los dos únicos lugares que hoy
                // limpian `name_preedit` a mano), no queda preedit
                // colgado la próxima vez que se abra esta pantalla.
                self.name_preedit.clear();
            }
            // Al cerrarse el teclado del sistema, Android suele volver a
            // mostrar las barras de estado/navegación (mismo reseteo de
            // flags que ya pasa al recuperar foco, ver el comentario en
            // `WindowEvent::Focused(true)` más abajo) — y a diferencia de
            // ese caso, cerrar el teclado NO siempre dispara un ciclo de
            // `Focused`, así que sin este llamado el juego podía quedar
            // con las barras del sistema visibles después de escribir el
            // nombre de un mundo.
            #[cfg(target_os = "android")]
            if was_nameworld && !is_nameworld {
                immersive::apply_immersive_fullscreen();
            }
        }
    }

    fn apply_menu_action_inner(&mut self, action: TouchAction, event_loop: &ActiveEventLoop) {
        match action {
            TouchAction::Place => self.handle_click(MouseButton::Right),
            TouchAction::SelectBlock(n) => {
                self.selected_block = match n {
                    1 => BlockType::Grass,
                    2 => BlockType::Dirt,
                    3 => BlockType::Stone,
                    4 => BlockType::Wood,
                    _ => BlockType::Leaves,
                };
            }
            TouchAction::PlayGame => {
                // "JUGAR" ya no entra directo: abre la lista de mundos
                // guardados (refrescada desde disco acá mismo, por si se
                // creó/borró algo mundo desde la última vez que se miró).
                self.available_worlds = environment::save_manager::list_worlds();
                self.game_screen = GameScreen::WorldList;
            }
            TouchAction::SelectWorld(index) => {
                if let Some(meta) = self.available_worlds.get(index).cloned() {
                    self.start_world(meta);
                }
            }
            TouchAction::OpenNameWorld => {
                // Prellenamos con un nombre por defecto: el jugador
                // puede borrarlo entero y escribir el suyo, o dejarlo
                // tal cual y tocar "CREAR MUNDO" directo.
                self.name_input = environment::save_manager::next_free_name();
                self.name_preedit.clear();
                self.game_screen = GameScreen::NameWorld;
            }
            TouchAction::KeyboardChar(c) => {
                if self.name_input.chars().count() < TouchController::NAME_INPUT_MAX_CHARS {
                    self.name_input.push(c);
                }
            }
            TouchAction::KeyboardBackspace => {
                self.name_input.pop();
            }
            TouchAction::ConfirmNameWorld => {
                let trimmed = self.name_input.trim();
                let name = if trimmed.is_empty() {
                    environment::save_manager::next_free_name()
                } else {
                    trimmed.to_string()
                };
                let meta = environment::save_manager::create_world(&name);
                self.name_input.clear();
                self.name_preedit.clear();
                self.start_world(meta);
            }
            TouchAction::RequestDeleteWorld(index) => {
                self.pending_delete_index = Some(index);
                self.game_screen = GameScreen::ConfirmDeleteWorld;
            }
            TouchAction::ConfirmDeleteWorld => {
                if let Some(index) = self.pending_delete_index.take() {
                    if let Some(meta) = self.available_worlds.get(index) {
                        environment::save_manager::delete_world(&meta.name);
                    }
                    self.available_worlds = environment::save_manager::list_worlds();
                }
                self.game_screen = GameScreen::WorldList;
            }
            TouchAction::OpenPause => {
                self.game_screen = GameScreen::Pause;
            }
            TouchAction::OpenGameModeScreen => {
                self.game_screen = GameScreen::GameMode;
            }
            TouchAction::OpenSettingsScreen => {
                // Recordamos desde dónde se abrió (MainMenu o Pause) para
                // que "< VOLVER" sepa a cuál de los dos regresar.
                self.settings_return = self.game_screen;
                self.game_screen = GameScreen::Settings;
            }
            TouchAction::OpenSettingsMore => {
                self.game_screen = GameScreen::SettingsMore;
            }
            TouchAction::ExitGame => {
                // "SALIR" del menú de pausa: NO cierra la app, guarda el
                // mundo y vuelve al menú principal (ver `ExitApp` para
                // cerrar la app de verdad, desde MainMenu). El guardado
                // en sí NO pasa acá: es síncrono y puede tardar (mundo
                // grande, muchos chunks sucios), así que lo diferimos al
                // frame siguiente para que antes se llegue a pintar la
                // pantalla "GUARDANDO..." (ver `update()` y
                // `saving_started`) — si guardáramos ahora mismo, el
                // frame se bloquearía sin que el jugador vea ningún
                // feedback antes.
                self.saving_started = false;
                self.game_screen = GameScreen::Saving;
            }
            TouchAction::ExitApp => {
                event_loop.exit();
            }
            TouchAction::Back => {
                // Sube un nivel en la jerarquía de menús (ver comentario
                // de `GameScreen`). Desde Pause, "Volver" reanuda el
                // juego. Desde Settings, vuelve a MainMenu o a Pause
                // según de dónde se haya abierto (`settings_return`).
                // Salir de NameWorld o ConfirmDeleteWorld sin confirmar
                // no debe dejar basura de estado atrás: se descarta lo
                // que se había escrito / qué mundo se iba a borrar.
                self.name_input.clear();
                self.name_preedit.clear();
                self.pending_delete_index = None;
                self.game_screen = match self.game_screen {
                    GameScreen::GameMode => GameScreen::Pause,
                    GameScreen::Settings => self.settings_return,
                    GameScreen::SettingsMore => GameScreen::Settings,
                    GameScreen::Pause => GameScreen::Playing,
                    GameScreen::WorldList => GameScreen::MainMenu,
                    GameScreen::NameWorld | GameScreen::ConfirmDeleteWorld => GameScreen::WorldList,
                    // `Saving` no tiene botón "< VOLVER" (no es
                    // interactiva), pero cubrimos el brazo para que el
                    // match siga siendo exhaustivo — no debería
                    // dispararse nunca en la práctica.
                    GameScreen::MainMenu | GameScreen::Playing | GameScreen::Saving => self.game_screen,
                };
                if self.game_screen == GameScreen::Playing {
                    // Reiniciamos last_frame para no acumular
                    // un dt gigante después de la pausa.
                    self.last_frame = std::time::Instant::now();
                }
            }
            TouchAction::ToggleFps => {
                self.show_fps = !self.show_fps;
            }
            TouchAction::SetGameMode(index) => {
                self.set_game_mode(GameMode::from_index(index));
            }
            TouchAction::DecreaseRenderRadius => {
                self.render_radius = (self.render_radius - 1).max(MIN_RENDER_RADIUS);
            }
            TouchAction::IncreaseRenderRadius => {
                self.render_radius = (self.render_radius + 1).min(MAX_RENDER_RADIUS);
            }
            TouchAction::ToggleClouds => {
                self.show_clouds = !self.show_clouds;
            }
            TouchAction::ToggleFog => {
                self.show_fog = !self.show_fog;
            }
            TouchAction::ToggleBuildInfo => {
                self.show_build_info = !self.show_build_info;
            }
            TouchAction::ToggleDebugPanel => {
                self.show_debug_panel = !self.show_debug_panel;
            }
            TouchAction::CycleAutosaveInterval => {
                let current_idx = AUTOSAVE_OPTIONS_SECS
                    .iter()
                    .position(|&s| s == self.autosave_interval_secs)
                    .unwrap_or(0);
                let next_idx = (current_idx + 1) % AUTOSAVE_OPTIONS_SECS.len();
                self.autosave_interval_secs = AUTOSAVE_OPTIONS_SECS[next_idx];
            }
            TouchAction::CopyDebugSnapshot => {
                self.copy_debug_snapshot();
            }
        }
    }

    /// Auto-rescate (paso 5, solo se llama cuando `game_mode.auto_rescue()`
    /// es true, hoy solo Creativo): si el jugador está atrapado dentro de
    /// un sólido, arranca un arrastre gradual hacia la celda de aire
    /// libre más cercana (ver `RESCUE_SPEED`/`step_rescue_drag`) en vez
    /// de teletransportarlo de golpe — se siente menos como un glitch.
    /// Tiene un pequeño cooldown para no intentar rescatar en cada frame
    /// mientras la física todavía se está asentando justo después de un
    /// rescate (por ejemplo, si el punto elegido resultó estar pegado a
    /// otro sólido y por redondeo el jugador vuelve a tocarlo un par de
    /// frames después).
    fn maybe_rescue_player(&mut self) {
        // Ya hay un arrastre en curso: no hace falta buscar de nuevo,
        // `step_rescue_drag` se encarga de terminarlo.
        if self.rescue_target.is_some() {
            return;
        }
        if let Some(until) = self.rescue_cooldown_until {
            if Instant::now() < until {
                return;
            }
        }
        if !self.player.is_trapped(&self.world) {
            return;
        }

        // Radio de búsqueda: 8 bloques alcanza para salir de la mayoría
        // de los casos reales (una pared fina, un techo, terreno
        // generado encima) sin que la búsqueda se vuelva costosa. Si no
        // encuentra nada en ese radio (por ejemplo, enterrado en pleno
        // centro de una montaña sólida), no hacemos nada más que
        // avisar por log — mejor eso que arrastrar al jugador a
        // cualquier lado muy lejos de donde estaba.
        match self.player.find_nearest_free_position(&self.world, 8) {
            Some(free_pos) => {
                log::info!(
                    "Auto-rescate: jugador atrapado en ({:.1}, {:.1}, {:.1}), arrastrando hacia ({:.1}, {:.1}, {:.1}).",
                    self.player.feet_position.x,
                    self.player.feet_position.y,
                    self.player.feet_position.z,
                    free_pos.x,
                    free_pos.y,
                    free_pos.z,
                );
                self.rescue_target = Some(free_pos);
                self.player.velocity = glam::Vec3::ZERO;
                // El cooldown se aplica recién cuando el arrastre
                // termina (`step_rescue_drag`), no acá — mientras
                // `rescue_target` sea `Some` esta función ya vuelve
                // apenas entra, así que no hace falta cooldown todavía.
            }
            None => {
                log::warn!(
                    "Auto-rescate: jugador atrapado en ({:.1}, {:.1}, {:.1}), pero no se encontró aire libre en 8 bloques a la redonda.",
                    self.player.feet_position.x,
                    self.player.feet_position.y,
                    self.player.feet_position.z,
                );
                // Igual aplicamos el cooldown: si no hay salida cercana,
                // no tiene sentido volver a intentarlo cada frame — con
                // el mismo mundo alrededor, el resultado va a ser el
                // mismo hasta que el jugador o el mundo cambien algo.
                self.rescue_cooldown_until = Some(Instant::now() + Duration::from_millis(500));
            }
        }
    }

    /// Avanza un frame del arrastre gradual hacia `target` (la celda de
    /// aire libre elegida por `maybe_rescue_player`), a velocidad
    /// constante `RESCUE_SPEED` bloques/segundo — así el rescate se
    /// siente como que "tira" del jugador en vez de teletransportarlo.
    /// Mientras el arrastre está activo, reemplaza por completo al
    /// movimiento normal del modo de juego (ver el `match self.game_mode`
    /// en `update`), para que la física normal no compita contra el
    /// arrastre frame a frame.
    fn step_rescue_drag(&mut self, target: glam::Vec3, dt: f32) {
        const RESCUE_SPEED: f32 = 1.25; // bloques por segundo (x2.5 respecto al original)

        let current = self.player.feet_position;
        let delta = target - current;
        let dist = delta.length();
        let max_step = RESCUE_SPEED * dt;

        if dist <= max_step || dist < 0.001 {
            // Llegamos: dejamos al jugador exactamente en el punto
            // elegido y cerramos el arrastre.
            self.player.feet_position = target;
            self.player.velocity = glam::Vec3::ZERO;
            self.rescue_target = None;
            self.rescue_cooldown_until = Some(Instant::now() + Duration::from_millis(500));
            log::info!(
                "Auto-rescate: jugador llegó a la posición segura ({:.1}, {:.1}, {:.1}).",
                target.x, target.y, target.z
            );
        } else {
            self.player.feet_position = current + delta / dist * max_step;
        }

        self.camera.position = self.player.eye_position();
    }

    /// Arma los datos del panel de debug (F3) a partir del estado actual
    /// del juego. Usado tanto para dibujar el panel (`render`) como para
    /// armar el texto que copia el botón "COPIAR" (`copy_debug_snapshot`)
    /// — un único lugar que arma estos datos evita que ambos se
    /// desincronicen si en el futuro se agrega/cambia algún campo.
    fn build_debug_panel_data(&self) -> ui_overlay::DebugPanelData {
        let pos = self.camera.position;
        let looking_at = raycast(&self.world, self.camera.position, self.camera.view_direction(), REACH)
            .map(|hit| (hit.block_type.label().to_string(), hit.block_pos));
        let game_mode_label = self.game_mode.label();
        let log_file_hint = match platform::file_logger::log_file_path() {
            Some(path) => path.display().to_string(),
            None => "(no disponible)".to_string(),
        };

        ui_overlay::DebugPanelData {
            player_pos: (pos.x, pos.y, pos.z),
            chunk_pos: self.current_player_chunk,
            looking_at,
            fps: self.current_fps,
            game_mode_label,
            log_file_hint,
        }
    }

    /// Copia un snapshot en texto plano de todo el panel de debug al
    /// portapapeles del sistema (reutiliza el mismo mecanismo que ya
    /// usaba la pantalla de crash — arboard en desktop, JNI en Android —
    /// ver platform/crash.rs::copy_text_to_clipboard). Se dispara desde
    /// el botón "COPIAR" del panel (click/touch, ver `handle_click` /
    /// `TouchAction::CopyDebugSnapshot`).
    fn copy_debug_snapshot(&mut self) {
        let data = self.build_debug_panel_data();
        let looking_at_text = match &data.looking_at {
            Some((label, (x, y, z))) => format!("{label} en ({x}, {y}, {z})"),
            None => "nada al alcance".to_string(),
        };
        let snapshot = format!(
            "=== Voxel Engine: snapshot de debug ===\n\
             Posición: ({:.2}, {:.2}, {:.2})\n\
             Chunk: ({}, {})\n\
             Mirando: {}\n\
             Modo: {}\n\
             FPS: {}\n\
             Archivo de log: {}\n",
            data.player_pos.0,
            data.player_pos.1,
            data.player_pos.2,
            data.chunk_pos.0,
            data.chunk_pos.1,
            looking_at_text,
            data.game_mode_label,
            data.fps.round() as i32,
            data.log_file_hint,
        );
        let copied = crash::copy_text_to_clipboard(&snapshot);
        if copied {
            self.debug_copy_flash_until = Some(Instant::now() + Duration::from_millis(1200));
        } else {
            log::warn!("No se pudo copiar el snapshot de debug al portapapeles.");
        }
    }

    /// Chequea si el jugador cruzó a un chunk distinto desde la última vez
    /// y, si es así, descarga los chunks que quedaron fuera del radio y
    /// **dispara** (no espera) la carga de los que entraron nuevos: cada
    /// uno se manda a `rayon::spawn` para generarse/mallearse en un hilo
    /// de fondo, y su resultado se recoge más tarde, de a poco, en
    /// `finalize_ready_chunks`. Esta función en sí misma es barata y
    /// nunca bloquea esperando a que un chunk termine.
    fn update_chunk_streaming(&mut self) {
        // Usamos la posición de la cámara (no la del jugador) porque en
        // modo Vuelo el jugador físico no se actualiza, y igual queremos
        // que el streaming siga a la cámara en ambos modos.
        let pos = self.camera.position;
        let player_chunk = World::world_pos_to_chunk(pos.x, pos.z);

        let radius_changed = self.render_radius != self.last_streamed_render_radius;
        if player_chunk == self.current_player_chunk && !radius_changed && !self.chunk_meshes.is_empty() {
            return;
        }
        self.current_player_chunk = player_chunk;
        self.last_streamed_render_radius = self.render_radius;

        let (pcx, pcz) = player_chunk;
        let radius = self.render_radius;
        let wanted: std::collections::HashSet<(i32, i32)> = (-radius..=radius)
            .flat_map(|dx| (-radius..=radius).map(move |dz| (pcx + dx, pcz + dz)))
            .collect();

        // Descargar los que quedaron fuera del radio. Esto es barato (solo
        // sacar del HashMap / opcionalmente escribir a disco si tenían
        // cambios sin guardar) así que se queda síncrono.
        let to_unload: Vec<(i32, i32)> = self
            .world
            .chunks
            .keys()
            .copied()
            .filter(|c| !wanted.contains(c))
            .collect();
        for (cx, cz) in to_unload {
            self.world.unload_chunk(cx, cz);
            self.chunk_meshes.remove(&(cx, cz));
        }
        // Si un chunk que quedó fuera del radio todavía estaba en vuelo
        // (pedido pero no recibido), lo dejamos que termine igual: cuando
        // llegue por el channel, `finalize_ready_chunks` lo va a insertar
        // aunque ya no se necesite. Se descarga solo en el próximo cruce
        // de chunk. Es más simple que cancelar la tarea, y el costo es
        // insignificante (un chunk de más, generado una vez).

        // Pedir los que entraron nuevos al radio y todavía no están ni
        // cargados ni pedidos.
        let to_request: Vec<(i32, i32)> = wanted
            .into_iter()
            .filter(|c| !self.world.chunks.contains_key(c) && !self.pending_chunks.contains(c))
            .collect();

        for (cx, cz) in to_request {
            self.pending_chunks.insert((cx, cz));
            let loader = self.chunk_loader.clone();
            let tx = self.chunk_result_tx.clone();

            // Fase 5: le mandamos al hilo de fondo una foto de los vecinos
            // que YA están cargados en este instante (clonar un Chunk es
            // barato: ~16 KB, nada comparado con generar ruido Perlin o
            // leer un archivo). Así el mesh que vuelve por el channel ya
            // viene con culling correcto contra esos vecinos, en vez de
            // tratarlos como aire y tener que corregir todo después. Si
            // un vecino llega a cargarse recién DESPUÉS de esta foto, el
            // paso de abajo en `finalize_ready_chunks` se encarga de
            // corregirlo re-mallando en el momento en que ese vecino
            // exista de verdad.
            let neighbor_snapshots: Vec<((i32, i32), Chunk)> = World::direct_neighbors(cx, cz)
                .into_iter()
                .filter_map(|coord| self.world.chunks.get(&coord).map(|c| (coord, c.clone())))
                .collect();

            rayon::spawn(move || {
                let chunk = loader.load_or_generate(cx, cz);

                let find = |dx: i32, dz: i32| {
                    neighbor_snapshots
                        .iter()
                        .find(|(coord, _)| *coord == (cx + dx, cz + dz))
                        .map(|(_, c)| c)
                };
                let neighborhood = environment::mesher::ChunkNeighborhood {
                    center: &chunk,
                    neg_x: find(-1, 0),
                    pos_x: find(1, 0),
                    neg_z: find(0, -1),
                    pos_z: find(0, 1),
                };
                let mesh = environment::mesher::generate_mesh(&neighborhood);

                // Si el receiver ya no existe (la ventana se cerró y State
                // se destruyó) el send simplemente falla; no hay nada que
                // limpiar del lado del hilo de fondo.
                let _ = tx.send(((cx, cz), chunk, mesh));
            });
        }
    }

    /// Recoge, como máximo `MAX_FINALIZED_CHUNKS_PER_FRAME` por llamada,
    /// los chunks que un hilo de fondo ya terminó de generar y mallear, y
    /// recién acá —en el hilo principal, único lugar donde es válido
    /// usar `self.render.device`— sube esa malla a la GPU. Se llama todos los
    /// frames desde `update()`, no solo cuando el jugador cruza de chunk,
    /// para ir vaciando el channel de a poco en vez de todo de golpe.
    fn finalize_ready_chunks(&mut self) {
        for _ in 0..MAX_FINALIZED_CHUNKS_PER_FRAME {
            let Ok((coord, chunk, mesh)) = self.chunk_result_rx.try_recv() else {
                break;
            };
            self.pending_chunks.remove(&coord);

            if !mesh.indices.is_empty() {
                let (cx, cz) = coord;
                let gpu_mesh = build_chunk_mesh(&self.render.device, cx, cz, &mesh);
                self.chunk_meshes.insert(coord, gpu_mesh);
            }
            self.world.insert_loaded_chunk(coord.0, coord.1, chunk);

            // Fase 5: el mesh que acabamos de subir ya tiene culling
            // correcto contra los vecinos que existían en el momento en
            // que se pidió (ver `update_chunk_streaming`). Pero si algún
            // vecino YA estaba cargado y mallado ANTES de eso, ese vecino
            // todavía tiene su cara del borde dibujada de más (la mandó a
            // mallear cuando este chunk nuevo todavía no existía). Ahora
            // que los dos están en `world.chunks`, lo corregimos
            // re-mallando esos vecinos ya cargados — síncrono, pero
            // barato: como mucho 4 chunks, cada uno un greedy meshing
            // normal, nada distinto en costo a un romper/colocar bloque.
            for (ncx, ncz) in World::direct_neighbors(coord.0, coord.1) {
                let already_loaded = self.world.chunks.contains_key(&(ncx, ncz));
                let in_flight = self.pending_chunks.contains(&(ncx, ncz));
                if already_loaded && !in_flight {
                    // Ese vecino ya estaba en memoria antes de que este
                    // chunk llegara, así que su mesh puede tener una cara
                    // de más en el borde compartido. Si en cambio está en
                    // vuelo (in_flight), cuando llegue va a usar una foto
                    // de vecinos que ya incluye a este chunk, así que no
                    // hace falta tocarlo acá.
                    self.remesh_chunk(ncx, ncz);
                }
            }
        }
    }

    /// Pantalla mínima que se muestra después de atrapar un panic: solo
    /// limpia la superficie a rojo oscuro, sin tocar el mundo, las
    /// mallas de chunks, ni ningún otro estado que pueda haber quedado a
    /// medio actualizar cuando pasó el panic. A propósito no dibuja nada
    /// del juego — la idea es no volver a ejecutar la lógica que
    /// crasheó.
    ///
    /// `flash`: true durante un instante corto justo después de copiar
    /// el log al portapapeles (tecla C en desktop, toque en Android).
    /// Como el engine no tiene pipeline de texto, es la única forma que
    /// tenemos hoy de confirmar "sí, se copió" sin depender de un log
    /// que el usuario no está mirando en ese momento.
    fn render_crash_screen(&mut self, flash: bool) -> Result<(), wgpu::SurfaceError> {
        let output = self.render.surface.get_current_texture()?;
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .render
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("crash_screen_encoder"),
            });

        let color = if flash {
            // Verde: "listo, se copió". Nada más que un color distinto
            // por un rato corto — ver comentario de `flash` arriba.
            wgpu::Color {
                r: 0.05,
                g: 0.45,
                b: 0.10,
                a: 1.0,
            }
        } else {
            wgpu::Color {
                r: 0.45,
                g: 0.02,
                b: 0.02,
                a: 1.0,
            }
        };

        {
            let _render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("crash_screen_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }

        self.render.queue.submit(std::iter::once(encoder.finish()));
        output.present();

        Ok(())
    }

    fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        self.render.resize(new_size);
    }

    fn update(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32();
        self.last_frame = now;

        // Pantalla de "GUARDANDO...": primer frame, solo dejamos pasar
        // (así `render()` de este mismo ciclo llega a pintar la
        // pantalla antes del guardado, que bloquea). Segundo frame en
        // adelante: recién ahí guardamos de verdad y volvemos al menú
        // principal.
        if self.game_screen == GameScreen::Saving {
            if !self.saving_started {
                self.saving_started = true;
                return;
            }
            let saved = self.world.save_dirty_chunks();
            self.save_player_state_now();
            if saved > 0 {
                log::info!("Guardados {} chunks modificados al volver al menú principal.", saved);
            }
            // FIX: sin esto, `chunk_meshes` seguía teniendo las mallas
            // del mundo que se acaba de abandonar, y el loop de render
            // las dibuja sin mirar `game_screen` — por eso se veía el
            // mundo anterior de fondo, un instante, al volver al menú
            // principal (y hasta la próxima vez que se entrara a un
            // mundo, ya que `start_world` recién ahí las reemplaza).
            self.unload_world();
            self.game_screen = GameScreen::MainMenu;
            return;
        }

        // En cualquier pantalla de menú (pausa, modo de juego, ajustes,
        // ajustes adicionales): el juego queda pausado. Solo
        // actualizamos el tiempo para no acumular un dt enorme al
        // volver al juego.
        if self.game_screen != GameScreen::Playing {
            return;
        }

        // Volcamos el estado del joystick/botones táctiles a la cámara
        // antes de moverla, con la misma interfaz que usan las teclas.
        self.camera.set_touch_move_axis(self.touch.move_axis());
        self.camera.set_touch_jump(self.touch.jump_held());
        self.camera.set_touch_down(self.touch.crouch_held());
        let (look_dx, look_dy) = self.touch.take_look_delta();
        if look_dx != 0.0 || look_dy != 0.0 {
            self.camera.process_touch_look(look_dx, look_dy);
        }
        // Estilo Minecraft: mantener el dedo apoyado en la zona de mirar
        // (sin soltarlo) rompe el bloque apuntado en repetición, sin
        // necesidad de un botón ROMPER separado. Ver `TouchController::
        // poll_hold_break` y el comentario al principio de touch.rs.
        if self.touch.poll_hold_break() {
            self.handle_click(MouseButton::Left);
        }

        // Si hay un auto-rescate en curso, el arrastre gradual
        // reemplaza por completo al movimiento normal de este frame —
        // así no compite frame a frame contra la física/input del
        // jugador mientras lo estamos "tirando" hacia la salida.
        if let Some(target) = self.rescue_target {
            self.step_rescue_drag(target, dt);
        } else {
            match self.game_mode {
                GameMode::Survival => {
                    if self.camera.wants_jump() {
                        self.player.jump();
                    }
                    // Agacharse (botón táctil de "bajar" o Shift izquierdo
                    // en escritorio, ver Camera::wants_crouch) reduce la
                    // velocidad de caminata, como el sneak de Minecraft.
                    const CROUCH_SPEED_FACTOR: f32 = 0.3;
                    let speed = if self.camera.wants_crouch() {
                        4.5 * CROUCH_SPEED_FACTOR
                    } else {
                        4.5
                    };
                    let horizontal = self.camera.horizontal_move_vector(speed);
                    self.player.update(&self.world, horizontal, dt, self.camera.wants_crouch());
                    self.camera.position = self.player.eye_position();
                }
                GameMode::Creative => {
                    // Vuelo con colisión: no atraviesa bloques, pero sin
                    // gravedad ni salto — subir/bajar es directo con
                    // Espacio/Shift, o con los botones táctiles de salto/
                    // agachar (ver Camera::free_move_vector).
                    let free_move = self.camera.free_move_vector(8.0);
                    self.player.fly_update(&self.world, free_move, dt);
                    self.camera.position = self.player.eye_position();
                }
                GameMode::Spectator => {
                    // Noclip: la cámara se mueve directo, sin pasar por
                    // Player ni chequear colisión contra el mundo.
                    self.camera.update(dt);
                }
            }

            // Auto-rescate (solo Creativo, ver GameMode::auto_rescue): si
            // el jugador quedó atrapado dentro de un sólido — por
            // streaming de chunks, un bug, o al cargar un guardado viejo
            // de antes del bloqueo de autoconstrucción — arranca un
            // arrastre gradual hacia la celda de aire libre más cercana
            // (ver `step_rescue_drag`, arriba). En Supervivencia esto NO
            // se ejecuta a propósito: como en Minecraft real, si te
            // encierras tienes que cavar para salir. En Espectador no
            // aplica porque sin colisión nunca se puede quedar
            // "atrapado".
            if self.game_mode.auto_rescue() {
                self.maybe_rescue_player();
            }
        }

        // Autoguardado periódico: solo mientras se está jugando de
        // verdad (estamos dentro del bloque gateado por `Playing` más
        // arriba). Igual que el guardado manual (F5) o al salir al menú,
        // solo re-escribe los chunks modificados desde el último
        // guardado (ver `World::save_dirty_chunks`), así que en la
        // mayoría de los frames donde toca autoguardar pero no se
        // rompió/colocó nada, es prácticamente gratis.
        if self.last_autosave.elapsed().as_secs() >= self.autosave_interval_secs as u64 {
            self.last_autosave = Instant::now();
            let saved = self.world.save_dirty_chunks();
            self.save_player_state_now();
            if saved > 0 {
                log::info!("Autoguardado: {} chunks escritos a disco.", saved);
            }
        }

        let aspect = self.render.config.width as f32 / self.render.config.height.max(1) as f32;
        let view_proj = projection_matrix(aspect) * self.camera.view_matrix();

        // Distancia de niebla en bloques (no en chunks): `render_radius`
        // son chunks, así que la pasamos a bloques con CHUNK_SIZE_X para
        // que `fog_end` caiga justo en (o un poco antes de) el borde
        // donde los chunks dejan de cargarse. `fog_start` es un 65% de
        // eso, para que la transición sea gradual y no un muro de niebla.
        // Como es un cálculo relativo al radio actual, funciona igual de
        // bien con radio 1 (niebla muy cerca, tapa el borde del mundo
        // cargado) que con radio 128 (niebla lejos, solo un detalle de
        // atmósfera en el horizonte).
        //
        // Si la niebla está apagada (fila "NIEBLA" en configuración),
        // mandamos un umbral centinela bien por encima de cualquier
        // distancia real del mundo, para que `fog_factor` en los
        // shaders (mezcla hacia SKY_COLOR) quede siempre en 0 sin tener
        // que tocar shader.wgsl/clouds_shader.wgsl con un flag aparte.
        // Ojo: esto también apaga el desvanecido del borde de la capa de
        // nubes (ver clouds_shader.wgsl), que reusa el mismo cálculo —
        // es la contrapartida esperada de apagar la niebla "de verdad".
        let (fog_start, fog_end) = if self.show_fog {
            let fog_end = (self.render_radius * CHUNK_SIZE_X as i32) as f32;
            (fog_end * FOG_START_FRACTION, fog_end)
        } else {
            const FOG_DISABLED_SENTINEL: f32 = 1.0e6;
            (FOG_DISABLED_SENTINEL, FOG_DISABLED_SENTINEL)
        };

        let uniforms = Uniforms {
            view_proj: view_proj.to_cols_array_2d(),
            camera_pos: self.camera.position.to_array(),
            fog_start,
            fog_color: SKY_COLOR,
            fog_end,
            time: [self.start_time.elapsed().as_secs_f32(), 0.0, 0.0, 0.0],
        };
        self.render.queue
            .write_buffer(&self.render.uniform_buffer, 0, bytemuck::cast_slice(&[uniforms]));

        self.update_chunk_streaming();
        self.finalize_ready_chunks();
    }

    fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
        let output = self.render.surface.get_current_texture()?;
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .render
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("render_encoder"),
            });

        // Geometría del overlay 2D de este frame.
        // En cualquier pantalla de menú: mostramos esa pantalla
        // fullscreen encima del mundo congelado. En juego: mira +
        // controles.
        let ui_vertices = match self.game_screen {
            GameScreen::MainMenu => ui_overlay::build_main_menu_screen(self.render.size),
            GameScreen::WorldList => {
                let names: Vec<String> = self.available_worlds.iter().map(|w| w.name.clone()).collect();
                ui_overlay::build_worldlist_screen(self.render.size, &names)
            }
            GameScreen::NameWorld => ui_overlay::build_nameworld_screen(self.render.size, &self.name_input, &self.name_preedit),
            GameScreen::ConfirmDeleteWorld => {
                let name = self
                    .pending_delete_index
                    .and_then(|i| self.available_worlds.get(i))
                    .map(|m| m.name.as_str())
                    .unwrap_or("");
                ui_overlay::build_confirm_delete_screen(self.render.size, name)
            }
            GameScreen::Pause => ui_overlay::build_pause_screen(self.render.size),
            GameScreen::GameMode => {
                ui_overlay::build_gamemode_screen(self.render.size, self.game_mode.index())
            }
            GameScreen::Settings => ui_overlay::build_settings_screen(
                self.render.size,
                self.show_fps,
                self.render_radius,
                self.show_clouds,
                self.show_fog,
            ),
            GameScreen::SettingsMore => ui_overlay::build_settings_more_screen(
                self.render.size,
                self.show_build_info,
                self.show_debug_panel,
                self.autosave_interval_secs,
            ),
            GameScreen::Saving => ui_overlay::build_saving_screen(self.render.size),
            GameScreen::Playing => {
                let mut verts = ui_overlay::build_crosshair(self.render.size);
                #[cfg(target_os = "android")]
                verts.extend(ui_overlay::build_touch_overlay(
                    &self.touch,
                    self.render.size,
                    self.selected_block,
                    self.show_fps,
                ));
                // Contador de FPS en la esquina superior derecha.
                if self.show_fps {
                    verts.extend(ui_overlay::build_fps_counter(self.current_fps, self.render.size));
                }
                // Info de build (etiqueta + plataforma) en la esquina
                // superior izquierda. `VOXEL_BUILD_TAG` se fija en
                // compilación (ver build.rs); por defecto es
                // "voxel-engine-dev" si no se pasó `BUILD_TAG=...`.
                if self.show_build_info {
                    verts.extend(ui_overlay::build_build_info_overlay(
                        self.render.size,
                        env!("VOXEL_BUILD_TAG"),
                    ));
                }
                // Panel de debug (F3): posición, chunk, bloque apuntado,
                // modo, fps, ruta de log. Se dibuja debajo del overlay de
                // build info si ambos están prendidos, para no superponerse.
                if self.show_debug_panel {
                    let y_offset = if self.show_build_info {
                        ui_overlay::build_info_overlay_height()
                    } else {
                        0.0
                    };
                    let copy_flash = self
                        .debug_copy_flash_until
                        .map(|until| Instant::now() < until)
                        .unwrap_or(false);
                    let data = self.build_debug_panel_data();
                    verts.extend(ui_overlay::build_debug_panel(
                        self.render.size,
                        &data,
                        y_offset,
                        copy_flash,
                    ));
                }
                verts
            }
        };
        let ui_vertex_buffer = self
            .render
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("ui_overlay_vertex_buffer"),
                contents: bytemuck::cast_slice(&ui_vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });

        // Contorno del bloque apuntado (Fase 5): mismo raycast DDA que
        // usa `handle_click`, pero solo para saber qué dibujar, sin tocar
        // el mundo. Si no hay nada al alcance (`REACH`), el buffer queda
        // vacío y el draw call de abajo no dibuja nada (0 vértices).
        let highlight_vertices: Vec<highlight::HighlightVertex> = raycast(
            &self.world,
            self.camera.position,
            self.camera.view_direction(),
            REACH,
        )
        .map(|hit| highlight::build_block_outline(hit.block_pos))
        .unwrap_or_default();
        // Ojo: un buffer de 0 bytes no es válido en todos los backends de
        // wgpu (algunos lo rechazan en tiempo de validación). Si no hay
        // nada al alcance, directamente no creamos el buffer ni el draw
        // call de abajo, en vez de mandar un buffer vacío.
        let highlight_vertex_buffer = if highlight_vertices.is_empty() {
            None
        } else {
            Some(
                self.render.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("highlight_vertex_buffer"),
                        contents: bytemuck::cast_slice(&highlight_vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    }),
            )
        };

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("render_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: SKY_COLOR[0] as f64,
                            g: SKY_COLOR[1] as f64,
                            b: SKY_COLOR[2] as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.render.depth_texture_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_pass.set_pipeline(&self.render.render_pipeline);
            render_pass.set_bind_group(0, &self.render.uniform_bind_group, &[]);
            render_pass.set_bind_group(1, &self.render.texture_atlas.bind_group, &[]);

            for mesh in self.chunk_meshes.values() {
                render_pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                render_pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                render_pass.draw_indexed(0..mesh.num_indices, 0, 0..1);
            }

            // Contorno del bloque apuntado: mismo bind group (view_proj)
            // que el terreno, porque dibuja en espacio de mundo real.
            if let Some(buffer) = &highlight_vertex_buffer {
                render_pass.set_pipeline(&self.render.highlight_pipeline);
                render_pass.set_bind_group(0, &self.render.uniform_bind_group, &[]);
                render_pass.set_vertex_buffer(0, buffer.slice(..));
                render_pass.draw(0..highlight_vertices.len() as u32, 0..1);
            }

            // Capa de nubes: se dibuja después del terreno (para que el
            // depth test pueda ocultarlas detrás de una montaña alta) y
            // con alpha blending sobre lo ya pintado. Mismo bind group
            // que el terreno; el shader recentra el quad en la cámara
            // usando `camera_pos` de ese mismo uniform. Si el jugador
            // apagó la fila "NUBES" en configuración, directamente no
            // hacemos el draw call.
            if self.show_clouds {
                render_pass.set_pipeline(&self.render.clouds_pipeline);
                render_pass.set_bind_group(0, &self.render.uniform_bind_group, &[]);
                render_pass.set_vertex_buffer(0, self.render.cloud_vertex_buffer.slice(..));
                render_pass.draw(0..self.render.cloud_num_vertices, 0..1);
            }

            // Overlay 2D (mira + controles táctiles) encima de todo (mismo
            // render pass, mismo formato de depth attachment que
            // render_pipeline por requisito de wgpu, pero con
            // depth_compare: Always y depth_write_enabled: false, así que
            // no testea ni pisa el depth buffer y siempre queda visible).
            render_pass.set_pipeline(&self.render.ui_pipeline);
            render_pass.set_vertex_buffer(0, ui_vertex_buffer.slice(..));
            render_pass.draw(0..ui_vertices.len() as u32, 0..1);
        }

        self.render.queue.submit(std::iter::once(encoder.finish()));
        output.present();

        Ok(())
    }
}


/// Construye los buffers de GPU (vértices + índices) para un chunk,
/// desplazando cada vértice a su posición de mundo según (cx, cz).
/// Se usa tanto en la generación inicial como al re-mallear un chunk
/// modificado por romper/colocar bloques.
fn build_chunk_mesh(
    device: &wgpu::Device,
    cx: i32,
    cz: i32,
    mesh: &environment::mesher::MeshData,
) -> ChunkMesh {
    let offset_x = (cx * CHUNK_SIZE_X as i32) as f32;
    let offset_z = (cz * CHUNK_SIZE_Z as i32) as f32;
    let vertices: Vec<Vertex> = mesh
        .vertices
        .iter()
        .map(|v| Vertex {
            position: [
                v.position[0] + offset_x,
                v.position[1],
                v.position[2] + offset_z,
            ],
            normal: v.normal,
            color: v.color,
            uv: v.uv,
            tile_origin: v.tile_origin,
        })
        .collect();

    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("chunk_vertex_buffer"),
        contents: bytemuck::cast_slice(&vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("chunk_index_buffer"),
        contents: bytemuck::cast_slice(&mesh.indices),
        usage: wgpu::BufferUsages::INDEX,
    });

    ChunkMesh {
        vertex_buffer,
        index_buffer,
        num_indices: mesh.indices.len() as u32,
    }
}

/// Punto de entrada nativo en Android. `cargo-apk` genera el glue que
/// invoca esta función (buscada por su símbolo `android_main`) cuando la
/// Activity arranca, pasándole el `AndroidApp` con el que se construye el
/// event loop específico de Android.
#[cfg(target_os = "android")]
#[no_mangle]
fn android_main(app: AndroidApp) {
    use winit::platform::android::EventLoopBuilderExtAndroid;

    // `external_data_path()` cae bajo Android/data/<paquete>/files —
    // visible sin root con un explorador de archivos o `adb pull`, a
    // diferencia de la carpeta interna. Si por lo que sea no está
    // disponible (algunos emuladores/ROMs raras) probamos con la
    // interna; si ninguna está, tanto el logger de archivo como
    // `crash::install` igual siguen funcionando, solo que sin poder
    // escribir sus archivos (todo queda en logcat).
    let crash_dir = app.external_data_path().or_else(|| app.internal_data_path());

    // Igual que `crash_dir`: `save_manager::saves_root()` por defecto es
    // una ruta relativa ("saves"), que en Android resuelve contra "/"
    // (solo lectura) en vez de una carpeta propia de la app. La pisamos
    // acá con la misma carpeta privada que ya usamos para crash logs,
    // ANTES de que cualquier código toque un mundo (list_worlds,
    // create_world, start_world, etc.) — si no, la carpeta "saves"
    // relativa ya podría haber fallado en crearse.
    if let Some(dir) = crash_dir.clone() {
        environment::save_manager::set_saves_root(dir.join("saves"));
    } else {
        log::warn!(
            "No hay carpeta de datos escribible (external/internal_data_path); \
             los mundos no van a poder guardarse en este dispositivo."
        );
    }

    // Instala el logger (reemplaza a `android_logger::init_once`: sigue
    // mandando todo a logcat exactamente igual que antes, y además
    // escribe cada línea a game_log.txt en crash_dir — ver
    // platform/file_logger.rs). Tiene que instalarse antes que
    // `crash::install` para que el log::error! del panic hook realmente
    // se imprima (antes de instalar un logger, los logs se descartan en
    // silencio).
    platform::file_logger::install(crash_dir.clone());

    // Antes que nada: instalar el panic hook.
    crash::install(crash_dir.clone());
    // Además del panic hook (que solo ve panics de Rust): instalamos
    // manejadores de señal para crashes nativos de verdad (SIGSEGV,
    // SIGABRT, etc. — ver la explicación larga en platform/crash.rs).
    // Sin esto, un crash nativo (el sospechoso más probable de "se
    // cierra sin dejar rastro") no deja ni archivo ni pantalla roja,
    // porque nunca pasa por ninguno de los dos mecanismos de arriba.
    // Antes de instalar los manejadores de señal de ESTA corrida (para
    // no pisar/leer un native_crash.txt a medio escribir por ellos),
    // preguntamos si la corrida ANTERIOR del proceso murió mal: primero
    // vía ApplicationExitInfo (API 30+, lo pregunta el propio Android),
    // si no vía el archivo que dejó nuestro manejador de señal (ver
    // crash.rs). Si hay algo, arrancamos directo en pantalla de crash —
    // sin esto, un crash nativo que mató el proceso quedaba sin ninguna
    // forma de verlo/copiarlo en el dispositivo (el proceso ya no existe
    // para dibujar nada), que es justo el síntoma original.
    let startup_crash = crash::check_previous_run_crash(crash_dir.as_deref());

    crash::install_native_signal_handlers(crash_dir.as_deref());

    let event_loop = match winit::event_loop::EventLoopBuilder::new()
        .with_android_app(app)
        .build()
    {
        Ok(el) => el,
        Err(e) => {
            // Android reutiliza el proceso entre relanzamientos rápidos (es
            // normal del sistema, para abrir apps más rápido sin recargar
            // todo desde cero) pero `winit` solo permite construir un
            // EventLoop una vez por proceso: la segunda vez, en vez de
            // crashear, devuelve este error (típicamente
            // `RecreationAttempt`; ver
            // https://github.com/rust-windowing/winit/issues/3325). No hay
            // forma de "recrear" el loop dentro del mismo proceso, así que
            // matamos el proceso entero a propósito: el próximo toque en el
            // ícono va a arrancar uno completamente nuevo, con la bandera
            // interna de winit ya reseteada.
            log::warn!(
                "No se pudo (re)crear el EventLoop ({:?}); reiniciando el proceso.",
                e
            );
            // OJO: nada de `std::process::exit()` acá. `android_main` corre
            // en un pthread que generó el glue de NativeActivity, no en el
            // hilo principal del proceso Linux. `std::process::exit()` llama
            // a `libc::exit()`, que antes de matar el proceso corre
            // `__cxa_finalize` (destructores globales/TLS de todo lo
            // enlazado, incluido el runtime de Android/JNI que está atado a
            // *este* hilo en particular). Ejecutar esa limpieza desde un
            // hilo que no es el dueño de ese estado es lo que producía el
            // SIGSEGV en __cxa_finalize del log. `libc::_exit()` es el
            // syscall crudo: mata el proceso ya, sin correr un solo
            // destructor, así que es seguro llamarlo desde cualquier hilo.
            unsafe { libc::_exit(0) };
        }
    };

    run(event_loop, startup_crash);

    // Mismo motivo que arriba: usamos _exit en vez de std::process::exit
    // para no disparar la limpieza global desde este hilo.
    unsafe { libc::_exit(0) };
}

/// Punto de entrada en desktop: lo llama `main.rs`. Separado de `run()`
/// solo para poder construir el `EventLoop` de forma distinta según la
/// plataforma (en Android lo arma `android_main` con `with_android_app`).
pub fn run_desktop() {
    // Reemplaza a `env_logger::init()`: sigue mandando todo a stderr
    // exactamente igual que antes (respeta RUST_LOG), y además escribe
    // cada línea a game_log.txt en la carpeta crash_logs (ver
    // platform/file_logger.rs).
    platform::file_logger::install(None);
    // Instalado después del logger para que el log::error! del hook
    // realmente se imprima (antes de inicializar el logger, los logs se
    // descartan en silencio).
    crash::install(None);
    let event_loop = EventLoop::new().unwrap();
    // En desktop no hay chequeo de "corrida anterior" (ver
    // crash::check_previous_run_crash): siempre arranca en modo normal.
    run(event_loop, None);
}

/// Snapshot en RAM de la partida en curso, armado en `suspended()` justo
/// antes de soltar el `State` (y con él, el `wgpu::Surface` que Android
/// ya invalidó al mandar la Activity a segundo plano). Sin esto,
/// `resumed()` reconstruía todo desde `State::new()` — perdiendo el
/// mundo cargado en memoria y volviendo siempre al menú principal, que
/// es justo lo que se pidió evitar. Con esto, en cambio, `resumed()`
/// reconstruye el `State` a partir de estos datos ya en RAM (más rápido
/// que releer todo de disco) y solo recrea lo que de verdad dependía de
/// la superficie destruida: el dispositivo/superficie de wgpu y los
/// buffers de GPU de cada chunk (`ChunkMesh`, que no se pueden guardar
/// acá porque están atados al `wgpu::Device` viejo).
struct SavedSession {
    world_name: Option<String>,
    chunks: HashMap<(i32, i32), environment::chunk::Chunk>,
    seed: u32,
    save_dir: std::path::PathBuf,
    camera: Camera,
    player: Player,
    game_mode: GameMode,
    selected_block: BlockType,
    game_screen: GameScreen,
    settings_return: GameScreen,
    render_radius: i32,
    show_fps: bool,
    show_clouds: bool,
    show_fog: bool,
    show_build_info: bool,
    show_debug_panel: bool,
    autosave_interval_secs: u32,
}

/// Estado de la app para el nuevo modelo de `winit` 0.30+ (`ApplicationHandler`,
/// en reemplazo del closure único que usaba `event_loop.run(...)` en 0.29).
/// Contiene lo mismo que antes vivía como variables capturadas por el closure.
///
/// `App::default()` (ver `run()`, más abajo) arranca la app entera con
/// `window`/`state` en `None` y las banderas de crash apagadas — la
/// ventana y el `State` de wgpu recién se crean en el primer
/// `resumed()` (ver el comentario grande ahí mismo). Todos los campos
/// son `Option`/`bool`, así que el `derive` no necesita que `Window`,
/// `State` ni `SavedSession` implementen `Default` — `Option<T>` ya es
/// `Default` para cualquier `T` (`None`).
#[derive(Default)]
struct App {
    window: Option<Arc<Window>>,
    state: Option<State>,

    // --- Manejo de crashes (ver crash.rs) ---
    // Una vez que `catch_unwind` atrapa un panic en cualquiera de los
    // callbacks de abajo, `crashed` queda en true para siempre (no hay
    // forma segura de "seguir jugando" después de un panic a mitad de
    // frame, así que no lo intentamos) y dejamos de correr toda la
    // lógica normal del juego. `crash_short_message` es el mensaje corto
    // para mostrar en el título de la ventana sin tener que ir a leer el
    // archivo.
    crashed: bool,
    // Se pone en true si la propia pantalla de crash vuelve a panickear
    // (ver el fix en window_event: esa rama ahora también está envuelta
    // en catch_unwind). En ese punto ya no intentamos redibujar nada más
    // — el log del primer crash, que es el que importa, ya se escribió a
    // disco/logcat antes de llegar hasta acá.
    double_crashed: bool,
    crash_short_message: Option<String>,
    // Instante hasta el cual `render_crash_screen` debe mostrar el color
    // de "copiado" en vez del rojo normal (ver comentario de `flash` en
    // `State::render_crash_screen`). `None` = sin flash activo.
    crash_copy_flash_until: Option<Instant>,

    // Snapshot en RAM de la partida, guardado por `suspended()` cuando
    // Android destruye la superficie con una partida cargada, y
    // consumido por el próximo `resumed()` (ver `SavedSession`). `None`
    // en desktop (donde `suspended()` nunca llega) y también en Android
    // mientras no haya ninguna partida en curso (menú principal/lista de
    // mundos) — ahí no hace falta preservar nada, `State::new()` normal
    // alcanza.
    saved_session: Option<SavedSession>,
}

impl App {
    /// Se llama la primera vez que `catch_unwind` atrapa un panic en
    /// alguno de los callbacks de `ApplicationHandler`. Es idempotente
    /// (no repite el diálogo si ya estábamos en pantalla de crash).
    fn mark_crashed(&mut self) {
        if self.crashed {
            return;
        }
        self.crashed = true;

        match crash::last_crash() {
            Some((short, _full, file_path)) => {
                self.crash_short_message = Some(short.clone());
                // Diálogo nativo bloqueante: solo desktop (ver crash.rs).
                // Se llama acá y no en el loop de render porque tiene que
                // pasar una sola vez, no todos los frames.
                #[cfg(not(target_os = "android"))]
                crash::show_crash_dialog(&short, file_path.as_ref());
                #[cfg(target_os = "android")]
                let _ = file_path;
            }
            None => {
                self.crash_short_message =
                    Some("(no se pudo leer el mensaje del panic)".to_string());
            }
        }
    }
}

impl ApplicationHandler for App {
    // En Android, la ventana no se crea hasta el primer `resumed()` (la
    // superficie nativa todavía no existe al arrancar la Activity), así
    // que el `State` de wgpu se inicializa recién ahí — y se reconstruye
    // si Android la destruye y recrea al volver de segundo plano.
    //
    // Todo el cuerpo va envuelto en `catch_unwind`: si `State::new()`
    // panickea (por ejemplo, un adaptador GPU que no cumple algún límite
    // en un dispositivo raro), lo atrapamos acá en vez de dejar que tire
    // abajo el proceso. En ese caso puntual no llegamos a tener ni
    // ventana ni superficie donde dibujar nada, así que la única señal
    // que le queda al usuario es el diálogo nativo (desktop) y el
    // archivo de log — pero al menos la app no se cierra de golpe sin
    // dejar rastro.
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if self.window.is_none() {
                let attrs = Window::default_attributes()
                    .with_title("Voxel Engine - Fase 4")
                    .with_inner_size(winit::dpi::LogicalSize::new(1280, 720));
                let win = Arc::new(event_loop.create_window(attrs).unwrap());
                let new_state = pollster::block_on(State::new(win.clone()));
                self.window = Some(win);
                self.state = Some(new_state);
                #[cfg(target_os = "android")]
                immersive::apply_immersive_fullscreen();
            } else if self.state.is_none() {
                // Android destruyó la superficie al pasar a segundo plano
                // (ver `suspended()` más abajo) y ahora, al volver, nos avisa
                // que hay una superficie nueva. Si `suspended()` alcanzó a
                // dejar un `SavedSession` (había una partida cargada),
                // restauramos desde ahí en vez de arrancar de cero — así el
                // mundo no se pierde ni hay que releerlo entero de disco.
                let win = self.window.as_ref().unwrap();
                self.state = Some(match self.saved_session.take() {
                    Some(session) => pollster::block_on(State::resume(win.clone(), session)),
                    None => pollster::block_on(State::new(win.clone())),
                });
                #[cfg(target_os = "android")]
                immersive::apply_immersive_fullscreen();
            }
        }));

        if panic_result.is_err() {
            self.mark_crashed();
        }
    }

    // Solo llega en Android: la Activity puede pasar a segundo plano y el
    // sistema operativo destruir la superficie nativa en cualquier
    // momento. Antes de soltar el `State` (que retiene el `wgpu::Surface`
    // apuntando a esa superficie, ya inválida) guardamos a disco lo que
    // esté sin guardar y, si había una partida cargada, armamos un
    // `SavedSession` con todo lo que hace falta para restaurarla en
    // `resumed()` sin perder progreso ni volver al menú principal.
    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(mut state) = self.state.take() {
            let saved = state.world.save_dirty_chunks();
            state.save_player_state_now();
            if saved > 0 {
                log::info!("Autoguardado antes de pasar a segundo plano: {} chunks.", saved);
            }
            if state.current_world_name.is_some() {
                self.saved_session = Some(state.to_session());
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let (Some(window), Some(state)) = (self.window.as_ref(), self.state.as_mut()) else {
            return;
        };
        if window_id != window.id() {
            return;
        }

        if self.crashed {
            // Ya atrapamos un panic antes: no volvemos a correr la
            // lógica normal del juego (mundo, input, cámara...), que
            // puede haber quedado en un estado a medio actualizar cuando
            // pasó. Solo dejamos: cerrar la ventana, redibujar la
            // pantalla roja, cambiarle el tamaño si hace falta, y copiar
            // el log (tecla C en desktop, tocar la pantalla en Android).
            //
            // TODO arreglado: esta rama entera va envuelta en catch_unwind
            // igual que la rama normal de abajo. Antes, si la propia
            // pantalla de crash volvía a panickear (por ejemplo porque lo
            // que rompió el primer panic dejó al Device/Surface en un
            // estado inválido), ese segundo panic no tenía red — se
            // propagaba sin filtro y tiraba abajo el proceso entero,
            // anulando todo el propósito de este sistema. Si vuelve a
            // panickear acá adentro, dejamos de intentar redibujar nada
            // (double_crashed) en vez de arriesgar un tercer intento.
            if self.double_crashed {
                return;
            }

            let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                match event {
                    WindowEvent::CloseRequested => event_loop.exit(),
                    WindowEvent::Resized(new_size) => state.resize(new_size),
                    WindowEvent::KeyboardInput {
                        event: key_event, ..
                    } => {
                        if key_event.state == ElementState::Pressed {
                            if let PhysicalKey::Code(winit::keyboard::KeyCode::KeyC) =
                                key_event.physical_key
                            {
                                if crash::copy_last_crash_to_clipboard() {
                                    log::info!("Log de crash copiado al portapapeles.");
                                    self.crash_copy_flash_until =
                                        Some(Instant::now() + Duration::from_millis(700));
                                }
                            }
                        }
                    }
                    // Solo llega en Android (desktop no genera eventos de
                    // touch): tocar en cualquier parte de la pantalla de
                    // crash copia el log. Se dispara en `Started` y no en
                    // cada `Moved`, para que no haga falta un tap perfecto
                    // ni se repita todo el rato mientras el dedo sigue
                    // apoyado.
                    WindowEvent::Touch(touch)
                        if touch.phase == winit::event::TouchPhase::Started =>
                    {
                        if crash::copy_last_crash_to_clipboard() {
                            log::info!("Log de crash copiado al portapapeles.");
                            self.crash_copy_flash_until =
                                Some(Instant::now() + Duration::from_millis(700));
                        }
                    }
                    WindowEvent::RedrawRequested => {
                        let flashing = self
                            .crash_copy_flash_until
                            .map(|t| Instant::now() < t)
                            .unwrap_or(false);
                        match state.render_crash_screen(flashing) {
                            Ok(_) => {}
                            Err(wgpu::SurfaceError::Lost) => state.resize(state.render.size),
                            Err(wgpu::SurfaceError::OutOfMemory) => event_loop.exit(),
                            Err(e) => log::warn!("Error de render (pantalla de crash): {:?}", e),
                        }
                        if let Some(msg) = &self.crash_short_message {
                            window.set_title(&format!(
                                "Voxel Engine — CRASHEADO: {} (C / tocar pantalla: copiar log)",
                                msg
                            ));
                        }
                    }
                    _ => {}
                }
            }));

            if panic_result.is_err() {
                self.double_crashed = true;
                log::error!(
                    "Panic DENTRO de la pantalla de crash — dejamos de intentar redibujar. \
                     El log del primer crash ya debería estar en disco/logcat."
                );
            }
            return;
        }

        // Todo lo que sigue (el juego en sí) va envuelto en
        // `catch_unwind`. Gracias a `panic = "unwind"` (Cargo.toml) un
        // panic acá adentro no mata el proceso: solo aborta esta llamada
        // puntual a `window_event`, y volvemos con `mark_crashed()` en
        // vez de dejar que se propague hacia winit.
        let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            match event {
                WindowEvent::Focused(true) => {
                    // Abrir el cajón de notificaciones y cerrarlo (o
                    // cualquier diálogo/overlay del sistema) NO pasa por
                    // `resumed()`: la Activity nunca llega a pausarse del
                    // todo, solo pierde y recupera el foco de la ventana.
                    // Android además resetea los flags de inmersivo cada
                    // vez que la ventana recupera el foco, así que hay
                    // que volver a pedirlos acá, no solo en `resumed()`.
                    #[cfg(target_os = "android")]
                    immersive::apply_immersive_fullscreen();
                }
                WindowEvent::CloseRequested => {
                    let saved = state.world.save_dirty_chunks();
                    state.save_player_state_now();
                    if saved > 0 {
                        log::info!("Guardados {} chunks modificados antes de salir.", saved);
                    }
                    event_loop.exit();
                }
                WindowEvent::Resized(physical_size) => state.resize(physical_size),
                WindowEvent::CursorMoved { position, .. } => {
                    state.cursor_pos = (position.x, position.y);
                }
                WindowEvent::MouseInput {
                    state: btn_state,
                    button,
                    ..
                } => {
                    if state.game_screen != GameScreen::Playing {
                        // En cualquier pantalla de menú: un click
                        // izquierdo hace hit-test contra los botones de
                        // esa pantalla en la última posición conocida del
                        // cursor (ver `CursorMoved` arriba), en vez de
                        // capturar el mouse o romper/colocar bloques —
                        // eso solo pasa con el juego corriendo de verdad
                        // (`GameScreen::Playing`).
                        if btn_state == ElementState::Pressed && button == MouseButton::Left {
                            let pos = state.cursor_pos;
                            let size = state.render.size;
                            let action = match state.game_screen {
                                GameScreen::MainMenu => state.touch.on_click_main_menu(pos, size),
                                GameScreen::WorldList => state.touch.on_click_worldlist(
                                    pos,
                                    size,
                                    state.available_worlds.len(),
                                ),
                                GameScreen::Pause => state.touch.on_click_pause(pos, size),
                                GameScreen::GameMode => state.touch.on_click_gamemode(pos, size),
                                GameScreen::Settings => state.touch.on_click_settings(
                                    pos,
                                    size,
                                    state.show_fps,
                                    state.show_clouds,
                                    state.show_fog,
                                ),
                                GameScreen::SettingsMore => state.touch.on_click_settings_more(
                                    pos,
                                    size,
                                    state.show_build_info,
                                    state.show_debug_panel,
                                ),
                                GameScreen::NameWorld => state.touch.on_click_nameworld(pos, size),
                                GameScreen::ConfirmDeleteWorld => {
                                    state.touch.on_click_confirmdelete(pos, size)
                                }
                                // No interactiva: mientras se guarda, un
                                // click no debe disparar ninguna acción
                                // (no hay botones dibujados en esta
                                // pantalla, ver `build_saving_screen`).
                                GameScreen::Saving | GameScreen::Playing => None,
                            };
                            if let Some(action) = action {
                                state.apply_menu_action(action, event_loop, window);
                            }
                        }
                    } else if !state.mouse_captured {
                        // El primer click solo captura el mouse (como en
                        // cualquier juego 3D en navegador/PC), no rompe ni
                        // coloca nada todavía.
                        if btn_state == ElementState::Pressed {
                            state.mouse_captured = true;
                            let _ = window
                                .set_cursor_grab(CursorGrabMode::Confined)
                                .or_else(|_| window.set_cursor_grab(CursorGrabMode::Locked));
                            window.set_cursor_visible(false);
                        }
                    } else if btn_state == ElementState::Pressed {
                        state.handle_click(button);
                    }
                }
                WindowEvent::Touch(touch) => {
                    let action = match state.game_screen {
                        GameScreen::MainMenu => {
                            state.touch.on_touch_main_menu(touch, state.render.size)
                        }
                        GameScreen::WorldList => state.touch.on_touch_worldlist(
                            touch,
                            state.render.size,
                            state.available_worlds.len(),
                        ),
                        GameScreen::Pause => state.touch.on_touch_pause(touch, state.render.size),
                        GameScreen::GameMode => {
                            state.touch.on_touch_gamemode(touch, state.render.size)
                        }
                        GameScreen::Settings => state.touch.on_touch_settings(
                            touch,
                            state.render.size,
                            state.show_fps,
                            state.show_clouds,
                            state.show_fog,
                        ),
                        GameScreen::SettingsMore => state.touch.on_touch_settings_more(
                            touch,
                            state.render.size,
                            state.show_build_info,
                            state.show_debug_panel,
                        ),
                        GameScreen::NameWorld => {
                            state.touch.on_touch_nameworld(touch, state.render.size)
                        }
                        GameScreen::ConfirmDeleteWorld => {
                            state.touch.on_touch_confirmdelete(touch, state.render.size)
                        }
                        // No interactiva: mientras se guarda, ignoramos
                        // cualquier toque (misma razón que en el match
                        // de click, arriba).
                        GameScreen::Saving => None,
                        GameScreen::Playing => {
                            // En juego: procesamos controles táctiles normales.
                            // Si el panel de debug está prendido, le pasamos
                            // el rect de su botón "COPIAR" para que
                            // on_touch_game lo detecte antes que cualquier
                            // otra zona táctil (joystick, mira, etc.).
                            let copy_rect = if state.show_debug_panel {
                                let y_offset = if state.show_build_info {
                                    ui_overlay::build_info_overlay_height()
                                } else {
                                    0.0
                                };
                                Some(ui_overlay::rect_debug_panel_copy_button(state.render.size, y_offset))
                            } else {
                                None
                            };
                            state.touch.on_touch_game(touch, state.render.size, copy_rect)
                        }
                    };
                    if let Some(action) = action {
                        state.apply_menu_action(action, event_loop, window);
                    }
                }
                WindowEvent::Ime(ime_event) => {
                    // El texto confirmado llega acá, del IME nativo del
                    // sistema (Gboard/SwiftKey en Android, el teclado
                    // del SO en desktop), habilitado solo mientras
                    // `GameScreen::NameWorld` está en pantalla (ver el
                    // toggle en `apply_menu_action`).
                    if state.game_screen == GameScreen::NameWorld {
                        match ime_event {
                            winit::event::Ime::Preedit(text, _cursor_range) => {
                                // Composición en curso (por ejemplo,
                                // sílabas de un IME de predicción o CJK
                                // que todavía no se confirmaron): se
                                // guarda aparte y se muestra con otro
                                // color en `build_nameworld_screen`, pero
                                // OJO que todavía no cuenta como texto
                                // del nombre — eso solo pasa con
                                // `Ime::Commit`, más abajo. Así el
                                // usuario puede seguir corrigiendo la
                                // composición sin que cada tecla ya
                                // quede pegada en `name_input`.
                                state.name_preedit = text;
                            }
                            winit::event::Ime::Commit(text) => {
                                state.name_preedit.clear();
                                for c in text.chars() {
                                    state.apply_menu_action(TouchAction::KeyboardChar(c), event_loop, window);
                                }
                            }
                            winit::event::Ime::Disabled => {
                                state.name_preedit.clear();
                            }
                            winit::event::Ime::Enabled => {}
                        }
                    }
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        if state.game_screen == GameScreen::NameWorld {
                            // Con el IME ya puesto, del teclado físico
                            // solo hacen falta las teclas de control que
                            // el IME no manda como `Ime::Commit`: borrar,
                            // confirmar y cancelar. Mientras esta
                            // pantalla está abierta el resto del teclado
                            // (cámara, F3, etc.) sigue deshabilitado a
                            // propósito, para no mover al jugador
                            // mientras tipea.
                            if event.state == ElementState::Pressed {
                                use winit::keyboard::KeyCode;
                                match code {
                                    KeyCode::Backspace => {
                                        state.apply_menu_action(TouchAction::KeyboardBackspace, event_loop, window);
                                    }
                                    KeyCode::Enter | KeyCode::NumpadEnter => {
                                        state.apply_menu_action(TouchAction::ConfirmNameWorld, event_loop, window);
                                    }
                                    KeyCode::Escape => {
                                        state.apply_menu_action(TouchAction::Back, event_loop, window);
                                    }
                                    _ => {}
                                }
                            }
                        } else if code == winit::keyboard::KeyCode::Escape
                            && event.state == ElementState::Pressed
                        {
                            if state.game_screen == GameScreen::Playing {
                                // Abrir el menú de pausa.
                                state.game_screen = GameScreen::Pause;
                                state.mouse_captured = false;
                                let _ = window.set_cursor_grab(CursorGrabMode::None);
                                window.set_cursor_visible(true);
                            } else if state.game_screen == GameScreen::MainMenu {
                                // En el menú principal Esc no hace nada:
                                // todavía no hay ninguna partida a la que
                                // volver (a diferencia de Pause y las
                                // pantallas que cuelgan de ella).
                            } else if state.game_screen == GameScreen::WorldList {
                                // Sin partida corriendo todavía: Esc solo
                                // sube un nivel, a MainMenu (no a
                                // "Playing" como en la rama general de
                                // abajo).
                                state.game_screen = GameScreen::MainMenu;
                            } else if state.game_screen == GameScreen::Settings
                                && state.settings_return == GameScreen::MainMenu
                            {
                                // Ajustes abiertos desde el menú
                                // principal: Esc vuelve ahí, no a
                                // "Playing" (todavía no hay partida
                                // corriendo).
                                state.game_screen = GameScreen::MainMenu;
                            } else if state.game_screen == GameScreen::Saving {
                                // Mientras se guarda: Esc no hace nada.
                                // En particular, NO debe reanudar el
                                // juego a mitad de un guardado (la rama
                                // general de abajo fuerza `Playing`,
                                // que acá sería incorrecto porque
                                // `unload_world()` todavía no corrió).
                            } else {
                                // Desde cualquier pantalla de menú
                                // colgando de Pause, Esc cierra todo de
                                // una y vuelve directo al juego (a
                                // diferencia del botón táctil "< VOLVER",
                                // que solo sube un nivel — en desktop es
                                // más cómodo que Esc siempre saque del
                                // menú del todo).
                                state.game_screen = GameScreen::Playing;
                                state.last_frame = std::time::Instant::now();
                                state.mouse_captured = true;
                                let _ = window.set_cursor_grab(CursorGrabMode::Confined)
                                    .or_else(|_| window.set_cursor_grab(CursorGrabMode::Locked));
                                window.set_cursor_visible(false);
                            }
                        } else if event.state == ElementState::Pressed
                            && code == winit::keyboard::KeyCode::F5
                        {
                            let saved = state.world.save_dirty_chunks();
                            state.save_player_state_now();
                            log::info!("Guardado manual: {} chunks escritos a disco.", saved);
                        } else if event.state == ElementState::Pressed
                            && code == winit::keyboard::KeyCode::F3
                        {
                            // Toggle del panel de debug (posición, chunk,
                            // bloque apuntado, modo, fps, ruta de log).
                            // Mismo estado que la fila "PANEL DE DEBUG
                            // (F3)" del panel de ajustes.
                            state.show_debug_panel = !state.show_debug_panel;
                        } else if event.state == ElementState::Pressed
                            && code == winit::keyboard::KeyCode::F4
                            && state.show_debug_panel
                        {
                            // Copia el snapshot del panel de debug al
                            // portapapeles — atajo de teclado equivalente
                            // al botón "COPIAR" en pantalla, útil en
                            // desktop porque con el mouse capturado
                            // (jugando en primera persona) no se puede
                            // clickear el botón sin soltar antes la
                            // cámara. Solo activo si el panel está
                            // prendido, para que F4 no haga nada
                            // inesperado el resto del tiempo.
                            state.copy_debug_snapshot();
                        } else if event.state == ElementState::Pressed
                            && matches!(
                                code,
                                winit::keyboard::KeyCode::Digit1
                                    | winit::keyboard::KeyCode::Digit2
                                    | winit::keyboard::KeyCode::Digit3
                                    | winit::keyboard::KeyCode::Digit4
                                    | winit::keyboard::KeyCode::Digit5
                            )
                        {
                            // Selección de bloque para colocar (hotbar simple).
                            state.selected_block = match code {
                                winit::keyboard::KeyCode::Digit1 => BlockType::Grass,
                                winit::keyboard::KeyCode::Digit2 => BlockType::Dirt,
                                winit::keyboard::KeyCode::Digit3 => BlockType::Stone,
                                winit::keyboard::KeyCode::Digit4 => BlockType::Wood,
                                _ => BlockType::Leaves,
                            };
                        } else {
                            state.camera.process_key(code, event.state);
                        }
                    }
                }
                WindowEvent::RedrawRequested => {
                    state.update();
                    match state.render() {
                        Ok(_) => {}
                        Err(wgpu::SurfaceError::Lost) => state.resize(state.render.size),
                        Err(wgpu::SurfaceError::OutOfMemory) => event_loop.exit(),
                        Err(e) => log::warn!("Error de render: {:?}", e),
                    }

                    // Actualizamos el título de la ventana con el FPS una vez
                    // por segundo, para no gastar tiempo de CPU formateando
                    // strings en cada frame.
                    if let Some(fps) = state.tick_fps() {
                        window.set_title(&format!(
                            "Voxel Engine - Fase 4 | {:.0} FPS | {} chunks | {} | Bloque: {:?} (1-5) | F3: debug, F5: guardar",
                            fps,
                            state.chunk_meshes.len(),
                            state.game_mode.label(),
                            state.selected_block
                        ));
                    }
                }
                _ => {}
            }
        }));

        if panic_result.is_err() {
            self.mark_crashed();
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        if self.crashed {
            return;
        }
        let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let DeviceEvent::MouseMotion { delta } = event {
                if let Some(state) = self.state.as_mut() {
                    if state.mouse_captured {
                        state.camera.process_mouse(delta.0, delta.1);
                    }
                }
            }
        }));
        if panic_result.is_err() {
            self.mark_crashed();
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Sigue pidiendo redraws aunque estemos crasheados: es lo que
        // mantiene a `render_crash_screen()` dibujando (y, en Android,
        // lo único que hace falta para que la pantalla roja quede
        // visible en vez de congelada en el último frame del juego).
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

/// Loop principal, común a desktop y Android. Arranca el `ApplicationHandler`
/// de arriba, que reemplaza al closure único que usaba `event_loop.run(...)`
/// en winit 0.29 (API vieja, deprecada y quitada en 0.30+).
///
/// `startup_crash`: si viene `Some(mensaje_corto)` (solo puede pasar en
/// Android — ver `crash::check_previous_run_crash`), el `App` arranca YA
/// marcado como crasheado, mostrando la pantalla roja desde el primer
/// frame en vez del juego. `resumed()` igual crea la ventana/`State`
/// normalmente (hace falta una superficie donde dibujar la pantalla de
/// crash); es `window_event` el que, al ver `self.crashed`, se desvía a
/// `render_crash_screen()` en vez de la lógica normal del juego — el
/// mismo mecanismo que ya se usa para un panic atrapado en esta misma
/// corrida, reutilizado acá para uno de la corrida anterior.
pub fn run(event_loop: EventLoop<()>, startup_crash: Option<String>) {
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::default();
    if let Some(short_message) = startup_crash {
        app.crashed = true;
        app.crash_short_message = Some(short_message);
    }
    event_loop.run_app(&mut app).unwrap();
}

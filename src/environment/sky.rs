/// sky.rs
/// Constantes de color de cielo y niebla, antes sueltas en lib.rs.
/// Separadas del "glue" de State porque son datos de ambiente puro, sin
/// ninguna lógica de wgpu/ventana atada.

// Color del cielo / clear color, reusado como color de niebla para que
// el horizonte se "funda" en vez de cortar en seco.
pub const SKY_COLOR: [f32; 3] = [0.53, 0.81, 0.92];

// Fracción del radio de renderizado (en bloques) a la que empieza la
// niebla. Con esto la niebla siempre arranca *antes* del borde donde
// desaparecen los chunks, sin importar qué tan corta o larga sea la
// distancia elegida — así el corte de carga de chunks queda escondido
// detrás de la niebla en vez de verse como un pop abrupto en el horizonte.
pub const FOG_START_FRACTION: f32 = 0.65;

//! GPU effects, drawn with one small fragment shader:
//!
//! * **Rays**: the backdrop. Volumetric light shafts fall from a source just
//!   above the window. The light is sampled on a coarse grid and reduced to
//!   four colours with an ordered (Bayer) dither, each cell drawn as a dot:
//!   the halftone, pixel-print look of the brand.
//! * **Halftone**: a dot gradient that fades out from one corner of a card,
//!   like the dot fields printed in the corners of the posters.
//!
//! When the shader cannot be used (old OpenGL, compile failure), `paint`
//! returns false and the caller draws a plain fallback.

use std::sync::{Arc, OnceLock};

use eframe::egui::{self, Color32, Rect};
use eframe::{egui_glow, glow};
use glow::HasContext;

const VERTEX: &str = r#"
const vec2 corners[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
void main() {
    gl_Position = vec4(corners[gl_VertexID], 0.0, 1.0);
}
"#;

const FRAGMENT: &str = r#"
#ifdef GL_ES
precision highp float;
#endif
uniform vec4 u_rect;     // left, bottom, width, height in pixels
uniform float u_mode;    // 0 = rays, 1 = halftone
uniform float u_time;
uniform float u_cell;    // grid cell size in pixels
uniform vec2 u_light;    // light position, rect uv, y down
uniform vec2 u_origin;   // halftone corner, rect uv, y down
uniform float u_reach;   // halftone reach, in rect heights
uniform float u_strength;
uniform float u_alpha;
uniform vec3 u_c0;
uniform vec3 u_c1;
uniform vec3 u_c2;
uniform vec3 u_c3;
out vec4 out_color;

float bayer2(vec2 a) {
    a = floor(a);
    return fract(a.x / 2.0 + a.y * a.y * 0.75);
}
float bayer4(vec2 a) { return bayer2(0.5 * a) * 0.25 + bayer2(a); }
float bayer8(vec2 a) { return bayer4(0.5 * a) * 0.25 + bayer2(a); }

float hash(vec2 p) {
    p = fract(p * vec2(123.34, 456.21));
    p += dot(p, p + 45.32);
    return fract(p.x * p.y);
}
float noise(vec2 p) {
    vec2 i = floor(p);
    vec2 f = fract(p);
    f = f * f * (3.0 - 2.0 * f);
    return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), f.x),
               mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), f.x), f.y);
}

// Antialiased round dot of the given radius (in cells) inside the cell.
float dot_mask(vec2 frag, vec2 cell, float radius) {
    vec2 local = frag / u_cell - cell - 0.5;
    float edge = 1.5 / u_cell;
    return 1.0 - smoothstep(radius - edge, radius + edge, length(local));
}

void main() {
    vec2 frag = gl_FragCoord.xy - u_rect.xy;
    vec2 cell = floor(frag / u_cell);
    vec2 uv = (cell + 0.5) * u_cell / u_rect.zw;
    uv.y = 1.0 - uv.y;
    float aspect = u_rect.z / u_rect.w;

    if (u_mode < 0.5) {
        // ------------------------------------------------------ god rays
        vec2 d = uv - u_light;
        d.x *= aspect;
        float dist = length(d);
        float ang = atan(d.x, d.y);
        float t = u_time;
        float shafts = noise(vec2(ang * 6.0, t * 0.22)) * 0.65
                     + noise(vec2(ang * 17.0 + 4.0, t * 0.37 + 9.0)) * 0.35;
        shafts = pow(shafts, 2.4) * 2.2;
        float fall = exp(-dist * 1.15);
        float core = exp(-dist * 5.5);
        float dust = noise(uv * vec2(aspect, 1.0) * 7.0 + vec2(t * 0.03, -t * 0.07));
        dust = smoothstep(0.72, 1.0, dust) * 0.5 * fall;
        float light = (shafts * fall * 0.85 + core * 1.1 + dust) * u_strength;
        // Darker towards the far corners.
        vec2 v = (uv - vec2(0.5, 0.45)) * vec2(aspect, 1.0);
        light *= smoothstep(1.35, 0.25, length(v));

        float level = floor(clamp(light * 3.4 + bayer4(cell) - 0.5, 0.0, 3.0));
        vec3 dotc = level < 1.5 ? u_c1 : (level < 2.5 ? u_c2 : u_c3);
        float radius = level < 0.5 ? 0.0 : 0.22 + 0.075 * level;
        float m = dot_mask(frag, cell, radius);
        // A faint smooth glow under the dots gives the dark depth.
        vec3 base = u_c0 + u_c1 * 0.35 * clamp(core * 1.5 + shafts * fall * 0.35, 0.0, 1.0) * u_strength;
        out_color = vec4(mix(base, dotc, m), 1.0);
    } else {
        // ---------------------------------------------------- halftone
        vec2 d = (uv - u_origin) * vec2(aspect, 1.0);
        float g = 1.0 - length(d) / u_reach;
        g += 0.06 * sin(u_time * 1.7 + (uv.x + uv.y) * 9.0);
        float on = step(bayer8(cell), g * u_strength);
        float m = dot_mask(frag, cell, 0.34) * on * u_alpha;
        out_color = vec4(u_c2 * m, m);
    }
}
"#;

/// What to draw, in the callback's own rectangle.
#[derive(Clone, Copy)]
pub struct Effect {
    mode: f32,
    time: f32,
    /// Grid cell size in points.
    cell: f32,
    light: [f32; 2],
    origin: [f32; 2],
    reach: f32,
    strength: f32,
    alpha: f32,
    colors: [[f32; 3]; 4],
}

fn rgb(colour: Color32) -> [f32; 3] {
    [
        colour.r() as f32 / 255.0,
        colour.g() as f32 / 255.0,
        colour.b() as f32 / 255.0,
    ]
}

impl Effect {
    /// The backdrop. `light` is the light position in the rect's uv space
    /// (x right, y down; above the top edge is fine).
    pub fn rays(time: f32, light: [f32; 2], strength: f32, palette: [Color32; 4]) -> Self {
        Self {
            mode: 0.0,
            time,
            cell: 6.0,
            light,
            origin: [0.0, 0.0],
            reach: 1.0,
            strength,
            alpha: 1.0,
            colors: palette.map(rgb),
        }
    }

    /// Dots in `colour` fading out from `origin` (rect uv) over `reach`
    /// rect heights.
    #[allow(dead_code)]
    pub fn halftone(time: f32, origin: [f32; 2], reach: f32, colour: Color32, alpha: f32) -> Self {
        Self {
            mode: 1.0,
            time,
            cell: 5.0,
            light: [0.0, 0.0],
            origin,
            reach,
            strength: 1.0,
            alpha,
            colors: [rgb(colour); 4],
        }
    }
}

struct Program {
    program: glow::Program,
    vao: glow::VertexArray,
    rect: Option<glow::UniformLocation>,
    mode: Option<glow::UniformLocation>,
    time: Option<glow::UniformLocation>,
    cell: Option<glow::UniformLocation>,
    light: Option<glow::UniformLocation>,
    origin: Option<glow::UniformLocation>,
    reach: Option<glow::UniformLocation>,
    strength: Option<glow::UniformLocation>,
    alpha: Option<glow::UniformLocation>,
    colors: [Option<glow::UniformLocation>; 4],
}

static PROGRAM: OnceLock<Option<Arc<Program>>> = OnceLock::new();

/// Compiles the shader. Call once, with the GL context current (at startup).
pub fn init(gl: Option<&glow::Context>) {
    let program = gl.and_then(|gl| match unsafe { compile(gl) } {
        Ok(program) => Some(Arc::new(program)),
        Err(error) => {
            eprintln!("Godrays: shader effects unavailable: {error}");
            None
        }
    });
    let _ = PROGRAM.set(program);
}

unsafe fn compile(gl: &glow::Context) -> Result<Program, String> {
    let version = egui_glow::ShaderVersion::get(gl);
    if !version.is_new_shader_interface() {
        return Err(format!("{version:?} is too old"));
    }
    let header = version.version_declaration();
    let program = gl.create_program()?;
    let mut shaders = Vec::new();
    for (kind, body) in [
        (glow::VERTEX_SHADER, VERTEX),
        (glow::FRAGMENT_SHADER, FRAGMENT),
    ] {
        let shader = gl.create_shader(kind)?;
        gl.shader_source(shader, &format!("{header}{body}"));
        gl.compile_shader(shader);
        if !gl.get_shader_compile_status(shader) {
            let log = gl.get_shader_info_log(shader);
            gl.delete_shader(shader);
            gl.delete_program(program);
            return Err(log);
        }
        gl.attach_shader(program, shader);
        shaders.push(shader);
    }
    gl.link_program(program);
    for shader in shaders {
        gl.detach_shader(program, shader);
        gl.delete_shader(shader);
    }
    if !gl.get_program_link_status(program) {
        let log = gl.get_program_info_log(program);
        gl.delete_program(program);
        return Err(log);
    }
    let vao = gl.create_vertex_array()?;
    let at = |name: &str| gl.get_uniform_location(program, name);
    Ok(Program {
        program,
        vao,
        rect: at("u_rect"),
        mode: at("u_mode"),
        time: at("u_time"),
        cell: at("u_cell"),
        light: at("u_light"),
        origin: at("u_origin"),
        reach: at("u_reach"),
        strength: at("u_strength"),
        alpha: at("u_alpha"),
        colors: [at("u_c0"), at("u_c1"), at("u_c2"), at("u_c3")],
    })
}

impl Program {
    unsafe fn draw(&self, gl: &glow::Context, info: &egui::PaintCallbackInfo, effect: &Effect) {
        let viewport = info.viewport_in_pixels();
        let cell = (effect.cell * info.pixels_per_point).round().max(2.0);
        gl.use_program(Some(self.program));
        gl.bind_vertex_array(Some(self.vao));
        gl.uniform_4_f32(
            self.rect.as_ref(),
            viewport.left_px as f32,
            viewport.from_bottom_px as f32,
            viewport.width_px.max(1) as f32,
            viewport.height_px.max(1) as f32,
        );
        gl.uniform_1_f32(self.mode.as_ref(), effect.mode);
        gl.uniform_1_f32(self.time.as_ref(), effect.time);
        gl.uniform_1_f32(self.cell.as_ref(), cell);
        gl.uniform_2_f32(self.light.as_ref(), effect.light[0], effect.light[1]);
        gl.uniform_2_f32(self.origin.as_ref(), effect.origin[0], effect.origin[1]);
        gl.uniform_1_f32(self.reach.as_ref(), effect.reach.max(0.01));
        gl.uniform_1_f32(self.strength.as_ref(), effect.strength);
        gl.uniform_1_f32(self.alpha.as_ref(), effect.alpha);
        for (location, colour) in self.colors.iter().zip(effect.colors) {
            gl.uniform_3_f32(location.as_ref(), colour[0], colour[1], colour[2]);
        }
        gl.draw_arrays(glow::TRIANGLES, 0, 3);
        gl.bind_vertex_array(None);
    }
}

/// Queues `effect` over `rect`. Returns false when shaders are unavailable,
/// so the caller can draw something simpler instead.
pub fn paint(painter: &egui::Painter, rect: Rect, effect: Effect) -> bool {
    let Some(Some(program)) = PROGRAM.get().cloned() else {
        return false;
    };
    let callback = egui_glow::CallbackFn::new(move |info, painter| unsafe {
        program.draw(painter.gl(), &info, &effect);
    });
    painter.add(egui::PaintCallback {
        rect,
        callback: Arc::new(callback),
    });
    true
}

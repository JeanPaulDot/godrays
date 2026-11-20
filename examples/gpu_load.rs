//! Keeps the GPU fully busy, like a demanding game, to test how hosting
//! behaves under load. A small window renders a deliberately expensive
//! fragment shader as fast as it can (no vsync).
//!
//!   cargo run --release --example gpu_load
//!   GPU_LOAD_ITER=40000   heavier work per pixel (default 20000)
//!   GPU_LOAD_DRAWS=64     split each frame into many draws, like a game

use std::sync::Arc;
use std::time::Instant;

use eframe::{egui, egui_glow, glow};
use glow::HasContext;

const VERTEX: &str = r#"
const vec2 corners[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
out vec2 uv;
void main() {
    uv = corners[gl_VertexID] * 0.5 + 0.5;
    gl_Position = vec4(corners[gl_VertexID], 0.0, 1.0);
}
"#;

const FRAGMENT: &str = r#"
#ifdef GL_ES
precision highp float;
#endif
in vec2 uv;
uniform int u_iter;
uniform float u_time;
out vec4 color;
void main() {
    float v = uv.x + u_time;
    float w = uv.y;
    for (int i = 0; i < u_iter; i++) {
        v = sin(v * 1.0001 + w) * cos(w * 0.9999 - v);
        w = fract(w + v * 0.5);
    }
    color = vec4(0.4 + 0.3 * v, 0.2, 0.6 + 0.3 * w, 1.0);
}
"#;

struct Program {
    program: glow::Program,
    vao: glow::VertexArray,
    iter: Option<glow::UniformLocation>,
    time: Option<glow::UniformLocation>,
}

struct Load {
    program: Arc<Program>,
    iterations: i32,
    draws: i32,
    started: Instant,
    frames: u64,
    last_title: Instant,
}

unsafe fn compile(gl: &glow::Context) -> Program {
    let header = egui_glow::ShaderVersion::get(gl).version_declaration();
    let program = gl.create_program().expect("program");
    for (kind, body) in [
        (glow::VERTEX_SHADER, VERTEX),
        (glow::FRAGMENT_SHADER, FRAGMENT),
    ] {
        let shader = gl.create_shader(kind).expect("shader");
        gl.shader_source(shader, &format!("{header}{body}"));
        gl.compile_shader(shader);
        assert!(
            gl.get_shader_compile_status(shader),
            "{}",
            gl.get_shader_info_log(shader)
        );
        gl.attach_shader(program, shader);
    }
    gl.link_program(program);
    assert!(
        gl.get_program_link_status(program),
        "{}",
        gl.get_program_info_log(program)
    );
    Program {
        program,
        vao: gl.create_vertex_array().expect("vao"),
        iter: gl.get_uniform_location(program, "u_iter"),
        time: gl.get_uniform_location(program, "u_time"),
    }
}

impl eframe::App for Load {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let program = self.program.clone();
        let (iterations, draws, time) = (
            self.iterations,
            self.draws,
            self.started.elapsed().as_secs_f32(),
        );
        egui::CentralPanel::default().show(ctx, |ui| {
            let rect = ui.max_rect();
            let callback = egui_glow::CallbackFn::new(move |info, painter| unsafe {
                let gl = painter.gl();
                gl.use_program(Some(program.program));
                gl.bind_vertex_array(Some(program.vao));
                gl.uniform_1_i32(program.iter.as_ref(), iterations);
                gl.uniform_1_f32(program.time.as_ref(), time);
                // The same total work, cut into horizontal strips: one draw each.
                let viewport = info.viewport_in_pixels();
                let strip = (viewport.height_px / draws).max(1);
                gl.enable(glow::SCISSOR_TEST);
                for index in 0..draws {
                    gl.scissor(
                        viewport.left_px,
                        viewport.from_bottom_px + index * strip,
                        viewport.width_px,
                        strip,
                    );
                    gl.draw_arrays(glow::TRIANGLES, 0, 3);
                }
            });
            ui.painter().add(egui::PaintCallback {
                rect,
                callback: Arc::new(callback),
            });
        });
        self.frames += 1;
        if self.last_title.elapsed().as_secs_f32() > 1.0 {
            let fps = self.frames as f32 / self.last_title.elapsed().as_secs_f32();
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
                "GPU load: {fps:.0} frames/s"
            )));
            self.frames = 0;
            self.last_title = Instant::now();
        }
        ctx.request_repaint();
    }
}

fn main() -> eframe::Result {
    let iterations = std::env::var("GPU_LOAD_ITER")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);
    let draws = std::env::var("GPU_LOAD_DRAWS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
        .max(1);
    let options = eframe::NativeOptions {
        vsync: false,
        viewport: egui::ViewportBuilder::default()
            .with_title("GPU load")
            .with_inner_size([480.0, 320.0])
            .with_position([40.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(
        "GPU load",
        options,
        Box::new(move |cc| {
            let gl = cc.gl.as_ref().expect("OpenGL");
            Ok(Box::new(Load {
                program: Arc::new(unsafe { compile(gl) }),
                iterations,
                draws,
                started: Instant::now(),
                frames: 0,
                last_title: Instant::now(),
            }))
        }),
    )
}

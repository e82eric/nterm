use crate::ghostty_ffi::{GlFrame, ghostty_egui_host_release_frame};
use glow::HasContext as _;
use std::ffi::c_void;
use std::num::NonZeroU32;

// Handles are stored as integers because egui callbacks must be Send. All GL
// access and frame releases happen on the UI thread with its context current.
struct OwnedFrame {
    handle: usize,
    texture: u32,
    size: [u32; 2],
    fence: Option<usize>,
}

impl OwnedFrame {
    unsafe fn release(self, gl: &glow::Context) {
        unsafe {
            if let Some(fence) = self.fence {
                gl.delete_sync(glow::NativeFence(fence as _));
            }
            ghostty_egui_host_release_frame(self.handle as *mut c_void);
        }
    }
}

pub(crate) struct TerminalPainter {
    radius: f32,
    radius_uniform: Option<glow::UniformLocation>,
    size_uniform: Option<glow::UniformLocation>,
    program: glow::Program,
    texture_uniform: Option<glow::UniformLocation>,
    texture_scale_uniform: Option<glow::UniformLocation>,
    background_uniform: Option<glow::UniformLocation>,
    vertex_array: glow::VertexArray,
    current: Option<OwnedFrame>,
    retired: Vec<OwnedFrame>,
}

impl TerminalPainter {
    pub(crate) fn set_corner_radius(&mut self, radius: f32) {
        self.radius = radius;
    }
    pub(crate) fn new(gl: &glow::Context) -> Result<Self, crate::native_error::FatalError> {
        use crate::native_error::{FatalError, FatalResult};
        unsafe {
            let program = gl
                .create_program()
                .fatal("create the terminal shader program")?;
            let mut shaders = Vec::new();
            let result = (|| {
                for (kind, source, create_step, compile_step) in [
                    (
                        glow::VERTEX_SHADER,
                        "#version 330\nconst vec2 p[3]=vec2[3](vec2(-1,-1),vec2(3,-1),vec2(-1,3));out vec2 uv;void main(){vec2 v=p[gl_VertexID];uv=vec2((v.x+1.0)*0.5,(1.0-v.y)*0.5);gl_Position=vec4(v,0,1);}",
                        "create the terminal vertex shader",
                        "compile the terminal vertex shader",
                    ),
                    (
                        glow::FRAGMENT_SHADER,
                        "#version 330\nin vec2 uv;uniform sampler2D terminal_texture;uniform vec2 texture_scale;uniform vec4 background;uniform vec2 pane_size;uniform float radius;out vec4 color;void main(){vec2 q=abs(uv*pane_size-pane_size*0.5)-(pane_size*0.5-vec2(radius));float d=length(max(q,vec2(0)))+min(max(q.x,q.y),0.0)-radius;if(d>0.0)discard;vec2 p=uv*texture_scale;color=(p.x>=1.0||p.y>=1.0)?background:texture(terminal_texture,p);}",
                        "create the terminal fragment shader",
                        "compile the terminal fragment shader",
                    ),
                ] {
                    let shader = gl.create_shader(kind).fatal(create_step)?;
                    shaders.push((shader, false));
                    gl.shader_source(shader, source);
                    gl.compile_shader(shader);
                    if !gl.get_shader_compile_status(shader) {
                        return Err(FatalError::new(
                            compile_step,
                            driver_log(gl.get_shader_info_log(shader)),
                        ));
                    }
                    gl.attach_shader(program, shader);
                    shaders.last_mut().unwrap().1 = true;
                }
                gl.link_program(program);
                if !gl.get_program_link_status(program) {
                    return Err(FatalError::new(
                        "link the terminal shader program",
                        driver_log(gl.get_program_info_log(program)),
                    ));
                }
                let vertex_array = gl
                    .create_vertex_array()
                    .fatal("create the terminal vertex array")?;
                Ok(Self {
                    radius: 0.0,
                    radius_uniform: gl.get_uniform_location(program, "radius"),
                    size_uniform: gl.get_uniform_location(program, "pane_size"),
                    program,
                    texture_uniform: gl.get_uniform_location(program, "terminal_texture"),
                    texture_scale_uniform: gl.get_uniform_location(program, "texture_scale"),
                    background_uniform: gl.get_uniform_location(program, "background"),
                    vertex_array,
                    current: None,
                    retired: Vec::new(),
                })
            })();
            // Delete all temporary objects on both success and partial failure.
            for (shader, attached) in shaders {
                if attached {
                    gl.detach_shader(program, shader);
                }
                gl.delete_shader(shader);
            }
            if result.is_err() {
                gl.delete_program(program);
            }
            result
        }
    }
    pub(crate) fn replace_frame(&mut self, frame: GlFrame) {
        let next = OwnedFrame {
            handle: frame.handle as usize,
            texture: frame.texture,
            size: [frame.width, frame.height],
            fence: None,
        };
        if let Some(old) = self.current.replace(next) {
            if old.fence.is_some() {
                self.retired.push(old);
            } else {
                // This frame was never sampled, or a fallback glFinish completed it.
                unsafe { ghostty_egui_host_release_frame(old.handle as *mut c_void) };
            }
        }
    }

    pub(crate) fn has_retired_frames(&self) -> bool {
        !self.retired.is_empty()
    }

    pub(crate) fn poll_retired(&mut self, gl: &glow::Context) {
        unsafe {
            let mut i = 0;
            while i < self.retired.len() {
                let fence = glow::NativeFence(self.retired[i].fence.expect("retired fence") as _);
                let status = gl.client_wait_sync(fence, 0, 0);
                if status == glow::ALREADY_SIGNALED || status == glow::CONDITION_SATISFIED {
                    self.retired.swap_remove(i).release(gl);
                } else {
                    i += 1;
                }
            }
        }
    }

    pub(crate) fn paint(&mut self, gl: &glow::Context) {
        let Some(texture) = self
            .current
            .as_ref()
            .and_then(|frame| NonZeroU32::new(frame.texture))
        else {
            return;
        };
        self.poll_retired(gl);
        unsafe {
            gl.disable(glow::BLEND);
            gl.use_program(Some(self.program));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(glow::NativeTexture(texture)));
            gl.uniform_1_i32(self.texture_uniform.as_ref(), 0);
            // Renderer frames arrive asynchronously. Sample the previous frame
            // at its original pixel size instead of stretching it during resize.
            let mut viewport = [0; 4];
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            gl.uniform_2_f32(
                self.size_uniform.as_ref(),
                viewport[2] as f32,
                viewport[3] as f32,
            );
            gl.uniform_1_f32(
                self.radius_uniform.as_ref(),
                self.radius.min(viewport[2].min(viewport[3]) as f32 / 2.0),
            );
            let size = self.current.as_ref().expect("current frame").size;
            let scale = frame_texture_scale(size, [viewport[2], viewport[3]]);
            gl.uniform_2_f32(self.texture_scale_uniform.as_ref(), scale[0], scale[1]);
            let background = crate::colors::palette().background;
            gl.uniform_4_f32(
                self.background_uniform.as_ref(),
                background.r() as f32 / 255.0,
                background.g() as f32 / 255.0,
                background.b() as f32 / 255.0,
                1.0,
            );
            gl.bind_vertex_array(Some(self.vertex_array));
            gl.draw_arrays(glow::TRIANGLES, 0, 3);

            let frame = self.current.as_mut().expect("current frame");
            if let Some(fence) = frame.fence.take() {
                gl.delete_sync(glow::NativeFence(fence as _));
            }
            if let Ok(fence) = gl.fence_sync(glow::SYNC_GPU_COMMANDS_COMPLETE, 0) {
                gl.flush();
                frame.fence = Some(fence.0 as usize);
            } else {
                // Fence creation is required for asynchronous retirement. If
                // the driver rejects it, complete this read before allowing
                // the texture to be returned to Ghostty's pool.
                gl.finish();
            }
        }
    }

    pub(crate) fn destroy(&mut self, gl: &glow::Context) {
        unsafe {
            // Shutdown is the only normal blocking wait for the UI's GPU work.
            gl.finish();
            if let Some(frame) = self.current.take() {
                frame.release(gl);
            }
            for frame in self.retired.drain(..) {
                frame.release(gl);
            }
            gl.delete_program(self.program);
            gl.delete_vertex_array(self.vertex_array);
        }
    }
}

fn driver_log(log: String) -> String {
    if log.trim().is_empty() {
        "The graphics driver reported failure without a diagnostic log.".into()
    } else {
        format!("Graphics driver log:\n{log}")
    }
}

fn frame_texture_scale(frame: [u32; 2], viewport: [i32; 2]) -> [f32; 2] {
    [
        viewport[0].max(0) as f32 / frame[0].max(1) as f32,
        viewport[1].max(0) as f32 / frame[1].max(1) as f32,
    ]
}

#[cfg(test)]
mod tests {
    #[test]
    fn resizing_keeps_old_frame_pixels_at_the_same_size() {
        for viewport in [[400, 300], [800, 600], [1200, 900]] {
            let scale = super::frame_texture_scale([800, 600], viewport);
            // A texel at x=80/y=60 stays at that pixel position in every pane.
            assert!((0.1 / scale[0] * viewport[0] as f32 - 80.0).abs() < 0.001);
            assert!((0.1 / scale[1] * viewport[1] as f32 - 60.0).abs() < 0.001);
        }
    }

    #[test]
    fn shader_diagnostics_preserve_driver_log_and_explain_missing_log() {
        let log = "0:2: error: unsupported instruction\nDriver details: test";
        let error = crate::native_error::FatalError::new(
            "compile the terminal fragment shader",
            super::driver_log(log.into()),
        );
        assert!(
            error
                .to_string()
                .contains("compile the terminal fragment shader")
        );
        assert!(error.to_string().contains(log));
        assert!(super::driver_log("\n  ".into()).contains("without a diagnostic log"));
    }
}

pub(crate) type PaintCallback = std::sync::Arc<egui_glow::CallbackFn>;

use crate::ghostty_ffi::{GlFrame, ghostty_egui_host_release_frame};
use glow::HasContext as _;
use std::ffi::c_void;
use std::num::NonZeroU32;

// Handles are stored as integers because egui callbacks must be Send. All GL
// access and frame releases happen on the UI thread with its context current.
struct OwnedFrame {
    handle: usize,
    texture: u32,
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
    program: glow::Program,
    texture_uniform: Option<glow::UniformLocation>,
    vertex_array: glow::VertexArray,
    current: Option<OwnedFrame>,
    retired: Vec<OwnedFrame>,
}

impl TerminalPainter {
    pub(crate) fn new(gl: &glow::Context) -> Self {
        unsafe {
            let program = gl.create_program().expect("create terminal program");
            let vertex = gl
                .create_shader(glow::VERTEX_SHADER)
                .expect("create vertex shader");
            gl.shader_source(
                vertex,
                "#version 330\nconst vec2 p[3]=vec2[3](vec2(-1,-1),vec2(3,-1),vec2(-1,3));out vec2 uv;void main(){vec2 v=p[gl_VertexID];uv=vec2((v.x+1.0)*0.5,(1.0-v.y)*0.5);gl_Position=vec4(v,0,1);}",
            );
            gl.compile_shader(vertex);
            assert!(
                gl.get_shader_compile_status(vertex),
                "{}",
                gl.get_shader_info_log(vertex)
            );
            let fragment = gl
                .create_shader(glow::FRAGMENT_SHADER)
                .expect("create fragment shader");
            gl.shader_source(
                fragment,
                "#version 330\nin vec2 uv;uniform sampler2D terminal_texture;out vec4 color;void main(){color=texture(terminal_texture,uv);}",
            );
            gl.compile_shader(fragment);
            assert!(
                gl.get_shader_compile_status(fragment),
                "{}",
                gl.get_shader_info_log(fragment)
            );
            gl.attach_shader(program, vertex);
            gl.attach_shader(program, fragment);
            gl.link_program(program);
            assert!(
                gl.get_program_link_status(program),
                "{}",
                gl.get_program_info_log(program)
            );
            gl.delete_shader(vertex);
            gl.delete_shader(fragment);
            let texture_uniform = gl.get_uniform_location(program, "terminal_texture");
            Self {
                program,
                texture_uniform,
                vertex_array: gl
                    .create_vertex_array()
                    .expect("create terminal vertex array"),
                current: None,
                retired: Vec::new(),
            }
        }
    }

    pub(crate) fn replace_frame(&mut self, frame: GlFrame) {
        let next = OwnedFrame {
            handle: frame.handle as usize,
            texture: frame.texture,
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

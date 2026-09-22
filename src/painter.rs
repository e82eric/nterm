use crate::ffi::ghostty_egui_host_release_frame;
use glow::HasContext as _;
use std::ffi::c_void;
use std::num::NonZeroU32;

pub(crate) struct TerminalPainter {
    program: glow::Program,
    vertex_array: glow::VertexArray,
    // Store native GLsync values as integers because egui requires paint
    // callbacks to be Send even though native OpenGL callbacks run on the UI
    // thread. The fences are only reconstructed and used on that thread.
    last_fence: Option<(usize, usize)>,
    retired: Vec<(usize, usize)>,
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
            Self {
                program,
                vertex_array: gl
                    .create_vertex_array()
                    .expect("create terminal vertex array"),
                last_fence: None,
                retired: Vec::new(),
            }
        }
    }

    pub(crate) fn retire(&mut self, handle: *mut c_void) {
        let handle = handle as usize;
        match self.last_fence.take() {
            Some((fenced_handle, fence)) if fenced_handle == handle => {
                self.retired.push((handle, fence));
            }
            Some(other) => {
                self.last_fence = Some(other);
                unsafe { ghostty_egui_host_release_frame(handle as *mut c_void) };
            }
            None => unsafe { ghostty_egui_host_release_frame(handle as *mut c_void) },
        }
    }

    pub(crate) fn has_retired_frames(&self) -> bool {
        !self.retired.is_empty()
    }

    pub(crate) fn paint(&mut self, gl: &glow::Context, texture: u32, handle: usize) {
        let Some(texture) = NonZeroU32::new(texture) else {
            return;
        };
        unsafe {
            let mut i = 0;
            while i < self.retired.len() {
                let (handle, fence) = self.retired[i];
                let fence = glow::NativeFence(fence as _);
                let status = gl.client_wait_sync(fence, 0, 0);
                if status == glow::ALREADY_SIGNALED || status == glow::CONDITION_SATISFIED {
                    gl.delete_sync(fence);
                    ghostty_egui_host_release_frame(handle as *mut c_void);
                    self.retired.swap_remove(i);
                } else {
                    i += 1;
                }
            }

            gl.disable(glow::BLEND);
            gl.use_program(Some(self.program));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(glow::NativeTexture(texture)));
            gl.uniform_1_i32(
                gl.get_uniform_location(self.program, "terminal_texture")
                    .as_ref(),
                0,
            );
            gl.bind_vertex_array(Some(self.vertex_array));
            gl.draw_arrays(glow::TRIANGLES, 0, 3);

            if let Some((_, fence)) = self.last_fence.take() {
                gl.delete_sync(glow::NativeFence(fence as _));
            }
            if let Ok(fence) = gl.fence_sync(glow::SYNC_GPU_COMMANDS_COMPLETE, 0) {
                gl.flush();
                self.last_fence = Some((handle, fence.0 as usize));
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
            if let Some((_, fence)) = self.last_fence.take() {
                gl.delete_sync(glow::NativeFence(fence as _));
            }
            for (handle, fence) in self.retired.drain(..) {
                gl.delete_sync(glow::NativeFence(fence as _));
                ghostty_egui_host_release_frame(handle as *mut c_void);
            }
            gl.delete_program(self.program);
            gl.delete_vertex_array(self.vertex_array);
        }
    }
}

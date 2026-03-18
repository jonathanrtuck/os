//! Virgl3D protocol constants and command buffer encoding.
//!
//! Encodes Gallium3D commands for submission via `VIRTIO_GPU_CMD_SUBMIT_3D`.
//! Each command is a sequence of u32 DWORDs: a header (cmd | obj<<8 | len<<16)
//! followed by `len` payload DWORDs.
//!
//! Reference: virglrenderer `src/virgl_protocol.h`, Mesa `p_defines.h`.

// ── virtio-gpu 3D command types ──────────────────────────────────────────

// 2D commands (shared with 3D path for display management):
pub const VIRTIO_GPU_CMD_GET_DISPLAY_INFO: u32 = 0x0100;
pub const VIRTIO_GPU_CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
pub const VIRTIO_GPU_CMD_SET_SCANOUT: u32 = 0x0103;
pub const VIRTIO_GPU_CMD_RESOURCE_FLUSH: u32 = 0x0104;
pub const VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
// Capset queries (part of base spec at 0x0108-0x0109):
pub const VIRTIO_GPU_CMD_GET_CAPSET_INFO: u32 = 0x0108;
pub const VIRTIO_GPU_CMD_GET_CAPSET: u32 = 0x0109;
// 3D context commands (0x0200 range — NOT 0x0100!):
pub const VIRTIO_GPU_CMD_CTX_CREATE: u32 = 0x0200;
pub const VIRTIO_GPU_CMD_CTX_DESTROY: u32 = 0x0201;
pub const VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE: u32 = 0x0202;
pub const VIRTIO_GPU_CMD_CTX_DETACH_RESOURCE: u32 = 0x0203;
pub const VIRTIO_GPU_CMD_RESOURCE_CREATE_3D: u32 = 0x0204;
pub const VIRTIO_GPU_CMD_TRANSFER_TO_HOST_3D: u32 = 0x0205;
pub const VIRTIO_GPU_CMD_TRANSFER_FROM_HOST_3D: u32 = 0x0206;
pub const VIRTIO_GPU_CMD_SUBMIT_3D: u32 = 0x0207;
// Responses:
pub const VIRTIO_GPU_RESP_OK_NODATA: u32 = 0x1100;
pub const VIRTIO_GPU_RESP_OK_DISPLAY_INFO: u32 = 0x1101;
pub const VIRTIO_GPU_RESP_OK_CAPSET_INFO: u32 = 0x1104;
pub const VIRTIO_GPU_RESP_OK_CAPSET: u32 = 0x1105;
pub const VIRTIO_GPU_FLAG_FENCE: u32 = 1;
pub const VIRTIO_GPU_CAPSET_VIRGL: u32 = 1;
pub const VIRTIO_GPU_CAPSET_VIRGL2: u32 = 2;

// ── Virgl command types (VIRGL_CCMD_*) ───────────────────────────────────

pub const VIRGL_CCMD_NOP: u32 = 0;
pub const VIRGL_CCMD_CREATE_OBJECT: u32 = 1;
pub const VIRGL_CCMD_BIND_OBJECT: u32 = 2;
pub const VIRGL_CCMD_DESTROY_OBJECT: u32 = 3;
pub const VIRGL_CCMD_SET_VIEWPORT_STATE: u32 = 4;
pub const VIRGL_CCMD_SET_FRAMEBUFFER_STATE: u32 = 5;
pub const VIRGL_CCMD_SET_VERTEX_BUFFERS: u32 = 6;
pub const VIRGL_CCMD_CLEAR: u32 = 7;
pub const VIRGL_CCMD_DRAW_VBO: u32 = 8;
pub const VIRGL_CCMD_RESOURCE_INLINE_WRITE: u32 = 9;
pub const VIRGL_CCMD_SET_SAMPLER_VIEWS: u32 = 10;
pub const VIRGL_CCMD_SET_INDEX_BUFFER: u32 = 11;
pub const VIRGL_CCMD_SET_CONSTANT_BUFFER: u32 = 12;
pub const VIRGL_CCMD_SET_SCISSOR_STATE: u32 = 15;
pub const VIRGL_CCMD_BLIT: u32 = 16;
pub const VIRGL_CCMD_BIND_SAMPLER_STATES: u32 = 18;
pub const VIRGL_CCMD_SET_UNIFORM_BUFFER: u32 = 27;
pub const VIRGL_CCMD_BIND_SHADER: u32 = 31;

// ── Virgl object types (VIRGL_OBJECT_*) ──────────────────────────────────

pub const VIRGL_OBJECT_BLEND: u32 = 1;
pub const VIRGL_OBJECT_RASTERIZER: u32 = 2;
pub const VIRGL_OBJECT_DSA: u32 = 3;
pub const VIRGL_OBJECT_SHADER: u32 = 4;
pub const VIRGL_OBJECT_VERTEX_ELEMENTS: u32 = 5;
pub const VIRGL_OBJECT_SAMPLER_VIEW: u32 = 6;
pub const VIRGL_OBJECT_SAMPLER_STATE: u32 = 7;
pub const VIRGL_OBJECT_SURFACE: u32 = 8;

// ── Gallium constants (Mesa p_defines.h) ─────────────────────────────────

pub const PIPE_CLEAR_DEPTH: u32 = 0x01;
pub const PIPE_CLEAR_STENCIL: u32 = 0x02;
pub const PIPE_CLEAR_COLOR0: u32 = 0x04;

pub const PIPE_PRIM_TRIANGLES: u32 = 4;
pub const PIPE_PRIM_TRIANGLE_STRIP: u32 = 5;
pub const PIPE_PRIM_TRIANGLE_FAN: u32 = 6;

pub const PIPE_TEXTURE_2D: u32 = 2;
pub const PIPE_BUFFER: u32 = 0;

pub const PIPE_BIND_RENDER_TARGET: u32 = 0x02;
pub const PIPE_BIND_SAMPLER_VIEW: u32 = 0x08;
pub const PIPE_BIND_VERTEX_BUFFER: u32 = 0x10;
pub const PIPE_BIND_CONSTANT_BUFFER: u32 = 0x40;

pub const PIPE_BLENDFACTOR_ONE: u32 = 1;
pub const PIPE_BLENDFACTOR_SRC_ALPHA: u32 = 3;
pub const PIPE_BLENDFACTOR_ZERO: u32 = 0x11;
pub const PIPE_BLENDFACTOR_INV_SRC_ALPHA: u32 = 0x13;
pub const PIPE_BLEND_ADD: u32 = 0;

pub const PIPE_FUNC_ALWAYS: u32 = 7;

pub const PIPE_MASK_RGBA: u32 = 0x0F;

// virgl_hw.h format values — NOT the same as Mesa pipe_format!
// Always verify against virglrenderer/src/virgl_hw.h.
pub const VIRGL_FORMAT_B8G8R8A8_UNORM: u32 = 1;
pub const VIRGL_FORMAT_R8_UNORM: u32 = 64;
pub const VIRGL_FORMAT_R32G32_FLOAT: u32 = 29;
pub const VIRGL_FORMAT_R32G32B32A32_FLOAT: u32 = 31;

// ── TGSI shader types ────────────────────────────────────────────────────

pub const PIPE_SHADER_VERTEX: u32 = 0;
pub const PIPE_SHADER_FRAGMENT: u32 = 1;

// ── Command header encoding ──────────────────────────────────────────────

/// Encode a virgl command header.
/// `cmd`: VIRGL_CCMD_*, `obj`: VIRGL_OBJECT_* (0 for non-object cmds),
/// `len`: payload length in u32 DWORDs (not including header).
#[inline]
pub const fn virgl_cmd0(cmd: u32, obj: u32, len: u32) -> u32 {
    (cmd & 0xFF) | ((obj & 0xFF) << 8) | ((len & 0xFFFF) << 16)
}

// ── Command buffer ───────────────────────────────────────────────────────

/// Maximum command buffer size in u32 DWORDs.
/// 16 KiB = 4096 DWORDs — fits comfortably in a single SUBMIT_3D.
const MAX_CMD_DWORDS: usize = 4096;

/// Accumulates virgl commands as a sequence of u32 DWORDs for submission
/// via `VIRTIO_GPU_CMD_SUBMIT_3D`.
///
/// # Stack allocation warning
///
/// This struct contains a `[u32; 4096]` = 16 KiB inline array. The userspace
/// stack is also 16 KiB. Always heap-allocate via `Box::new(CommandBuffer::new())`
/// — never place on the stack. Any `VirglContext` containing a `CommandBuffer`
/// must likewise be heap-allocated.
pub struct CommandBuffer {
    buf: [u32; MAX_CMD_DWORDS],
    len: usize,
}

impl CommandBuffer {
    pub const fn new() -> Self {
        Self {
            buf: [0; MAX_CMD_DWORDS],
            len: 0,
        }
    }

    /// Reset the buffer for reuse (avoids reallocation).
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Current contents as a u32 slice (clamped to buffer capacity).
    pub fn as_dwords(&self) -> &[u32] {
        let end = if self.len > MAX_CMD_DWORDS {
            MAX_CMD_DWORDS
        } else {
            self.len
        };
        &self.buf[..end]
    }

    /// Current size in bytes (clamped to buffer capacity).
    pub fn size_bytes(&self) -> u32 {
        (self.as_dwords().len() * 4) as u32
    }

    /// Returns true if the buffer has overflowed (commands will be incomplete).
    pub fn overflowed(&self) -> bool {
        self.len > MAX_CMD_DWORDS
    }

    /// Push a single DWORD. Sets overflow flag if buffer is full.
    fn push(&mut self, val: u32) {
        if self.len < MAX_CMD_DWORDS {
            self.buf[self.len] = val;
        }
        // Always increment len so overflowed() can detect overflow.
        self.len += 1;
    }

    /// Push a float as its bit representation.
    fn push_f32(&mut self, val: f32) {
        self.push(val.to_bits());
    }

    // ── High-level command emitters ──────────────────────────────────────

    /// VIRGL_CCMD_CLEAR — clear color buffer to RGBA.
    pub fn cmd_clear(&mut self, r: f32, g: f32, b: f32, a: f32) {
        self.push(virgl_cmd0(VIRGL_CCMD_CLEAR, 0, 8));
        self.push(PIPE_CLEAR_COLOR0);
        self.push_f32(r);
        self.push_f32(g);
        self.push_f32(b);
        self.push_f32(a);
        self.push(0); // depth lo
        self.push(0); // depth hi
        self.push(0); // stencil
    }

    /// VIRGL_CCMD_DRAW_VBO — draw primitives.
    pub fn cmd_draw_vbo(&mut self, start: u32, count: u32, mode: u32, indexed: bool) {
        self.push(virgl_cmd0(VIRGL_CCMD_DRAW_VBO, 0, 12));
        self.push(start);
        self.push(count);
        self.push(mode);
        self.push(if indexed { 1 } else { 0 });
        self.push(1); // instance_count
        self.push(0); // index_bias
        self.push(0); // start_instance
        self.push(0); // primitive_restart
        self.push(0); // restart_index
        self.push(0); // min_index
        self.push(if count > 0 { count - 1 } else { 0 }); // max_index
        self.push(0); // count_from_so
    }

    /// VIRGL_CCMD_SET_FRAMEBUFFER_STATE — bind render target.
    /// `cbuf_handle`: surface handle for color buffer (0 = unbound).
    /// `zsurf_handle`: surface handle for depth/stencil (0 = none).
    pub fn cmd_set_framebuffer_state(&mut self, cbuf_handle: u32, zsurf_handle: u32) {
        self.push(virgl_cmd0(VIRGL_CCMD_SET_FRAMEBUFFER_STATE, 0, 3));
        self.push(1); // nr_cbufs = 1
        self.push(zsurf_handle);
        self.push(cbuf_handle);
    }

    /// VIRGL_CCMD_SET_VIEWPORT_STATE — set viewport transform.
    /// Maps NDC [-1,1] to pixel coordinates.
    pub fn cmd_set_viewport(&mut self, width: f32, height: f32) {
        self.push(virgl_cmd0(VIRGL_CCMD_SET_VIEWPORT_STATE, 0, 7));
        self.push(0); // start_slot
        self.push_f32(width / 2.0); // scale_x
        self.push_f32(height / -2.0); // scale_y (flip Y: NDC Y-up → screen Y-down)
        self.push_f32(0.5); // scale_z
        self.push_f32(width / 2.0); // translate_x
        self.push_f32(height / 2.0); // translate_y
        self.push_f32(0.5); // translate_z
    }

    /// VIRGL_CCMD_SET_VERTEX_BUFFERS — bind a vertex buffer.
    pub fn cmd_set_vertex_buffers(&mut self, stride: u32, offset: u32, res_handle: u32) {
        self.push(virgl_cmd0(VIRGL_CCMD_SET_VERTEX_BUFFERS, 0, 3));
        self.push(stride);
        self.push(offset);
        self.push(res_handle);
    }

    /// VIRGL_CCMD_BIND_OBJECT — bind a previously created object.
    pub fn cmd_bind_object(&mut self, obj_type: u32, handle: u32) {
        self.push(virgl_cmd0(VIRGL_CCMD_BIND_OBJECT, obj_type, 1));
        self.push(handle);
    }

    /// VIRGL_CCMD_BIND_SHADER — bind a shader.
    pub fn cmd_bind_shader(&mut self, handle: u32, shader_type: u32) {
        self.push(virgl_cmd0(VIRGL_CCMD_BIND_SHADER, 0, 2));
        self.push(handle);
        self.push(shader_type);
    }

    /// VIRGL_CCMD_SET_SCISSOR_STATE — set scissor rect (for clipping).
    pub fn cmd_set_scissor(&mut self, x: u16, y: u16, w: u16, h: u16) {
        self.push(virgl_cmd0(VIRGL_CCMD_SET_SCISSOR_STATE, 0, 3));
        self.push(0); // start_slot
        self.push((x as u32) | ((y as u32) << 16));
        self.push((x as u32 + w as u32) | ((y as u32 + h as u32) << 16));
    }

    /// VIRGL_CCMD_CREATE_OBJECT — create a blend state for alpha blending.
    /// Standard Porter-Duff source-over: src_alpha, 1-src_alpha.
    pub fn cmd_create_blend(&mut self, handle: u32) {
        self.push(virgl_cmd0(VIRGL_CCMD_CREATE_OBJECT, VIRGL_OBJECT_BLEND, 11));
        self.push(handle);
        self.push(0); // S0: no independent blend, no logicop, no dither
        self.push(0); // S1: logicop_func = 0
                      // RT0 blend state (one u32 per field, 8 fields total):
                      // Pack: blend_enable=1, rgb_func=ADD, rgb_src=SRC_ALPHA, rgb_dst=INV_SRC_ALPHA
                      //       alpha_func=ADD, alpha_src=SRC_ALPHA, alpha_dst=INV_SRC_ALPHA, colormask=RGBA
        let rt0 = 1 // blend_enable (bit 0)
            | (PIPE_BLEND_ADD << 1)                  // rgb_func (bits 1-3)
            | (PIPE_BLENDFACTOR_SRC_ALPHA << 4)      // rgb_src_factor (bits 4-8)
            | (PIPE_BLENDFACTOR_INV_SRC_ALPHA << 9)  // rgb_dst_factor (bits 9-13)
            | (PIPE_BLEND_ADD << 14)                 // alpha_func (bits 14-16)
            | (PIPE_BLENDFACTOR_SRC_ALPHA << 17)     // alpha_src_factor (bits 17-21)
            | (PIPE_BLENDFACTOR_INV_SRC_ALPHA << 22) // alpha_dst_factor (bits 22-26)
            | (PIPE_MASK_RGBA << 27); // colormask (bits 27-30)
        self.push(rt0);
        // RT1-RT7: zero (unused)
        for _ in 0..7 {
            self.push(0);
        }
    }

    /// VIRGL_CCMD_CREATE_OBJECT — create a DSA state (depth/stencil/alpha).
    /// Depth test disabled (2D rendering), always pass.
    pub fn cmd_create_dsa(&mut self, handle: u32) {
        self.push(virgl_cmd0(VIRGL_CCMD_CREATE_OBJECT, VIRGL_OBJECT_DSA, 5));
        self.push(handle);
        // S0: depth_enabled=0, depth_writemask=0, depth_func=ALWAYS
        self.push(PIPE_FUNC_ALWAYS << 2);
        self.push(0); // S1: stencil[0] (disabled)
        self.push(0); // S2: stencil[1] (disabled)
        self.push(0); // S3: alpha ref + alpha func
    }

    /// VIRGL_CCMD_CREATE_OBJECT — create a rasterizer state.
    /// No culling, fill mode, scissor test enabled.
    pub fn cmd_create_rasterizer(&mut self, handle: u32, scissor: bool) {
        self.push(virgl_cmd0(
            VIRGL_CCMD_CREATE_OBJECT,
            VIRGL_OBJECT_RASTERIZER,
            9,
        ));
        self.push(handle);
        // S0: flatshade=0, depth_clip_near=1, depth_clip_far=1, clip_halfz=0,
        //     multisample=0, fill_front=FILL(0), fill_back=FILL(0), cull_face=NONE(0),
        //     scissor=bit
        let s0 = (1 << 1)   // depth_clip_near
            | (1 << 2)      // depth_clip_far
            | if scissor { 1 << 8 } else { 0 }; // scissor
        self.push(s0);
        self.push_f32(0.0); // point_size
        self.push(0); // sprite_coord_enable
                      // S3: half_pixel_center=1, bottom_edge_rule=1
        self.push((1 << 0) | (1 << 1));
        self.push_f32(0.0); // point_size_per_vertex
        self.push(0); // offset stuff
        self.push_f32(0.0); // line_width
        self.push(0); // line_stipple
    }

    /// VIRGL_CCMD_CREATE_OBJECT — create vertex element layout.
    /// Single attribute: 2D position + RGBA color = 6 floats = 24 bytes per vertex.
    /// Or for textured: 2D position + 2D texcoord + RGBA color = 8 floats = 32 bytes.
    pub fn cmd_create_vertex_elements_color(&mut self, handle: u32) {
        // 2 elements: position (float2) + color (float4)
        self.push(virgl_cmd0(
            VIRGL_CCMD_CREATE_OBJECT,
            VIRGL_OBJECT_VERTEX_ELEMENTS,
            9,
        ));
        self.push(handle);
        // Element 0: position (offset=0, float2)
        self.push(0); // src_offset
        self.push(0); // instance_divisor
        self.push(0); // vertex_buffer_index
        self.push(VIRGL_FORMAT_R32G32_FLOAT); // src_format (virgl_hw.h = 29)
        // Element 1: color (offset=8, float4)
        self.push(8); // src_offset (after 2 floats)
        self.push(0); // instance_divisor
        self.push(0); // vertex_buffer_index
        self.push(VIRGL_FORMAT_R32G32B32A32_FLOAT); // src_format (virgl_hw.h = 31)
    }

    /// VIRGL_CCMD_RESOURCE_INLINE_WRITE — upload data to a resource.
    pub fn cmd_resource_inline_write(
        &mut self,
        res_handle: u32,
        data: &[u32],
        width: u32,
        height: u32,
        stride: u32,
    ) {
        let payload_len = 11 + data.len() as u32;
        self.push(virgl_cmd0(VIRGL_CCMD_RESOURCE_INLINE_WRITE, 0, payload_len));
        self.push(res_handle);
        self.push(0); // level
        self.push(0); // usage (PIPE_USAGE_DEFAULT)
        self.push(stride);
        self.push(0); // layer_stride
        self.push(0); // x
        self.push(0); // y
        self.push(0); // z
        self.push(width);
        self.push(height);
        self.push(1); // d (depth = 1)
        for &dw in data {
            self.push(dw);
        }
    }

    /// VIRGL_CCMD_SET_SAMPLER_VIEWS — bind a sampler view to a shader stage.
    pub fn cmd_set_sampler_views(&mut self, shader_type: u32, view_handle: u32) {
        self.push(virgl_cmd0(VIRGL_CCMD_SET_SAMPLER_VIEWS, 0, 3));
        self.push(shader_type);
        self.push(0); // start_slot
        self.push(view_handle);
    }

    /// VIRGL_CCMD_BIND_SAMPLER_STATES — bind sampler states.
    pub fn cmd_bind_sampler_states(&mut self, shader_type: u32, sampler_handle: u32) {
        self.push(virgl_cmd0(VIRGL_CCMD_BIND_SAMPLER_STATES, 0, 3));
        self.push(shader_type);
        self.push(0); // start_slot
        self.push(sampler_handle);
    }

    /// VIRGL_CCMD_CREATE_OBJECT — create a sampler state.
    /// Nearest-neighbor filtering, clamp-to-edge (for pixel-perfect glyph rendering).
    pub fn cmd_create_sampler_state(&mut self, handle: u32) {
        // VIRGL_OBJ_SAMPLER_STATE_SIZE = 9 (virglrenderer validates this).
        self.push(virgl_cmd0(
            VIRGL_CCMD_CREATE_OBJECT,
            VIRGL_OBJECT_SAMPLER_STATE,
            9,
        ));
        self.push(handle);
        // S0: wrap_s=CLAMP_TO_EDGE(2), wrap_t=CLAMP_TO_EDGE(2), wrap_r=0,
        //     min_img_filter=NEAREST(0), min_mip_filter=NONE(3), mag_img_filter=NEAREST(0)
        // Bit layout per virgl_protocol.h:
        //   bits 0-2:   wrap_s (CLAMP_TO_EDGE = 2)
        //   bits 3-5:   wrap_t (CLAMP_TO_EDGE = 2)
        //   bits 6-8:   wrap_r (0)
        //   bits 9-10:  min_img_filter (NEAREST = 0)
        //   bits 11-12: min_mip_filter (NONE = 3)
        //   bits 13-14: mag_img_filter (NEAREST = 0)
        let s0 = 2 | (2 << 3) | (3 << 11);
        self.push(s0);
        self.push(0); // lod_bias (float bits)
        self.push(0); // min_lod (float bits)
        self.push(0); // max_lod (float bits)
        self.push(0); // border_color[0]
        self.push(0); // border_color[1]
        self.push(0); // border_color[2]
        self.push(0); // border_color[3]
    }

    /// VIRGL_CCMD_CREATE_OBJECT — create a sampler view wrapping a texture resource.
    pub fn cmd_create_sampler_view(&mut self, handle: u32, res_handle: u32, format: u32) {
        self.push(virgl_cmd0(
            VIRGL_CCMD_CREATE_OBJECT,
            VIRGL_OBJECT_SAMPLER_VIEW,
            6,
        ));
        self.push(handle);
        self.push(res_handle);
        self.push(format);
        self.push(0); // first_element / first_layer
        self.push(0); // last_element / last_layer
                      // swizzle: identity (R=0, G=1, B=2, A=3)
        self.push(0 | (1 << 3) | (2 << 6) | (3 << 9));
    }

    /// VIRGL_CCMD_CREATE_OBJECT — create a surface (render target wrapper).
    pub fn cmd_create_surface(&mut self, handle: u32, res_handle: u32, format: u32) {
        self.push(virgl_cmd0(
            VIRGL_CCMD_CREATE_OBJECT,
            VIRGL_OBJECT_SURFACE,
            5,
        ));
        self.push(handle);
        self.push(res_handle);
        self.push(format);
        self.push(0); // first_element
        self.push(0); // last_element
    }

    /// VIRGL_CCMD_CREATE_OBJECT — create a shader from TGSI tokens.
    pub fn cmd_create_shader(&mut self, handle: u32, shader_type: u32, tgsi_tokens: &[u32]) {
        let payload_len = 5 + tgsi_tokens.len() as u32;
        self.push(virgl_cmd0(
            VIRGL_CCMD_CREATE_OBJECT,
            VIRGL_OBJECT_SHADER,
            payload_len,
        ));
        self.push(handle);
        self.push(shader_type);
        self.push(0); // offset (no continuation)
        self.push(tgsi_tokens.len() as u32); // num_tokens
        self.push(0); // num_so_outputs
        for &tok in tgsi_tokens {
            self.push(tok);
        }
    }

    /// VIRGL_CCMD_CREATE_OBJECT — create a shader from TGSI text.
    ///
    /// virglrenderer's `vrend_create_shader` always calls `tgsi_text_translate`,
    /// so the wire payload is the shader source as UTF-8 text (null-terminated),
    /// packed into u32 DWORDs (last DWORD zero-padded).
    ///
    /// The `offlen` field encodes the total byte length of the text (including
    /// the null terminator) in bits [30:0]. Bit 31 is the continuation flag
    /// (0 = new shader). `num_tokens` is a host-side allocation hint; 300 is
    /// a safe value for the simple shaders we use.
    ///
    /// `text` must be a null-terminated byte slice (the final byte must be `\0`).
    pub fn cmd_create_shader_text(&mut self, handle: u32, shader_type: u32, text: &[u8]) {
        // text must be null-terminated; byte length includes the '\0'.
        // Hard assert (not debug_assert) — violating this sends a non-terminated
        // string to virglrenderer which would read past the allocation.
        assert!(!text.is_empty() && text[text.len() - 1] == 0);

        // Pack bytes into DWORDs (4 bytes each, last one zero-padded).
        let text_dwords = (text.len() + 3) / 4;
        let payload_len = 5 + text_dwords as u32;

        self.push(virgl_cmd0(
            VIRGL_CCMD_CREATE_OBJECT,
            VIRGL_OBJECT_SHADER,
            payload_len,
        ));
        self.push(handle);
        self.push(shader_type);
        // offlen: total byte length of text in bits [30:0], bit 31 = 0 (new shader).
        self.push(text.len() as u32 & 0x7FFF_FFFF);
        self.push(300); // num_tokens: allocation hint for the host token array
        self.push(0); // num_so_outputs

        // Pack text bytes as little-endian u32 DWORDs.
        let chunks = text.chunks(4);
        for chunk in chunks {
            let mut dword = 0u32;
            for (i, &byte) in chunk.iter().enumerate() {
                dword |= (byte as u32) << (i * 8);
            }
            self.push(dword);
        }
    }

    /// VIRGL_CCMD_SET_CONSTANT_BUFFER — upload uniform data to a shader stage.
    pub fn cmd_set_constant_buffer(&mut self, shader_type: u32, index: u32, data: &[u32]) {
        let payload_len = 2 + data.len() as u32;
        self.push(virgl_cmd0(VIRGL_CCMD_SET_CONSTANT_BUFFER, 0, payload_len));
        self.push(shader_type);
        self.push(index);
        for &dw in data {
            self.push(dw);
        }
    }
}

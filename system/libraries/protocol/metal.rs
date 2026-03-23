//! Metal command protocol — wire format for Metal-over-virtio.
//!
//! The guest metal-render driver writes a flat command buffer (sequence of
//! commands) into the virtio backing store. Each command is:
//!
//!     [u16 method_id] [u16 flags] [u32 payload_size] [payload bytes...]
//!
//! The host reads commands sequentially and replays them via Metal.
//!
//! Handle model: the guest pre-assigns u32 IDs for all Metal objects.
//! The host maps IDs to real Metal objects. Invalid handles are no-ops.

extern crate alloc;

use alloc::vec::Vec;

// ── Command header ──────────────────────────────────────────────────────

/// Every command starts with this 8-byte header.
#[repr(C)]
pub struct CommandHeader {
    pub method_id: u16,
    pub flags: u16,
    pub payload_size: u32,
}

// ── Setup commands (virtqueue 0) ────────────────────────────────────────

pub const CMD_COMPILE_LIBRARY: u16 = 0x0001;
pub const CMD_GET_FUNCTION: u16 = 0x0002;
pub const CMD_CREATE_RENDER_PIPELINE: u16 = 0x0010;
pub const CMD_CREATE_COMPUTE_PIPELINE: u16 = 0x0011;
pub const CMD_CREATE_DEPTH_STENCIL_STATE: u16 = 0x0012;
pub const CMD_CREATE_SAMPLER: u16 = 0x0013;
pub const CMD_CREATE_TEXTURE: u16 = 0x0020;
pub const CMD_UPLOAD_TEXTURE: u16 = 0x0021;
pub const CMD_DESTROY_OBJECT: u16 = 0x00FF;

// ── Render commands (virtqueue 1) ───────────────────────────────────────

pub const CMD_BEGIN_RENDER_PASS: u16 = 0x0100;
pub const CMD_END_RENDER_PASS: u16 = 0x0101;
pub const CMD_SET_RENDER_PIPELINE: u16 = 0x0110;
pub const CMD_SET_DEPTH_STENCIL_STATE: u16 = 0x0111;
pub const CMD_SET_STENCIL_REF: u16 = 0x0112;
pub const CMD_SET_SCISSOR: u16 = 0x0113;
pub const CMD_SET_VERTEX_BYTES: u16 = 0x0120;
pub const CMD_SET_FRAGMENT_TEXTURE: u16 = 0x0121;
pub const CMD_SET_FRAGMENT_SAMPLER: u16 = 0x0122;
pub const CMD_SET_FRAGMENT_BYTES: u16 = 0x0123;
pub const CMD_DRAW_PRIMITIVES: u16 = 0x0130;
pub const CMD_BEGIN_COMPUTE_PASS: u16 = 0x0200;
pub const CMD_END_COMPUTE_PASS: u16 = 0x0201;
pub const CMD_SET_COMPUTE_PIPELINE: u16 = 0x0210;
pub const CMD_SET_COMPUTE_TEXTURE: u16 = 0x0211;
pub const CMD_SET_COMPUTE_BYTES: u16 = 0x0212;
pub const CMD_DISPATCH_THREADS: u16 = 0x0220;
pub const CMD_BEGIN_BLIT_PASS: u16 = 0x0300;
pub const CMD_END_BLIT_PASS: u16 = 0x0301;
pub const CMD_COPY_TEXTURE_REGION: u16 = 0x0310;
pub const CMD_PRESENT_AND_COMMIT: u16 = 0x0F00;

// ── Special handles ─────────────────────────────────────────────────────

/// When used as a texture handle in `begin_render_pass`, the host acquires
/// the next drawable from CAMetalLayer and uses its texture. This is how
/// the guest references the display surface without knowing the host's
/// drawable lifecycle.
pub const DRAWABLE_HANDLE: u32 = 0xFFFF_FFFF;

// ── Pixel formats ───────────────────────────────────────────────────────

pub const PIXEL_FORMAT_BGRA8: u8 = 1;
pub const PIXEL_FORMAT_RGBA8: u8 = 2;
pub const PIXEL_FORMAT_R8: u8 = 3;
pub const PIXEL_FORMAT_STENCIL8: u8 = 4;
pub const PIXEL_FORMAT_RGBA16F: u8 = 5;
/// sRGB-encoded BGRA8. Hardware blender operates in linear space:
/// fragment outputs are converted linear→sRGB on store, and
/// destination values are converted sRGB→linear before blending.
/// Compute shader `read()`/`write()` bypass the conversion.
pub const PIXEL_FORMAT_BGRA8_SRGB: u8 = 6;

// ── Texture usage flags ─────────────────────────────────────────────────

pub const USAGE_SHADER_READ: u8 = 0x01;
pub const USAGE_SHADER_WRITE: u8 = 0x02;
pub const USAGE_RENDER_TARGET: u8 = 0x04;

// ── Primitive types ─────────────────────────────────────────────────────

pub const PRIM_TRIANGLE: u8 = 0;
pub const PRIM_TRIANGLE_STRIP: u8 = 1;

// ── Load/store actions ──────────────────────────────────────────────────

pub const LOAD_DONT_CARE: u8 = 0;
pub const LOAD_LOAD: u8 = 1;
pub const LOAD_CLEAR: u8 = 2;

pub const STORE_DONT_CARE: u8 = 0;
pub const STORE_STORE: u8 = 1;
pub const STORE_MSAA_RESOLVE: u8 = 2;

// ── Stencil compare functions ───────────────────────────────────────────

pub const CMP_NEVER: u8 = 0;
pub const CMP_ALWAYS: u8 = 1;
pub const CMP_EQUAL: u8 = 2;
pub const CMP_NOT_EQUAL: u8 = 3;

// ── Stencil operations ──────────────────────────────────────────────────

pub const STENCIL_KEEP: u8 = 0;
pub const STENCIL_ZERO: u8 = 1;
pub const STENCIL_REPLACE: u8 = 2;
pub const STENCIL_INCR_CLAMP: u8 = 3;
pub const STENCIL_DECR_CLAMP: u8 = 4;
pub const STENCIL_INVERT: u8 = 5;
pub const STENCIL_INCR_WRAP: u8 = 6;
pub const STENCIL_DECR_WRAP: u8 = 7;

// ── Filter modes ────────────────────────────────────────────────────────

pub const FILTER_NEAREST: u8 = 0;
pub const FILTER_LINEAR: u8 = 1;

// ── Command buffer builder ──────────────────────────────────────────────

/// Builds a sequence of Metal commands into a flat byte buffer.
pub struct CommandBuffer {
    data: Vec<u8>,
}

impl CommandBuffer {
    pub fn new() -> Self {
        Self {
            data: Vec::with_capacity(16384),
        }
    }

    pub fn clear(&mut self) {
        self.data.clear();
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    fn push_header(&mut self, method_id: u16, payload_size: u32) {
        self.data.extend_from_slice(&method_id.to_le_bytes());
        self.data.extend_from_slice(&0u16.to_le_bytes()); // flags
        self.data.extend_from_slice(&payload_size.to_le_bytes());
    }

    fn push_u8(&mut self, v: u8) {
        self.data.push(v);
    }

    fn push_u16(&mut self, v: u16) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    fn push_u32(&mut self, v: u32) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    fn push_f32(&mut self, v: f32) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    fn push_bytes(&mut self, data: &[u8]) {
        self.data.extend_from_slice(data);
    }

    // ── Setup commands ──────────────────────────────────────────────────

    /// Compile MSL shader source into a library.
    pub fn compile_library(&mut self, handle: u32, source: &[u8]) {
        let payload_size = 8 + source.len() as u32;
        self.push_header(CMD_COMPILE_LIBRARY, payload_size);
        self.push_u32(handle);
        self.push_u32(source.len() as u32);
        self.push_bytes(source);
    }

    /// Get a named function from a compiled library.
    pub fn get_function(&mut self, handle: u32, library: u32, name: &[u8]) {
        let payload_size = 12 + name.len() as u32;
        self.push_header(CMD_GET_FUNCTION, payload_size);
        self.push_u32(handle);
        self.push_u32(library);
        self.push_u32(name.len() as u32);
        self.push_bytes(name);
    }

    /// Create a render pipeline state.
    pub fn create_render_pipeline(
        &mut self,
        handle: u32,
        vertex_fn: u32,
        fragment_fn: u32,
        blend_enabled: bool,
        color_write_mask: u8,
        has_stencil: bool,
        sample_count: u8,
    ) {
        self.push_header(CMD_CREATE_RENDER_PIPELINE, 16);
        self.push_u32(handle);
        self.push_u32(vertex_fn);
        self.push_u32(fragment_fn);
        self.push_u8(blend_enabled as u8);
        self.push_u8(color_write_mask);
        self.push_u8(has_stencil as u8);
        self.push_u8(sample_count);
    }

    /// Create a compute pipeline state.
    pub fn create_compute_pipeline(&mut self, handle: u32, function: u32) {
        self.push_header(CMD_CREATE_COMPUTE_PIPELINE, 8);
        self.push_u32(handle);
        self.push_u32(function);
    }

    /// Create a depth/stencil state.
    /// Create a depth/stencil state. Front and back face use the same ops.
    pub fn create_depth_stencil_state(
        &mut self,
        handle: u32,
        stencil_enabled: bool,
        compare_fn: u8,
        pass_op: u8,
        fail_op: u8,
    ) {
        self.push_header(CMD_CREATE_DEPTH_STENCIL_STATE, 8);
        self.push_u32(handle);
        self.push_u8(stencil_enabled as u8);
        self.push_u8(compare_fn);
        self.push_u8(pass_op);
        self.push_u8(fail_op);
    }

    /// Create a depth/stencil state with separate front/back stencil ops.
    /// Used for non-zero winding fill: front = INCR_WRAP, back = DECR_WRAP.
    pub fn create_depth_stencil_state_two_sided(
        &mut self,
        handle: u32,
        compare_fn: u8,
        front_pass_op: u8,
        front_fail_op: u8,
        back_pass_op: u8,
        back_fail_op: u8,
    ) {
        self.push_header(CMD_CREATE_DEPTH_STENCIL_STATE, 12);
        self.push_u32(handle);
        self.push_u8(1); // stencil_enabled = true
        self.push_u8(compare_fn);
        self.push_u8(front_pass_op);
        self.push_u8(front_fail_op);
        // Back-face ops (bytes 8..11). Hypervisor reads these if size >= 12.
        self.push_u8(compare_fn);
        self.push_u8(back_pass_op);
        self.push_u8(back_fail_op);
        self.push_u8(0); // padding
    }

    /// Create a texture sampler.
    pub fn create_sampler(&mut self, handle: u32, min_filter: u8, mag_filter: u8) {
        self.push_header(CMD_CREATE_SAMPLER, 8);
        self.push_u32(handle);
        self.push_u8(min_filter);
        self.push_u8(mag_filter);
        self.push_u8(0); // padding
        self.push_u8(0);
    }

    /// Create a texture.
    pub fn create_texture(
        &mut self,
        handle: u32,
        width: u16,
        height: u16,
        format: u8,
        sample_count: u8,
        usage: u8,
    ) {
        self.push_header(CMD_CREATE_TEXTURE, 12);
        self.push_u32(handle);
        self.push_u16(width);
        self.push_u16(height);
        self.push_u8(format);
        self.push_u8(0); // texture type (reserved)
        self.push_u8(sample_count);
        self.push_u8(usage);
    }

    /// Upload pixel data to a texture region.
    pub fn upload_texture(
        &mut self,
        handle: u32,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        bytes_per_row: u32,
        pixel_data: &[u8],
    ) {
        let payload_size = 16 + pixel_data.len() as u32;
        self.push_header(CMD_UPLOAD_TEXTURE, payload_size);
        self.push_u32(handle);
        self.push_u16(x);
        self.push_u16(y);
        self.push_u16(width);
        self.push_u16(height);
        self.push_u32(bytes_per_row);
        self.push_bytes(pixel_data);
    }

    // ── Render commands ─────────────────────────────────────────────────

    /// Begin a render pass.
    pub fn begin_render_pass(
        &mut self,
        color_texture: u32,
        resolve_texture: u32,
        stencil_texture: u32,
        load_action: u8,
        store_action: u8,
        clear_r: f32,
        clear_g: f32,
        clear_b: f32,
        clear_a: f32,
    ) {
        self.push_header(CMD_BEGIN_RENDER_PASS, 32);
        self.push_u32(color_texture);
        self.push_u32(resolve_texture);
        self.push_u32(stencil_texture);
        self.push_u8(load_action);
        self.push_u8(store_action);
        self.push_u8(0); // stencil load
        self.push_u8(0); // stencil store
        self.push_f32(clear_r);
        self.push_f32(clear_g);
        self.push_f32(clear_b);
        self.push_f32(clear_a);
    }

    /// End the current render pass.
    pub fn end_render_pass(&mut self) {
        self.push_header(CMD_END_RENDER_PASS, 0);
    }

    /// Set the active render pipeline.
    pub fn set_render_pipeline(&mut self, handle: u32) {
        self.push_header(CMD_SET_RENDER_PIPELINE, 4);
        self.push_u32(handle);
    }

    /// Set the active depth/stencil state.
    pub fn set_depth_stencil_state(&mut self, handle: u32) {
        self.push_header(CMD_SET_DEPTH_STENCIL_STATE, 4);
        self.push_u32(handle);
    }

    /// Set the stencil reference value.
    pub fn set_stencil_ref(&mut self, value: u32) {
        self.push_header(CMD_SET_STENCIL_REF, 4);
        self.push_u32(value);
    }

    /// Set the scissor rectangle (pixel coordinates).
    pub fn set_scissor(&mut self, x: u16, y: u16, width: u16, height: u16) {
        self.push_header(CMD_SET_SCISSOR, 8);
        self.push_u16(x);
        self.push_u16(y);
        self.push_u16(width);
        self.push_u16(height);
    }

    /// Set inline vertex data.
    pub fn set_vertex_bytes(&mut self, index: u8, data: &[u8]) {
        let payload_size = 8 + data.len() as u32;
        self.push_header(CMD_SET_VERTEX_BYTES, payload_size);
        self.push_u8(index);
        self.push_u8(0);
        self.push_u16(0);
        self.push_u32(data.len() as u32);
        self.push_bytes(data);
    }

    /// Bind a texture to a fragment shader slot.
    pub fn set_fragment_texture(&mut self, handle: u32, index: u8) {
        self.push_header(CMD_SET_FRAGMENT_TEXTURE, 8);
        self.push_u32(handle);
        self.push_u8(index);
        self.push_u8(0);
        self.push_u16(0);
    }

    /// Set inline fragment shader data (uniform buffer).
    pub fn set_fragment_bytes(&mut self, index: u8, data: &[u8]) {
        let payload_size = 8 + data.len() as u32;
        self.push_header(CMD_SET_FRAGMENT_BYTES, payload_size);
        self.push_u8(index);
        self.push_u8(0);
        self.push_u16(0);
        self.push_u32(data.len() as u32);
        self.push_bytes(data);
    }

    /// Bind a sampler to a fragment shader slot.
    pub fn set_fragment_sampler(&mut self, handle: u32, index: u8) {
        self.push_header(CMD_SET_FRAGMENT_SAMPLER, 8);
        self.push_u32(handle);
        self.push_u8(index);
        self.push_u8(0);
        self.push_u16(0);
    }

    /// Draw primitives.
    pub fn draw_primitives(&mut self, primitive_type: u8, vertex_start: u32, vertex_count: u32) {
        self.push_header(CMD_DRAW_PRIMITIVES, 12);
        self.push_u8(primitive_type);
        self.push_u8(0);
        self.push_u16(0);
        self.push_u32(vertex_start);
        self.push_u32(vertex_count);
    }

    /// Begin a compute pass.
    pub fn begin_compute_pass(&mut self) {
        self.push_header(CMD_BEGIN_COMPUTE_PASS, 0);
    }

    /// End the current compute pass.
    pub fn end_compute_pass(&mut self) {
        self.push_header(CMD_END_COMPUTE_PASS, 0);
    }

    /// Set the active compute pipeline.
    pub fn set_compute_pipeline(&mut self, handle: u32) {
        self.push_header(CMD_SET_COMPUTE_PIPELINE, 4);
        self.push_u32(handle);
    }

    /// Bind a texture to a compute shader slot.
    pub fn set_compute_texture(&mut self, handle: u32, index: u8) {
        self.push_header(CMD_SET_COMPUTE_TEXTURE, 8);
        self.push_u32(handle);
        self.push_u8(index);
        self.push_u8(0);
        self.push_u16(0);
    }

    /// Set inline compute buffer data.
    pub fn set_compute_bytes(&mut self, index: u8, data: &[u8]) {
        let payload_size = 8 + data.len() as u32;
        self.push_header(CMD_SET_COMPUTE_BYTES, payload_size);
        self.push_u8(index);
        self.push_u8(0);
        self.push_u16(0);
        self.push_u32(data.len() as u32);
        self.push_bytes(data);
    }

    /// Dispatch compute threads.
    pub fn dispatch_threads(
        &mut self,
        grid_x: u16,
        grid_y: u16,
        grid_z: u16,
        tg_x: u16,
        tg_y: u16,
        tg_z: u16,
    ) {
        self.push_header(CMD_DISPATCH_THREADS, 12);
        self.push_u16(grid_x);
        self.push_u16(grid_y);
        self.push_u16(grid_z);
        self.push_u16(tg_x);
        self.push_u16(tg_y);
        self.push_u16(tg_z);
    }

    /// Begin a blit pass.
    pub fn begin_blit_pass(&mut self) {
        self.push_header(CMD_BEGIN_BLIT_PASS, 0);
    }

    /// End the current blit pass.
    pub fn end_blit_pass(&mut self) {
        self.push_header(CMD_END_BLIT_PASS, 0);
    }

    /// Copy a region from one texture to another.
    pub fn copy_texture_region(
        &mut self,
        src: u32,
        dst: u32,
        src_x: u16,
        src_y: u16,
        src_w: u16,
        src_h: u16,
        dst_x: u16,
        dst_y: u16,
    ) {
        self.push_header(CMD_COPY_TEXTURE_REGION, 20);
        self.push_u32(src);
        self.push_u32(dst);
        self.push_u16(src_x);
        self.push_u16(src_y);
        self.push_u16(src_w);
        self.push_u16(src_h);
        self.push_u16(dst_x);
        self.push_u16(dst_y);
        self.push_u16(0); // padding
        self.push_u16(0);
    }

    /// Present and commit the frame.
    pub fn present_and_commit(&mut self) {
        self.push_header(CMD_PRESENT_AND_COMMIT, 0);
    }
}

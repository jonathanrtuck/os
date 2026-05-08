#![no_std]
#![allow(clippy::too_many_arguments)]

//! Metal-over-virtio command protocol — wire format constants.
//!
//! The guest metal-render driver serializes Metal API calls into a flat
//! command buffer and sends it over virtio device 22. Each command is
//! an 8-byte header (method\_id, flags, payload\_size) followed by payload.
//!
//! The host deserializes and replays them via the Metal API.
//!
//! Handle model: the guest pre-assigns u32 IDs for all Metal objects.
//! The host maps IDs to real Metal objects.
//!
//! Two virtqueues:
//!   - Queue 0 (setup): shader compilation, pipeline/texture creation
//!   - Queue 1 (render): per-frame command buffers

// ── Command header ──────────────────────────────────────────────────

pub const HEADER_SIZE: usize = 8;

// ── Setup commands (virtqueue 0) ────────────────────────────────────

pub const CMD_COMPILE_LIBRARY: u16 = 0x0001;
pub const CMD_GET_FUNCTION: u16 = 0x0002;
pub const CMD_CREATE_RENDER_PIPELINE: u16 = 0x0010;
pub const CMD_CREATE_TEXTURE: u16 = 0x0020;
pub const CMD_UPLOAD_TEXTURE: u16 = 0x0021;
pub const CMD_DESTROY_OBJECT: u16 = 0x00FF;

// ── Render commands (virtqueue 1) ───────────────────────────────────

pub const CMD_BEGIN_RENDER_PASS: u16 = 0x0100;
pub const CMD_END_RENDER_PASS: u16 = 0x0101;
pub const CMD_SET_RENDER_PIPELINE: u16 = 0x0110;
pub const CMD_SET_SCISSOR: u16 = 0x0113;
pub const CMD_SET_VERTEX_BYTES: u16 = 0x0120;
pub const CMD_SET_FRAGMENT_TEXTURE: u16 = 0x0121;
pub const CMD_SET_FRAGMENT_SAMPLER: u16 = 0x0122;
pub const CMD_SET_FRAGMENT_BYTES: u16 = 0x0123;
pub const CMD_DRAW_PRIMITIVES: u16 = 0x0130;
pub const CMD_PRESENT_AND_COMMIT: u16 = 0x0F00;

// ── Special handles ─────────────────────────────────────────────────

pub const DRAWABLE_HANDLE: u32 = 0xFFFF_FFFF;

// ── Pixel formats ───────────────────────────────────────────────────

pub const PIXEL_FORMAT_BGRA8_SRGB: u8 = 6;

// ── Load/store actions ──────────────────────────────────────────────

pub const LOAD_CLEAR: u8 = 2;
pub const STORE_STORE: u8 = 1;

// ── Primitive types ─────────────────────────────────────────────────

pub const PRIM_TRIANGLE: u8 = 0;

// ── Virtqueue indices ───────────────────────────────────────────────

pub const VIRTQ_SETUP: u32 = 0;
pub const VIRTQ_RENDER: u32 = 1;

// ── Command buffer writer (no_std, no alloc) ────────────────────────

pub struct CommandWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
    overflow: bool,
}

impl<'a> CommandWriter<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            pos: 0,
            overflow: false,
        }
    }

    pub fn len(&self) -> usize {
        self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.pos == 0
    }

    pub fn has_overflow(&self) -> bool {
        self.overflow
    }

    fn put_u8(&mut self, v: u8) {
        if self.overflow || self.pos >= self.buf.len() {
            self.overflow = true;

            return;
        }

        self.buf[self.pos] = v;
        self.pos += 1;
    }

    fn put_u16(&mut self, v: u16) {
        if self.overflow || self.pos + 2 > self.buf.len() {
            self.overflow = true;

            return;
        }

        let b = v.to_le_bytes();

        self.buf[self.pos..self.pos + 2].copy_from_slice(&b);
        self.pos += 2;
    }

    fn put_u32(&mut self, v: u32) {
        if self.overflow || self.pos + 4 > self.buf.len() {
            self.overflow = true;

            return;
        }

        let b = v.to_le_bytes();

        self.buf[self.pos..self.pos + 4].copy_from_slice(&b);
        self.pos += 4;
    }

    fn put_f32(&mut self, v: f32) {
        self.put_u32(v.to_bits());
    }

    fn put_bytes(&mut self, data: &[u8]) {
        if self.overflow || self.pos + data.len() > self.buf.len() {
            self.overflow = true;

            return;
        }

        self.buf[self.pos..self.pos + data.len()].copy_from_slice(data);
        self.pos += data.len();
    }

    fn header(&mut self, method: u16, payload_size: u32) {
        self.put_u16(method);
        self.put_u16(0);
        self.put_u32(payload_size);
    }

    pub fn compile_library(&mut self, handle: u32, source: &[u8]) {
        self.header(CMD_COMPILE_LIBRARY, 8 + source.len() as u32);
        self.put_u32(handle);
        self.put_u32(source.len() as u32);
        self.put_bytes(source);
    }

    pub fn get_function(&mut self, handle: u32, library: u32, name: &[u8]) {
        self.header(CMD_GET_FUNCTION, 12 + name.len() as u32);
        self.put_u32(handle);
        self.put_u32(library);
        self.put_u32(name.len() as u32);
        self.put_bytes(name);
    }

    pub fn create_render_pipeline(
        &mut self,
        handle: u32,
        vertex_fn: u32,
        fragment_fn: u32,
        blend_enabled: bool,
        color_write_mask: u8,
        has_stencil: bool,
        sample_count: u8,
        pixel_format: u8,
    ) {
        self.header(CMD_CREATE_RENDER_PIPELINE, 17);
        self.put_u32(handle);
        self.put_u32(vertex_fn);
        self.put_u32(fragment_fn);
        self.put_u8(blend_enabled as u8);
        self.put_u8(color_write_mask);
        self.put_u8(has_stencil as u8);
        self.put_u8(sample_count);
        self.put_u8(pixel_format);
    }

    pub fn begin_render_pass(
        &mut self,
        color_texture: u32,
        resolve_texture: u32,
        stencil_texture: u32,
        load_action: u8,
        store_action: u8,
        stencil_load: u8,
        stencil_store: u8,
        clear_r: f32,
        clear_g: f32,
        clear_b: f32,
        clear_a: f32,
    ) {
        self.header(CMD_BEGIN_RENDER_PASS, 32);
        self.put_u32(color_texture);
        self.put_u32(resolve_texture);
        self.put_u32(stencil_texture);
        self.put_u8(load_action);
        self.put_u8(store_action);
        self.put_u8(stencil_load);
        self.put_u8(stencil_store);
        self.put_f32(clear_r);
        self.put_f32(clear_g);
        self.put_f32(clear_b);
        self.put_f32(clear_a);
    }

    pub fn end_render_pass(&mut self) {
        self.header(CMD_END_RENDER_PASS, 0);
    }

    pub fn set_render_pipeline(&mut self, handle: u32) {
        self.header(CMD_SET_RENDER_PIPELINE, 4);
        self.put_u32(handle);
    }

    pub fn set_vertex_bytes(&mut self, buffer_index: u8, data: &[u8]) {
        self.header(CMD_SET_VERTEX_BYTES, 8 + data.len() as u32);
        self.put_u8(buffer_index);
        self.put_u8(0);
        self.put_u16(0);
        self.put_u32(data.len() as u32);
        self.put_bytes(data);
    }

    pub fn draw_primitives(&mut self, primitive_type: u8, vertex_start: u32, vertex_count: u32) {
        self.header(CMD_DRAW_PRIMITIVES, 12);
        self.put_u8(primitive_type);
        self.put_u8(0);
        self.put_u16(0);
        self.put_u32(vertex_start);
        self.put_u32(vertex_count);
    }

    pub fn present_and_commit(&mut self, frame_id: u32) {
        self.header(CMD_PRESENT_AND_COMMIT, 4);
        self.put_u32(frame_id);
    }

    pub fn set_scissor(&mut self, x: u32, y: u32, w: u32, h: u32) {
        self.header(CMD_SET_SCISSOR, 16);
        self.put_u32(x);
        self.put_u32(y);
        self.put_u32(w);
        self.put_u32(h);
    }
}

// ── Compositor IPC protocol ────────────────────────────────────────
//
// Sync call/reply between the presenter and the render driver (compositor).
// The Metal command protocol above is between the compositor and the GPU.

pub mod comp {
    pub use ipc::MAX_PAYLOAD;

    /// Presenter sends scene graph VMO handle → compositor maps it RO.
    /// Reply includes display dimensions.
    pub const SETUP: u32 = 1;

    /// Trigger scene graph read + GPU frame render.
    pub const RENDER: u32 = 2;

    /// Query display dimensions and frame count.
    pub const GET_INFO: u32 = 3;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SetupReply {
        pub display_width: u32,
        pub display_height: u32,
    }

    impl SetupReply {
        pub const SIZE: usize = 8;

        pub fn write_to(&self, buf: &mut [u8]) {
            buf[0..4].copy_from_slice(&self.display_width.to_le_bytes());
            buf[4..8].copy_from_slice(&self.display_height.to_le_bytes());
        }

        #[must_use]
        pub fn read_from(buf: &[u8]) -> Self {
            Self {
                display_width: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
                display_height: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct InfoReply {
        pub display_width: u32,
        pub display_height: u32,
        pub frame_count: u32,
    }

    impl InfoReply {
        pub const SIZE: usize = 12;

        pub fn write_to(&self, buf: &mut [u8]) {
            buf[0..4].copy_from_slice(&self.display_width.to_le_bytes());
            buf[4..8].copy_from_slice(&self.display_height.to_le_bytes());
            buf[8..12].copy_from_slice(&self.frame_count.to_le_bytes());
        }

        #[must_use]
        pub fn read_from(buf: &[u8]) -> Self {
            Self {
                display_width: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
                display_height: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
                frame_count: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn setup_reply_round_trip() {
            let reply = SetupReply {
                display_width: 1440,
                display_height: 900,
            };
            let mut buf = [0u8; SetupReply::SIZE];

            reply.write_to(&mut buf);

            assert_eq!(SetupReply::read_from(&buf), reply);
        }

        #[test]
        fn info_reply_round_trip() {
            let reply = InfoReply {
                display_width: 1440,
                display_height: 900,
                frame_count: 42,
            };
            let mut buf = [0u8; InfoReply::SIZE];

            reply.write_to(&mut buf);

            assert_eq!(InfoReply::read_from(&buf), reply);
        }

        #[test]
        fn method_ids_distinct() {
            assert_ne!(SETUP, RENDER);
            assert_ne!(SETUP, GET_INFO);
            assert_ne!(RENDER, GET_INFO);
        }

        #[test]
        fn sizes_fit_payload() {
            assert!(SetupReply::SIZE <= MAX_PAYLOAD);
            assert!(InfoReply::SIZE <= MAX_PAYLOAD);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_header_size() {
        assert_eq!(HEADER_SIZE, 8);
    }

    #[test]
    fn drawable_handle_sentinel() {
        assert_eq!(DRAWABLE_HANDLE, 0xFFFF_FFFF);
    }

    #[test]
    fn command_writer_begin_render_pass() {
        let mut buf = [0u8; 256];
        let mut w = CommandWriter::new(&mut buf);

        w.begin_render_pass(
            DRAWABLE_HANDLE,
            0,
            0,
            LOAD_CLEAR,
            STORE_STORE,
            0,
            0,
            0.2,
            0.2,
            0.2,
            1.0,
        );
        w.end_render_pass();
        w.present_and_commit(0);

        assert_eq!(
            u16::from_le_bytes(buf[0..2].try_into().unwrap()),
            CMD_BEGIN_RENDER_PASS
        );

        let pass_payload_size = u32::from_le_bytes(buf[4..8].try_into().unwrap());

        assert_eq!(pass_payload_size, 32);

        let color_tex = u32::from_le_bytes(buf[8..12].try_into().unwrap());

        assert_eq!(color_tex, DRAWABLE_HANDLE);
    }

    #[test]
    fn command_writer_compile_library() {
        let mut buf = [0u8; 256];
        let mut w = CommandWriter::new(&mut buf);
        let source = b"test source";

        w.compile_library(1, source);

        let method = u16::from_le_bytes(buf[0..2].try_into().unwrap());

        assert_eq!(method, CMD_COMPILE_LIBRARY);

        let payload_size = u32::from_le_bytes(buf[4..8].try_into().unwrap());

        assert_eq!(payload_size, 8 + source.len() as u32);

        let handle = u32::from_le_bytes(buf[8..12].try_into().unwrap());

        assert_eq!(handle, 1);
    }

    #[test]
    fn command_writer_draw_pipeline() {
        let mut buf = [0u8; 512];
        let mut w = CommandWriter::new(&mut buf);

        w.create_render_pipeline(10, 2, 3, false, 0xF, false, 1, PIXEL_FORMAT_BGRA8_SRGB);

        let method = u16::from_le_bytes(buf[0..2].try_into().unwrap());

        assert_eq!(method, CMD_CREATE_RENDER_PIPELINE);

        let payload_size = u32::from_le_bytes(buf[4..8].try_into().unwrap());

        assert_eq!(payload_size, 17);
    }

    #[test]
    fn command_writer_vertex_bytes() {
        let mut buf = [0u8; 256];
        let mut w = CommandWriter::new(&mut buf);
        let verts = [0u8; 96];

        w.set_vertex_bytes(0, &verts);

        let method = u16::from_le_bytes(buf[0..2].try_into().unwrap());

        assert_eq!(method, CMD_SET_VERTEX_BYTES);

        let payload_size = u32::from_le_bytes(buf[4..8].try_into().unwrap());

        assert_eq!(payload_size, 8 + 96);
    }

    #[test]
    fn command_writer_full_frame_sequence() {
        let mut buf = [0u8; 512];
        let mut w = CommandWriter::new(&mut buf);

        w.begin_render_pass(
            DRAWABLE_HANDLE,
            0,
            0,
            LOAD_CLEAR,
            STORE_STORE,
            0,
            0,
            0.0,
            0.0,
            0.0,
            1.0,
        );
        w.set_render_pipeline(10);
        w.draw_primitives(PRIM_TRIANGLE, 0, 6);
        w.end_render_pass();
        w.present_and_commit(1);

        // begin_render_pass(8+32) + set_pipeline(8+4) + draw(8+12) + end(8) + present(8+4)
        assert_eq!(w.len(), 40 + 12 + 20 + 8 + 12);
    }

    #[test]
    fn command_writer_overflow_detected() {
        let mut buf = [0u8; 16];
        let mut w = CommandWriter::new(&mut buf);

        assert!(!w.has_overflow());
        assert!(w.is_empty());

        w.compile_library(1, b"this source is way too long for 16 bytes");

        assert!(w.has_overflow());
    }

    #[test]
    fn command_ids_distinct() {
        let setup = [
            CMD_COMPILE_LIBRARY,
            CMD_GET_FUNCTION,
            CMD_CREATE_RENDER_PIPELINE,
            CMD_CREATE_TEXTURE,
            CMD_UPLOAD_TEXTURE,
            CMD_DESTROY_OBJECT,
        ];
        let render = [
            CMD_BEGIN_RENDER_PASS,
            CMD_END_RENDER_PASS,
            CMD_SET_RENDER_PIPELINE,
            CMD_SET_SCISSOR,
            CMD_SET_VERTEX_BYTES,
            CMD_SET_FRAGMENT_TEXTURE,
            CMD_SET_FRAGMENT_SAMPLER,
            CMD_SET_FRAGMENT_BYTES,
            CMD_DRAW_PRIMITIVES,
            CMD_PRESENT_AND_COMMIT,
        ];

        for i in 0..setup.len() {
            for j in (i + 1)..setup.len() {
                assert_ne!(setup[i], setup[j], "duplicate setup cmd");
            }
        }
        for i in 0..render.len() {
            for j in (i + 1)..render.len() {
                assert_ne!(render[i], render[j], "duplicate render cmd");
            }
        }
    }
}

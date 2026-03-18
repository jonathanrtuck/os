//! Host-side tests for Virgl protocol encoding.

use protocol::virgl::*;

#[test]
fn cmd_header_encoding() {
    // VIRGL_CMD0(cmd=7, obj=0, len=8) for CLEAR
    let hdr = virgl_cmd0(VIRGL_CCMD_CLEAR, 0, 8);
    assert_eq!(hdr, 0x00_08_00_07);
    assert_eq!(hdr & 0xFF, VIRGL_CCMD_CLEAR);
    assert_eq!((hdr >> 8) & 0xFF, 0); // object type
    assert_eq!((hdr >> 16) & 0xFFFF, 8); // payload length in dwords
}

#[test]
fn cmd_header_with_object_type() {
    // CREATE_OBJECT with BLEND type, 10 dwords payload
    let hdr = virgl_cmd0(VIRGL_CCMD_CREATE_OBJECT, VIRGL_OBJECT_BLEND, 10);
    assert_eq!(hdr & 0xFF, VIRGL_CCMD_CREATE_OBJECT);
    assert_eq!((hdr >> 8) & 0xFF, VIRGL_OBJECT_BLEND);
    assert_eq!((hdr >> 16) & 0xFFFF, 10);
}

#[test]
fn command_buffer_clear() {
    let mut buf = CommandBuffer::new();
    buf.cmd_clear(0.2, 0.2, 0.25, 1.0); // dark background
    let words = buf.as_dwords();
    assert_eq!(words.len(), 9); // 1 header + 8 payload
    assert_eq!(words[0] & 0xFF, VIRGL_CCMD_CLEAR);
    assert_eq!(words[1], PIPE_CLEAR_COLOR0);
    // Verify float encoding
    assert_eq!(words[2], 0.2_f32.to_bits());
}

#[test]
fn command_buffer_draw_vbo() {
    let mut buf = CommandBuffer::new();
    buf.cmd_draw_vbo(0, 6, PIPE_PRIM_TRIANGLES, false);
    let words = buf.as_dwords();
    assert_eq!(words.len(), 13); // 1 header + 12 payload
    assert_eq!(words[0] & 0xFF, VIRGL_CCMD_DRAW_VBO);
    assert_eq!(words[1], 0); // start
    assert_eq!(words[2], 6); // count
    assert_eq!(words[3], PIPE_PRIM_TRIANGLES);
    assert_eq!(words[4], 0); // not indexed
    assert_eq!(words[5], 1); // instance_count
}

#[test]
fn command_buffer_multiple_commands() {
    let mut buf = CommandBuffer::new();
    buf.cmd_clear(0.0, 0.0, 0.0, 1.0);
    let before = buf.as_dwords().len();
    buf.cmd_draw_vbo(0, 3, PIPE_PRIM_TRIANGLES, false);
    assert_eq!(buf.as_dwords().len(), before + 13);
}

#[test]
fn command_buffer_set_framebuffer() {
    let mut buf = CommandBuffer::new();
    buf.cmd_set_framebuffer_state(1, 0); // 1 color buffer (handle=1), no depth
    let words = buf.as_dwords();
    assert_eq!(words.len(), 4); // header + nr_cbufs + zsurf + cbuf[0]
    assert_eq!(words[0] & 0xFF, VIRGL_CCMD_SET_FRAMEBUFFER_STATE);
}

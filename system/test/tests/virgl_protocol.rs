//! Host-side tests for Virgl protocol encoding.

use protocol::virgl::*;

// Include the virgil-render driver's shader definitions directly.
// shaders.rs is pure const data with no runtime dependencies and no
// use of std, so it compiles cleanly in the host test environment.
#[path = "../../services/drivers/virgil-render/shaders.rs"]
mod shaders;

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

// ── Shader definition tests ──────────────────────────────────────────────

#[test]
fn color_vertex_shader_valid() {
    let text = shaders::COLOR_VS;
    assert!(!text.is_empty(), "COLOR_VS must be non-empty");
    // Must be null-terminated (virglrenderer checks for '\0' in the last 4 bytes)
    assert_eq!(text[text.len() - 1], 0, "COLOR_VS must be null-terminated");
    // Must start with VERT keyword
    assert!(
        text.starts_with(b"VERT\n"),
        "COLOR_VS must start with 'VERT\\n'"
    );
    // Must be valid UTF-8 (excluding the null terminator)
    let src = core::str::from_utf8(&text[..text.len() - 1]).expect("COLOR_VS must be valid UTF-8");
    assert!(src.contains("DCL IN[0]"), "must declare IN[0]");
    assert!(
        src.contains("DCL OUT[0], POSITION"),
        "must declare POSITION output"
    );
    assert!(src.contains("END"), "must have END instruction");
}

#[test]
fn color_fragment_shader_valid() {
    let text = shaders::COLOR_FS;
    assert!(!text.is_empty(), "COLOR_FS must be non-empty");
    assert_eq!(text[text.len() - 1], 0, "COLOR_FS must be null-terminated");
    assert!(
        text.starts_with(b"FRAG\n"),
        "COLOR_FS must start with 'FRAG\\n'"
    );
    let src = core::str::from_utf8(&text[..text.len() - 1]).expect("COLOR_FS must be valid UTF-8");
    assert!(src.contains("DCL IN[0], COLOR"), "must declare COLOR input");
    assert!(
        src.contains("DCL OUT[0], COLOR"),
        "must declare COLOR output"
    );
    assert!(src.contains("END"), "must have END instruction");
}

#[test]
fn textured_vertex_shader_valid() {
    let text = shaders::TEXTURED_VS;
    assert!(!text.is_empty(), "TEXTURED_VS must be non-empty");
    assert_eq!(
        text[text.len() - 1],
        0,
        "TEXTURED_VS must be null-terminated"
    );
    assert!(
        text.starts_with(b"VERT\n"),
        "TEXTURED_VS must start with 'VERT\\n'"
    );
    let src =
        core::str::from_utf8(&text[..text.len() - 1]).expect("TEXTURED_VS must be valid UTF-8");
    assert!(
        src.contains("GENERIC[0]"),
        "must pass texcoord as GENERIC[0]"
    );
    assert!(src.contains("END"), "must have END instruction");
}

#[test]
fn textured_fragment_shader_valid() {
    let text = shaders::TEXTURED_FS;
    assert!(!text.is_empty(), "TEXTURED_FS must be non-empty");
    assert_eq!(
        text[text.len() - 1],
        0,
        "TEXTURED_FS must be null-terminated"
    );
    assert!(
        text.starts_with(b"FRAG\n"),
        "TEXTURED_FS must start with 'FRAG\\n'"
    );
    let src =
        core::str::from_utf8(&text[..text.len() - 1]).expect("TEXTURED_FS must be valid UTF-8");
    assert!(src.contains("DCL SAMP[0]"), "must declare sampler");
    assert!(src.contains("TEX"), "must have TEX instruction");
    assert!(src.contains("MUL"), "must multiply texel by vertex color");
    assert!(src.contains("2D"), "must sample a 2D texture");
    assert!(src.contains("END"), "must have END instruction");
}

#[test]
fn cmd_create_shader_text_encoding() {
    // Verify that cmd_create_shader_text produces the correct wire encoding.
    // The command structure is:
    //   DW0: virgl_cmd0 header (CREATE_OBJECT | SHADER | payload_len)
    //   DW1: handle
    //   DW2: shader_type
    //   DW3: offlen = byte_length_of_text (bits [30:0], bit 31 = 0)
    //   DW4: num_tokens = 300
    //   DW5: num_so_outputs = 0
    //   DW6..: text bytes packed as little-endian u32s
    let mut buf = CommandBuffer::new();
    buf.cmd_create_shader_text(1, PIPE_SHADER_VERTEX, shaders::COLOR_VS);
    let words = buf.as_dwords();

    // Header
    assert_eq!(
        words[0] & 0xFF,
        VIRGL_CCMD_CREATE_OBJECT,
        "cmd must be CREATE_OBJECT"
    );
    assert_eq!(
        (words[0] >> 8) & 0xFF,
        VIRGL_OBJECT_SHADER,
        "obj must be SHADER"
    );

    // Fixed fields
    assert_eq!(words[1], 1, "handle");
    assert_eq!(words[2], PIPE_SHADER_VERTEX, "shader type");

    // offlen = total byte length including null terminator, bit 31 = 0
    let offlen = words[3];
    assert_eq!(
        offlen & 0x8000_0000,
        0,
        "continuation bit must be clear (new shader)"
    );
    assert_eq!(
        offlen as usize,
        shaders::COLOR_VS.len(),
        "offlen must equal text byte length (including null)"
    );

    // num_tokens hint
    assert_eq!(words[4], 300, "num_tokens allocation hint");

    // num_so_outputs
    assert_eq!(words[5], 0, "no stream outputs");

    // payload_len in header = 5 (fixed fields) + ceil(text_len / 4)
    let text_dwords = (shaders::COLOR_VS.len() + 3) / 4;
    let expected_payload = 5 + text_dwords as u32;
    assert_eq!(
        (words[0] >> 16) & 0xFFFF,
        expected_payload,
        "payload dword count in header"
    );

    // Verify the packed bytes round-trip back to the original text.
    // Collect packed DWORDs and extract bytes.
    let packed_dwords = &words[6..];
    let mut recovered: Vec<u8> = Vec::new();
    for &dw in packed_dwords {
        recovered.push((dw & 0xFF) as u8);
        recovered.push(((dw >> 8) & 0xFF) as u8);
        recovered.push(((dw >> 16) & 0xFF) as u8);
        recovered.push(((dw >> 24) & 0xFF) as u8);
    }
    // The original text must be a prefix of the recovered bytes
    // (last DWORD may be zero-padded).
    assert!(
        recovered.starts_with(shaders::COLOR_VS),
        "packed bytes must round-trip to original text"
    );
}

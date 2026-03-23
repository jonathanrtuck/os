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

// ── DSA state encoding tests ────────────────────────────────────────

#[test]
fn cmd_create_dsa_basic_encoding() {
    // Default DSA: depth disabled, no stencil, depth_func=ALWAYS.
    let mut buf = CommandBuffer::new();
    buf.cmd_create_dsa(42);
    let words = buf.as_dwords();

    // Header: CREATE_OBJECT, DSA, 5 dwords payload
    assert_eq!(words[0] & 0xFF, VIRGL_CCMD_CREATE_OBJECT);
    assert_eq!((words[0] >> 8) & 0xFF, VIRGL_OBJECT_DSA);
    assert_eq!((words[0] >> 16) & 0xFFFF, 5);

    assert_eq!(words[1], 42, "handle");

    // S0: depth_enabled=0, depth_writemask=0, depth_func=ALWAYS(7) at bits 2-4
    // ALWAYS=7, shifted left by 2 = 28 = 0x1C
    let s0 = words[2];
    let depth_enabled = s0 & 1;
    let depth_writemask = (s0 >> 1) & 1;
    let depth_func = (s0 >> 2) & 7;
    assert_eq!(depth_enabled, 0, "depth should be disabled");
    assert_eq!(depth_writemask, 0, "depth write should be disabled");
    assert_eq!(depth_func, PIPE_FUNC_ALWAYS, "depth_func should be ALWAYS");

    // S1, S2: stencil disabled (0)
    assert_eq!(words[3], 0, "stencil[0] should be 0 (disabled)");
    assert_eq!(words[4], 0, "stencil[1] should be 0 (disabled)");

    // S3: alpha ref + alpha func = 0
    assert_eq!(words[5], 0, "alpha state should be 0");
}

#[test]
fn cmd_create_dsa_stencil_write_encoding() {
    // Stencil write DSA: depth disabled, front=incr_wrap, back=decr_wrap.
    let mut buf = CommandBuffer::new();
    buf.cmd_create_dsa_stencil_write(10);
    let words = buf.as_dwords();

    assert_eq!(words[1], 10, "handle");
    assert_eq!(words[2], 0, "S0: depth disabled entirely");

    // S1: stencil[0] (front face)
    let s1 = words[3];
    let enabled = s1 & 1;
    let func = (s1 >> 1) & 7;
    let fail_op = (s1 >> 4) & 7;
    let zpass_op = (s1 >> 7) & 7;
    let zfail_op = (s1 >> 10) & 7;
    let valuemask = (s1 >> 13) & 0xFF;
    let writemask = (s1 >> 21) & 0xFF;
    assert_eq!(enabled, 1, "front stencil should be enabled");
    assert_eq!(func, PIPE_FUNC_ALWAYS, "front func should be ALWAYS");
    assert_eq!(
        fail_op, PIPE_STENCIL_OP_KEEP,
        "front fail_op should be KEEP"
    );
    assert_eq!(
        zpass_op, PIPE_STENCIL_OP_INCR_WRAP,
        "front zpass_op should be INCR_WRAP"
    );
    assert_eq!(
        zfail_op, PIPE_STENCIL_OP_KEEP,
        "front zfail_op should be KEEP"
    );
    assert_eq!(valuemask, 0xFF, "front valuemask should be 0xFF");
    assert_eq!(writemask, 0xFF, "front writemask should be 0xFF");

    // S2: stencil[1] (back face) — same except DECR_WRAP
    let s2 = words[4];
    let back_enabled = s2 & 1;
    let back_func = (s2 >> 1) & 7;
    let back_zpass_op = (s2 >> 7) & 7;
    assert_eq!(back_enabled, 1, "back stencil should be enabled");
    assert_eq!(back_func, PIPE_FUNC_ALWAYS, "back func should be ALWAYS");
    assert_eq!(
        back_zpass_op, PIPE_STENCIL_OP_DECR_WRAP,
        "back zpass_op should be DECR_WRAP"
    );

    assert_eq!(words[5], 0, "S3: alpha should be 0");
}

#[test]
fn cmd_create_dsa_stencil_test_encoding() {
    // Stencil test DSA: pass where stencil != 0, zero on pass.
    let mut buf = CommandBuffer::new();
    buf.cmd_create_dsa_stencil_test(20);
    let words = buf.as_dwords();

    assert_eq!(words[1], 20, "handle");
    assert_eq!(words[2], 0, "S0: depth disabled");

    // S1 and S2 should be identical (front and back same).
    assert_eq!(words[3], words[4], "front and back stencil should match");

    let face = words[3];
    let enabled = face & 1;
    let func = (face >> 1) & 7;
    let fail_op = (face >> 4) & 7;
    let zpass_op = (face >> 7) & 7;
    let zfail_op = (face >> 10) & 7;
    let valuemask = (face >> 13) & 0xFF;
    let writemask = (face >> 21) & 0xFF;
    assert_eq!(enabled, 1, "stencil should be enabled");
    assert_eq!(func, PIPE_FUNC_NOTEQUAL, "func should be NOTEQUAL");
    assert_eq!(fail_op, PIPE_STENCIL_OP_KEEP, "fail_op should be KEEP");
    assert_eq!(zpass_op, PIPE_STENCIL_OP_ZERO, "zpass_op should be ZERO");
    assert_eq!(zfail_op, PIPE_STENCIL_OP_KEEP, "zfail_op should be KEEP");
    assert_eq!(valuemask, 0xFF);
    assert_eq!(writemask, 0xFF);
}

// ── Blend state encoding tests ──────────────────────────────────────

#[test]
fn cmd_create_blend_alpha_encoding() {
    // Standard Porter-Duff source-over blend.
    let mut buf = CommandBuffer::new();
    buf.cmd_create_blend(30);
    let words = buf.as_dwords();

    // Header: CREATE_OBJECT, BLEND, 11 dwords payload
    assert_eq!(words[0] & 0xFF, VIRGL_CCMD_CREATE_OBJECT);
    assert_eq!((words[0] >> 8) & 0xFF, VIRGL_OBJECT_BLEND);
    assert_eq!((words[0] >> 16) & 0xFFFF, 11);

    assert_eq!(words[1], 30, "handle");
    assert_eq!(
        words[2], 0,
        "S0: no independent blend, no logicop, no dither"
    );
    assert_eq!(words[3], 0, "S1: logicop_func = 0");

    // RT0 blend state (words[4])
    let rt0 = words[4];
    let blend_enable = rt0 & 1;
    let rgb_func = (rt0 >> 1) & 7;
    let rgb_src = (rt0 >> 4) & 0x1F;
    let rgb_dst = (rt0 >> 9) & 0x1F;
    let alpha_func = (rt0 >> 14) & 7;
    let alpha_src = (rt0 >> 17) & 0x1F;
    let alpha_dst = (rt0 >> 22) & 0x1F;
    let colormask = (rt0 >> 27) & 0xF;

    assert_eq!(blend_enable, 1, "blend should be enabled");
    assert_eq!(rgb_func, PIPE_BLEND_ADD, "rgb_func should be ADD");
    assert_eq!(
        rgb_src, PIPE_BLENDFACTOR_SRC_ALPHA,
        "rgb_src should be SRC_ALPHA"
    );
    assert_eq!(
        rgb_dst, PIPE_BLENDFACTOR_INV_SRC_ALPHA,
        "rgb_dst should be INV_SRC_ALPHA"
    );
    assert_eq!(alpha_func, PIPE_BLEND_ADD, "alpha_func should be ADD");
    assert_eq!(
        alpha_src, PIPE_BLENDFACTOR_SRC_ALPHA,
        "alpha_src should be SRC_ALPHA"
    );
    assert_eq!(
        alpha_dst, PIPE_BLENDFACTOR_INV_SRC_ALPHA,
        "alpha_dst should be INV_SRC_ALPHA"
    );
    assert_eq!(colormask, PIPE_MASK_RGBA, "colormask should be RGBA");

    // RT1-RT7 should all be zero.
    for i in 5..12 {
        assert_eq!(words[i], 0, "RT{} should be zero", i - 4);
    }
}

#[test]
fn cmd_create_blend_no_color_encoding() {
    // Blend with color writes disabled (for stencil-only pass).
    let mut buf = CommandBuffer::new();
    buf.cmd_create_blend_no_color(50);
    let words = buf.as_dwords();

    assert_eq!(words[0] & 0xFF, VIRGL_CCMD_CREATE_OBJECT);
    assert_eq!((words[0] >> 8) & 0xFF, VIRGL_OBJECT_BLEND);
    assert_eq!((words[0] >> 16) & 0xFFFF, 11);

    assert_eq!(words[1], 50, "handle");
    assert_eq!(words[2], 0, "S0");
    assert_eq!(words[3], 0, "S1 logicop");

    // RT0-RT7: all zero (blend disabled, colormask = 0).
    for i in 4..12 {
        assert_eq!(words[i], 0, "RT{}: all zero means no color write", i - 4);
    }
}

#[test]
fn cmd_create_blend_rt0_bit_packing_round_trip() {
    // Verify that the RT0 u32 value can be decoded back to the original
    // blend factors by direct bit extraction, ensuring no field overlaps.
    let mut buf = CommandBuffer::new();
    buf.cmd_create_blend(1);
    let rt0 = buf.as_dwords()[4];

    // Reconstruct the expected value.
    let expected = 1 // blend_enable
        | (PIPE_BLEND_ADD << 1)
        | (PIPE_BLENDFACTOR_SRC_ALPHA << 4)
        | (PIPE_BLENDFACTOR_INV_SRC_ALPHA << 9)
        | (PIPE_BLEND_ADD << 14)
        | (PIPE_BLENDFACTOR_SRC_ALPHA << 17)
        | (PIPE_BLENDFACTOR_INV_SRC_ALPHA << 22)
        | (PIPE_MASK_RGBA << 27);

    assert_eq!(
        rt0, expected,
        "RT0 should match reconstructed value: got {rt0:#010x}, expected {expected:#010x}"
    );
}

// ── Stencil face bit layout test ────────────────────────────────────

#[test]
fn stencil_face_bit_layout_no_overlap() {
    // Verify that setting each field to its maximum value doesn't overlap
    // with adjacent fields. Each field has a known bit width:
    //   enabled:   1 bit  (0)
    //   func:      3 bits (1-3)
    //   fail_op:   3 bits (4-6)
    //   zpass_op:  3 bits (7-9)
    //   zfail_op:  3 bits (10-12)
    //   valuemask: 8 bits (13-20)
    //   writemask: 8 bits (21-28)
    //
    // The stencil_face function is private, so we reconstruct and verify
    // through the stencil write DSA which exercises it.
    let mut buf = CommandBuffer::new();
    buf.cmd_create_dsa_stencil_write(1);
    let front = buf.as_dwords()[3];

    // Verify no bits above bit 28 are set (bits 29-31 should be 0).
    assert_eq!(
        front & 0xE000_0000,
        0,
        "bits 29-31 should be unused, got {front:#010x}"
    );

    // Verify each field is individually addressable.
    // Front face uses: ALWAYS(7), KEEP(0), INCR_WRAP(5), KEEP(0), 0xFF, 0xFF
    let expected = 1
        | (PIPE_FUNC_ALWAYS << 1)
        | (PIPE_STENCIL_OP_KEEP << 4)
        | (PIPE_STENCIL_OP_INCR_WRAP << 7)
        | (PIPE_STENCIL_OP_KEEP << 10)
        | (0xFF << 13)
        | (0xFF << 21);
    assert_eq!(
        front, expected,
        "front face encoding: got {front:#010x}, expected {expected:#010x}"
    );
}

// ── Rasterizer state tests ──────────────────────────────────────────

#[test]
fn cmd_create_rasterizer_scissor_bit() {
    let mut buf_no_scissor = CommandBuffer::new();
    buf_no_scissor.cmd_create_rasterizer(1, false);
    let s0_no = buf_no_scissor.as_dwords()[2];

    let mut buf_scissor = CommandBuffer::new();
    buf_scissor.cmd_create_rasterizer(2, true);
    let s0_yes = buf_scissor.as_dwords()[2];

    // Scissor is bit 8.
    assert_eq!(s0_no & (1 << 8), 0, "scissor should be off");
    assert_eq!(s0_yes & (1 << 8), 1 << 8, "scissor should be on");

    // Depth clip bits (1, 2) should be set in both.
    assert_eq!(s0_no & 0x06, 0x06, "depth_clip_near/far should be set");
    assert_eq!(s0_yes & 0x06, 0x06, "depth_clip_near/far should be set");
}

#[test]
fn cmd_set_stencil_ref_packing() {
    let mut buf = CommandBuffer::new();
    buf.cmd_set_stencil_ref(0x42, 0xAB);
    let words = buf.as_dwords();

    assert_eq!(words[0] & 0xFF, VIRGL_CCMD_SET_STENCIL_REF);
    let packed = words[1];
    assert_eq!(packed & 0xFF, 0x42, "front ref");
    assert_eq!((packed >> 8) & 0xFF, 0xAB, "back ref");
}

#[test]
fn cmd_clear_stencil_encoding() {
    let mut buf = CommandBuffer::new();
    buf.cmd_clear_stencil();
    let words = buf.as_dwords();

    assert_eq!(words[0] & 0xFF, VIRGL_CCMD_CLEAR);
    assert_eq!(words[1], PIPE_CLEAR_DEPTH | PIPE_CLEAR_STENCIL);
    // Color should be zero.
    assert_eq!(words[2], 0);
    assert_eq!(words[3], 0);
    assert_eq!(words[4], 0);
    assert_eq!(words[5], 0);
    // Depth = 1.0 as f64 bits split into two u32s.
    let depth_bits = 1.0f64.to_bits();
    assert_eq!(words[6], depth_bits as u32, "depth lo");
    assert_eq!(words[7], (depth_bits >> 32) as u32, "depth hi");
    // Stencil = 0.
    assert_eq!(words[8], 0, "stencil clear value");
}

#[test]
fn cmd_set_scissor_rect_packing() {
    let mut buf = CommandBuffer::new();
    buf.cmd_set_scissor(10, 20, 100, 50);
    let words = buf.as_dwords();

    assert_eq!(words[0] & 0xFF, VIRGL_CCMD_SET_SCISSOR_STATE);
    assert_eq!(words[1], 0, "start_slot");
    // min: x | (y << 16)
    assert_eq!(words[2], 10 | (20 << 16));
    // max: (x+w) | ((y+h) << 16)
    assert_eq!(words[3], 110 | (70 << 16));
}

/// CompositorConfig includes font_size and screen_dpi fields and fits
/// within the 60-byte IPC payload limit.
#[test]
fn compositor_config_includes_font_fields() {
    use protocol::compose::CompositorConfig;
    let config = CompositorConfig {
        scene_va: 0,
        font_buf_va: 0,
        fb_width: 1024,
        fb_height: 768,
        mono_font_len: 100,
        sans_font_len: 0,
        serif_font_len: 0,
        scale_factor: 1.0,
        frame_rate: 60,
        font_size: 18,
        screen_dpi: 96,
        _pad: 0,
        pointer_state_va: 0,
    };
    assert_eq!(config.frame_rate, 60);
    assert_eq!(config.font_size, 18);
    assert_eq!(config.screen_dpi, 96);
    assert!(core::mem::size_of::<CompositorConfig>() <= 60);
}

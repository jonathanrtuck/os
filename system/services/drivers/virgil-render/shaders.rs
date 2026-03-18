//! TGSI shader text definitions for the virgil-render driver.
//!
//! TGSI (Tungsten Graphics Shader Infrastructure) is Gallium's shader IR.
//! virglrenderer's `vrend_create_shader` always calls `tgsi_text_translate`,
//! so the wire format is TGSI text, not binary tokens. The `offlen` field
//! in the CREATE_OBJECT command encodes the total byte length (including the
//! null terminator) of the text buffer.
//!
//! We need exactly 4 shaders:
//! 1. `COLOR_VS`: vertex shader — position + color passthrough
//! 2. `COLOR_FS`: fragment shader — color passthrough
//! 3. `TEXTURED_VS`: vertex shader — position + texcoord + color passthrough
//! 4. `TEXTURED_FS`: fragment shader — texture sample × vertex color
//!
//! TGSI text format reference: virglrenderer `src/gallium/auxiliary/tgsi/tgsi_text.c`
//! and the test suite in `tests/test_virgl_cmd.c`.
//!
//! Syntax conventions (from parsing `tgsi_text.c`):
//!   - Processor keyword: `VERT` or `FRAG` (first line)
//!   - File names: `IN`, `OUT`, `TEMP`, `SAMP`
//!   - Semantic names: `POSITION`, `COLOR`, `GENERIC`
//!   - Interpolation: `CONSTANT`, `LINEAR`, `PERSPECTIVE`
//!   - TEX instruction: `TEX dst, coord, samp, 2D`
//!   - Each instruction prefixed with `  N: ` (index + colon + space)
//!   - Must end with `END`

// ── Color vertex shader ──────────────────────────────────────────────────
//
// Passes position and color from vertex inputs to rasterizer outputs.
// Vertex layout (from cmd_create_vertex_elements_color):
//   IN[0] = position (float2 at offset 0)
//   IN[1] = color    (float4 at offset 8)
//
// Equivalent GLSL:
//   gl_Position = vec4(in_pos, 0.0, 1.0);  // (set by clip-space transform)
//   frag_color  = in_color;

pub const COLOR_VS: &[u8] = b"VERT\n\
DCL IN[0]\n\
DCL IN[1]\n\
DCL OUT[0], POSITION\n\
DCL OUT[1], COLOR, LINEAR\n\
DCL TEMP[0]\n\
IMM[0] FLT32 { 0.0, 0.0, 0.0, 1.0 }\n\
  0: MOV TEMP[0], IMM[0]\n\
  1: MOV TEMP[0].xy, IN[0].xyxy\n\
  2: MOV OUT[0], TEMP[0]\n\
  3: MOV OUT[1], IN[1]\n\
  4: END\n\0";

// ── Color fragment shader ────────────────────────────────────────────────
//
// Outputs interpolated vertex color as the fragment color.
// LINEAR interpolation matches how the rasterizer interpolates between
// vertices (perspective-correct would also work for 2D).

pub const COLOR_FS: &[u8] = b"FRAG\n\
DCL IN[0], COLOR, LINEAR\n\
DCL OUT[0], COLOR\n\
  0: MOV OUT[0], IN[0]\n\
  1: END\n\0";

// ── Textured vertex shader ───────────────────────────────────────────────
//
// Passes position, texcoord, and color from vertex inputs to rasterizer.
// Vertex layout (8 floats = 32 bytes per vertex):
//   IN[0] = position (float2 at offset 0)
//   IN[1] = texcoord (float2 at offset 8)   — GENERIC[0]
//   IN[2] = color    (float4 at offset 16)
//
// Texcoord uses GENERIC semantic (not TEXCOORD) for compatibility with
// virglrenderer's GLSL emission, which uses location-based attribute
// binding. GENERIC[0] maps to the first varying slot.

pub const TEXTURED_VS: &[u8] = b"VERT\n\
DCL IN[0]\n\
DCL IN[1]\n\
DCL IN[2]\n\
DCL OUT[0], POSITION\n\
DCL OUT[1], GENERIC[0], PERSPECTIVE\n\
DCL OUT[2], COLOR, LINEAR\n\
DCL TEMP[0]\n\
IMM[0] FLT32 { 0.0, 0.0, 0.0, 1.0 }\n\
  0: MOV TEMP[0], IMM[0]\n\
  1: MOV TEMP[0].xy, IN[0].xyxy\n\
  2: MOV OUT[0], TEMP[0]\n\
  3: MOV OUT[1], IN[1]\n\
  4: MOV OUT[2], IN[2]\n\
  5: END\n\0";

// ── Textured fragment shader ─────────────────────────────────────────────
//
// Samples a 2D texture at the interpolated texcoord and multiplies
// the result by the vertex color. This allows per-glyph tinting and
// global alpha modulation with a single shader.
//
// Inputs from rasterizer:
//   IN[0] = GENERIC[0] (texcoord, perspective interpolation)
//   IN[1] = COLOR      (vertex color, linear interpolation)
//
// TEMP[0] holds the texel sample before multiplication.

pub const TEXTURED_FS: &[u8] = b"FRAG\n\
DCL IN[0], GENERIC[0], PERSPECTIVE\n\
DCL IN[1], COLOR, LINEAR\n\
DCL OUT[0], COLOR\n\
DCL SAMP[0]\n\
DCL TEMP[0]\n\
  0: TEX TEMP[0], IN[0], SAMP[0], 2D\n\
  1: MUL OUT[0], TEMP[0], IN[1]\n\
  2: END\n\0";

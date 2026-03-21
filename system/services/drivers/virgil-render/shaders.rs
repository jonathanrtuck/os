//! TGSI shader text definitions for the virgil-render driver.
//!
//! TGSI (Tungsten Graphics Shader Infrastructure) is Gallium's shader IR.
//! virglrenderer's `vrend_create_shader` always calls `tgsi_text_translate`,
//! so the wire format is TGSI text, not binary tokens. The `offlen` field
//! in the CREATE_OBJECT command encodes the total byte length (including the
//! null terminator) of the text buffer.
//!
//! Shaders defined here:
//! 1. `COLOR_VS`: vertex shader — position + color passthrough
//! 2. `COLOR_FS`: fragment shader — color passthrough
//! 3. `TEXTURED_VS`: vertex shader — position + texcoord + color passthrough
//! 4. `TEXTURED_FS`: fragment shader — texture sample × vertex color
//! 5. `GLYPH_FS`: R8 glyph atlas with coverage × vertex alpha
//! 6. `BLUR_H_FS`: loop-based horizontal box blur (any radius)
//! 7. `BLUR_V_FS`: loop-based vertical box blur (any radius)
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
//!   - Source modifier: `-SRC` negates; `.xyzw` swizzles
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

// ── Glyph fragment shader ──────────────────────────────────────────────
//
// Specialized for R8_UNORM glyph atlas textures. The texture contains
// grayscale coverage (0.0–1.0 in the R channel). The shader uses the
// vertex color as the glyph color and multiplies vertex alpha by the
// sampled coverage to produce the output alpha.
//
// With SRC_ALPHA / INV_SRC_ALPHA blending, the result is:
//   out.rgb = glyph_color.rgb * (alpha * coverage) + dst.rgb * (1 - alpha * coverage)
//
// This is correct non-premultiplied alpha compositing. The R8 texture
// returns (coverage, 0, 0, 1); we read only .x for the coverage value.

pub const GLYPH_FS: &[u8] = b"FRAG\n\
DCL IN[0], GENERIC[0], PERSPECTIVE\n\
DCL IN[1], COLOR, LINEAR\n\
DCL OUT[0], COLOR\n\
DCL SAMP[0]\n\
DCL TEMP[0]\n\
DCL TEMP[1]\n\
  0: TEX TEMP[0], IN[0], SAMP[0], 2D\n\
  1: MOV TEMP[1], IN[1]\n\
  2: MUL TEMP[1].w, IN[1].wwww, TEMP[0].xxxx\n\
  3: MOV OUT[0], TEMP[1]\n\
  4: END\n\0";

// ── Loop-based horizontal box blur fragment shader ──────────────────────
//
// Accumulates (2*half_width + 1) texel samples along the X axis with
// uniform weight, producing a box-averaged output. Used as one pass of
// a 3-pass box blur that converges to Gaussian (CLT).
//
// The loop iterates from -half_width to +half_width (inclusive).
// Each tap's texcoord is clamped to [0, CONST[0].z] to implement
// CLAMP_TO_EDGE at the captured sub-region boundary.
//
// Constant buffer (binding 0, 8 floats = 2 vec4):
//   CONST[0] = [h_texel_step, v_texel_step, max_u, max_v]
//   CONST[1] = [half_width, 1/(2*half+1), 0, 0]
//
// Registers:
//   TEMP[0] = accumulator (RGBA)
//   TEMP[1] = texel sample
//   TEMP[2] = mutable UV (x modified per tap)
//   TEMP[3].x = loop counter (starts at -half_width)
//   TEMP[3].y = upper bound (half_width + 1, exclusive)
//   TEMP[4].x = comparison result

pub const BLUR_H_FS: &[u8] = b"FRAG\n\
DCL IN[0], GENERIC[0], PERSPECTIVE\n\
DCL OUT[0], COLOR\n\
DCL SAMP[0]\n\
DCL CONST[0]\n\
DCL CONST[1]\n\
DCL TEMP[0]\n\
DCL TEMP[1]\n\
DCL TEMP[2]\n\
DCL TEMP[3]\n\
DCL TEMP[4]\n\
IMM[0] FLT32 { 0.0, 1.0, 0.0, 0.0 }\n\
  0: MOV TEMP[0], IMM[0].xxxx\n\
  1: MOV TEMP[2], IN[0]\n\
  2: MOV TEMP[3].x, -CONST[1].xxxx\n\
  3: ADD TEMP[3].y, CONST[1].xxxx, IMM[0].yyyy\n\
  4: BGNLOOP\n\
  5: SGE TEMP[4].x, TEMP[3].xxxx, TEMP[3].yyyy\n\
  6: IF TEMP[4].xxxx\n\
  7: BRK\n\
  8: ENDIF\n\
  9: MAD TEMP[2].x, TEMP[3].xxxx, CONST[0].xxxx, IN[0].xxxx\n\
 10: MAX TEMP[2].x, TEMP[2].xxxx, IMM[0].xxxx\n\
 11: MIN TEMP[2].x, TEMP[2].xxxx, CONST[0].zzzz\n\
 12: TEX TEMP[1], TEMP[2], SAMP[0], 2D\n\
 13: ADD TEMP[0], TEMP[0], TEMP[1]\n\
 14: ADD TEMP[3].x, TEMP[3].xxxx, IMM[0].yyyy\n\
 15: ENDLOOP\n\
 16: MUL OUT[0], TEMP[0], CONST[1].yyyy\n\
 17: END\n\0";

// ── Loop-based vertical box blur fragment shader ────────────────────────
//
// Same algorithm as BLUR_H_FS but samples along the Y axis.
// Texcoord clamping uses CONST[0].w (max V) and 0.0 (min V).
//
// Constant buffer layout: same as BLUR_H_FS (binding 0, 8 floats).

pub const BLUR_V_FS: &[u8] = b"FRAG\n\
DCL IN[0], GENERIC[0], PERSPECTIVE\n\
DCL OUT[0], COLOR\n\
DCL SAMP[0]\n\
DCL CONST[0]\n\
DCL CONST[1]\n\
DCL TEMP[0]\n\
DCL TEMP[1]\n\
DCL TEMP[2]\n\
DCL TEMP[3]\n\
DCL TEMP[4]\n\
IMM[0] FLT32 { 0.0, 1.0, 0.0, 0.0 }\n\
  0: MOV TEMP[0], IMM[0].xxxx\n\
  1: MOV TEMP[2], IN[0]\n\
  2: MOV TEMP[3].x, -CONST[1].xxxx\n\
  3: ADD TEMP[3].y, CONST[1].xxxx, IMM[0].yyyy\n\
  4: BGNLOOP\n\
  5: SGE TEMP[4].x, TEMP[3].xxxx, TEMP[3].yyyy\n\
  6: IF TEMP[4].xxxx\n\
  7: BRK\n\
  8: ENDIF\n\
  9: MAD TEMP[2].y, TEMP[3].xxxx, CONST[0].yyyy, IN[0].yyyy\n\
 10: MAX TEMP[2].y, TEMP[2].yyyy, IMM[0].xxxx\n\
 11: MIN TEMP[2].y, TEMP[2].yyyy, CONST[0].wwww\n\
 12: TEX TEMP[1], TEMP[2], SAMP[0], 2D\n\
 13: ADD TEMP[0], TEMP[0], TEMP[1]\n\
 14: ADD TEMP[3].x, TEMP[3].xxxx, IMM[0].yyyy\n\
 15: ENDLOOP\n\
 16: MUL OUT[0], TEMP[0], CONST[1].yyyy\n\
 17: END\n\0";

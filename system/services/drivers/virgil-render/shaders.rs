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
//! 6. `BLUR_H_FS`: 9-tap horizontal Gaussian blur
//! 7. `BLUR_V_FS`: 9-tap vertical Gaussian blur
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

// ── Horizontal 9-tap Gaussian blur fragment shader ───────────────────────
//
// Samples 9 texels along the X axis, weighted by a Gaussian kernel (σ≈2).
// Kernel: [0.0162, 0.0540, 0.1216, 0.1945, 0.2270, 0.1945, 0.1216, 0.0540, 0.0162]
// Sum = 1.0 (normalised).
//
// Inputs:
//   IN[0]  = GENERIC[0]  — UV texcoord (PERSPECTIVE)
//   SAMP[0]              — source texture to blur
//   CONST[0].x           — horizontal texel size (1.0 / texture_width)
//
// TEMP layout:
//   TEMP[0] — running accumulator (RGBA)
//   TEMP[1] — single texel sample (RGBA)
//   TEMP[2] — mutable UV for off-centre taps (xy used, zw ignored)
//
// Instruction numbering is sequential (TGSI text requires it).

pub const BLUR_H_FS: &[u8] = b"FRAG\n\
DCL IN[0], GENERIC[0], PERSPECTIVE\n\
DCL OUT[0], COLOR\n\
DCL SAMP[0]\n\
DCL CONST[0]\n\
DCL TEMP[0]\n\
DCL TEMP[1]\n\
DCL TEMP[2]\n\
IMM[0] FLT32 { 0.2270, 0.1945, 0.1216, 0.0540 }\n\
IMM[1] FLT32 { 0.0162, 1.0, 2.0, 3.0 }\n\
IMM[2] FLT32 { 4.0, 0.0, 0.0, 0.0 }\n\
  0: MOV TEMP[2], IN[0]\n\
  1: TEX TEMP[1], TEMP[2], SAMP[0], 2D\n\
  2: MUL TEMP[0], TEMP[1], IMM[0].xxxx\n\
  3: MAD TEMP[2].x, CONST[0].xxxx, IMM[1].yyyy, IN[0].xxxx\n\
  4: TEX TEMP[1], TEMP[2], SAMP[0], 2D\n\
  5: MAD TEMP[0], TEMP[1], IMM[0].yyyy, TEMP[0]\n\
  6: MAD TEMP[2].x, -CONST[0].xxxx, IMM[1].yyyy, IN[0].xxxx\n\
  7: TEX TEMP[1], TEMP[2], SAMP[0], 2D\n\
  8: MAD TEMP[0], TEMP[1], IMM[0].yyyy, TEMP[0]\n\
  9: MAD TEMP[2].x, CONST[0].xxxx, IMM[1].zzzz, IN[0].xxxx\n\
 10: TEX TEMP[1], TEMP[2], SAMP[0], 2D\n\
 11: MAD TEMP[0], TEMP[1], IMM[0].zzzz, TEMP[0]\n\
 12: MAD TEMP[2].x, -CONST[0].xxxx, IMM[1].zzzz, IN[0].xxxx\n\
 13: TEX TEMP[1], TEMP[2], SAMP[0], 2D\n\
 14: MAD TEMP[0], TEMP[1], IMM[0].zzzz, TEMP[0]\n\
 15: MAD TEMP[2].x, CONST[0].xxxx, IMM[1].wwww, IN[0].xxxx\n\
 16: TEX TEMP[1], TEMP[2], SAMP[0], 2D\n\
 17: MAD TEMP[0], TEMP[1], IMM[0].wwww, TEMP[0]\n\
 18: MAD TEMP[2].x, -CONST[0].xxxx, IMM[1].wwww, IN[0].xxxx\n\
 19: TEX TEMP[1], TEMP[2], SAMP[0], 2D\n\
 20: MAD TEMP[0], TEMP[1], IMM[0].wwww, TEMP[0]\n\
 21: MAD TEMP[2].x, CONST[0].xxxx, IMM[2].xxxx, IN[0].xxxx\n\
 22: TEX TEMP[1], TEMP[2], SAMP[0], 2D\n\
 23: MAD TEMP[0], TEMP[1], IMM[1].xxxx, TEMP[0]\n\
 24: MAD TEMP[2].x, -CONST[0].xxxx, IMM[2].xxxx, IN[0].xxxx\n\
 25: TEX TEMP[1], TEMP[2], SAMP[0], 2D\n\
 26: MAD TEMP[0], TEMP[1], IMM[1].xxxx, TEMP[0]\n\
 27: MOV OUT[0], TEMP[0]\n\
 28: END\n\0";

// ── Vertical 9-tap Gaussian blur fragment shader ─────────────────────────
//
// Same Gaussian kernel as BLUR_H_FS but samples along the Y axis.
//
// Inputs:
//   IN[0]  = GENERIC[0]  — UV texcoord (PERSPECTIVE)
//   SAMP[0]              — horizontal-blurred intermediate texture
//   CONST[0].y           — vertical texel size (1.0 / texture_height)
//
// Identical structure to BLUR_H_FS; only the swizzle component changes
// from .x to .y in the MAD offset computations.

pub const BLUR_V_FS: &[u8] = b"FRAG\n\
DCL IN[0], GENERIC[0], PERSPECTIVE\n\
DCL OUT[0], COLOR\n\
DCL SAMP[0]\n\
DCL CONST[0]\n\
DCL TEMP[0]\n\
DCL TEMP[1]\n\
DCL TEMP[2]\n\
IMM[0] FLT32 { 0.2270, 0.1945, 0.1216, 0.0540 }\n\
IMM[1] FLT32 { 0.0162, 1.0, 2.0, 3.0 }\n\
IMM[2] FLT32 { 4.0, 0.0, 0.0, 0.0 }\n\
  0: MOV TEMP[2], IN[0]\n\
  1: TEX TEMP[1], TEMP[2], SAMP[0], 2D\n\
  2: MUL TEMP[0], TEMP[1], IMM[0].xxxx\n\
  3: MAD TEMP[2].y, CONST[0].yyyy, IMM[1].yyyy, IN[0].yyyy\n\
  4: TEX TEMP[1], TEMP[2], SAMP[0], 2D\n\
  5: MAD TEMP[0], TEMP[1], IMM[0].yyyy, TEMP[0]\n\
  6: MAD TEMP[2].y, -CONST[0].yyyy, IMM[1].yyyy, IN[0].yyyy\n\
  7: TEX TEMP[1], TEMP[2], SAMP[0], 2D\n\
  8: MAD TEMP[0], TEMP[1], IMM[0].yyyy, TEMP[0]\n\
  9: MAD TEMP[2].y, CONST[0].yyyy, IMM[1].zzzz, IN[0].yyyy\n\
 10: TEX TEMP[1], TEMP[2], SAMP[0], 2D\n\
 11: MAD TEMP[0], TEMP[1], IMM[0].zzzz, TEMP[0]\n\
 12: MAD TEMP[2].y, -CONST[0].yyyy, IMM[1].zzzz, IN[0].yyyy\n\
 13: TEX TEMP[1], TEMP[2], SAMP[0], 2D\n\
 14: MAD TEMP[0], TEMP[1], IMM[0].zzzz, TEMP[0]\n\
 15: MAD TEMP[2].y, CONST[0].yyyy, IMM[1].wwww, IN[0].yyyy\n\
 16: TEX TEMP[1], TEMP[2], SAMP[0], 2D\n\
 17: MAD TEMP[0], TEMP[1], IMM[0].wwww, TEMP[0]\n\
 18: MAD TEMP[2].y, -CONST[0].yyyy, IMM[1].wwww, IN[0].yyyy\n\
 19: TEX TEMP[1], TEMP[2], SAMP[0], 2D\n\
 20: MAD TEMP[0], TEMP[1], IMM[0].wwww, TEMP[0]\n\
 21: MAD TEMP[2].y, CONST[0].yyyy, IMM[2].xxxx, IN[0].yyyy\n\
 22: TEX TEMP[1], TEMP[2], SAMP[0], 2D\n\
 23: MAD TEMP[0], TEMP[1], IMM[1].xxxx, TEMP[0]\n\
 24: MAD TEMP[2].y, -CONST[0].yyyy, IMM[2].xxxx, IN[0].yyyy\n\
 25: TEX TEMP[1], TEMP[2], SAMP[0], 2D\n\
 26: MAD TEMP[0], TEMP[1], IMM[1].xxxx, TEMP[0]\n\
 27: MOV OUT[0], TEMP[0]\n\
 28: END\n\0";

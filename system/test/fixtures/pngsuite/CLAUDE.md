# test/fixtures/pngsuite

180 PNG images from the PngSuite conformance test set. Covers all color types (grayscale, RGB, palette, grayscale+alpha, RGBA), bit depths (1/2/4/8/16), filter methods, Adam7 interlacing, palette transparency, and intentionally corrupt files.

Used by `tests/png.rs` and `tests/png_decoder.rs` to verify the PNG decoder produces correct BGRA output for every valid image and rejects every corrupt one. 162 valid images must decode; 18 corrupt images must error.

//! Image texture rendering for GPU scene walk.
//!
//! `ImageBatch` collects image draw requests from the scene graph.
//! Each image is rendered as a textured quad with its own GPU texture upload.

/// Maximum images per frame.
pub const MAX_IMAGES: usize = 4;

/// Dwords per image quad: 6 vertices x 8 floats = 48.
pub const DWORDS_PER_IMAGE_QUAD: usize = 48;

/// A single image to render as a textured quad.
#[derive(Clone, Copy)]
pub struct ImageQuad {
    /// Screen-space position (physical pixels).
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Offset into the scene graph data buffer (BGRA pixel data).
    pub data_offset: u32,
    pub data_length: u32,
    /// Source image dimensions (pixels).
    pub src_width: u16,
    pub src_height: u16,
    /// True if this image is inside a clip region (requires stencil test).
    pub clipped: bool,
}

/// Collected image draw requests from a scene walk.
pub struct ImageBatch {
    images: [ImageQuad; MAX_IMAGES],
    pub count: usize,
}

impl ImageBatch {
    pub const fn new() -> Self {
        Self {
            images: [ImageQuad {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
                data_offset: 0,
                data_length: 0,
                src_width: 0,
                src_height: 0,
                clipped: false,
            }; MAX_IMAGES],
            count: 0,
        }
    }

    pub fn clear(&mut self) {
        self.count = 0;
    }

    pub fn push(&mut self, img: ImageQuad) {
        if self.count < MAX_IMAGES {
            self.images[self.count] = img;
            self.count += 1;
        }
    }

    pub fn get(&self, i: usize) -> Option<&ImageQuad> {
        if i < self.count {
            Some(&self.images[i])
        } else {
            None
        }
    }
}

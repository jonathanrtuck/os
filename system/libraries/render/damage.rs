//! Damage tracking — dirty rectangle management for partial GPU transfer.

use protocol::DirtyRect;

/// Maximum number of dirty rects tracked per frame.
///
/// The incremental pipeline can produce many dirty rects (e.g., a line
/// insert shifts 30+ nodes). Coalescing reduces these, but pre-coalescing
/// capacity must handle the worst case. When exceeded, `full_screen`
/// fallback kicks in.
pub const MAX_DIRTY_RECTS: usize = 32;

/// Collects dirty rectangles during a render pass.
///
/// When the number of rects exceeds MAX_DIRTY_RECTS, the tracker
/// falls back to a single full-screen rect (signaled by `full_screen = true`).
pub struct DamageTracker {
    pub rects: [DirtyRect; MAX_DIRTY_RECTS],
    pub count: usize,
    pub full_screen: bool,
    fb_width: u16,
    fb_height: u16,
}

impl DamageTracker {
    /// Create a new damage tracker for the given framebuffer dimensions.
    pub const fn new(fb_width: u16, fb_height: u16) -> Self {
        Self {
            rects: [DirtyRect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            }; MAX_DIRTY_RECTS],
            count: 0,
            full_screen: false,
            fb_width,
            fb_height,
        }
    }

    /// Add a dirty rectangle. If too many rects accumulate, falls back
    /// to full-screen damage.
    pub fn add(&mut self, x: u16, y: u16, w: u16, h: u16) {
        if self.full_screen || w == 0 || h == 0 {
            return;
        }

        if self.count >= MAX_DIRTY_RECTS {
            self.full_screen = true;

            return;
        }

        self.rects[self.count] = DirtyRect::new(x, y, w, h);
        self.count += 1;
    }
    /// Get the bounding box of all dirty rects, or full screen if needed.
    pub fn bounding_box(&self) -> DirtyRect {
        if self.full_screen || self.count == 0 {
            DirtyRect::new(0, 0, self.fb_width, self.fb_height)
        } else {
            DirtyRect::union_all(&self.rects[..self.count])
        }
    }
    /// Get the dirty rects for this frame. Returns `None` if full-screen
    /// transfer is needed (either explicitly marked or overflow).
    pub fn dirty_rects(&self) -> Option<&[DirtyRect]> {
        if self.full_screen || self.count == 0 {
            None
        } else {
            Some(&self.rects[..self.count])
        }
    }
    /// Mark the entire framebuffer as dirty.
    pub fn mark_full_screen(&mut self) {
        self.full_screen = true;
    }
    /// Reset the tracker for a new frame.
    pub fn reset(&mut self) {
        self.count = 0;
        self.full_screen = false;
    }

    /// Framebuffer width this tracker was created with.
    pub fn fb_width(&self) -> u16 {
        self.fb_width
    }

    /// Framebuffer height this tracker was created with.
    pub fn fb_height(&self) -> u16 {
        self.fb_height
    }
}

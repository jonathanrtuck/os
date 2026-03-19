//! Scene graph data structures for the compositor interface.
//!
//! The OS service builds a tree of `Node` values in shared memory.
//! The compositor reads this tree and renders it to pixels.
//!
//! # Memory layout
//!
//! A scene graph occupies a contiguous shared memory region:
//!
//! ```text
//! ┌──────────┬─────────────────────┬──────────────────────┐
//! │  Header  │  Node array         │  Data buffer          │
//! │  80 B    │  N × NODE_SIZE      │  variable-length      │
//! └──────────┴─────────────────────┴──────────────────────┘
//! ```
//!
//! - **Header:** generation counter, node count, data buffer usage, dirty bitmap.
//! - **Node array:** fixed-size entries, indexed by `NodeId`.
//! - **Data buffer:** text strings and path commands referenced by
//!   offset+length from nodes.
//!
//! # Design
//!
//! One node type with optional content (Core Animation model). Every node
//! can have children, visual decoration (background, border, corner radius),
//! and an optional content variant (Image, Glyphs). This avoids
//! wrapper nodes in compound documents where containers routinely need
//! backgrounds and borders.

#![no_std]

extern crate alloc;

mod diff;
mod node;
mod primitives;
mod reader;
mod transform;
mod triple;
mod writer;

// Re-export everything that was previously pub so downstream crates
// see no change to the `scene::` public API.
pub use diff::*;
pub use node::*;
pub use primitives::*;
pub use reader::*;
pub use transform::*;
pub use triple::*;
pub use writer::*;

//! Compound document manifest — types and binary serialization.
//!
//! A manifest defines a document: metadata, optional layout, and a list
//! of children (URI content references). Simple documents have one child
//! and no layout. Compound documents have multiple children arranged by
//! display axes and a positioning mode.

#![no_std]

extern crate alloc;

mod serialize;

use alloc::{string::String, vec::Vec};

// ── Magic ───────────────────────────────────────────────────────────

/// Binary format magic number ("MANF").
pub const MANIFEST_MAGIC: u32 = 0x4D41_4E46;

/// Current binary format version.
pub const FORMAT_VERSION: u8 = 1;

// ── Display axes ────────────────────────────────────────────────────

/// A display axis — the perceptual dimension a layout operates on.
///
/// Spatial axes use millipoints (Mpt, i32). Time uses milliseconds (i32).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Axis {
    Width = 0,
    Height = 1,
    Depth = 2,
    Time = 3,
}

impl Axis {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Width),
            1 => Some(Self::Height),
            2 => Some(Self::Depth),
            3 => Some(Self::Time),
            _ => None,
        }
    }
}

// ── Positioning modes ───────────────────────────────────────────────

/// How children are positioned within the layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Positioning {
    /// Layout engine computes positions from content sizes.
    Flow = 0,
    /// Container divides space into regular regions.
    Grid = 1,
    /// Children carry explicit coordinates.
    Absolute = 2,
}

impl Positioning {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Flow),
            1 => Some(Self::Grid),
            2 => Some(Self::Absolute),
            _ => None,
        }
    }
}

// ── Alignment ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Align {
    Start = 0,
    Center = 1,
    End = 2,
}

impl Align {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Start),
            1 => Some(Self::Center),
            2 => Some(Self::End),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Justify {
    Start = 0,
    Center = 1,
    End = 2,
    Between = 3,
    Around = 4,
}

impl Justify {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Start),
            1 => Some(Self::Center),
            2 => Some(Self::End),
            3 => Some(Self::Between),
            4 => Some(Self::Around),
            _ => None,
        }
    }
}

// ── Per-axis values ─────────────────────────────────────────────────

/// A value per display axis. Only axes declared in the manifest's `axes`
/// list should have values set. Unset axes are `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PerAxis<T> {
    pub width: Option<T>,
    pub height: Option<T>,
    pub depth: Option<T>,
    pub time: Option<T>,
}

impl<T: Copy> PerAxis<T> {
    pub fn get(&self, axis: Axis) -> Option<T> {
        match axis {
            Axis::Width => self.width,
            Axis::Height => self.height,
            Axis::Depth => self.depth,
            Axis::Time => self.time,
        }
    }

    pub fn set(&mut self, axis: Axis, value: T) {
        match axis {
            Axis::Width => self.width = Some(value),
            Axis::Height => self.height = Some(value),
            Axis::Depth => self.depth = Some(value),
            Axis::Time => self.time = Some(value),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.width.is_none() && self.height.is_none() && self.depth.is_none() && self.time.is_none()
    }
}

// ── Layout properties ───────────────────────────────────────────────

/// Layout mode with associated properties.
///
/// The positioning mode and its properties are bundled together because
/// they are always set/read as a unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutMode {
    Flow(FlowProperties),
    Grid(GridProperties),
    Absolute(AbsoluteProperties),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowProperties {
    pub wrap: bool,
    pub align: Align,
    pub justify: Justify,
    pub gap: PerAxis<i32>,
}

impl Default for FlowProperties {
    fn default() -> Self {
        Self {
            wrap: true,
            align: Align::Start,
            justify: Justify::Start,
            gap: PerAxis::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridProperties {
    pub divisions: PerAxis<u32>,
    pub gap: PerAxis<i32>,
}

impl Default for GridProperties {
    fn default() -> Self {
        Self {
            divisions: PerAxis::default(),
            gap: PerAxis::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbsoluteProperties {
    /// Axis bounds (None per axis = unbounded).
    pub bounds: PerAxis<i32>,
    /// Initial viewport for large or unbounded spaces.
    pub viewport: Option<Viewport>,
}

impl Default for AbsoluteProperties {
    fn default() -> Self {
        Self {
            bounds: PerAxis::default(),
            viewport: None,
        }
    }
}

/// Initial view region for absolute layouts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewport {
    pub center: PerAxis<i32>,
    /// Zoom as fixed-point 16.16 (65536 = 1.0).
    pub zoom: u32,
}

// ── Layout ──────────────────────────────────────────────────────────

/// Complete layout specification: which axes, how positioned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// Display axes. Order matters: first axis is the primary direction.
    pub axes: Vec<Axis>,
    /// Positioning mode with properties.
    pub mode: LayoutMode,
}

// ── Child ───────────────────────────────────────────────────────────

/// A child content reference with optional edge data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Child {
    /// Content URI (e.g. "store:12", "https://...").
    pub uri: String,
    /// Where this child sits in the parent's layout.
    pub placement: Option<Placement>,
    /// What region of the child's content to show.
    pub viewport: Option<ChildViewport>,
}

/// Placement — positioning-mode-dependent child location.
///
/// Fields are per-axis. Which fields are meaningful depends on the
/// parent manifest's positioning mode:
/// - Absolute: `position` and `size`
/// - Grid: `cell` and `span`
/// - Flow: typically empty (children flow in order)
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Placement {
    /// Absolute position per axis (Mpt or ms).
    pub position: PerAxis<i32>,
    /// Explicit size per axis (None = intrinsic).
    pub size: PerAxis<i32>,
    /// Grid cell index per axis (None = auto-place).
    pub cell: PerAxis<u32>,
    /// Grid span per axis (default: 1).
    pub span: PerAxis<u32>,
}

/// Viewport into child content — crop/pan/zoom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildViewport {
    /// Crop/pan offset per axis (Mpt or ms).
    pub offset: PerAxis<i32>,
    /// Zoom as fixed-point 16.16 (65536 = 1.0).
    pub zoom: u32,
}

// ── Manifest ────────────────────────────────────────────────────────

/// A document manifest.
///
/// Every manifest has the same shape: metadata + optional layout +
/// children list. A simple document has one child and no layout.
/// A compound document has multiple children with a layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    // Metadata
    pub title: Option<String>,
    pub tags: Vec<String>,
    /// Original source URI for imported documents.
    pub provenance: Option<String>,
    /// Open key-value attribute pairs.
    pub attributes: Vec<(String, String)>,

    // Layout (None for simple documents)
    pub layout: Option<Layout>,

    // Children (exactly one for simple documents)
    pub children: Vec<Child>,
}

impl Manifest {
    /// A simple document: one child, no layout.
    pub fn simple(uri: String) -> Self {
        Self {
            title: None,
            tags: Vec::new(),
            provenance: None,
            attributes: Vec::new(),
            layout: None,
            children: alloc::vec![Child {
                uri,
                placement: None,
                viewport: None,
            }],
        }
    }

    /// Whether this is a simple (single-content) document.
    pub fn is_simple(&self) -> bool {
        self.layout.is_none() && self.children.len() == 1
    }

    /// The positioning mode, if this is a compound document.
    pub fn positioning(&self) -> Option<Positioning> {
        self.layout.as_ref().map(|l| match &l.mode {
            LayoutMode::Flow(_) => Positioning::Flow,
            LayoutMode::Grid(_) => Positioning::Grid,
            LayoutMode::Absolute(_) => Positioning::Absolute,
        })
    }
}

// ── URI resolution ──────────────────────────────────────────────────

/// Parse a `store:N` URI to a file ID (u64).
///
/// Returns `None` for non-store URIs or malformed store URIs.
pub fn resolve_store_uri(uri: &str) -> Option<u64> {
    let id_str = uri.strip_prefix("store:")?;

    id_str.parse::<u64>().ok()
}

// ── Errors ──────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ManifestError {
    Truncated,
    BadMagic,
    BadVersion(u8),
    InvalidAxis(u8),
    InvalidPositioning(u8),
    InvalidAlign(u8),
    InvalidJustify(u8),
    InvalidUtf8,
    NoChildren,
}

// ── Serialization ───────────────────────────────────────────────────

pub use serialize::{decode, encode};

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::*;

    #[test]
    fn simple_manifest_construction() {
        let m = Manifest::simple("store:12".to_string());

        assert!(m.is_simple());
        assert_eq!(m.children.len(), 1);
        assert_eq!(m.children[0].uri, "store:12");
        assert!(m.layout.is_none());
        assert!(m.title.is_none());
        assert_eq!(m.positioning(), None);
    }

    #[test]
    fn compound_manifest_construction() {
        let m = Manifest {
            title: Some("Trip Report".to_string()),
            tags: alloc::vec!["travel".to_string()],
            provenance: None,
            attributes: Vec::new(),
            layout: Some(Layout {
                axes: alloc::vec![Axis::Width, Axis::Height],
                mode: LayoutMode::Flow(FlowProperties::default()),
            }),
            children: alloc::vec![
                Child {
                    uri: "store:12".to_string(),
                    placement: None,
                    viewport: None,
                },
                Child {
                    uri: "store:14".to_string(),
                    placement: None,
                    viewport: None,
                },
                Child {
                    uri: "store:15".to_string(),
                    placement: None,
                    viewport: None,
                },
            ],
        };

        assert!(!m.is_simple());
        assert_eq!(m.children.len(), 3);
        assert_eq!(m.positioning(), Some(Positioning::Flow));
        assert_eq!(m.title.as_deref(), Some("Trip Report"));
    }

    #[test]
    fn absolute_layout_with_placement() {
        let m = Manifest {
            title: Some("Title Slide".to_string()),
            tags: Vec::new(),
            provenance: None,
            attributes: Vec::new(),
            layout: Some(Layout {
                axes: alloc::vec![Axis::Width, Axis::Height],
                mode: LayoutMode::Absolute(AbsoluteProperties {
                    bounds: PerAxis {
                        width: Some(1920 * 1024),
                        height: Some(1080 * 1024),
                        ..Default::default()
                    },
                    viewport: None,
                }),
            }),
            children: alloc::vec![Child {
                uri: "store:30".to_string(),
                placement: Some(Placement {
                    position: PerAxis {
                        width: Some(100 * 1024),
                        height: Some(80 * 1024),
                        ..Default::default()
                    },
                    size: PerAxis {
                        width: Some(800 * 1024),
                        height: Some(120 * 1024),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
                viewport: None,
            }],
        };

        assert_eq!(m.positioning(), Some(Positioning::Absolute));

        let p = m.children[0].placement.as_ref().unwrap();

        assert_eq!(p.position.get(Axis::Width), Some(100 * 1024));
        assert_eq!(p.size.get(Axis::Height), Some(120 * 1024));
        assert_eq!(p.position.get(Axis::Time), None);
    }

    #[test]
    fn grid_with_divisions() {
        let m = Manifest {
            title: Some("Vacation 2026".to_string()),
            tags: alloc::vec!["photos".to_string()],
            provenance: None,
            attributes: Vec::new(),
            layout: Some(Layout {
                axes: alloc::vec![Axis::Width, Axis::Height],
                mode: LayoutMode::Grid(GridProperties {
                    divisions: PerAxis {
                        width: Some(3),
                        ..Default::default()
                    },
                    gap: PerAxis {
                        width: Some(8 * 1024),
                        height: Some(8 * 1024),
                        ..Default::default()
                    },
                }),
            }),
            children: alloc::vec![
                Child {
                    uri: "store:40".to_string(),
                    placement: None,
                    viewport: None
                },
                Child {
                    uri: "store:41".to_string(),
                    placement: None,
                    viewport: None
                },
                Child {
                    uri: "store:42".to_string(),
                    placement: None,
                    viewport: None
                },
            ],
        };

        assert_eq!(m.positioning(), Some(Positioning::Grid));

        if let Some(Layout {
            mode: LayoutMode::Grid(ref g),
            ..
        }) = m.layout
        {
            assert_eq!(g.divisions.get(Axis::Width), Some(3));
            assert_eq!(g.gap.get(Axis::Width), Some(8 * 1024));
        } else {
            panic!("expected grid layout");
        }
    }

    #[test]
    fn timeline_layout() {
        let m = Manifest {
            title: Some("Vacation Edit".to_string()),
            tags: Vec::new(),
            provenance: None,
            attributes: Vec::new(),
            layout: Some(Layout {
                axes: alloc::vec![Axis::Time, Axis::Height],
                mode: LayoutMode::Absolute(AbsoluteProperties {
                    bounds: PerAxis {
                        time: Some(120_000),
                        ..Default::default()
                    },
                    viewport: None,
                }),
            }),
            children: alloc::vec![
                Child {
                    uri: "store:90".to_string(),
                    placement: Some(Placement {
                        position: PerAxis {
                            time: Some(0),
                            height: Some(0),
                            ..Default::default()
                        },
                        size: PerAxis {
                            time: Some(5400),
                            ..Default::default()
                        },
                        ..Default::default()
                    }),
                    viewport: None,
                },
                Child {
                    uri: "store:91".to_string(),
                    placement: Some(Placement {
                        position: PerAxis {
                            time: Some(5400),
                            height: Some(0),
                            ..Default::default()
                        },
                        size: PerAxis {
                            time: Some(6600),
                            ..Default::default()
                        },
                        ..Default::default()
                    }),
                    viewport: None,
                },
            ],
        };

        assert_eq!(m.layout.as_ref().unwrap().axes, [Axis::Time, Axis::Height]);
        assert_eq!(m.positioning(), Some(Positioning::Absolute));
    }

    #[test]
    fn per_axis_operations() {
        let mut pa: PerAxis<i32> = PerAxis::default();

        assert!(pa.is_empty());

        pa.set(Axis::Width, 100);

        assert!(!pa.is_empty());
        assert_eq!(pa.get(Axis::Width), Some(100));
        assert_eq!(pa.get(Axis::Height), None);
    }

    #[test]
    fn axis_roundtrip() {
        for i in 0..4u8 {
            let axis = Axis::from_u8(i).unwrap();

            assert_eq!(axis as u8, i);
        }

        assert!(Axis::from_u8(4).is_none());
    }

    #[test]
    fn positioning_roundtrip() {
        for i in 0..3u8 {
            let p = Positioning::from_u8(i).unwrap();

            assert_eq!(p as u8, i);
        }

        assert!(Positioning::from_u8(3).is_none());
    }

    #[test]
    fn child_viewport() {
        let child = Child {
            uri: "store:31".to_string(),
            placement: None,
            viewport: Some(ChildViewport {
                offset: PerAxis {
                    width: Some(500 * 1024),
                    height: Some(200 * 1024),
                    ..Default::default()
                },
                zoom: 98304, // 1.5 in 16.16 fixed-point
            }),
        };

        let vp = child.viewport.as_ref().unwrap();

        assert_eq!(vp.offset.get(Axis::Width), Some(500 * 1024));
        assert_eq!(vp.zoom, 98304);
    }

    // ── Serialization round-trip tests ──────────────────────────────

    #[test]
    fn roundtrip_simple() {
        let m = Manifest::simple("store:12".to_string());
        let bytes = encode(&m);
        let decoded = decode(&bytes).unwrap();

        assert_eq!(m, decoded);
    }

    #[test]
    fn roundtrip_with_metadata() {
        let m = Manifest {
            title: Some("Meeting Notes".to_string()),
            tags: alloc::vec!["work".to_string(), "notes".to_string()],
            provenance: Some("https://example.com/doc".to_string()),
            attributes: alloc::vec![
                ("author".to_string(), "alice".to_string()),
                ("priority".to_string(), "high".to_string()),
            ],
            layout: None,
            children: alloc::vec![Child {
                uri: "store:42".to_string(),
                placement: None,
                viewport: None,
            }],
        };
        let bytes = encode(&m);
        let decoded = decode(&bytes).unwrap();

        assert_eq!(m, decoded);
    }

    #[test]
    fn roundtrip_flow_layout() {
        let m = Manifest {
            title: Some("Article".to_string()),
            tags: Vec::new(),
            provenance: None,
            attributes: Vec::new(),
            layout: Some(Layout {
                axes: alloc::vec![Axis::Width, Axis::Height],
                mode: LayoutMode::Flow(FlowProperties {
                    wrap: true,
                    align: Align::Center,
                    justify: Justify::Between,
                    gap: PerAxis {
                        width: Some(1024),
                        height: Some(2048),
                        ..Default::default()
                    },
                }),
            }),
            children: alloc::vec![
                Child {
                    uri: "store:1".to_string(),
                    placement: None,
                    viewport: None
                },
                Child {
                    uri: "store:2".to_string(),
                    placement: None,
                    viewport: None
                },
            ],
        };
        let bytes = encode(&m);
        let decoded = decode(&bytes).unwrap();

        assert_eq!(m, decoded);
    }

    #[test]
    fn roundtrip_grid_layout() {
        let m = Manifest {
            title: None,
            tags: Vec::new(),
            provenance: None,
            attributes: Vec::new(),
            layout: Some(Layout {
                axes: alloc::vec![Axis::Width, Axis::Height],
                mode: LayoutMode::Grid(GridProperties {
                    divisions: PerAxis {
                        width: Some(3),
                        height: Some(2),
                        ..Default::default()
                    },
                    gap: PerAxis {
                        width: Some(512),
                        ..Default::default()
                    },
                }),
            }),
            children: alloc::vec![
                Child {
                    uri: "store:10".to_string(),
                    placement: None,
                    viewport: None
                },
                Child {
                    uri: "store:11".to_string(),
                    placement: Some(Placement {
                        cell: PerAxis {
                            width: Some(0),
                            height: Some(1),
                            ..Default::default()
                        },
                        span: PerAxis {
                            width: Some(2),
                            ..Default::default()
                        },
                        ..Default::default()
                    }),
                    viewport: None,
                },
            ],
        };
        let bytes = encode(&m);
        let decoded = decode(&bytes).unwrap();

        assert_eq!(m, decoded);
    }

    #[test]
    fn roundtrip_absolute_with_viewport() {
        let m = Manifest {
            title: Some("Whiteboard".to_string()),
            tags: Vec::new(),
            provenance: None,
            attributes: Vec::new(),
            layout: Some(Layout {
                axes: alloc::vec![Axis::Width, Axis::Height],
                mode: LayoutMode::Absolute(AbsoluteProperties {
                    bounds: PerAxis::default(),
                    viewport: Some(Viewport {
                        center: PerAxis {
                            width: Some(400 * 1024),
                            height: Some(300 * 1024),
                            ..Default::default()
                        },
                        zoom: 65536, // 1.0
                    }),
                }),
            }),
            children: alloc::vec![Child {
                uri: "store:50".to_string(),
                placement: Some(Placement {
                    position: PerAxis {
                        width: Some(100 * 1024),
                        height: Some(200 * 1024),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
                viewport: Some(ChildViewport {
                    offset: PerAxis {
                        width: Some(10 * 1024),
                        ..Default::default()
                    },
                    zoom: 131072, // 2.0
                }),
            }],
        };
        let bytes = encode(&m);
        let decoded = decode(&bytes).unwrap();

        assert_eq!(m, decoded);
    }

    #[test]
    fn roundtrip_timeline() {
        let m = Manifest {
            title: None,
            tags: Vec::new(),
            provenance: None,
            attributes: Vec::new(),
            layout: Some(Layout {
                axes: alloc::vec![Axis::Time, Axis::Height],
                mode: LayoutMode::Absolute(AbsoluteProperties {
                    bounds: PerAxis {
                        time: Some(120_000),
                        ..Default::default()
                    },
                    viewport: None,
                }),
            }),
            children: alloc::vec![Child {
                uri: "store:90".to_string(),
                placement: Some(Placement {
                    position: PerAxis {
                        time: Some(0),
                        height: Some(0),
                        ..Default::default()
                    },
                    size: PerAxis {
                        time: Some(5400),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
                viewport: None,
            },],
        };
        let bytes = encode(&m);
        let decoded = decode(&bytes).unwrap();

        assert_eq!(m, decoded);
    }

    #[test]
    fn decode_bad_magic() {
        let bytes = [0x00, 0x00, 0x00, 0x00];

        assert!(matches!(decode(&bytes), Err(ManifestError::BadMagic)));
    }

    #[test]
    fn decode_truncated() {
        let bytes = [0x46, 0x4E]; // partial magic

        assert!(matches!(decode(&bytes), Err(ManifestError::Truncated)));
    }

    #[test]
    fn decode_bad_version() {
        let mut bytes = MANIFEST_MAGIC.to_le_bytes().to_vec();

        bytes.push(99); // bad version

        assert!(matches!(decode(&bytes), Err(ManifestError::BadVersion(99))));
    }

    // ── URI resolution tests ────────────────────────────────────────

    #[test]
    fn resolve_store_uri_valid() {
        assert_eq!(resolve_store_uri("store:12"), Some(12));
        assert_eq!(resolve_store_uri("store:0"), Some(0));
        assert_eq!(resolve_store_uri("store:999999"), Some(999_999));
    }

    #[test]
    fn resolve_store_uri_invalid() {
        assert_eq!(resolve_store_uri("https://example.com"), None);
        assert_eq!(resolve_store_uri("store:"), None);
        assert_eq!(resolve_store_uri("store:abc"), None);
        assert_eq!(resolve_store_uri("file:12"), None);
        assert_eq!(resolve_store_uri(""), None);
    }
}

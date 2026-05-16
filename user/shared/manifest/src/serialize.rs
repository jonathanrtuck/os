//! Binary manifest serialization.
//!
//! Format:
//! ```text
//! [magic: u32] [version: u8] [flags: u8]
//!   flags bit 0: has_layout
//!   flags bit 1: has_title
//!   flags bit 2: has_provenance
//!
//! [tag_count: u16] per tag: [len: u16] [bytes]
//! [attr_count: u16] per attr: [key_len: u16] [key] [val_len: u16] [val]
//! if has_title: [title_len: u16] [title bytes]
//! if has_provenance: [provenance_len: u16] [provenance bytes]
//!
//! if has_layout:
//!   [axis_count: u8] [axes: u8 each]
//!   [positioning: u8]
//!   [mode-specific properties...]
//!
//! [child_count: u16]
//! per child:
//!   [uri_len: u16] [uri bytes]
//!   [child_flags: u8]
//!     bit 0: has_placement
//!     bit 1: has_viewport
//!   if has_placement: [per_axis fields...]
//!   if has_viewport: [per_axis offset + zoom]
//! ```

use alloc::{string::String, vec::Vec};

use crate::{
    AbsoluteProperties, Align, Axis, Child, ChildViewport, FORMAT_VERSION, FlowProperties,
    GridProperties, Justify, Layout, LayoutMode, MANIFEST_MAGIC, Manifest, ManifestError, PerAxis,
    Placement, Positioning, Viewport,
};

// ── Encoder ─────────────────────────────────────────────────────────

pub fn encode(m: &Manifest) -> Vec<u8> {
    let mut buf = Vec::new();

    buf.extend_from_slice(&MANIFEST_MAGIC.to_le_bytes());
    buf.push(FORMAT_VERSION);

    let flags: u8 = if m.layout.is_some() { 1 } else { 0 }
        | if m.title.is_some() { 2 } else { 0 }
        | if m.provenance.is_some() { 4 } else { 0 };

    buf.push(flags);

    write_u16(&mut buf, m.tags.len() as u16);

    for tag in &m.tags {
        write_string(&mut buf, tag);
    }

    write_u16(&mut buf, m.attributes.len() as u16);

    for (key, val) in &m.attributes {
        write_string(&mut buf, key);
        write_string(&mut buf, val);
    }

    if let Some(ref title) = m.title {
        write_string(&mut buf, title);
    }

    if let Some(ref prov) = m.provenance {
        write_string(&mut buf, prov);
    }

    if let Some(ref layout) = m.layout {
        encode_layout(&mut buf, layout);
    }

    write_u16(&mut buf, m.children.len() as u16);

    for child in &m.children {
        encode_child(&mut buf, child);
    }

    buf
}

fn encode_layout(buf: &mut Vec<u8>, layout: &Layout) {
    buf.push(layout.axes.len() as u8);

    for &axis in &layout.axes {
        buf.push(axis as u8);
    }

    match &layout.mode {
        LayoutMode::Flow(f) => {
            buf.push(Positioning::Flow as u8);
            buf.push(u8::from(f.wrap));
            buf.push(f.align as u8);
            buf.push(f.justify as u8);

            encode_per_axis_i32(buf, &f.gap);
        }
        LayoutMode::Grid(g) => {
            buf.push(Positioning::Grid as u8);

            encode_per_axis_u32(buf, &g.divisions);
            encode_per_axis_i32(buf, &g.gap);
        }
        LayoutMode::Absolute(a) => {
            buf.push(Positioning::Absolute as u8);

            encode_per_axis_i32(buf, &a.bounds);

            let has_vp = a.viewport.is_some();

            buf.push(u8::from(has_vp));

            if let Some(ref vp) = a.viewport {
                encode_per_axis_i32(buf, &vp.center);
                write_u32(buf, vp.zoom);
            }
        }
    }
}

fn encode_child(buf: &mut Vec<u8>, child: &Child) {
    write_string(buf, &child.uri);

    let flags: u8 = if child.placement.is_some() { 1 } else { 0 }
        | if child.viewport.is_some() { 2 } else { 0 };

    buf.push(flags);

    if let Some(ref p) = child.placement {
        encode_per_axis_i32(buf, &p.position);
        encode_per_axis_i32(buf, &p.size);
        encode_per_axis_u32(buf, &p.cell);
        encode_per_axis_u32(buf, &p.span);
    }

    if let Some(ref vp) = child.viewport {
        encode_per_axis_i32(buf, &vp.offset);
        write_u32(buf, vp.zoom);
    }
}

fn encode_per_axis_i32(buf: &mut Vec<u8>, pa: &PerAxis<i32>) {
    let mask: u8 = if pa.width.is_some() { 1 } else { 0 }
        | if pa.height.is_some() { 2 } else { 0 }
        | if pa.depth.is_some() { 4 } else { 0 }
        | if pa.time.is_some() { 8 } else { 0 };

    buf.push(mask);

    if let Some(v) = pa.width {
        write_i32(buf, v);
    }
    if let Some(v) = pa.height {
        write_i32(buf, v);
    }
    if let Some(v) = pa.depth {
        write_i32(buf, v);
    }
    if let Some(v) = pa.time {
        write_i32(buf, v);
    }
}

fn encode_per_axis_u32(buf: &mut Vec<u8>, pa: &PerAxis<u32>) {
    let mask: u8 = if pa.width.is_some() { 1 } else { 0 }
        | if pa.height.is_some() { 2 } else { 0 }
        | if pa.depth.is_some() { 4 } else { 0 }
        | if pa.time.is_some() { 8 } else { 0 };

    buf.push(mask);

    if let Some(v) = pa.width {
        write_u32(buf, v);
    }
    if let Some(v) = pa.height {
        write_u32(buf, v);
    }
    if let Some(v) = pa.depth {
        write_u32(buf, v);
    }
    if let Some(v) = pa.time {
        write_u32(buf, v);
    }
}

fn write_u16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_i32(buf: &mut Vec<u8>, v: i32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_string(buf: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();

    write_u16(buf, bytes.len() as u16);

    buf.extend_from_slice(bytes);
}

// ── Decoder ─────────────────────────────────────────────────────────

pub fn decode(data: &[u8]) -> Result<Manifest, ManifestError> {
    let mut r = Reader { data, pos: 0 };
    let magic = r.u32()?;

    if magic != MANIFEST_MAGIC {
        return Err(ManifestError::BadMagic);
    }

    let version = r.u8()?;

    if version != FORMAT_VERSION {
        return Err(ManifestError::BadVersion(version));
    }

    let flags = r.u8()?;
    let has_layout = flags & 1 != 0;
    let has_title = flags & 2 != 0;
    let has_provenance = flags & 4 != 0;
    let tag_count = r.u16()? as usize;
    let mut tags = Vec::with_capacity(tag_count);

    for _ in 0..tag_count {
        tags.push(r.string()?);
    }

    let attr_count = r.u16()? as usize;
    let mut attributes = Vec::with_capacity(attr_count);

    for _ in 0..attr_count {
        let key = r.string()?;
        let val = r.string()?;

        attributes.push((key, val));
    }

    let title = if has_title { Some(r.string()?) } else { None };
    let provenance = if has_provenance {
        Some(r.string()?)
    } else {
        None
    };
    let layout = if has_layout {
        Some(decode_layout(&mut r)?)
    } else {
        None
    };
    let child_count = r.u16()? as usize;

    if child_count == 0 {
        return Err(ManifestError::NoChildren);
    }

    let mut children = Vec::with_capacity(child_count);

    for _ in 0..child_count {
        children.push(decode_child(&mut r)?);
    }

    Ok(Manifest {
        title,
        tags,
        provenance,
        attributes,
        layout,
        children,
    })
}

fn decode_layout(r: &mut Reader<'_>) -> Result<Layout, ManifestError> {
    let axis_count = r.u8()? as usize;
    let mut axes = Vec::with_capacity(axis_count);

    for _ in 0..axis_count {
        let v = r.u8()?;

        axes.push(Axis::from_u8(v).ok_or(ManifestError::InvalidAxis(v))?);
    }

    let pos_byte = r.u8()?;
    let positioning =
        Positioning::from_u8(pos_byte).ok_or(ManifestError::InvalidPositioning(pos_byte))?;
    let mode = match positioning {
        Positioning::Flow => {
            let wrap = r.u8()? != 0;
            let align_byte = r.u8()?;
            let align =
                Align::from_u8(align_byte).ok_or(ManifestError::InvalidAlign(align_byte))?;
            let justify_byte = r.u8()?;
            let justify = Justify::from_u8(justify_byte)
                .ok_or(ManifestError::InvalidJustify(justify_byte))?;
            let gap = r.per_axis_i32()?;

            LayoutMode::Flow(FlowProperties {
                wrap,
                align,
                justify,
                gap,
            })
        }
        Positioning::Grid => {
            let divisions = r.per_axis_u32()?;
            let gap = r.per_axis_i32()?;

            LayoutMode::Grid(GridProperties { divisions, gap })
        }
        Positioning::Absolute => {
            let bounds = r.per_axis_i32()?;
            let has_vp = r.u8()? != 0;
            let viewport = if has_vp {
                let center = r.per_axis_i32()?;
                let zoom = r.u32()?;

                Some(Viewport { center, zoom })
            } else {
                None
            };

            LayoutMode::Absolute(AbsoluteProperties { bounds, viewport })
        }
    };

    Ok(Layout { axes, mode })
}

fn decode_child(r: &mut Reader<'_>) -> Result<Child, ManifestError> {
    let uri = r.string()?;
    let flags = r.u8()?;
    let has_placement = flags & 1 != 0;
    let has_viewport = flags & 2 != 0;
    let placement = if has_placement {
        let position = r.per_axis_i32()?;
        let size = r.per_axis_i32()?;
        let cell = r.per_axis_u32()?;
        let span = r.per_axis_u32()?;

        Some(Placement {
            position,
            size,
            cell,
            span,
        })
    } else {
        None
    };
    let viewport = if has_viewport {
        let offset = r.per_axis_i32()?;
        let zoom = r.u32()?;

        Some(ChildViewport { offset, zoom })
    } else {
        None
    };

    Ok(Child {
        uri,
        placement,
        viewport,
    })
}

// ── Reader ──────────────────────────────────────────────────────────

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Result<u8, ManifestError> {
        if self.pos >= self.data.len() {
            return Err(ManifestError::Truncated);
        }

        let v = self.data[self.pos];

        self.pos += 1;

        Ok(v)
    }

    fn u16(&mut self) -> Result<u16, ManifestError> {
        if self.pos + 2 > self.data.len() {
            return Err(ManifestError::Truncated);
        }

        let v = u16::from_le_bytes([self.data[self.pos], self.data[self.pos + 1]]);

        self.pos += 2;

        Ok(v)
    }

    fn u32(&mut self) -> Result<u32, ManifestError> {
        if self.pos + 4 > self.data.len() {
            return Err(ManifestError::Truncated);
        }

        let v = u32::from_le_bytes([
            self.data[self.pos],
            self.data[self.pos + 1],
            self.data[self.pos + 2],
            self.data[self.pos + 3],
        ]);

        self.pos += 4;

        Ok(v)
    }

    fn i32(&mut self) -> Result<i32, ManifestError> {
        if self.pos + 4 > self.data.len() {
            return Err(ManifestError::Truncated);
        }

        let v = i32::from_le_bytes([
            self.data[self.pos],
            self.data[self.pos + 1],
            self.data[self.pos + 2],
            self.data[self.pos + 3],
        ]);

        self.pos += 4;

        Ok(v)
    }

    fn string(&mut self) -> Result<String, ManifestError> {
        let len = self.u16()? as usize;

        if self.pos + len > self.data.len() {
            return Err(ManifestError::Truncated);
        }

        let s = core::str::from_utf8(&self.data[self.pos..self.pos + len])
            .map_err(|_| ManifestError::InvalidUtf8)?;

        self.pos += len;

        Ok(String::from(s))
    }

    fn per_axis_i32(&mut self) -> Result<PerAxis<i32>, ManifestError> {
        let mask = self.u8()?;
        let width = if mask & 1 != 0 {
            Some(self.i32()?)
        } else {
            None
        };
        let height = if mask & 2 != 0 {
            Some(self.i32()?)
        } else {
            None
        };
        let depth = if mask & 4 != 0 {
            Some(self.i32()?)
        } else {
            None
        };
        let time = if mask & 8 != 0 {
            Some(self.i32()?)
        } else {
            None
        };

        Ok(PerAxis {
            width,
            height,
            depth,
            time,
        })
    }

    fn per_axis_u32(&mut self) -> Result<PerAxis<u32>, ManifestError> {
        let mask = self.u8()?;
        let width = if mask & 1 != 0 {
            Some(self.u32()?)
        } else {
            None
        };
        let height = if mask & 2 != 0 {
            Some(self.u32()?)
        } else {
            None
        };
        let depth = if mask & 4 != 0 {
            Some(self.u32()?)
        } else {
            None
        };
        let time = if mask & 8 != 0 {
            Some(self.u32()?)
        } else {
            None
        };

        Ok(PerAxis {
            width,
            height,
            depth,
            time,
        })
    }
}

//! AVI container parser — extracts video stream metadata and frame data
//! from RIFF/AVI files. Designed for MJPEG but codec-agnostic.
//!
//! no_std, no alloc. All operations reference the caller's data slice.

#![no_std]

// ── Types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FourCC(pub [u8; 4]);

impl FourCC {
    pub const MJPG: Self = Self(*b"MJPG");
    pub const MJPEG: Self = Self(*b"mjpg");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AviInfo {
    pub width: u32,
    pub height: u32,
    pub us_per_frame: u32,
    pub total_frames: u32,
    pub codec: FourCC,
}

impl AviInfo {
    pub fn fps(&self) -> f32 {
        if self.us_per_frame == 0 {
            0.0
        } else {
            1_000_000.0 / self.us_per_frame as f32
        }
    }

    pub fn ns_per_frame(&self) -> u64 {
        self.us_per_frame as u64 * 1000
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRef {
    pub offset: u32,
    pub size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    NotRiff,
    NotAvi,
    TooShort,
    NoVideoStream,
    NoMoviList,
    BadChunkSize,
}

// ── RIFF primitives ────────────────────────────────────────────────

const RIFF: [u8; 4] = *b"RIFF";
const LIST: [u8; 4] = *b"LIST";
const AVI_FORM: [u8; 4] = *b"AVI ";
const HDRL: [u8; 4] = *b"hdrl";
const STRL: [u8; 4] = *b"strl";
const MOVI: [u8; 4] = *b"movi";
const AVIH: [u8; 4] = *b"avih";
const STRH: [u8; 4] = *b"strh";
const STRF: [u8; 4] = *b"strf";
const IDX1: [u8; 4] = *b"idx1";
const VIDS: [u8; 4] = *b"vids";

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    data.get(offset..offset + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn read_fourcc(data: &[u8], offset: usize) -> Option<[u8; 4]> {
    data.get(offset..offset + 4)
        .map(|b| [b[0], b[1], b[2], b[3]])
}

fn chunk_at(data: &[u8], offset: usize) -> Option<([u8; 4], u32, usize)> {
    let id = read_fourcc(data, offset)?;
    let size = read_u32(data, offset + 4)?;
    let data_start = offset + 8;

    if data_start + size as usize > data.len() {
        return None;
    }

    Some((id, size, data_start))
}

// ── Header parsing ─────────────────────────────────────────────────

pub fn parse(data: &[u8]) -> Result<AviInfo, Error> {
    if data.len() < 12 {
        return Err(Error::TooShort);
    }

    let riff_id = read_fourcc(data, 0).ok_or(Error::TooShort)?;

    if riff_id != RIFF {
        return Err(Error::NotRiff);
    }

    let form = read_fourcc(data, 8).ok_or(Error::TooShort)?;

    if form != AVI_FORM {
        return Err(Error::NotAvi);
    }

    let mut us_per_frame: u32 = 0;
    let mut total_frames: u32 = 0;
    let mut avi_width: u32 = 0;
    let mut avi_height: u32 = 0;
    let mut codec = FourCC([0; 4]);
    let mut found_video = false;
    let mut pos = 12;

    while pos + 8 <= data.len() {
        let (id, size, data_start) = match chunk_at(data, pos) {
            Some(c) => c,
            None => break,
        };

        if id == LIST {
            let list_type = match read_fourcc(data, data_start) {
                Some(t) => t,
                None => break,
            };

            if list_type == HDRL {
                parse_hdrl(
                    data,
                    data_start + 4,
                    data_start + size as usize,
                    &mut us_per_frame,
                    &mut total_frames,
                    &mut avi_width,
                    &mut avi_height,
                    &mut codec,
                    &mut found_video,
                );
            }
        }

        pos = data_start + ((size as usize + 1) & !1);
    }

    if !found_video {
        return Err(Error::NoVideoStream);
    }

    Ok(AviInfo {
        width: avi_width,
        height: avi_height,
        us_per_frame,
        total_frames,
        codec,
    })
}

fn parse_hdrl(
    data: &[u8],
    start: usize,
    end: usize,
    us_per_frame: &mut u32,
    total_frames: &mut u32,
    width: &mut u32,
    height: &mut u32,
    codec: &mut FourCC,
    found_video: &mut bool,
) {
    let mut pos = start;

    while pos + 8 <= end {
        let (id, size, data_start) = match chunk_at(data, pos) {
            Some(c) => c,
            None => break,
        };

        if id == AVIH && size >= 40 {
            *us_per_frame = read_u32(data, data_start).unwrap_or(0);
            *total_frames = read_u32(data, data_start + 16).unwrap_or(0);
            *width = read_u32(data, data_start + 32).unwrap_or(0);
            *height = read_u32(data, data_start + 36).unwrap_or(0);
        } else if id == LIST {
            let list_type = read_fourcc(data, data_start).unwrap_or([0; 4]);

            if list_type == STRL {
                parse_strl(
                    data,
                    data_start + 4,
                    data_start + size as usize,
                    width,
                    height,
                    codec,
                    found_video,
                );
            }
        }

        pos = data_start + ((size as usize + 1) & !1);
    }
}

fn parse_strl(
    data: &[u8],
    start: usize,
    end: usize,
    width: &mut u32,
    height: &mut u32,
    codec: &mut FourCC,
    found_video: &mut bool,
) {
    let mut pos = start;
    let mut is_video = false;

    while pos + 8 <= end {
        let (id, size, data_start) = match chunk_at(data, pos) {
            Some(c) => c,
            None => break,
        };

        if id == STRH && size >= 8 {
            let stream_type = read_fourcc(data, data_start).unwrap_or([0; 4]);

            if stream_type == VIDS {
                is_video = true;
                *found_video = true;
                *codec = FourCC(read_fourcc(data, data_start + 4).unwrap_or([0; 4]));
            }
        }

        if id == STRF && is_video && size >= 40 {
            let bmp_w = read_u32(data, data_start + 4).unwrap_or(0);
            let bmp_h = read_u32(data, data_start + 8).unwrap_or(0);

            if bmp_w > 0 {
                *width = bmp_w;
            }

            if bmp_h > 0 {
                *height = bmp_h;
            }
        }

        pos = data_start + ((size as usize + 1) & !1);
    }
}

// ── Frame access ───────────────────────────────────────────────────

pub fn find_movi(data: &[u8]) -> Option<(usize, usize)> {
    if data.len() < 12 {
        return None;
    }

    let mut pos = 12;

    while pos + 8 <= data.len() {
        let (id, size, data_start) = chunk_at(data, pos)?;

        if id == LIST {
            let list_type = read_fourcc(data, data_start)?;

            if list_type == MOVI {
                return Some((data_start + 4, data_start + size as usize));
            }
        }

        pos = data_start + ((size as usize + 1) & !1);
    }

    None
}

pub struct VideoFrameIter<'a> {
    data: &'a [u8],
    pos: usize,
    end: usize,
}

impl<'a> VideoFrameIter<'a> {
    pub fn new(data: &'a [u8]) -> Result<Self, Error> {
        let (start, end) = find_movi(data).ok_or(Error::NoMoviList)?;

        Ok(Self {
            data,
            pos: start,
            end,
        })
    }
}

impl<'a> Iterator for VideoFrameIter<'a> {
    type Item = FrameRef;

    fn next(&mut self) -> Option<Self::Item> {
        while self.pos + 8 <= self.end {
            let id = read_fourcc(self.data, self.pos)?;
            let size = read_u32(self.data, self.pos + 4)?;
            let data_start = self.pos + 8;

            self.pos = data_start + ((size as usize + 1) & !1);

            if is_video_chunk(&id) && size > 0 {
                return Some(FrameRef {
                    offset: data_start as u32,
                    size,
                });
            }
        }

        None
    }
}

fn is_video_chunk(id: &[u8; 4]) -> bool {
    // Video stream 0: "00dc" (compressed) or "00db" (uncompressed)
    id[0] == b'0' && id[1] == b'0' && (id[2] == b'd') && (id[3] == b'c' || id[3] == b'b')
}

pub fn frame_data<'a>(data: &'a [u8], frame: &FrameRef) -> Option<&'a [u8]> {
    let start = frame.offset as usize;
    let end = start + frame.size as usize;

    data.get(start..end)
}

// ── idx1 parsing ───────────────────────────────────────────────────

pub fn parse_idx1(data: &[u8]) -> Option<Idx1Iter<'_>> {
    if data.len() < 12 {
        return None;
    }

    let mut pos = 12;

    while pos + 8 <= data.len() {
        let (id, size, data_start) = chunk_at(data, pos)?;

        if id == IDX1 {
            return Some(Idx1Iter {
                data,
                pos: data_start,
                end: data_start + size as usize,
            });
        }

        if id == LIST {
            pos = data_start + ((size as usize + 1) & !1);
        } else {
            pos = data_start + ((size as usize + 1) & !1);
        }
    }

    None
}

pub struct Idx1Iter<'a> {
    data: &'a [u8],
    pos: usize,
    end: usize,
}

pub struct Idx1Entry {
    pub chunk_id: [u8; 4],
    pub flags: u32,
    pub offset: u32,
    pub size: u32,
}

impl Idx1Entry {
    pub const KEYFRAME: u32 = 0x10;

    pub fn is_keyframe(&self) -> bool {
        self.flags & Self::KEYFRAME != 0
    }

    pub fn is_video(&self) -> bool {
        is_video_chunk(&self.chunk_id)
    }
}

impl<'a> Iterator for Idx1Iter<'a> {
    type Item = Idx1Entry;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos + 16 > self.end {
            return None;
        }

        let chunk_id = read_fourcc(self.data, self.pos)?;
        let flags = read_u32(self.data, self.pos + 4)?;
        let offset = read_u32(self.data, self.pos + 8)?;
        let size = read_u32(self.data, self.pos + 12)?;

        self.pos += 16;

        Some(Idx1Entry {
            chunk_id,
            flags,
            offset,
            size,
        })
    }
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    extern crate alloc;

    use alloc::vec::Vec;

    use super::*;

    fn write_u32(buf: &mut [u8], offset: usize, val: u32) {
        buf[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
    }

    fn write_fourcc(buf: &mut [u8], offset: usize, fourcc: &[u8; 4]) {
        buf[offset..offset + 4].copy_from_slice(fourcc);
    }

    fn minimal_avi(width: u32, height: u32, us_per_frame: u32, frames: &[&[u8]]) -> Vec<u8> {
        let mut buf = Vec::new();

        // Placeholder for RIFF header (12 bytes).
        buf.extend_from_slice(&[0u8; 12]);

        write_fourcc(&mut buf, 0, b"RIFF");
        write_fourcc(&mut buf, 8, b"AVI ");

        // hdrl LIST.
        let hdrl_start = buf.len();

        buf.extend_from_slice(&[0u8; 8]); // LIST header

        write_fourcc(&mut buf, hdrl_start, b"LIST");

        buf.extend_from_slice(b"hdrl");

        // avih chunk (56 bytes).
        let avih_start = buf.len();

        buf.extend_from_slice(&[0u8; 8 + 56]);

        write_fourcc(&mut buf, avih_start, b"avih");
        write_u32(&mut buf, avih_start + 4, 56);
        write_u32(&mut buf, avih_start + 8, us_per_frame);
        write_u32(&mut buf, avih_start + 8 + 16, frames.len() as u32);
        write_u32(&mut buf, avih_start + 8 + 24, 1); // streams
        write_u32(&mut buf, avih_start + 8 + 32, width);
        write_u32(&mut buf, avih_start + 8 + 36, height);

        // strl LIST.
        let strl_start = buf.len();

        buf.extend_from_slice(&[0u8; 8]);

        write_fourcc(&mut buf, strl_start, b"LIST");

        buf.extend_from_slice(b"strl");

        // strh chunk (56 bytes).
        let strh_start = buf.len();

        buf.extend_from_slice(&[0u8; 8 + 56]);

        write_fourcc(&mut buf, strh_start, b"strh");
        write_u32(&mut buf, strh_start + 4, 56);
        write_fourcc(&mut buf, strh_start + 8, b"vids");
        write_fourcc(&mut buf, strh_start + 12, b"MJPG");

        // strf chunk (40 bytes — BITMAPINFOHEADER).
        let strf_start = buf.len();

        buf.extend_from_slice(&[0u8; 8 + 40]);

        write_fourcc(&mut buf, strf_start, b"strf");
        write_u32(&mut buf, strf_start + 4, 40);
        write_u32(&mut buf, strf_start + 8, 40); // biSize
        write_u32(&mut buf, strf_start + 12, width); // biWidth
        write_u32(&mut buf, strf_start + 16, height); // biHeight

        // Fix strl LIST size.
        let strl_size = buf.len() - strl_start - 8;

        write_u32(&mut buf, strl_start + 4, strl_size as u32);

        // Fix hdrl LIST size.
        let hdrl_size = buf.len() - hdrl_start - 8;

        write_u32(&mut buf, hdrl_start + 4, hdrl_size as u32);

        // movi LIST.
        let movi_start = buf.len();

        buf.extend_from_slice(&[0u8; 8]);

        write_fourcc(&mut buf, movi_start, b"LIST");

        buf.extend_from_slice(b"movi");

        for frame in frames {
            let chunk_start = buf.len();

            buf.extend_from_slice(&[0u8; 8]);

            write_fourcc(&mut buf, chunk_start, b"00dc");
            write_u32(&mut buf, chunk_start + 4, frame.len() as u32);

            buf.extend_from_slice(frame);

            if frame.len() % 2 != 0 {
                buf.push(0); // pad byte
            }
        }

        let movi_size = buf.len() - movi_start - 8;

        write_u32(&mut buf, movi_start + 4, movi_size as u32);

        // idx1.
        let idx1_start = buf.len();
        let idx1_data_size = frames.len() * 16;

        buf.extend_from_slice(&[0u8; 8]);

        write_fourcc(&mut buf, idx1_start, b"idx1");
        write_u32(&mut buf, idx1_start + 4, idx1_data_size as u32);

        let mut frame_offset: u32 = 0;

        for frame in frames {
            let entry_start = buf.len();

            buf.extend_from_slice(&[0u8; 16]);

            write_fourcc(&mut buf, entry_start, b"00dc");
            write_u32(&mut buf, entry_start + 4, Idx1Entry::KEYFRAME);
            write_u32(&mut buf, entry_start + 8, frame_offset);
            write_u32(&mut buf, entry_start + 12, frame.len() as u32);

            let padded = ((frame.len() + 1) & !1) as u32;

            frame_offset += 8 + padded; // chunk header + padded data
        }

        // Fix RIFF size.
        let riff_size = buf.len() - 8;

        write_u32(&mut buf, 4, riff_size as u32);

        buf
    }

    #[test]
    fn parse_minimal_avi() {
        let data = minimal_avi(320, 240, 33333, &[b"frame0", b"frame1", b"frame2"]);
        let info = parse(&data).unwrap();

        assert_eq!(info.width, 320);
        assert_eq!(info.height, 240);
        assert_eq!(info.us_per_frame, 33333);
        assert_eq!(info.total_frames, 3);
        assert_eq!(info.codec, FourCC::MJPG);
    }

    #[test]
    fn fps_calculation() {
        let info = AviInfo {
            width: 320,
            height: 240,
            us_per_frame: 33333,
            total_frames: 100,
            codec: FourCC::MJPG,
        };

        assert!((info.fps() - 30.0).abs() < 0.1);
        assert_eq!(info.ns_per_frame(), 33_333_000);
    }

    #[test]
    fn iterate_video_frames() {
        let data = minimal_avi(320, 240, 33333, &[b"JPEG0", b"JPEG1", b"JPEG2"]);
        let frames: Vec<FrameRef> = VideoFrameIter::new(&data).unwrap().collect();

        assert_eq!(frames.len(), 3);
        assert_eq!(frame_data(&data, &frames[0]), Some(b"JPEG0".as_slice()));
        assert_eq!(frame_data(&data, &frames[1]), Some(b"JPEG1".as_slice()));
        assert_eq!(frame_data(&data, &frames[2]), Some(b"JPEG2".as_slice()));
    }

    #[test]
    fn iterate_with_odd_frame_sizes() {
        let data = minimal_avi(160, 120, 40000, &[b"A", b"BC", b"DEF"]);
        let frames: Vec<FrameRef> = VideoFrameIter::new(&data).unwrap().collect();

        assert_eq!(frames.len(), 3);
        assert_eq!(frame_data(&data, &frames[0]), Some(b"A".as_slice()));
        assert_eq!(frame_data(&data, &frames[1]), Some(b"BC".as_slice()));
        assert_eq!(frame_data(&data, &frames[2]), Some(b"DEF".as_slice()));
    }

    #[test]
    fn parse_idx1_entries() {
        let data = minimal_avi(320, 240, 33333, &[b"F0", b"F1"]);
        let entries: Vec<Idx1Entry> = parse_idx1(&data).unwrap().collect();

        assert_eq!(entries.len(), 2);
        assert!(entries[0].is_video());
        assert!(entries[0].is_keyframe());
        assert!(entries[1].is_video());
        assert!(entries[1].is_keyframe());
        assert_eq!(entries[0].size, 2);
        assert_eq!(entries[1].size, 2);
    }

    #[test]
    fn idx1_offsets_resolve_to_frame_data() {
        let data = minimal_avi(320, 240, 33333, &[b"AAA", b"BBBBB"]);
        let (movi_start, _) = find_movi(&data).unwrap();
        let entries: Vec<Idx1Entry> = parse_idx1(&data).unwrap().collect();

        for entry in &entries {
            let abs_offset = movi_start + entry.offset as usize + 8;
            let frame = &data[abs_offset..abs_offset + entry.size as usize];

            assert!(frame == b"AAA" || frame == b"BBBBB");
        }
    }

    #[test]
    fn reject_non_riff() {
        assert_eq!(parse(b"NOT_RIFF_DATA_HERE"), Err(Error::NotRiff));
    }

    #[test]
    fn reject_non_avi() {
        let mut data = [0u8; 12];

        data[0..4].copy_from_slice(b"RIFF");
        data[4..8].copy_from_slice(&0u32.to_le_bytes());
        data[8..12].copy_from_slice(b"WAVE");

        assert_eq!(parse(&data), Err(Error::NotAvi));
    }

    #[test]
    fn reject_too_short() {
        assert_eq!(parse(b"RIFF"), Err(Error::TooShort));
        assert_eq!(parse(b""), Err(Error::TooShort));
    }

    #[test]
    fn no_video_stream_returns_error() {
        let mut data = [0u8; 12];

        data[0..4].copy_from_slice(b"RIFF");
        data[4..8].copy_from_slice(&4u32.to_le_bytes());
        data[8..12].copy_from_slice(b"AVI ");

        assert_eq!(parse(&data), Err(Error::NoVideoStream));
    }

    #[test]
    fn empty_movi_yields_no_frames() {
        let data = minimal_avi(320, 240, 33333, &[]);
        let frames: Vec<FrameRef> = VideoFrameIter::new(&data).unwrap().collect();

        assert_eq!(frames.len(), 0);
    }

    #[test]
    fn large_frame_count() {
        let frames: Vec<&[u8]> = (0..100).map(|_| b"JPEGDATA".as_slice()).collect();
        let data = minimal_avi(640, 480, 16667, &frames);
        let info = parse(&data).unwrap();

        assert_eq!(info.total_frames, 100);

        let parsed_frames: Vec<FrameRef> = VideoFrameIter::new(&data).unwrap().collect();

        assert_eq!(parsed_frames.len(), 100);

        for f in &parsed_frames {
            assert_eq!(frame_data(&data, f), Some(b"JPEGDATA".as_slice()));
        }
    }

    #[test]
    fn dimensions_from_strf_override_avih() {
        let data = minimal_avi(320, 240, 33333, &[b"F"]);
        let info = parse(&data).unwrap();

        assert_eq!(info.width, 320);
        assert_eq!(info.height, 240);
    }
}

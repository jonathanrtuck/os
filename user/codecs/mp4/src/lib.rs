//! MP4 (ISO 14496-12) container parser — extracts video and audio stream
//! metadata, codec configuration, and per-sample data references from MP4 files.
//!
//! no_std, no alloc. All operations reference the caller's data slice.

#![no_std]

// ── Types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    TooShort,
    NotMp4,
    NoVideoTrack,
    NoSampleTable,
    BadBox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleRef {
    pub offset: u64,
    pub size: u32,
    pub dts_ticks: u64,
    pub pts_ticks: u64,
    pub is_keyframe: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackType {
    Video,
    Audio,
    Other,
}

pub struct Mp4<'a> {
    data: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub timescale: u32,
    pub duration: u64,
    pub total_samples: u32,
    avcc_offset: usize,
    avcc_size: usize,
    stts_offset: usize,
    stts_count: u32,
    ctts_offset: usize,
    ctts_count: u32,
    stsc_offset: usize,
    stsc_count: u32,
    stsz_offset: usize,
    stsz_default_size: u32,
    stco_offset: usize,
    stco_count: u32,
    co64: bool,
    stss_offset: usize,
    stss_count: u32,
    // Audio track
    audio_timescale: u32,
    audio_duration: u64,
    audio_total_samples: u32,
    audio_sample_rate: u32,
    audio_channels: u16,
    audio_config_offset: usize,
    audio_config_size: usize,
    audio_stts_offset: usize,
    audio_stts_count: u32,
    audio_stsc_offset: usize,
    audio_stsc_count: u32,
    audio_stsz_offset: usize,
    audio_stsz_default_size: u32,
    audio_stco_offset: usize,
    audio_stco_count: u32,
    audio_co64: bool,
}

// ── Read helpers ───────────────────────────────────────────────────

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    data.get(offset..offset + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn read_u64(data: &[u8], offset: usize) -> Option<u64> {
    data.get(offset..offset + 8)
        .map(|b| u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
}

fn fourcc(data: &[u8], offset: usize) -> Option<[u8; 4]> {
    data.get(offset..offset + 4)
        .map(|b| [b[0], b[1], b[2], b[3]])
}

// ── Box primitives ─────────────────────────────────────────────────

/// Returns (fourcc, data_start, box_end) for the box at `offset`.
fn box_at(data: &[u8], offset: usize) -> Option<([u8; 4], usize, usize)> {
    let size32 = read_u32(data, offset)?;
    let tag = fourcc(data, offset + 4)?;
    let (header_size, box_size) = if size32 == 1 {
        let size64 = read_u64(data, offset + 8)? as usize;
        (16, size64)
    } else if size32 == 0 {
        (8, data.len() - offset)
    } else {
        (8, size32 as usize)
    };

    if box_size < header_size {
        return None;
    }

    let box_end = offset.checked_add(box_size)?;

    if box_end > data.len() {
        return None;
    }

    Some((tag, offset + header_size, box_end))
}

struct BoxIter<'a> {
    data: &'a [u8],
    pos: usize,
    end: usize,
}

impl<'a> BoxIter<'a> {
    fn new(data: &'a [u8], start: usize, end: usize) -> Self {
        Self {
            data,
            pos: start,
            end,
        }
    }
}

impl<'a> Iterator for BoxIter<'a> {
    type Item = ([u8; 4], usize, usize);

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.end {
            return None;
        }

        let (tag, data_start, box_end) = box_at(self.data, self.pos)?;

        self.pos = box_end;

        Some((tag, data_start, box_end))
    }
}

// ── Full-box version/flags skip ────────────────────────────────────

/// Skip version (1 byte) + flags (3 bytes) of a full box. Returns payload start.
fn fullbox_payload(data_start: usize, end: usize) -> Option<usize> {
    if data_start + 4 > end {
        return None;
    }

    Some(data_start + 4)
}

// ── Parse ──────────────────────────────────────────────────────────

pub fn parse(data: &[u8]) -> Result<Mp4<'_>, Error> {
    if data.len() < 8 {
        return Err(Error::TooShort);
    }

    let mut found_ftyp = false;
    let mut moov_range: Option<(usize, usize)> = None;

    for (tag, ds, be) in BoxIter::new(data, 0, data.len()) {
        match &tag {
            b"ftyp" => found_ftyp = true,
            b"moov" => moov_range = Some((ds, be)),
            _ => {}
        }
    }

    if !found_ftyp {
        return Err(Error::NotMp4);
    }

    let (moov_start, moov_end) = moov_range.ok_or(Error::NotMp4)?;
    let mut mp4 = Mp4 {
        data,
        width: 0,
        height: 0,
        timescale: 0,
        duration: 0,
        total_samples: 0,
        avcc_offset: 0,
        avcc_size: 0,
        stts_offset: 0,
        stts_count: 0,
        ctts_offset: 0,
        ctts_count: 0,
        stsc_offset: 0,
        stsc_count: 0,
        stsz_offset: 0,
        stsz_default_size: 0,
        stco_offset: 0,
        stco_count: 0,
        co64: false,
        stss_offset: 0,
        stss_count: 0,
        audio_timescale: 0,
        audio_duration: 0,
        audio_total_samples: 0,
        audio_sample_rate: 0,
        audio_channels: 0,
        audio_config_offset: 0,
        audio_config_size: 0,
        audio_stts_offset: 0,
        audio_stts_count: 0,
        audio_stsc_offset: 0,
        audio_stsc_count: 0,
        audio_stsz_offset: 0,
        audio_stsz_default_size: 0,
        audio_stco_offset: 0,
        audio_stco_count: 0,
        audio_co64: false,
    };

    parse_moov(data, moov_start, moov_end, &mut mp4)?;

    if mp4.width == 0 || mp4.height == 0 {
        return Err(Error::NoVideoTrack);
    }

    if mp4.stts_count == 0 || mp4.stsc_count == 0 || mp4.stco_count == 0 {
        return Err(Error::NoSampleTable);
    }

    Ok(mp4)
}

fn parse_moov<'a>(
    data: &'a [u8],
    start: usize,
    end: usize,
    mp4: &mut Mp4<'a>,
) -> Result<(), Error> {
    let mut found_video = false;
    let mut found_audio = false;

    for (tag, ds, be) in BoxIter::new(data, start, end) {
        match &tag {
            b"mvhd" => parse_mvhd(data, ds, be, mp4),
            b"trak" => match parse_trak_type(data, ds, be) {
                TrackType::Video if !found_video => {
                    if parse_video_trak(data, ds, be, mp4)? {
                        found_video = true;
                    }
                }
                TrackType::Audio if !found_audio => {
                    if parse_audio_trak(data, ds, be, mp4) {
                        found_audio = true;
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    if !found_video {
        return Err(Error::NoVideoTrack);
    }

    Ok(())
}

fn parse_mvhd(data: &[u8], data_start: usize, end: usize, mp4: &mut Mp4<'_>) {
    let Some(pos) = fullbox_payload(data_start, end) else {
        return;
    };

    let version = data[data_start];

    if version == 1 {
        if pos + 28 > end {
            return;
        }

        mp4.timescale = read_u32(data, pos + 16).unwrap_or(0);
        mp4.duration = read_u64(data, pos + 20).unwrap_or(0);
    } else {
        if pos + 16 > end {
            return;
        }

        mp4.timescale = read_u32(data, pos + 8).unwrap_or(0);
        mp4.duration = read_u32(data, pos + 12).unwrap_or(0) as u64;
    }
}

fn parse_trak_type(data: &[u8], start: usize, end: usize) -> TrackType {
    for (tag, ds, be) in BoxIter::new(data, start, end) {
        if &tag == b"mdia" {
            for (mtag, mds, mbe) in BoxIter::new(data, ds, be) {
                if &mtag == b"hdlr" {
                    return parse_hdlr(data, mds, mbe);
                }
            }
        }
    }

    TrackType::Other
}

fn parse_video_trak<'a>(
    data: &'a [u8],
    start: usize,
    end: usize,
    mp4: &mut Mp4<'a>,
) -> Result<bool, Error> {
    let mut tkhd_width: u32 = 0;
    let mut tkhd_height: u32 = 0;
    let mut mdia_range: Option<(usize, usize)> = None;

    for (tag, ds, be) in BoxIter::new(data, start, end) {
        match &tag {
            b"tkhd" => parse_tkhd(data, ds, be, &mut tkhd_width, &mut tkhd_height),
            b"mdia" => mdia_range = Some((ds, be)),
            _ => {}
        }
    }

    if let Some((ms, me)) = mdia_range {
        if !parse_video_mdia(data, ms, me, mp4)? {
            return Ok(false);
        }
    } else {
        return Ok(false);
    }

    mp4.width = tkhd_width;
    mp4.height = tkhd_height;

    Ok(true)
}

fn parse_tkhd(data: &[u8], data_start: usize, end: usize, width: &mut u32, height: &mut u32) {
    let Some(pos) = fullbox_payload(data_start, end) else {
        return;
    };
    let version = data[data_start];
    // Width and height are 16.16 fixed-point at end of tkhd.
    let wh_offset = if version == 1 { pos + 84 } else { pos + 72 };

    if wh_offset + 8 > end {
        return;
    }

    *width = read_u32(data, wh_offset).unwrap_or(0) >> 16;
    *height = read_u32(data, wh_offset + 4).unwrap_or(0) >> 16;
}

fn parse_video_mdia<'a>(
    data: &'a [u8],
    start: usize,
    end: usize,
    mp4: &mut Mp4<'a>,
) -> Result<bool, Error> {
    let mut minf_range: Option<(usize, usize)> = None;

    for (tag, ds, be) in BoxIter::new(data, start, end) {
        match &tag {
            b"mdhd" => parse_mdhd(data, ds, be, &mut mp4.timescale, &mut mp4.duration),
            b"minf" => minf_range = Some((ds, be)),
            _ => {}
        }
    }

    if let Some((ms, me)) = minf_range {
        parse_minf(data, ms, me, mp4)?;
    }

    Ok(mp4.stts_count > 0)
}

fn parse_mdhd(data: &[u8], data_start: usize, end: usize, timescale: &mut u32, duration: &mut u64) {
    let Some(pos) = fullbox_payload(data_start, end) else {
        return;
    };
    let version = data[data_start];

    if version == 1 {
        if pos + 28 > end {
            return;
        }

        *timescale = read_u32(data, pos + 16).unwrap_or(0);
        *duration = read_u64(data, pos + 20).unwrap_or(0);
    } else {
        if pos + 16 > end {
            return;
        }

        *timescale = read_u32(data, pos + 8).unwrap_or(0);
        *duration = read_u32(data, pos + 12).unwrap_or(0) as u64;
    }
}

fn parse_hdlr(data: &[u8], data_start: usize, end: usize) -> TrackType {
    let Some(pos) = fullbox_payload(data_start, end) else {
        return TrackType::Other;
    };

    if pos + 8 > end {
        return TrackType::Other;
    }

    match fourcc(data, pos + 4) {
        Some(b) if b == *b"vide" => TrackType::Video,
        Some(b) if b == *b"soun" => TrackType::Audio,
        _ => TrackType::Other,
    }
}

fn parse_minf<'a>(
    data: &'a [u8],
    start: usize,
    end: usize,
    mp4: &mut Mp4<'a>,
) -> Result<(), Error> {
    for (tag, ds, be) in BoxIter::new(data, start, end) {
        if &tag == b"stbl" {
            parse_stbl(data, ds, be, mp4)?;

            return Ok(());
        }
    }

    Err(Error::NoSampleTable)
}

fn parse_stbl<'a>(
    data: &'a [u8],
    start: usize,
    end: usize,
    mp4: &mut Mp4<'a>,
) -> Result<(), Error> {
    for (tag, ds, be) in BoxIter::new(data, start, end) {
        match &tag {
            b"stsd" => parse_stsd(data, ds, be, mp4),
            b"stts" => parse_stts(data, ds, be, mp4),
            b"ctts" => parse_ctts(data, ds, be, mp4),
            b"stsc" => parse_stsc(data, ds, be, mp4),
            b"stsz" => parse_stsz(data, ds, be, mp4),
            b"stco" => parse_stco(data, ds, be, mp4),
            b"co64" => parse_co64(data, ds, be, mp4),
            b"stss" => parse_stss(data, ds, be, mp4),
            _ => {}
        }
    }

    Ok(())
}

fn parse_stsd(data: &[u8], data_start: usize, end: usize, mp4: &mut Mp4<'_>) {
    let Some(pos) = fullbox_payload(data_start, end) else {
        return;
    };

    // entry_count at pos, then first entry follows
    if pos + 4 > end {
        return;
    }

    let entry_start = pos + 4;

    // The first sample entry is an avc1 (or similar) box
    for (tag, ds, be) in BoxIter::new(data, entry_start, end) {
        if &tag == b"avc1" {
            parse_avc1(data, ds, be, mp4);

            return;
        }
    }
}

fn parse_avc1(data: &[u8], data_start: usize, end: usize, mp4: &mut Mp4<'_>) {
    // avc1 sample entry: 6 reserved + 2 data_ref_index + fields... + child boxes
    // The avcC child box starts after the 78-byte fixed portion of avc1.
    let children_start = data_start + 78;

    if children_start > end {
        return;
    }

    for (tag, ds, be) in BoxIter::new(data, children_start, end) {
        if &tag == b"avcC" {
            mp4.avcc_offset = ds;
            mp4.avcc_size = be - ds;

            return;
        }
    }
}

fn parse_stts(data: &[u8], data_start: usize, end: usize, mp4: &mut Mp4<'_>) {
    let Some(pos) = fullbox_payload(data_start, end) else {
        return;
    };

    if pos + 4 > end {
        return;
    }

    mp4.stts_count = read_u32(data, pos).unwrap_or(0);
    mp4.stts_offset = pos + 4;
}

fn parse_ctts(data: &[u8], data_start: usize, end: usize, mp4: &mut Mp4<'_>) {
    let Some(pos) = fullbox_payload(data_start, end) else {
        return;
    };

    if pos + 4 > end {
        return;
    }

    mp4.ctts_count = read_u32(data, pos).unwrap_or(0);
    mp4.ctts_offset = pos + 4;
}

fn parse_stsc(data: &[u8], data_start: usize, end: usize, mp4: &mut Mp4<'_>) {
    let Some(pos) = fullbox_payload(data_start, end) else {
        return;
    };

    if pos + 4 > end {
        return;
    }

    mp4.stsc_count = read_u32(data, pos).unwrap_or(0);
    mp4.stsc_offset = pos + 4;
}

fn parse_stsz(data: &[u8], data_start: usize, end: usize, mp4: &mut Mp4<'_>) {
    let Some(pos) = fullbox_payload(data_start, end) else {
        return;
    };

    if pos + 8 > end {
        return;
    }

    mp4.stsz_default_size = read_u32(data, pos).unwrap_or(0);
    mp4.total_samples = read_u32(data, pos + 4).unwrap_or(0);
    mp4.stsz_offset = pos + 8;
}

fn parse_stco(data: &[u8], data_start: usize, end: usize, mp4: &mut Mp4<'_>) {
    let Some(pos) = fullbox_payload(data_start, end) else {
        return;
    };

    if pos + 4 > end {
        return;
    }

    mp4.stco_count = read_u32(data, pos).unwrap_or(0);
    mp4.stco_offset = pos + 4;
    mp4.co64 = false;
}

fn parse_co64(data: &[u8], data_start: usize, end: usize, mp4: &mut Mp4<'_>) {
    let Some(pos) = fullbox_payload(data_start, end) else {
        return;
    };

    if pos + 4 > end {
        return;
    }

    mp4.stco_count = read_u32(data, pos).unwrap_or(0);
    mp4.stco_offset = pos + 4;
    mp4.co64 = true;
}

fn parse_stss(data: &[u8], data_start: usize, end: usize, mp4: &mut Mp4<'_>) {
    let Some(pos) = fullbox_payload(data_start, end) else {
        return;
    };

    if pos + 4 > end {
        return;
    }

    mp4.stss_count = read_u32(data, pos).unwrap_or(0);
    mp4.stss_offset = pos + 4;
}

// ── Audio track parsing ───────────────────────────────────────────

fn parse_audio_trak(data: &[u8], start: usize, end: usize, mp4: &mut Mp4<'_>) -> bool {
    for (tag, ds, be) in BoxIter::new(data, start, end) {
        if &tag == b"mdia" {
            return parse_audio_mdia(data, ds, be, mp4);
        }
    }

    false
}

fn parse_audio_mdia(data: &[u8], start: usize, end: usize, mp4: &mut Mp4<'_>) -> bool {
    let mut minf_range: Option<(usize, usize)> = None;

    for (tag, ds, be) in BoxIter::new(data, start, end) {
        match &tag {
            b"mdhd" => parse_mdhd(
                data,
                ds,
                be,
                &mut mp4.audio_timescale,
                &mut mp4.audio_duration,
            ),
            b"minf" => minf_range = Some((ds, be)),
            _ => {}
        }
    }

    if let Some((ms, me)) = minf_range {
        parse_audio_minf(data, ms, me, mp4)
    } else {
        false
    }
}

fn parse_audio_minf(data: &[u8], start: usize, end: usize, mp4: &mut Mp4<'_>) -> bool {
    for (tag, ds, be) in BoxIter::new(data, start, end) {
        if &tag == b"stbl" {
            return parse_audio_stbl(data, ds, be, mp4);
        }
    }

    false
}

fn parse_audio_stbl(data: &[u8], start: usize, end: usize, mp4: &mut Mp4<'_>) -> bool {
    for (tag, ds, be) in BoxIter::new(data, start, end) {
        match &tag {
            b"stsd" => parse_audio_stsd(data, ds, be, mp4),
            b"stts" => {
                if let Some(pos) = fullbox_payload(ds, be) {
                    if pos + 4 <= be {
                        mp4.audio_stts_count = read_u32(data, pos).unwrap_or(0);
                        mp4.audio_stts_offset = pos + 4;
                    }
                }
            }
            b"stsc" => {
                if let Some(pos) = fullbox_payload(ds, be) {
                    if pos + 4 <= be {
                        mp4.audio_stsc_count = read_u32(data, pos).unwrap_or(0);
                        mp4.audio_stsc_offset = pos + 4;
                    }
                }
            }
            b"stsz" => {
                if let Some(pos) = fullbox_payload(ds, be) {
                    if pos + 8 <= be {
                        mp4.audio_stsz_default_size = read_u32(data, pos).unwrap_or(0);
                        mp4.audio_total_samples = read_u32(data, pos + 4).unwrap_or(0);
                        mp4.audio_stsz_offset = pos + 8;
                    }
                }
            }
            b"stco" => {
                if let Some(pos) = fullbox_payload(ds, be) {
                    if pos + 4 <= be {
                        mp4.audio_stco_count = read_u32(data, pos).unwrap_or(0);
                        mp4.audio_stco_offset = pos + 4;
                        mp4.audio_co64 = false;
                    }
                }
            }
            b"co64" => {
                if let Some(pos) = fullbox_payload(ds, be) {
                    if pos + 4 <= be {
                        mp4.audio_stco_count = read_u32(data, pos).unwrap_or(0);
                        mp4.audio_stco_offset = pos + 4;
                        mp4.audio_co64 = true;
                    }
                }
            }
            _ => {}
        }
    }

    mp4.audio_stts_count > 0 && mp4.audio_stsc_count > 0 && mp4.audio_stco_count > 0
}

fn parse_audio_stsd(data: &[u8], data_start: usize, end: usize, mp4: &mut Mp4<'_>) {
    let Some(pos) = fullbox_payload(data_start, end) else {
        return;
    };

    if pos + 4 > end {
        return;
    }

    let entry_start = pos + 4;

    for (tag, ds, be) in BoxIter::new(data, entry_start, end) {
        if &tag == b"mp4a" {
            parse_mp4a(data, ds, be, mp4);

            return;
        }
    }
}

fn parse_mp4a(data: &[u8], data_start: usize, end: usize, mp4: &mut Mp4<'_>) {
    // mp4a sample entry (ISO 14496-14):
    //   6 reserved + 2 data_ref_index + 8 reserved
    //   2 channel_count + 2 sample_size + 2 pre_defined + 2 reserved
    //   4 sample_rate (16.16 fixed point)
    //   child boxes (esds, etc.)
    let fixed_size = 28;

    if data_start + fixed_size > end {
        return;
    }

    mp4.audio_channels = read_u16(data, data_start + 16).unwrap_or(0);
    mp4.audio_sample_rate = read_u32(data, data_start + 24).unwrap_or(0) >> 16;

    let children_start = data_start + fixed_size;

    for (tag, ds, be) in BoxIter::new(data, children_start, end) {
        if &tag == b"esds" {
            parse_esds(data, ds, be, mp4);

            return;
        }
    }
}

fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    data.get(offset..offset + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
}

fn read_descriptor_length(data: &[u8], offset: usize, end: usize) -> Option<(u32, usize)> {
    let mut len: u32 = 0;
    let mut pos = offset;

    for _ in 0..4 {
        if pos >= end {
            return None;
        }

        let b = data[pos];

        len = (len << 7) | (b & 0x7F) as u32;
        pos += 1;

        if b & 0x80 == 0 {
            return Some((len, pos));
        }
    }

    None
}

fn parse_esds(data: &[u8], data_start: usize, end: usize, mp4: &mut Mp4<'_>) {
    // AudioToolbox's kAudioConverterDecompressionMagicCookie expects the
    // full ESDS descriptor payload (everything after version+flags).
    let Some(pos) = fullbox_payload(data_start, end) else {
        return;
    };

    if pos >= end || data[pos] != 0x03 {
        return;
    }

    mp4.audio_config_offset = pos;
    mp4.audio_config_size = end - pos;
}

// ── Public API ─────────────────────────────────────────────────────

impl<'a> Mp4<'a> {
    pub fn ns_per_frame(&self) -> u64 {
        if self.timescale == 0 || self.total_samples == 0 {
            return 0;
        }

        self.duration * 1_000_000_000 / (self.timescale as u64 * self.total_samples as u64)
    }

    pub fn avc_config(&self) -> Option<(u8, &'a [u8])> {
        if self.avcc_size < 7 {
            return None;
        }

        let body = self
            .data
            .get(self.avcc_offset..self.avcc_offset + self.avcc_size)?;
        let nal_length_size = (body[4] & 0x03) + 1;

        Some((nal_length_size, body))
    }

    pub fn samples(&self) -> SampleIter<'a> {
        let first_chunk_offset = if self.stco_count > 0 {
            if self.co64 {
                read_u64(self.data, self.stco_offset).unwrap_or(0)
            } else {
                read_u32(self.data, self.stco_offset).unwrap_or(0) as u64
            }
        } else {
            0
        };
        let first_samples_per_chunk = if self.stsc_count > 0 {
            read_u32(self.data, self.stsc_offset + 4).unwrap_or(1)
        } else {
            1
        };
        let first_stts_delta = if self.stts_count > 0 {
            read_u32(self.data, self.stts_offset + 4).unwrap_or(0)
        } else {
            0
        };
        let first_stts_remaining = if self.stts_count > 0 {
            read_u32(self.data, self.stts_offset).unwrap_or(0)
        } else {
            0
        };
        let first_stss_sample = if self.stss_count > 0 {
            read_u32(self.data, self.stss_offset).unwrap_or(0)
        } else {
            0
        };

        SampleIter {
            data: self.data,
            total_samples: self.total_samples,
            sample_idx: 0,
            stsc_offset: self.stsc_offset,
            stsc_count: self.stsc_count,
            stsc_idx: 0,
            samples_per_chunk: first_samples_per_chunk,
            chunk_idx: 1,
            sample_in_chunk: 0,
            chunk_offset: first_chunk_offset,
            stco_offset: self.stco_offset,
            stco_count: self.stco_count,
            co64: self.co64,
            stsz_offset: self.stsz_offset,
            stsz_default_size: self.stsz_default_size,
            stts_offset: self.stts_offset,
            stts_count: self.stts_count,
            stts_idx: 0,
            stts_remaining: first_stts_remaining,
            stts_delta: first_stts_delta,
            dts_ticks: 0,
            ctts_offset: self.ctts_offset,
            ctts_count: self.ctts_count,
            ctts_idx: 0,
            ctts_remaining: if self.ctts_count > 0 {
                read_u32(self.data, self.ctts_offset).unwrap_or(0)
            } else {
                0
            },
            ctts_offset_val: if self.ctts_count > 0 {
                read_u32(self.data, self.ctts_offset + 4).unwrap_or(0)
            } else {
                0
            },
            stss_offset: self.stss_offset,
            stss_count: self.stss_count,
            stss_idx: 0,
            stss_next: first_stss_sample,
        }
    }

    pub fn sample_data(&self, sample: &SampleRef) -> Option<&'a [u8]> {
        let start = sample.offset as usize;
        let end = start + sample.size as usize;

        self.data.get(start..end)
    }

    pub fn has_audio(&self) -> bool {
        self.audio_total_samples > 0 && self.audio_stts_count > 0
    }

    pub fn audio_sample_rate(&self) -> u32 {
        self.audio_sample_rate
    }

    pub fn audio_channels(&self) -> u16 {
        self.audio_channels
    }

    pub fn audio_timescale(&self) -> u32 {
        self.audio_timescale
    }

    pub fn audio_duration_ns(&self) -> u64 {
        if self.audio_timescale == 0 {
            return 0;
        }

        self.audio_duration * 1_000_000_000 / self.audio_timescale as u64
    }

    pub fn audio_config(&self) -> Option<&'a [u8]> {
        if self.audio_config_size == 0 {
            return None;
        }

        self.data
            .get(self.audio_config_offset..self.audio_config_offset + self.audio_config_size)
    }

    pub fn audio_samples(&self) -> SampleIter<'a> {
        let first_chunk_offset = if self.audio_stco_count > 0 {
            if self.audio_co64 {
                read_u64(self.data, self.audio_stco_offset).unwrap_or(0)
            } else {
                read_u32(self.data, self.audio_stco_offset).unwrap_or(0) as u64
            }
        } else {
            0
        };
        let first_samples_per_chunk = if self.audio_stsc_count > 0 {
            read_u32(self.data, self.audio_stsc_offset + 4).unwrap_or(1)
        } else {
            1
        };
        let first_stts_delta = if self.audio_stts_count > 0 {
            read_u32(self.data, self.audio_stts_offset + 4).unwrap_or(0)
        } else {
            0
        };
        let first_stts_remaining = if self.audio_stts_count > 0 {
            read_u32(self.data, self.audio_stts_offset).unwrap_or(0)
        } else {
            0
        };

        SampleIter {
            data: self.data,
            total_samples: self.audio_total_samples,
            sample_idx: 0,
            stsc_offset: self.audio_stsc_offset,
            stsc_count: self.audio_stsc_count,
            stsc_idx: 0,
            samples_per_chunk: first_samples_per_chunk,
            chunk_idx: 1,
            sample_in_chunk: 0,
            chunk_offset: first_chunk_offset,
            stco_offset: self.audio_stco_offset,
            stco_count: self.audio_stco_count,
            co64: self.audio_co64,
            stsz_offset: self.audio_stsz_offset,
            stsz_default_size: self.audio_stsz_default_size,
            stts_offset: self.audio_stts_offset,
            stts_count: self.audio_stts_count,
            stts_idx: 0,
            stts_remaining: first_stts_remaining,
            stts_delta: first_stts_delta,
            dts_ticks: 0,
            ctts_offset: 0,
            ctts_count: 0,
            ctts_idx: 0,
            ctts_remaining: 0,
            ctts_offset_val: 0,
            stss_offset: 0,
            stss_count: 0,
            stss_idx: 0,
            stss_next: 0,
        }
    }
}

// ── SampleIter ─────────────────────────────────────────────────────

pub struct SampleIter<'a> {
    data: &'a [u8],
    total_samples: u32,
    sample_idx: u32,

    stsc_offset: usize,
    stsc_count: u32,
    stsc_idx: u32,
    samples_per_chunk: u32,

    chunk_idx: u32,
    sample_in_chunk: u32,
    chunk_offset: u64,

    stco_offset: usize,
    stco_count: u32,
    co64: bool,

    stsz_offset: usize,
    stsz_default_size: u32,

    stts_offset: usize,
    stts_count: u32,
    stts_idx: u32,
    stts_remaining: u32,
    stts_delta: u32,
    dts_ticks: u64,

    ctts_offset: usize,
    ctts_count: u32,
    ctts_idx: u32,
    ctts_remaining: u32,
    ctts_offset_val: u32,

    stss_offset: usize,
    stss_count: u32,
    stss_idx: u32,
    stss_next: u32,
}

impl<'a> SampleIter<'a> {
    fn read_chunk_offset(&self, chunk_0based: u32) -> u64 {
        if self.co64 {
            read_u64(self.data, self.stco_offset + chunk_0based as usize * 8).unwrap_or(0)
        } else {
            read_u32(self.data, self.stco_offset + chunk_0based as usize * 4).unwrap_or(0) as u64
        }
    }

    fn read_sample_size(&self, sample_0based: u32) -> u32 {
        if self.stsz_default_size != 0 {
            self.stsz_default_size
        } else {
            read_u32(self.data, self.stsz_offset + sample_0based as usize * 4).unwrap_or(0)
        }
    }

    fn stsc_entry_first_chunk(&self, idx: u32) -> u32 {
        read_u32(self.data, self.stsc_offset + idx as usize * 12).unwrap_or(0)
    }

    fn stsc_entry_samples(&self, idx: u32) -> u32 {
        read_u32(self.data, self.stsc_offset + idx as usize * 12 + 4).unwrap_or(0)
    }
}

impl<'a> Iterator for SampleIter<'a> {
    type Item = SampleRef;

    fn next(&mut self) -> Option<SampleRef> {
        if self.sample_idx >= self.total_samples {
            return None;
        }

        // Advance to next chunk if current chunk is exhausted.
        if self.sample_in_chunk >= self.samples_per_chunk {
            self.chunk_idx += 1;
            self.sample_in_chunk = 0;

            let chunk_0based = self.chunk_idx - 1;

            if chunk_0based < self.stco_count {
                self.chunk_offset = self.read_chunk_offset(chunk_0based);
            }

            // Check if the next stsc entry starts at this chunk.
            let next_stsc = self.stsc_idx + 1;

            if next_stsc < self.stsc_count
                && self.stsc_entry_first_chunk(next_stsc) == self.chunk_idx
            {
                self.stsc_idx = next_stsc;
                self.samples_per_chunk = self.stsc_entry_samples(next_stsc);
            }
        }

        let size = self.read_sample_size(self.sample_idx);
        let offset = self.chunk_offset;

        self.chunk_offset += size as u64;

        let dts = self.dts_ticks;
        // PTS = DTS + ctts offset
        let pts = if self.ctts_count > 0 {
            dts + self.ctts_offset_val as u64
        } else {
            dts
        };

        // Advance DTS
        self.dts_ticks += self.stts_delta as u64;
        self.stts_remaining -= 1;

        if self.stts_remaining == 0 {
            self.stts_idx += 1;

            if self.stts_idx < self.stts_count {
                let entry_off = self.stts_offset + self.stts_idx as usize * 8;

                self.stts_remaining = read_u32(self.data, entry_off).unwrap_or(0);
                self.stts_delta = read_u32(self.data, entry_off + 4).unwrap_or(0);
            }
        }

        // Advance ctts
        if self.ctts_count > 0 {
            self.ctts_remaining -= 1;

            if self.ctts_remaining == 0 {
                self.ctts_idx += 1;

                if self.ctts_idx < self.ctts_count {
                    let entry_off = self.ctts_offset + self.ctts_idx as usize * 8;

                    self.ctts_remaining = read_u32(self.data, entry_off).unwrap_or(0);
                    self.ctts_offset_val = read_u32(self.data, entry_off + 4).unwrap_or(0);
                }
            }
        }

        // Keyframe: stss_count == 0 means all are keyframes
        let is_keyframe = if self.stss_count == 0 {
            true
        } else {
            let sample_1based = self.sample_idx + 1;
            let kf = sample_1based == self.stss_next;

            if kf {
                self.stss_idx += 1;

                if self.stss_idx < self.stss_count {
                    self.stss_next =
                        read_u32(self.data, self.stss_offset + self.stss_idx as usize * 4)
                            .unwrap_or(0);
                }
            }
            kf
        };

        self.sample_idx += 1;
        self.sample_in_chunk += 1;

        Some(SampleRef {
            offset,
            size,
            dts_ticks: dts,
            pts_ticks: pts,
            is_keyframe,
        })
    }
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    extern crate alloc;

    use alloc::vec::Vec;

    use super::*;

    fn write_u16_be(buf: &mut Vec<u8>, val: u16) {
        buf.extend_from_slice(&val.to_be_bytes());
    }

    fn write_u32_be(buf: &mut Vec<u8>, val: u32) {
        buf.extend_from_slice(&val.to_be_bytes());
    }

    fn write_u64_be(buf: &mut Vec<u8>, val: u64) {
        buf.extend_from_slice(&val.to_be_bytes());
    }

    fn write_fourcc(buf: &mut Vec<u8>, tag: &[u8; 4]) {
        buf.extend_from_slice(tag);
    }

    fn patch_u32_be(buf: &mut Vec<u8>, offset: usize, val: u32) {
        buf[offset..offset + 4].copy_from_slice(&val.to_be_bytes());
    }

    /// Start a box, return the offset where size was written (to be patched).
    fn begin_box(buf: &mut Vec<u8>, tag: &[u8; 4]) -> usize {
        let size_offset = buf.len();

        write_u32_be(buf, 0); // placeholder size
        write_fourcc(buf, tag);

        size_offset
    }

    fn end_box(buf: &mut Vec<u8>, size_offset: usize) {
        let size = (buf.len() - size_offset) as u32;

        patch_u32_be(buf, size_offset, size);
    }

    /// Start a full box (version + flags after the header).
    fn begin_fullbox(buf: &mut Vec<u8>, tag: &[u8; 4], version: u8) -> usize {
        let off = begin_box(buf, tag);

        buf.push(version);
        buf.extend_from_slice(&[0, 0, 0]); // flags

        off
    }

    struct SampleSpec<'a> {
        data: &'a [u8],
        duration: u32,
    }

    fn build_avcc(
        buf: &mut Vec<u8>,
        profile: u8,
        level: u8,
        nal_length_size: u8,
        sps: &[&[u8]],
        pps: &[&[u8]],
    ) {
        let off = begin_box(buf, b"avcC");

        buf.push(1); // configurationVersion
        buf.push(profile); // AVCProfileIndication
        buf.push(0); // profile_compatibility
        buf.push(level); // AVCLevelIndication
        buf.push(0xFC | (nal_length_size - 1)); // lengthSizeMinusOne (top 6 bits set)
        buf.push(0xE0 | sps.len() as u8); // numSPS (top 3 bits set)

        for s in sps {
            write_u16_be(buf, s.len() as u16);

            buf.extend_from_slice(s);
        }

        buf.push(pps.len() as u8); // numPPS

        for p in pps {
            write_u16_be(buf, p.len() as u16);

            buf.extend_from_slice(p);
        }

        end_box(buf, off);
    }

    /// Build a minimal valid MP4 with a single H.264 video track.
    fn minimal_mp4(width: u32, height: u32, timescale: u32, samples: &[SampleSpec<'_>]) -> Vec<u8> {
        minimal_mp4_ex(width, height, timescale, samples, &[], false, &[])
    }

    fn minimal_mp4_ex(
        width: u32,
        height: u32,
        timescale: u32,
        samples: &[SampleSpec<'_>],
        keyframes: &[u32],
        use_co64: bool,
        ctts_offsets: &[u32],
    ) -> Vec<u8> {
        let mut buf = Vec::new();
        // ftyp
        let ftyp = begin_box(&mut buf, b"ftyp");

        write_fourcc(&mut buf, b"isom");
        write_u32_be(&mut buf, 0x200); // minor_version
        write_fourcc(&mut buf, b"isom");
        end_box(&mut buf, ftyp);

        // Compute mdat content and chunk offsets.
        // We put one sample per chunk by default unless samples are back-to-back.
        // For simplicity: all samples in a single chunk.
        let mut sample_data_concat = Vec::new();

        for s in samples {
            sample_data_concat.extend_from_slice(s.data);
        }

        // We need to know mdat offset to compute stco. mdat comes after moov.
        // Build moov first as a vec, measure its size, then compute mdat_offset.
        let moov_buf = build_moov(
            width,
            height,
            timescale,
            samples,
            keyframes,
            use_co64,
            ctts_offsets,
            0, // placeholder chunk offset, will be patched
        );
        let moov_size = moov_buf.len();
        let mdat_header_size = 8u64;
        let mdat_offset = buf.len() as u64 + moov_size as u64 + mdat_header_size;
        // Rebuild moov with correct chunk offset.
        let moov_buf = build_moov(
            width,
            height,
            timescale,
            samples,
            keyframes,
            use_co64,
            ctts_offsets,
            mdat_offset,
        );

        buf.extend_from_slice(&moov_buf);

        // mdat
        let mdat = begin_box(&mut buf, b"mdat");

        buf.extend_from_slice(&sample_data_concat);

        end_box(&mut buf, mdat);

        buf
    }

    fn build_moov(
        width: u32,
        height: u32,
        timescale: u32,
        samples: &[SampleSpec<'_>],
        keyframes: &[u32],
        use_co64: bool,
        ctts_offsets: &[u32],
        chunk_offset: u64,
    ) -> Vec<u8> {
        let mut buf = Vec::new();
        let total_duration: u64 = samples.iter().map(|s| s.duration as u64).sum();

        let moov = begin_box(&mut buf, b"moov");

        // mvhd
        {
            let off = begin_fullbox(&mut buf, b"mvhd", 0);

            write_u32_be(&mut buf, 0); // creation_time
            write_u32_be(&mut buf, 0); // modification_time
            write_u32_be(&mut buf, timescale);
            write_u32_be(&mut buf, total_duration as u32);
            write_u32_be(&mut buf, 0x00010000); // rate 1.0
            write_u16_be(&mut buf, 0x0100); // volume 1.0

            buf.extend_from_slice(&[0u8; 10]); // reserved

            // matrix (9 * u32 = 36 bytes)
            for &v in &[0x00010000u32, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000] {
                write_u32_be(&mut buf, v);
            }

            buf.extend_from_slice(&[0u8; 24]); // pre_defined

            write_u32_be(&mut buf, 2); // next_track_ID
            end_box(&mut buf, off);
        }
        // trak
        {
            let trak = begin_box(&mut buf, b"trak");

            // tkhd
            {
                let off = begin_fullbox(&mut buf, b"tkhd", 0);

                buf[off + 8 + 1] = 0;
                buf[off + 8 + 2] = 0;
                buf[off + 8 + 3] = 3; // flags = track_enabled | track_in_movie

                write_u32_be(&mut buf, 0); // creation_time
                write_u32_be(&mut buf, 0); // modification_time
                write_u32_be(&mut buf, 1); // track_ID
                write_u32_be(&mut buf, 0); // reserved
                write_u32_be(&mut buf, total_duration as u32); // duration

                buf.extend_from_slice(&[0u8; 8]); // reserved

                write_u16_be(&mut buf, 0); // layer
                write_u16_be(&mut buf, 0); // alternate_group
                write_u16_be(&mut buf, 0); // volume
                write_u16_be(&mut buf, 0); // reserved

                for &v in &[0x00010000u32, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000] {
                    write_u32_be(&mut buf, v);
                }

                write_u32_be(&mut buf, width << 16); // width 16.16
                write_u32_be(&mut buf, height << 16); // height 16.16
                end_box(&mut buf, off);
            }
            // mdia
            {
                let mdia = begin_box(&mut buf, b"mdia");

                // mdhd
                {
                    let off = begin_fullbox(&mut buf, b"mdhd", 0);

                    write_u32_be(&mut buf, 0); // creation_time
                    write_u32_be(&mut buf, 0); // modification_time
                    write_u32_be(&mut buf, timescale);
                    write_u32_be(&mut buf, total_duration as u32);
                    write_u16_be(&mut buf, 0x55C4); // language (und)
                    write_u16_be(&mut buf, 0); // pre_defined
                    end_box(&mut buf, off);
                }
                // hdlr
                {
                    let off = begin_fullbox(&mut buf, b"hdlr", 0);

                    write_u32_be(&mut buf, 0); // pre_defined
                    write_fourcc(&mut buf, b"vide"); // handler_type

                    buf.extend_from_slice(&[0u8; 12]); // reserved
                    buf.push(0); // name (null terminated)

                    end_box(&mut buf, off);
                }
                // minf
                {
                    let minf = begin_box(&mut buf, b"minf");

                    // vmhd
                    {
                        let off = begin_fullbox(&mut buf, b"vmhd", 0);

                        write_u16_be(&mut buf, 0); // graphicsmode

                        buf.extend_from_slice(&[0u8; 6]); // opcolor

                        end_box(&mut buf, off);
                    }
                    // stbl
                    {
                        let stbl = begin_box(&mut buf, b"stbl");

                        // stsd
                        {
                            let off = begin_fullbox(&mut buf, b"stsd", 0);

                            write_u32_be(&mut buf, 1); // entry_count

                            // avc1
                            {
                                let avc1 = begin_box(&mut buf, b"avc1");

                                buf.extend_from_slice(&[0u8; 6]); // reserved

                                write_u16_be(&mut buf, 1); // data_reference_index

                                buf.extend_from_slice(&[0u8; 16]); // pre_defined + reserved

                                write_u16_be(&mut buf, width as u16);
                                write_u16_be(&mut buf, height as u16);
                                write_u32_be(&mut buf, 0x00480000); // horizresolution 72 dpi
                                write_u32_be(&mut buf, 0x00480000); // vertresolution 72 dpi
                                write_u32_be(&mut buf, 0); // reserved
                                write_u16_be(&mut buf, 1); // frame_count

                                buf.extend_from_slice(&[0u8; 32]); // compressorname

                                write_u16_be(&mut buf, 0x0018); // depth
                                write_u16_be(&mut buf, 0xFFFF); // pre_defined = -1
                                // avcC
                                build_avcc(
                                    &mut buf,
                                    66,
                                    30,
                                    4,
                                    &[b"\x67\x42\x00\x1E\xAB\xCD"],
                                    &[b"\x68\xCE\x38\x80"],
                                );
                                end_box(&mut buf, avc1);
                            }

                            end_box(&mut buf, off);
                        }
                        // stts
                        {
                            let off = begin_fullbox(&mut buf, b"stts", 0);

                            write_u32_be(&mut buf, 1); // entry_count
                            write_u32_be(&mut buf, samples.len() as u32); // sample_count

                            let delta = if samples.is_empty() {
                                0
                            } else {
                                samples[0].duration
                            };

                            write_u32_be(&mut buf, delta); // sample_delta
                            end_box(&mut buf, off);
                        }

                        // ctts (optional)
                        if !ctts_offsets.is_empty() {
                            let off = begin_fullbox(&mut buf, b"ctts", 0);

                            write_u32_be(&mut buf, ctts_offsets.len() as u32);

                            for &ct in ctts_offsets {
                                write_u32_be(&mut buf, 1); // sample_count = 1 per entry
                                write_u32_be(&mut buf, ct); // sample_offset
                            }

                            end_box(&mut buf, off);
                        }

                        // stsc — one entry: all samples in one chunk
                        {
                            let off = begin_fullbox(&mut buf, b"stsc", 0);

                            write_u32_be(&mut buf, 1); // entry_count
                            write_u32_be(&mut buf, 1); // first_chunk (1-based)
                            write_u32_be(&mut buf, samples.len() as u32); // samples_per_chunk
                            write_u32_be(&mut buf, 1); // sample_description_index
                            end_box(&mut buf, off);
                        }
                        // stsz
                        {
                            let off = begin_fullbox(&mut buf, b"stsz", 0);

                            write_u32_be(&mut buf, 0); // sample_size (0 = variable)
                            write_u32_be(&mut buf, samples.len() as u32);

                            for s in samples {
                                write_u32_be(&mut buf, s.data.len() as u32);
                            }

                            end_box(&mut buf, off);
                        }

                        // stco or co64
                        if use_co64 {
                            let off = begin_fullbox(&mut buf, b"co64", 0);

                            write_u32_be(&mut buf, 1); // entry_count
                            write_u64_be(&mut buf, chunk_offset);
                            end_box(&mut buf, off);
                        } else {
                            let off = begin_fullbox(&mut buf, b"stco", 0);

                            write_u32_be(&mut buf, 1); // entry_count
                            write_u32_be(&mut buf, chunk_offset as u32);
                            end_box(&mut buf, off);
                        }

                        // stss (optional)
                        if !keyframes.is_empty() {
                            let off = begin_fullbox(&mut buf, b"stss", 0);

                            write_u32_be(&mut buf, keyframes.len() as u32);

                            for &kf in keyframes {
                                write_u32_be(&mut buf, kf); // 1-based sample number
                            }

                            end_box(&mut buf, off);
                        }

                        end_box(&mut buf, stbl);
                    }

                    end_box(&mut buf, minf);
                }

                end_box(&mut buf, mdia);
            }

            end_box(&mut buf, trak);
        }

        end_box(&mut buf, moov);

        buf
    }

    fn minimal_mp4_multi_chunk(
        width: u32,
        height: u32,
        timescale: u32,
        chunks: &[&[SampleSpec<'_>]],
    ) -> Vec<u8> {
        let mut buf = Vec::new();
        // ftyp
        let ftyp = begin_box(&mut buf, b"ftyp");

        write_fourcc(&mut buf, b"isom");
        write_u32_be(&mut buf, 0x200);
        write_fourcc(&mut buf, b"isom");
        end_box(&mut buf, ftyp);

        let all_samples: Vec<&SampleSpec<'_>> = chunks.iter().flat_map(|c| c.iter()).collect();
        let total_duration: u64 = all_samples.iter().map(|s| s.duration as u64).sum();
        let placeholder_offsets: Vec<u64> = chunks.iter().map(|_| 0u64).collect();
        let moov_placeholder = build_moov_multi_chunk(
            width,
            height,
            timescale,
            total_duration,
            chunks,
            &placeholder_offsets,
        );
        let mdat_data_start = buf.len() + moov_placeholder.len() + 8; // +8 for mdat header
        let mut chunk_offsets = Vec::new();
        let mut offset = mdat_data_start as u64;

        for chunk in chunks {
            chunk_offsets.push(offset);

            for s in *chunk {
                offset += s.data.len() as u64;
            }
        }

        let moov_buf = build_moov_multi_chunk(
            width,
            height,
            timescale,
            total_duration,
            chunks,
            &chunk_offsets,
        );

        buf.extend_from_slice(&moov_buf);

        // mdat
        let mdat = begin_box(&mut buf, b"mdat");

        for chunk in chunks {
            for s in *chunk {
                buf.extend_from_slice(s.data);
            }
        }

        end_box(&mut buf, mdat);

        buf
    }

    fn build_moov_multi_chunk(
        width: u32,
        height: u32,
        timescale: u32,
        total_duration: u64,
        chunks: &[&[SampleSpec<'_>]],
        chunk_offsets: &[u64],
    ) -> Vec<u8> {
        let mut buf = Vec::new();
        let all_samples: Vec<&SampleSpec<'_>> = chunks.iter().flat_map(|c| c.iter()).collect();
        let moov = begin_box(&mut buf, b"moov");

        // mvhd
        {
            let off = begin_fullbox(&mut buf, b"mvhd", 0);

            write_u32_be(&mut buf, 0);
            write_u32_be(&mut buf, 0);
            write_u32_be(&mut buf, timescale);
            write_u32_be(&mut buf, total_duration as u32);
            write_u32_be(&mut buf, 0x00010000);
            write_u16_be(&mut buf, 0x0100);

            buf.extend_from_slice(&[0u8; 10]);

            for &v in &[0x00010000u32, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000] {
                write_u32_be(&mut buf, v);
            }

            buf.extend_from_slice(&[0u8; 24]);

            write_u32_be(&mut buf, 2);
            end_box(&mut buf, off);
        }
        // trak
        {
            let trak = begin_box(&mut buf, b"trak");

            // tkhd
            {
                let off = begin_fullbox(&mut buf, b"tkhd", 0);
                let flags_offset = off + 8 + 1;

                buf[flags_offset] = 0;
                buf[flags_offset + 1] = 0;
                buf[flags_offset + 2] = 3;

                write_u32_be(&mut buf, 0);
                write_u32_be(&mut buf, 0);
                write_u32_be(&mut buf, 1);
                write_u32_be(&mut buf, 0);
                write_u32_be(&mut buf, total_duration as u32);

                buf.extend_from_slice(&[0u8; 8]);

                write_u16_be(&mut buf, 0);
                write_u16_be(&mut buf, 0);
                write_u16_be(&mut buf, 0);
                write_u16_be(&mut buf, 0);

                for &v in &[0x00010000u32, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000] {
                    write_u32_be(&mut buf, v);
                }

                write_u32_be(&mut buf, width << 16);
                write_u32_be(&mut buf, height << 16);
                end_box(&mut buf, off);
            }
            // mdia
            {
                let mdia = begin_box(&mut buf, b"mdia");

                // mdhd
                {
                    let off = begin_fullbox(&mut buf, b"mdhd", 0);

                    write_u32_be(&mut buf, 0);
                    write_u32_be(&mut buf, 0);
                    write_u32_be(&mut buf, timescale);
                    write_u32_be(&mut buf, total_duration as u32);
                    write_u16_be(&mut buf, 0x55C4);
                    write_u16_be(&mut buf, 0);
                    end_box(&mut buf, off);
                }
                // hdlr
                {
                    let off = begin_fullbox(&mut buf, b"hdlr", 0);

                    write_u32_be(&mut buf, 0);
                    write_fourcc(&mut buf, b"vide");

                    buf.extend_from_slice(&[0u8; 12]);
                    buf.push(0);

                    end_box(&mut buf, off);
                }
                // minf
                {
                    let minf = begin_box(&mut buf, b"minf");

                    // vmhd
                    {
                        let off = begin_fullbox(&mut buf, b"vmhd", 0);

                        write_u16_be(&mut buf, 0);

                        buf.extend_from_slice(&[0u8; 6]);

                        end_box(&mut buf, off);
                    }
                    // stbl
                    {
                        let stbl = begin_box(&mut buf, b"stbl");

                        // stsd
                        {
                            let off = begin_fullbox(&mut buf, b"stsd", 0);

                            write_u32_be(&mut buf, 1);

                            let avc1 = begin_box(&mut buf, b"avc1");

                            buf.extend_from_slice(&[0u8; 6]);

                            write_u16_be(&mut buf, 1);

                            buf.extend_from_slice(&[0u8; 16]);

                            write_u16_be(&mut buf, width as u16);
                            write_u16_be(&mut buf, height as u16);
                            write_u32_be(&mut buf, 0x00480000);
                            write_u32_be(&mut buf, 0x00480000);
                            write_u32_be(&mut buf, 0);
                            write_u16_be(&mut buf, 1);

                            buf.extend_from_slice(&[0u8; 32]);

                            write_u16_be(&mut buf, 0x0018);
                            write_u16_be(&mut buf, 0xFFFF);
                            build_avcc(
                                &mut buf,
                                66,
                                30,
                                4,
                                &[b"\x67\x42\x00\x1E\xAB\xCD"],
                                &[b"\x68\xCE\x38\x80"],
                            );
                            end_box(&mut buf, avc1);
                            end_box(&mut buf, off);
                        }
                        // stts — single entry with uniform delta
                        {
                            let off = begin_fullbox(&mut buf, b"stts", 0);

                            write_u32_be(&mut buf, 1);
                            write_u32_be(&mut buf, all_samples.len() as u32);

                            let delta = if all_samples.is_empty() {
                                0
                            } else {
                                all_samples[0].duration
                            };

                            write_u32_be(&mut buf, delta);
                            end_box(&mut buf, off);
                        }
                        // stsc — one entry per chunk with different samples_per_chunk
                        {
                            let off = begin_fullbox(&mut buf, b"stsc", 0);

                            write_u32_be(&mut buf, chunks.len() as u32);

                            for (i, chunk) in chunks.iter().enumerate() {
                                write_u32_be(&mut buf, (i + 1) as u32); // first_chunk (1-based)
                                write_u32_be(&mut buf, chunk.len() as u32); // samples_per_chunk
                                write_u32_be(&mut buf, 1); // sample_description_index
                            }

                            end_box(&mut buf, off);
                        }
                        // stsz
                        {
                            let off = begin_fullbox(&mut buf, b"stsz", 0);

                            write_u32_be(&mut buf, 0);
                            write_u32_be(&mut buf, all_samples.len() as u32);

                            for s in &all_samples {
                                write_u32_be(&mut buf, s.data.len() as u32);
                            }

                            end_box(&mut buf, off);
                        }
                        // stco
                        {
                            let off = begin_fullbox(&mut buf, b"stco", 0);

                            write_u32_be(&mut buf, chunk_offsets.len() as u32);

                            for &co in chunk_offsets {
                                write_u32_be(&mut buf, co as u32);
                            }

                            end_box(&mut buf, off);
                        }

                        end_box(&mut buf, stbl);
                    }

                    end_box(&mut buf, minf);
                }

                end_box(&mut buf, mdia);
            }

            end_box(&mut buf, trak);
        }

        end_box(&mut buf, moov);
        buf
    }

    #[test]
    fn parse_minimal_mp4() {
        let data = minimal_mp4(
            1920,
            1080,
            30000,
            &[
                SampleSpec {
                    data: b"frame0",
                    duration: 1001,
                },
                SampleSpec {
                    data: b"frame1",
                    duration: 1001,
                },
                SampleSpec {
                    data: b"frame2",
                    duration: 1001,
                },
            ],
        );
        let mp4 = parse(&data).unwrap();

        assert_eq!(mp4.width, 1920);
        assert_eq!(mp4.height, 1080);
        assert_eq!(mp4.timescale, 30000);
        assert_eq!(mp4.total_samples, 3);
        assert_eq!(mp4.duration, 3003);
    }

    #[test]
    fn ns_per_frame_calculation() {
        let data = minimal_mp4(
            640,
            480,
            30000,
            &[
                SampleSpec {
                    data: b"a",
                    duration: 1001,
                },
                SampleSpec {
                    data: b"b",
                    duration: 1001,
                },
            ],
        );
        let mp4 = parse(&data).unwrap();
        // duration=2002, timescale=30000, total_samples=2
        // ns_per_frame = 2002 * 1_000_000_000 / (30000 * 2) = 33_366_666
        let ns = mp4.ns_per_frame();

        assert_eq!(ns, 2002 * 1_000_000_000 / (30000 * 2));
    }

    #[test]
    fn iterate_samples() {
        let samples = [
            SampleSpec {
                data: b"AAAA",
                duration: 1000,
            },
            SampleSpec {
                data: b"BBBBBB",
                duration: 1000,
            },
            SampleSpec {
                data: b"CC",
                duration: 1000,
            },
        ];
        let data = minimal_mp4(320, 240, 24000, &samples);
        let mp4 = parse(&data).unwrap();
        let refs: Vec<SampleRef> = mp4.samples().collect();

        assert_eq!(refs.len(), 3);
        assert_eq!(refs[0].size, 4);
        assert_eq!(refs[1].size, 6);
        assert_eq!(refs[2].size, 2);
        assert_eq!(refs[0].dts_ticks, 0);
        assert_eq!(refs[1].dts_ticks, 1000);
        assert_eq!(refs[2].dts_ticks, 2000);
        // No stss → all keyframes
        assert!(refs[0].is_keyframe);
        assert!(refs[1].is_keyframe);
        assert!(refs[2].is_keyframe);
        assert_eq!(mp4.sample_data(&refs[0]), Some(b"AAAA".as_slice()));
        assert_eq!(mp4.sample_data(&refs[1]), Some(b"BBBBBB".as_slice()));
        assert_eq!(mp4.sample_data(&refs[2]), Some(b"CC".as_slice()));
        // Consecutive offsets
        assert_eq!(refs[1].offset, refs[0].offset + 4);
        assert_eq!(refs[2].offset, refs[1].offset + 6);
    }

    #[test]
    fn keyframe_detection() {
        let samples = [
            SampleSpec {
                data: b"I",
                duration: 1000,
            },
            SampleSpec {
                data: b"P",
                duration: 1000,
            },
            SampleSpec {
                data: b"P",
                duration: 1000,
            },
            SampleSpec {
                data: b"I",
                duration: 1000,
            },
            SampleSpec {
                data: b"P",
                duration: 1000,
            },
        ];
        // stss entries are 1-indexed: keyframes at sample 1 and 4
        let data = minimal_mp4_ex(320, 240, 24000, &samples, &[1, 4], false, &[]);
        let mp4 = parse(&data).unwrap();
        let refs: Vec<SampleRef> = mp4.samples().collect();

        assert_eq!(refs.len(), 5);
        assert!(refs[0].is_keyframe);
        assert!(!refs[1].is_keyframe);
        assert!(!refs[2].is_keyframe);
        assert!(refs[3].is_keyframe);
        assert!(!refs[4].is_keyframe);
    }

    #[test]
    fn avc_config_extraction() {
        let data = minimal_mp4(
            320,
            240,
            24000,
            &[SampleSpec {
                data: b"x",
                duration: 1000,
            }],
        );
        let mp4 = parse(&data).unwrap();
        let (nal_len_size, body) = mp4.avc_config().unwrap();

        assert_eq!(nal_len_size, 4);
        assert_eq!(body[0], 1); // configurationVersion
        assert_eq!(body[1], 66); // profile (Baseline)
        assert_eq!(body[3], 30); // level

        let num_sps = (body[5] & 0x1F) as usize;

        assert_eq!(num_sps, 1);

        let sps_len = u16::from_be_bytes([body[6], body[7]]) as usize;

        assert_eq!(sps_len, 6);
        assert_eq!(&body[8..8 + sps_len], b"\x67\x42\x00\x1E\xAB\xCD");

        let pps_count_offset = 8 + sps_len;
        let num_pps = body[pps_count_offset] as usize;

        assert_eq!(num_pps, 1);

        let pps_len =
            u16::from_be_bytes([body[pps_count_offset + 1], body[pps_count_offset + 2]]) as usize;

        assert_eq!(pps_len, 4);
        assert_eq!(
            &body[pps_count_offset + 3..pps_count_offset + 3 + pps_len],
            b"\x68\xCE\x38\x80"
        );
    }

    #[test]
    fn multi_chunk_stsc() {
        let chunk1 = [
            SampleSpec {
                data: b"A1",
                duration: 1000,
            },
            SampleSpec {
                data: b"A2A2",
                duration: 1000,
            },
        ];
        let chunk2 = [SampleSpec {
            data: b"B1B1B1",
            duration: 1000,
        }];
        let chunk3 = [
            SampleSpec {
                data: b"C1",
                duration: 1000,
            },
            SampleSpec {
                data: b"C2C",
                duration: 1000,
            },
            SampleSpec {
                data: b"C3C3",
                duration: 1000,
            },
        ];
        let data =
            minimal_mp4_multi_chunk(640, 480, 24000, &[&chunk1[..], &chunk2[..], &chunk3[..]]);
        let mp4 = parse(&data).unwrap();

        assert_eq!(mp4.total_samples, 6);

        let refs: Vec<SampleRef> = mp4.samples().collect();

        assert_eq!(refs.len(), 6);
        assert_eq!(mp4.sample_data(&refs[0]), Some(b"A1".as_slice()));
        assert_eq!(mp4.sample_data(&refs[1]), Some(b"A2A2".as_slice()));
        assert_eq!(mp4.sample_data(&refs[2]), Some(b"B1B1B1".as_slice()));
        assert_eq!(mp4.sample_data(&refs[3]), Some(b"C1".as_slice()));
        assert_eq!(mp4.sample_data(&refs[4]), Some(b"C2C".as_slice()));
        assert_eq!(mp4.sample_data(&refs[5]), Some(b"C3C3".as_slice()));
    }

    #[test]
    fn co64_large_offsets() {
        let samples = [SampleSpec {
            data: b"frame",
            duration: 1000,
        }];
        let data = minimal_mp4_ex(320, 240, 24000, &samples, &[], true, &[]);
        let mp4 = parse(&data).unwrap();

        assert!(mp4.co64);

        let refs: Vec<SampleRef> = mp4.samples().collect();

        assert_eq!(refs.len(), 1);
        assert_eq!(mp4.sample_data(&refs[0]), Some(b"frame".as_slice()));
    }

    fn expect_err(data: &[u8], expected: Error) {
        match parse(data) {
            Err(e) => assert_eq!(e, expected),
            Ok(_) => panic!("expected {expected:?}, got Ok"),
        }
    }

    #[test]
    fn reject_too_short() {
        expect_err(b"", Error::TooShort);
        expect_err(b"short", Error::TooShort);
    }

    #[test]
    fn reject_not_mp4() {
        let mut buf = Vec::new();
        let off = begin_box(&mut buf, b"free");

        buf.extend_from_slice(&[0u8; 4]);

        end_box(&mut buf, off);
        expect_err(&buf, Error::NotMp4);
    }

    #[test]
    fn no_video_track() {
        let mut buf = Vec::new();
        let ftyp = begin_box(&mut buf, b"ftyp");

        write_fourcc(&mut buf, b"isom");
        write_u32_be(&mut buf, 0x200);
        end_box(&mut buf, ftyp);

        let moov = begin_box(&mut buf, b"moov");
        let mvhd = begin_fullbox(&mut buf, b"mvhd", 0);

        buf.extend_from_slice(&[0u8; 96]);

        end_box(&mut buf, mvhd);
        end_box(&mut buf, moov);

        expect_err(&buf, Error::NoVideoTrack);
    }

    #[test]
    fn single_sample_per_chunk() {
        let data = minimal_mp4(
            320,
            240,
            24000,
            &[SampleSpec {
                data: b"only_one",
                duration: 1000,
            }],
        );
        let mp4 = parse(&data).unwrap();

        assert_eq!(mp4.total_samples, 1);

        let refs: Vec<SampleRef> = mp4.samples().collect();

        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].dts_ticks, 0);
        assert_eq!(refs[0].pts_ticks, 0);
        assert!(refs[0].is_keyframe);
        assert_eq!(mp4.sample_data(&refs[0]), Some(b"only_one".as_slice()));
    }

    #[test]
    fn composition_time_offsets() {
        let samples = [
            SampleSpec {
                data: b"I",
                duration: 1000,
            },
            SampleSpec {
                data: b"B",
                duration: 1000,
            },
            SampleSpec {
                data: b"P",
                duration: 1000,
            },
        ];
        // ctts: sample 0 offset=2000, sample 1 offset=0, sample 2 offset=1000
        let data = minimal_mp4_ex(320, 240, 24000, &samples, &[], false, &[2000, 0, 1000]);
        let mp4 = parse(&data).unwrap();
        let refs: Vec<SampleRef> = mp4.samples().collect();

        assert_eq!(refs.len(), 3);
        // DTS: 0, 1000, 2000
        assert_eq!(refs[0].dts_ticks, 0);
        assert_eq!(refs[1].dts_ticks, 1000);
        assert_eq!(refs[2].dts_ticks, 2000);
        // PTS = DTS + ctts_offset
        assert_eq!(refs[0].pts_ticks, 2000);
        assert_eq!(refs[1].pts_ticks, 1000);
        assert_eq!(refs[2].pts_ticks, 3000);
    }

    #[test]
    fn parse_real_h264_mp4() {
        let data = include_bytes!("/tmp/test-h264.mp4");
        let mp4 = parse(data).expect("parse failed");

        assert!(mp4.width > 0);
        assert!(mp4.height > 0);
        assert_eq!(mp4.total_samples, 3);
        assert!(mp4.timescale > 0);

        let (nal_len, config) = mp4.avc_config().expect("should have avcC");

        assert_eq!(nal_len, 4);
        assert!(!config.is_empty());

        let refs: Vec<SampleRef> = mp4.samples().collect();

        assert_eq!(refs.len(), 3);

        for s in &refs {
            assert!(s.size > 0);

            let slice = mp4.sample_data(s).expect("sample data should resolve");

            assert_eq!(slice.len(), s.size as usize);
        }

        assert!(refs[0].is_keyframe);
    }

    #[test]
    fn parse_zoey_1080p_mp4() {
        let data = include_bytes!("/Users/user/Sites/os/assets/zoey.mp4");
        let mp4 = parse(data).expect("parse zoey.mp4");

        assert_eq!(mp4.width, 1920);
        assert_eq!(mp4.height, 1080);
        assert_eq!(mp4.total_samples, 534);
        assert!(mp4.timescale > 0);

        let (nal_len, config) = mp4.avc_config().expect("avcC");

        assert_eq!(nal_len, 4);
        assert!(!config.is_empty());

        let refs: Vec<SampleRef> = mp4.samples().collect();

        assert_eq!(refs.len(), 534);
        assert!(refs[0].is_keyframe);
        assert!(refs[0].size > 0);

        let first = mp4.sample_data(&refs[0]).expect("first sample data");

        assert_eq!(first.len(), refs[0].size as usize);
    }

    #[test]
    fn parse_zoey_audio_track() {
        let data = include_bytes!("/Users/user/Sites/os/assets/zoey.mp4");
        let mp4 = parse(data).expect("parse zoey.mp4");

        assert!(mp4.has_audio());
        assert_eq!(mp4.audio_sample_rate(), 44100);
        assert_eq!(mp4.audio_channels(), 2);
        assert_eq!(mp4.audio_timescale(), 44100);
        assert!(mp4.audio_duration_ns() > 0);

        let config = mp4.audio_config().expect("AAC config");

        assert!(config.len() >= 2);

        let audio_refs: Vec<SampleRef> = mp4.audio_samples().collect();

        assert!(!audio_refs.is_empty());
        assert!(audio_refs[0].size > 0);

        let first = mp4.sample_data(&audio_refs[0]).expect("first audio sample");

        assert_eq!(first.len(), audio_refs[0].size as usize);

        // All audio samples should be keyframes (no stss = all keyframes)
        for s in &audio_refs {
            assert!(s.is_keyframe);
        }
    }

    #[test]
    fn video_only_mp4_has_no_audio() {
        let data = minimal_mp4(
            320,
            240,
            24000,
            &[SampleSpec {
                data: b"frame",
                duration: 1000,
            }],
        );
        let mp4 = parse(&data).unwrap();

        assert!(!mp4.has_audio());
        assert_eq!(mp4.audio_sample_rate(), 0);
        assert_eq!(mp4.audio_channels(), 0);
        assert!(mp4.audio_config().is_none());

        let audio_refs: Vec<SampleRef> = mp4.audio_samples().collect();

        assert!(audio_refs.is_empty());
    }
}

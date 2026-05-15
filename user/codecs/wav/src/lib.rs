//! WAV decoder — no external dependencies, no_std, no alloc.
//!
//! Parses RIFF/WAVE headers and extracts PCM audio data. Supports
//! format codes 1 (integer PCM) and 3 (IEEE float).

#![no_std]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    PcmS16,
    PcmS24,
    PcmS32,
    Float32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavInfo {
    pub format: Format,
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    pub data_offset: usize,
    pub data_len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    TooShort,
    BadRiffMagic,
    BadWaveMagic,
    BadFmtChunk,
    UnsupportedFormat,
    NoDataChunk,
}

const RIFF_MAGIC: u32 = 0x4646_4952;
const WAVE_MAGIC: u32 = 0x4556_4157;
const FMT_ID: u32 = 0x2074_6D66;
const DATA_ID: u32 = 0x6174_6164;

const FORMAT_PCM: u16 = 1;
const FORMAT_FLOAT: u16 = 3;

fn u16_le(data: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([data[off], data[off + 1]])
}

fn u32_le(data: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
}

pub fn parse(data: &[u8]) -> Result<WavInfo, Error> {
    if data.len() < 44 {
        return Err(Error::TooShort);
    }

    if u32_le(data, 0) != RIFF_MAGIC {
        return Err(Error::BadRiffMagic);
    }

    if u32_le(data, 8) != WAVE_MAGIC {
        return Err(Error::BadWaveMagic);
    }

    let mut pos = 12;
    let mut fmt_found = false;
    let mut format_code: u16 = 0;
    let mut channels: u16 = 0;
    let mut sample_rate: u32 = 0;
    let mut bits_per_sample: u16 = 0;

    while pos + 8 <= data.len() {
        let chunk_id = u32_le(data, pos);
        let chunk_size = u32_le(data, pos + 4) as usize;

        if chunk_id == FMT_ID {
            if chunk_size < 16 || pos + 8 + 16 > data.len() {
                return Err(Error::BadFmtChunk);
            }

            let fmt_start = pos + 8;

            format_code = u16_le(data, fmt_start);
            channels = u16_le(data, fmt_start + 2);
            sample_rate = u32_le(data, fmt_start + 4);
            bits_per_sample = u16_le(data, fmt_start + 14);
            fmt_found = true;
        }

        if chunk_id == DATA_ID {
            if !fmt_found {
                return Err(Error::BadFmtChunk);
            }

            let data_offset = pos + 8;
            let data_len = chunk_size.min(data.len() - data_offset);
            let format = match (format_code, bits_per_sample) {
                (FORMAT_PCM, 16) => Format::PcmS16,
                (FORMAT_PCM, 24) => Format::PcmS24,
                (FORMAT_PCM, 32) => Format::PcmS32,
                (FORMAT_FLOAT, 32) => Format::Float32,
                _ => return Err(Error::UnsupportedFormat),
            };

            return Ok(WavInfo {
                format,
                channels,
                sample_rate,
                bits_per_sample,
                data_offset,
                data_len,
            });
        }

        pos += 8 + ((chunk_size + 1) & !1);
    }

    Err(Error::NoDataChunk)
}

pub fn frame_count(info: &WavInfo) -> usize {
    info.data_len / (info.channels as usize * (info.bits_per_sample as usize / 8))
}

pub fn to_f32_stereo_48k(data: &[u8], info: &WavInfo, out: &mut [f32]) -> usize {
    let pcm = &data[info.data_offset..info.data_offset + info.data_len];

    match info.format {
        Format::PcmS16 => convert_s16_to_f32_stereo(pcm, info, out),
        Format::PcmS32 => convert_s32_to_f32_stereo(pcm, info, out),
        Format::Float32 => convert_f32_to_f32_stereo(pcm, info, out),
        Format::PcmS24 => convert_s24_to_f32_stereo(pcm, info, out),
    }
}

fn convert_s16_to_f32_stereo(pcm: &[u8], info: &WavInfo, out: &mut [f32]) -> usize {
    let frame_bytes = info.channels as usize * 2;
    let num_frames = pcm.len() / frame_bytes;
    let out_frames = out.len() / 2;
    let frames = num_frames.min(out_frames);

    for i in 0..frames {
        let src = i * frame_bytes;
        let left = i16::from_le_bytes([pcm[src], pcm[src + 1]]) as f32 / 32768.0;
        let right = if info.channels >= 2 {
            i16::from_le_bytes([pcm[src + 2], pcm[src + 3]]) as f32 / 32768.0
        } else {
            left
        };

        out[i * 2] = left;
        out[i * 2 + 1] = right;
    }

    frames
}

fn convert_s24_to_f32_stereo(pcm: &[u8], info: &WavInfo, out: &mut [f32]) -> usize {
    let frame_bytes = info.channels as usize * 3;
    let num_frames = pcm.len() / frame_bytes;
    let out_frames = out.len() / 2;
    let frames = num_frames.min(out_frames);

    for i in 0..frames {
        let src = i * frame_bytes;
        let left = s24_to_f32(pcm[src], pcm[src + 1], pcm[src + 2]);
        let right = if info.channels >= 2 {
            s24_to_f32(pcm[src + 3], pcm[src + 4], pcm[src + 5])
        } else {
            left
        };

        out[i * 2] = left;
        out[i * 2 + 1] = right;
    }

    frames
}

fn s24_to_f32(b0: u8, b1: u8, b2: u8) -> f32 {
    let raw = (b0 as i32) | ((b1 as i32) << 8) | ((b2 as i32) << 16);
    let signed = if raw & 0x80_0000 != 0 {
        raw | !0xFF_FFFF
    } else {
        raw
    };

    signed as f32 / 8_388_608.0
}

fn convert_s32_to_f32_stereo(pcm: &[u8], info: &WavInfo, out: &mut [f32]) -> usize {
    let frame_bytes = info.channels as usize * 4;
    let num_frames = pcm.len() / frame_bytes;
    let out_frames = out.len() / 2;
    let frames = num_frames.min(out_frames);

    for i in 0..frames {
        let src = i * frame_bytes;
        let left = i32::from_le_bytes([pcm[src], pcm[src + 1], pcm[src + 2], pcm[src + 3]]) as f32
            / 2_147_483_648.0;
        let right = if info.channels >= 2 {
            i32::from_le_bytes([pcm[src + 4], pcm[src + 5], pcm[src + 6], pcm[src + 7]]) as f32
                / 2_147_483_648.0
        } else {
            left
        };

        out[i * 2] = left;
        out[i * 2 + 1] = right;
    }

    frames
}

fn convert_f32_to_f32_stereo(pcm: &[u8], info: &WavInfo, out: &mut [f32]) -> usize {
    let frame_bytes = info.channels as usize * 4;
    let num_frames = pcm.len() / frame_bytes;
    let out_frames = out.len() / 2;
    let frames = num_frames.min(out_frames);

    for i in 0..frames {
        let src = i * frame_bytes;
        let left = f32::from_le_bytes([pcm[src], pcm[src + 1], pcm[src + 2], pcm[src + 3]]);
        let right = if info.channels >= 2 {
            f32::from_le_bytes([pcm[src + 4], pcm[src + 5], pcm[src + 6], pcm[src + 7]])
        } else {
            left
        };

        out[i * 2] = left;
        out[i * 2 + 1] = right;
    }

    frames
}

pub fn generate_sine_wav(_freq_hz: f32, duration_secs: f32, sample_rate: u32) -> ([u8; 44], usize) {
    let num_samples = (sample_rate as f32 * duration_secs) as usize;
    let data_len = num_samples * 2 * 2;
    let file_len = 36 + data_len;
    let mut header = [0u8; 44];

    header[0..4].copy_from_slice(b"RIFF");
    header[4..8].copy_from_slice(&(file_len as u32).to_le_bytes());
    header[8..12].copy_from_slice(b"WAVE");
    header[12..16].copy_from_slice(b"fmt ");
    header[16..20].copy_from_slice(&16u32.to_le_bytes());
    header[20..22].copy_from_slice(&1u16.to_le_bytes());
    header[22..24].copy_from_slice(&2u16.to_le_bytes());
    header[24..28].copy_from_slice(&sample_rate.to_le_bytes());
    header[28..32].copy_from_slice(&(sample_rate * 4).to_le_bytes());
    header[32..34].copy_from_slice(&4u16.to_le_bytes());
    header[34..36].copy_from_slice(&16u16.to_le_bytes());
    header[36..40].copy_from_slice(b"data");
    header[40..44].copy_from_slice(&(data_len as u32).to_le_bytes());

    (header, num_samples)
}

fn sin_approx(x: f32) -> f32 {
    const TAU: f32 = core::f32::consts::TAU;
    let x = x - TAU * floor_f32(x / TAU + 0.5);
    let x2 = x * x;

    x * (1.0 - x2 / 6.0 * (1.0 - x2 / 20.0 * (1.0 - x2 / 42.0)))
}

fn floor_f32(x: f32) -> f32 {
    let i = x as i32;
    let f = i as f32;

    if x < f { f - 1.0 } else { f }
}

pub fn generate_sine_samples(
    freq_hz: f32,
    sample_rate: u32,
    sample_index: usize,
    out: &mut [i16],
) -> usize {
    let frames = out.len() / 2;

    for i in 0..frames {
        let t = (sample_index + i) as f32 / sample_rate as f32;
        let val = sin_approx(t * freq_hz * 2.0 * core::f32::consts::PI);
        let sample = (val * 24000.0) as i16;

        out[i * 2] = sample;
        out[i * 2 + 1] = sample;
    }

    frames
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_wav_s16(sample_rate: u32, channels: u16, samples: &[i16]) -> alloc::vec::Vec<u8> {
        let data_len = samples.len() * 2;
        let file_len = 36 + data_len;
        let mut buf = alloc::vec![0u8; 44 + data_len];

        buf[0..4].copy_from_slice(b"RIFF");
        buf[4..8].copy_from_slice(&(file_len as u32).to_le_bytes());
        buf[8..12].copy_from_slice(b"WAVE");
        buf[12..16].copy_from_slice(b"fmt ");
        buf[16..20].copy_from_slice(&16u32.to_le_bytes());
        buf[20..22].copy_from_slice(&1u16.to_le_bytes());
        buf[22..24].copy_from_slice(&channels.to_le_bytes());
        buf[24..28].copy_from_slice(&sample_rate.to_le_bytes());
        buf[28..32].copy_from_slice(&(sample_rate * channels as u32 * 2).to_le_bytes());
        buf[32..34].copy_from_slice(&(channels * 2).to_le_bytes());
        buf[34..36].copy_from_slice(&16u16.to_le_bytes());
        buf[36..40].copy_from_slice(b"data");
        buf[40..44].copy_from_slice(&(data_len as u32).to_le_bytes());

        for (i, s) in samples.iter().enumerate() {
            buf[44 + i * 2..44 + i * 2 + 2].copy_from_slice(&s.to_le_bytes());
        }

        buf
    }

    extern crate alloc;

    #[test]
    fn parse_valid_s16_mono() {
        let wav = make_wav_s16(44100, 1, &[0, 1000, -1000, 0]);
        let info = parse(&wav).unwrap();

        assert_eq!(info.format, Format::PcmS16);
        assert_eq!(info.channels, 1);
        assert_eq!(info.sample_rate, 44100);
        assert_eq!(info.bits_per_sample, 16);
        assert_eq!(info.data_offset, 44);
        assert_eq!(info.data_len, 8);
    }

    #[test]
    fn parse_valid_s16_stereo() {
        let wav = make_wav_s16(48000, 2, &[100, -100, 200, -200]);
        let info = parse(&wav).unwrap();

        assert_eq!(info.format, Format::PcmS16);
        assert_eq!(info.channels, 2);
        assert_eq!(info.sample_rate, 48000);
        assert_eq!(info.data_len, 8);
    }

    #[test]
    fn parse_too_short() {
        assert_eq!(parse(&[0; 10]), Err(Error::TooShort));
    }

    #[test]
    fn parse_bad_riff_magic() {
        let mut wav = make_wav_s16(44100, 1, &[0]);

        wav[0] = b'X';

        assert_eq!(parse(&wav), Err(Error::BadRiffMagic));
    }

    #[test]
    fn parse_bad_wave_magic() {
        let mut wav = make_wav_s16(44100, 1, &[0]);

        wav[8] = b'X';

        assert_eq!(parse(&wav), Err(Error::BadWaveMagic));
    }

    #[test]
    fn convert_s16_mono_to_f32_stereo() {
        let wav = make_wav_s16(48000, 1, &[16384, -16384]);
        let info = parse(&wav).unwrap();
        let mut out = [0.0f32; 4];
        let frames = to_f32_stereo_48k(&wav, &info, &mut out);

        assert_eq!(frames, 2);
        assert!((out[0] - 0.5).abs() < 0.001);
        assert!((out[1] - 0.5).abs() < 0.001);
        assert!((out[2] - (-0.5)).abs() < 0.001);
        assert!((out[3] - (-0.5)).abs() < 0.001);
    }

    #[test]
    fn convert_s16_stereo_to_f32_stereo() {
        let wav = make_wav_s16(48000, 2, &[32767, -32768]);
        let info = parse(&wav).unwrap();
        let mut out = [0.0f32; 2];
        let frames = to_f32_stereo_48k(&wav, &info, &mut out);

        assert_eq!(frames, 1);
        assert!(out[0] > 0.99);
        assert!(out[1] < -0.99);
    }

    #[test]
    fn sample_count() {
        let wav = make_wav_s16(48000, 2, &[0, 0, 0, 0, 0, 0, 0, 0]);
        let info = parse(&wav).unwrap();

        assert_eq!(frame_count(&info), 4);
    }

    #[test]
    fn generate_sine_header() {
        let (header, num_samples) = generate_sine_wav(440.0, 0.1, 48000);

        assert_eq!(&header[0..4], b"RIFF");
        assert_eq!(&header[8..12], b"WAVE");
        assert_eq!(&header[36..40], b"data");
        assert_eq!(num_samples, 4800);

        let info = parse(&header).unwrap();

        assert_eq!(info.format, Format::PcmS16);
        assert_eq!(info.channels, 2);
        assert_eq!(info.sample_rate, 48000);
    }

    #[test]
    fn generate_sine_samples_nonzero() {
        let mut out = [0i16; 200];

        generate_sine_samples(440.0, 48000, 0, &mut out);

        let has_nonzero = out.iter().any(|&s| s != 0);

        assert!(has_nonzero);
    }

    #[test]
    fn generate_sine_samples_range() {
        let mut out = [0i16; 2000];

        generate_sine_samples(440.0, 48000, 0, &mut out);

        for &s in &out {
            assert!(s.abs() <= 24001);
        }
    }
}

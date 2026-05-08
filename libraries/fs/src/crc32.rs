//! CRC32 (IEEE 802.3) checksum.
//!
//! Compile-time 256-entry lookup table, reflected polynomial 0xEDB88320.
//! Same algorithm as the PNG decoder in `services/decoders/png/`.

const TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0u32;

    while i < 256 {
        let mut crc = i;
        let mut j = 0;

        while j < 8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }

            j += 1;
        }

        table[i as usize] = crc;
        i += 1;
    }

    table
};

/// Compute CRC32 over `data`.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;

    for &byte in data {
        crc = (crc >> 8) ^ TABLE[((crc ^ byte as u32) & 0xFF) as usize];
    }

    crc ^ 0xFFFF_FFFF
}

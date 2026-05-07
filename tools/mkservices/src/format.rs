//! Service pack binary format — header, entry table, page-aligned binaries.
//!
//! ```text
//! [PackHeader: 16 bytes]
//! [PackEntry × count: 48 bytes each]
//! [padding to PAGE_SIZE boundary]
//! [service 0 binary, PAGE_SIZE-aligned]
//! [padding to PAGE_SIZE boundary]
//! [service 1 binary, PAGE_SIZE-aligned]
//! ...
//! ```
//!
//! Init maps the pack as a single VMO, reads the header and entry
//! table, then copies each service binary into its own code VMO.

// Reader functions are used by tests and will be used by init's parser.
#![allow(dead_code)]

pub const MAGIC: [u8; 4] = *b"SVPK";
pub const VERSION: u32 = 1;
pub const PAGE_SIZE: usize = 16384;
pub const MAX_NAME_LEN: usize = 32;

pub const HEADER_SIZE: usize = 16;
pub const ENTRY_SIZE: usize = 48;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackHeader {
    pub magic: [u8; 4],
    pub version: u32,
    pub count: u32,
    pub total_size: u32,
}

impl PackHeader {
    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.magic);
        buf[4..8].copy_from_slice(&self.version.to_le_bytes());
        buf[8..12].copy_from_slice(&self.count.to_le_bytes());
        buf[12..16].copy_from_slice(&self.total_size.to_le_bytes());
    }

    pub fn read_from(buf: &[u8]) -> Self {
        let mut magic = [0u8; 4];

        magic.copy_from_slice(&buf[0..4]);

        Self {
            magic,
            version: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            count: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            total_size: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
        }
    }

    pub fn is_valid(&self) -> bool {
        self.magic == MAGIC && self.version == VERSION
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackEntry {
    pub name: [u8; 32],
    pub offset: u32,
    pub size: u32,
    pub entry_point: u32,
    pub flags: u32,
}

impl PackEntry {
    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..32].copy_from_slice(&self.name);
        buf[32..36].copy_from_slice(&self.offset.to_le_bytes());
        buf[36..40].copy_from_slice(&self.size.to_le_bytes());
        buf[40..44].copy_from_slice(&self.entry_point.to_le_bytes());
        buf[44..48].copy_from_slice(&self.flags.to_le_bytes());
    }

    pub fn read_from(buf: &[u8]) -> Self {
        let mut name = [0u8; 32];

        name.copy_from_slice(&buf[0..32]);

        Self {
            name,
            offset: u32::from_le_bytes(buf[32..36].try_into().unwrap()),
            size: u32::from_le_bytes(buf[36..40].try_into().unwrap()),
            entry_point: u32::from_le_bytes(buf[40..44].try_into().unwrap()),
            flags: u32::from_le_bytes(buf[44..48].try_into().unwrap()),
        }
    }

    pub fn name_str(&self) -> &str {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(32);

        std::str::from_utf8(&self.name[..end]).unwrap_or("<invalid>")
    }
}

fn make_name(s: &str) -> [u8; 32] {
    let mut buf = [0u8; 32];
    let len = s.len().min(MAX_NAME_LEN);

    buf[..len].copy_from_slice(&s.as_bytes()[..len]);

    buf
}

fn align_up(n: usize, align: usize) -> usize {
    (n + align - 1) & !(align - 1)
}

pub struct PackBuilder {
    services: Vec<(String, Vec<u8>)>,
}

impl PackBuilder {
    pub fn new() -> Self {
        Self {
            services: Vec::new(),
        }
    }

    pub fn add_service(&mut self, name: &str, binary: Vec<u8>) {
        self.services.push((name.to_string(), binary));
    }

    pub fn service_count(&self) -> usize {
        self.services.len()
    }

    pub fn build(&self) -> Vec<u8> {
        let entry_table_size = self.services.len() * ENTRY_SIZE;
        let first_binary_offset = align_up(HEADER_SIZE + entry_table_size, PAGE_SIZE);
        let mut offsets = Vec::with_capacity(self.services.len());
        let mut current = first_binary_offset;

        for (_, binary) in &self.services {
            offsets.push(current);
            current = align_up(current + binary.len(), PAGE_SIZE);
        }

        let total_size = current;
        let mut pack = vec![0u8; total_size];
        let header = PackHeader {
            magic: MAGIC,
            version: VERSION,
            count: self.services.len() as u32,
            total_size: total_size as u32,
        };

        header.write_to(&mut pack[..HEADER_SIZE]);

        for (i, (name, binary)) in self.services.iter().enumerate() {
            let entry = PackEntry {
                name: make_name(name),
                offset: offsets[i] as u32,
                size: binary.len() as u32,
                entry_point: 0,
                flags: 0,
            };
            let entry_offset = HEADER_SIZE + i * ENTRY_SIZE;

            entry.write_to(&mut pack[entry_offset..entry_offset + ENTRY_SIZE]);

            pack[offsets[i]..offsets[i] + binary.len()].copy_from_slice(binary);
        }

        pack
    }
}

pub fn read_header(pack: &[u8]) -> Option<PackHeader> {
    if pack.len() < HEADER_SIZE {
        return None;
    }

    let header = PackHeader::read_from(pack);

    if !header.is_valid() {
        return None;
    }

    Some(header)
}

pub fn read_entry(pack: &[u8], index: usize) -> Option<PackEntry> {
    let offset = HEADER_SIZE + index * ENTRY_SIZE;

    if offset + ENTRY_SIZE > pack.len() {
        return None;
    }

    Some(PackEntry::read_from(&pack[offset..]))
}

pub fn service_binary<'a>(pack: &'a [u8], entry: &PackEntry) -> Option<&'a [u8]> {
    let start = entry.offset as usize;
    let end = start + entry.size as usize;

    if end > pack.len() {
        return None;
    }

    Some(&pack[start..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip() {
        let header = PackHeader {
            magic: MAGIC,
            version: VERSION,
            count: 3,
            total_size: 65536,
        };
        let mut buf = [0u8; HEADER_SIZE];

        header.write_to(&mut buf);

        let decoded = PackHeader::read_from(&buf);

        assert_eq!(header, decoded);
    }

    #[test]
    fn header_is_valid() {
        let header = PackHeader {
            magic: MAGIC,
            version: VERSION,
            count: 0,
            total_size: 0,
        };

        assert!(header.is_valid());
    }

    #[test]
    fn header_bad_magic() {
        let header = PackHeader {
            magic: *b"XXXX",
            version: VERSION,
            count: 0,
            total_size: 0,
        };

        assert!(!header.is_valid());
    }

    #[test]
    fn header_bad_version() {
        let header = PackHeader {
            magic: MAGIC,
            version: 99,
            count: 0,
            total_size: 0,
        };

        assert!(!header.is_valid());
    }

    #[test]
    fn entry_round_trip() {
        let entry = PackEntry {
            name: make_name("console"),
            offset: PAGE_SIZE as u32,
            size: 4096,
            entry_point: 0,
            flags: 0,
        };
        let mut buf = [0u8; ENTRY_SIZE];

        entry.write_to(&mut buf);

        let decoded = PackEntry::read_from(&buf);

        assert_eq!(entry, decoded);
    }

    #[test]
    fn entry_name_str() {
        let entry = PackEntry {
            name: make_name("document"),
            offset: 0,
            size: 0,
            entry_point: 0,
            flags: 0,
        };

        assert_eq!(entry.name_str(), "document");
    }

    #[test]
    fn single_service_pack() {
        let mut builder = PackBuilder::new();
        let binary = vec![0xDE, 0xAD, 0xBE, 0xEF];

        builder.add_service("test", binary.clone());

        let pack = builder.build();
        let header = read_header(&pack).unwrap();

        assert_eq!(header.count, 1);
        assert_eq!(header.total_size as usize, pack.len());

        let entry = read_entry(&pack, 0).unwrap();

        assert_eq!(entry.name_str(), "test");
        assert_eq!(entry.size, 4);
        assert_eq!(entry.offset as usize % PAGE_SIZE, 0);

        let data = service_binary(&pack, &entry).unwrap();

        assert_eq!(data, &binary);
    }

    #[test]
    fn multiple_services_pack() {
        let mut builder = PackBuilder::new();

        builder.add_service("alpha", vec![1; 100]);
        builder.add_service("beta", vec![2; 200]);
        builder.add_service("gamma", vec![3; 300]);

        let pack = builder.build();
        let header = read_header(&pack).unwrap();

        assert_eq!(header.count, 3);

        for i in 0..3 {
            let entry = read_entry(&pack, i).unwrap();

            assert_eq!(entry.offset as usize % PAGE_SIZE, 0);

            let data = service_binary(&pack, &entry).unwrap();
            let expected_byte = (i + 1) as u8;
            let expected_len = (i + 1) * 100;

            assert_eq!(data.len(), expected_len);
            assert!(data.iter().all(|&b| b == expected_byte));
        }
    }

    #[test]
    fn binaries_page_aligned() {
        let mut builder = PackBuilder::new();

        builder.add_service("a", vec![0xFF; 1]);
        builder.add_service("b", vec![0xAA; PAGE_SIZE + 1]);

        let pack = builder.build();

        let entry_a = read_entry(&pack, 0).unwrap();
        let entry_b = read_entry(&pack, 1).unwrap();

        assert_eq!(entry_a.offset as usize % PAGE_SIZE, 0);
        assert_eq!(entry_b.offset as usize % PAGE_SIZE, 0);

        assert!(entry_b.offset > entry_a.offset);

        let gap = entry_b.offset as usize - (entry_a.offset as usize + entry_a.size as usize);

        assert!(gap > 0);
    }

    #[test]
    fn total_size_page_aligned() {
        let mut builder = PackBuilder::new();

        builder.add_service("x", vec![0; 1]);

        let pack = builder.build();

        assert_eq!(pack.len() % PAGE_SIZE, 0);
    }

    #[test]
    fn empty_pack() {
        let builder = PackBuilder::new();
        let pack = builder.build();
        let header = read_header(&pack).unwrap();

        assert_eq!(header.count, 0);
        assert_eq!(pack.len() % PAGE_SIZE, 0);
    }

    #[test]
    fn large_binary() {
        let mut builder = PackBuilder::new();
        let big = vec![0x42; PAGE_SIZE * 3 + 7];

        builder.add_service("big", big.clone());

        let pack = builder.build();
        let entry = read_entry(&pack, 0).unwrap();
        let data = service_binary(&pack, &entry).unwrap();

        assert_eq!(data, &big);
    }

    #[test]
    fn read_header_too_short() {
        assert!(read_header(&[0; 4]).is_none());
    }

    #[test]
    fn read_header_bad_magic() {
        let mut buf = [0u8; HEADER_SIZE];

        buf[0..4].copy_from_slice(b"NOPE");

        assert!(read_header(&buf).is_none());
    }

    #[test]
    fn read_entry_out_of_bounds() {
        let pack = [0u8; HEADER_SIZE];

        assert!(read_entry(&pack, 0).is_none());
    }

    #[test]
    fn padding_is_zeroed() {
        let mut builder = PackBuilder::new();

        builder.add_service("x", vec![0xFF; 10]);

        let pack = builder.build();
        let entry = read_entry(&pack, 0).unwrap();
        let data_end = entry.offset as usize + entry.size as usize;

        if data_end < pack.len() {
            let next_page = align_up(data_end, PAGE_SIZE);
            let padding = &pack[data_end..next_page.min(pack.len())];

            assert!(padding.iter().all(|&b| b == 0));
        }
    }

    #[test]
    fn name_truncation() {
        let long_name = "a".repeat(64);
        let name_buf = make_name(&long_name);
        let end = name_buf.iter().position(|&b| b == 0).unwrap_or(32);

        assert_eq!(end, 32);
    }

    #[test]
    fn format_constants() {
        assert_eq!(HEADER_SIZE, 16);
        assert_eq!(ENTRY_SIZE, 48);
        assert_eq!(MAX_NAME_LEN, 32);
        assert_eq!(PAGE_SIZE, 16384);
    }
}

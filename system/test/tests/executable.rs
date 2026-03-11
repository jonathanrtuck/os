//! Host-side tests for the kernel ELF64 parser.
//!
//! The executable.rs module depends on `super::address_space::PageAttrs` for
//! `segment_attrs()`. We provide a minimal stub so the source compiles
//! on the host.

// Stub for executable.rs's `use super::address_space::PageAttrs`.
mod address_space {
    #[derive(Debug)]
    pub struct PageAttrs(pub u64);

    impl PageAttrs {
        pub fn user_ro() -> Self {
            Self(0)
        }
        pub fn user_rw() -> Self {
            Self(1)
        }
        pub fn user_rx() -> Self {
            Self(2)
        }
    }
}
#[path = "../../kernel/executable.rs"]
mod executable;

/// Build a minimal valid ELF64 aarch64 executable header (64 bytes).
fn minimal_elf_header(entry: u64, ph_offset: u64, ph_count: u16) -> Vec<u8> {
    let mut buf = vec![0u8; 64];

    // e_ident
    buf[0..4].copy_from_slice(&[0x7F, b'E', b'L', b'F']);
    buf[4] = 2; // ELFCLASS64
    buf[5] = 1; // ELFDATA2LSB
    buf[6] = 1; // EV_CURRENT
                // e_type = ET_EXEC (2)
    buf[16..18].copy_from_slice(&2u16.to_le_bytes());
    // e_machine = EM_AARCH64 (183)
    buf[18..20].copy_from_slice(&183u16.to_le_bytes());
    // e_version
    buf[20..24].copy_from_slice(&1u32.to_le_bytes());
    // e_entry
    buf[24..32].copy_from_slice(&entry.to_le_bytes());
    // e_phoff
    buf[32..40].copy_from_slice(&ph_offset.to_le_bytes());
    // e_phentsize = 56
    buf[54..56].copy_from_slice(&56u16.to_le_bytes());
    // e_phnum
    buf[56..58].copy_from_slice(&ph_count.to_le_bytes());

    buf
}
/// Build a PT_LOAD program header (56 bytes).
fn pt_load_phdr(vaddr: u64, offset: u64, filesz: u64, memsz: u64, flags: u32) -> Vec<u8> {
    let mut buf = vec![0u8; 56];

    // p_type = PT_LOAD (1)
    buf[0..4].copy_from_slice(&1u32.to_le_bytes());
    // p_flags
    buf[4..8].copy_from_slice(&flags.to_le_bytes());
    // p_offset
    buf[8..16].copy_from_slice(&offset.to_le_bytes());
    // p_vaddr
    buf[16..24].copy_from_slice(&vaddr.to_le_bytes());
    // p_paddr (unused)
    // p_filesz
    buf[32..40].copy_from_slice(&filesz.to_le_bytes());
    // p_memsz
    buf[40..48].copy_from_slice(&memsz.to_le_bytes());

    buf
}

#[test]
fn load_segment_out_of_bounds() {
    let data = minimal_elf_header(0, 64, 1);
    // No program header data appended — offset 64..120 is out of bounds.
    let h = executable::parse_header(&data).unwrap();

    assert!(matches!(
        executable::load_segment(&data, &h, 0),
        Err(executable::Error::SegmentOutOfBounds)
    ));
}
#[test]
fn load_segment_pt_load() {
    let mut data = minimal_elf_header(0x400000, 64, 1);

    data.extend(pt_load_phdr(0x400000, 120, 32, 64, 5));
    data.extend(vec![0u8; 64]); // segment data padding

    let h = executable::parse_header(&data).unwrap();
    let seg = executable::load_segment(&data, &h, 0).unwrap().unwrap();

    assert_eq!(seg.vaddr, 0x400000);
    assert_eq!(seg.file_offset, 120);
    assert_eq!(seg.file_size, 32);
    assert_eq!(seg.mem_size, 64);
    assert_eq!(seg.flags, 5); // PF_R | PF_X
}
#[test]
fn load_segment_skips_non_load() {
    let mut data = minimal_elf_header(0, 64, 1);
    let mut phdr = vec![0u8; 56];

    phdr[0..4].copy_from_slice(&6u32.to_le_bytes()); // PT_PHDR
    data.extend(phdr);

    let h = executable::parse_header(&data).unwrap();

    assert!(executable::load_segment(&data, &h, 0).unwrap().is_none());
}
#[test]
fn parse_bad_magic() {
    let mut data = minimal_elf_header(0, 0, 0);

    data[0] = 0;

    assert!(matches!(
        executable::parse_header(&data),
        Err(executable::Error::BadMagic)
    ));
}
#[test]
fn parse_bad_phentsize() {
    let mut data = minimal_elf_header(0, 0, 0);

    data[54..56].copy_from_slice(&32u16.to_le_bytes()); // too small

    assert!(matches!(
        executable::parse_header(&data),
        Err(executable::Error::BadPhEntSize)
    ));
}
#[test]
fn parse_not_aarch64() {
    let mut data = minimal_elf_header(0, 0, 0);

    data[18..20].copy_from_slice(&62u16.to_le_bytes()); // EM_X86_64

    assert!(matches!(
        executable::parse_header(&data),
        Err(executable::Error::NotAarch64)
    ));
}
#[test]
fn parse_not_elf64() {
    let mut data = minimal_elf_header(0, 0, 0);

    data[4] = 1; // ELFCLASS32

    assert!(matches!(
        executable::parse_header(&data),
        Err(executable::Error::NotElf64)
    ));
}
#[test]
fn parse_not_executable() {
    let mut data = minimal_elf_header(0, 0, 0);

    data[16..18].copy_from_slice(&3u16.to_le_bytes()); // ET_DYN

    assert!(matches!(
        executable::parse_header(&data),
        Err(executable::Error::NotExecutable)
    ));
}
#[test]
fn parse_not_little_endian() {
    let mut data = minimal_elf_header(0, 0, 0);

    data[5] = 2; // ELFDATA2MSB

    assert!(matches!(
        executable::parse_header(&data),
        Err(executable::Error::NotLittleEndian)
    ));
}
#[test]
fn parse_too_small() {
    let data = vec![0u8; 32];

    assert!(matches!(
        executable::parse_header(&data),
        Err(executable::Error::TooSmall)
    ));
}
#[test]
fn parse_valid_header() {
    let data = minimal_elf_header(0x400000, 64, 2);
    let h = executable::parse_header(&data).unwrap();

    assert_eq!(h.entry, 0x400000);
    assert_eq!(h.ph_offset, 64);
    assert_eq!(h.ph_count, 2);
    assert_eq!(h.ph_ent_size, 56);
}
#[test]
fn segment_attrs_executable() {
    let a = executable::segment_attrs(5); // PF_R | PF_X

    assert!(matches!(a, address_space::PageAttrs(2))); // user_rx
}
#[test]
fn segment_attrs_readonly() {
    let a = executable::segment_attrs(4); // PF_R

    assert!(matches!(a, address_space::PageAttrs(0))); // user_ro
}
#[test]
fn segment_attrs_writable() {
    let a = executable::segment_attrs(6); // PF_R | PF_W

    assert!(matches!(a, address_space::PageAttrs(1))); // user_rw
}
#[test]
fn segment_attrs_wx_prefers_x() {
    // W^X enforcement: both W and X → RX
    let a = executable::segment_attrs(7); // PF_R | PF_W | PF_X

    assert!(matches!(a, address_space::PageAttrs(2))); // user_rx
}
#[test]
fn segment_data_out_of_bounds() {
    let data = vec![0u8; 64];
    let seg = executable::LoadSegment {
        vaddr: 0,
        file_offset: 32,
        file_size: 64,
        mem_size: 64,
        flags: 0,
    };

    assert!(matches!(
        executable::segment_data(&data, &seg),
        Err(executable::Error::SegmentOutOfBounds)
    ));
}
#[test]
fn segment_data_valid() {
    let payload = b"hello ELF segment";
    let mut data = vec![0u8; 128];

    data[64..64 + payload.len()].copy_from_slice(payload);

    let seg = executable::LoadSegment {
        vaddr: 0x400000,
        file_offset: 64,
        file_size: payload.len() as u64,
        mem_size: 4096,
        flags: 4,
    };
    let slice = executable::segment_data(&data, &seg).unwrap();

    assert_eq!(slice, payload);
}

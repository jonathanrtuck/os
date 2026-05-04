//! Service manifest — static array of services to launch at boot.

use libsys::types::Priority;

#[allow(dead_code)]
pub struct ServiceEntry {
    pub name: &'static str,
    pub code_vmo_handle_index: u32,
    pub priority: Priority,
}

pub static SERVICES: &[ServiceEntry] = &[];

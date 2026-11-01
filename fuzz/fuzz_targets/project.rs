//! ETS export import never panics on arbitrary bytes.
#![no_main]

use knxide_project::GroupAddressTable;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = GroupAddressTable::parse_bytes(data);
});

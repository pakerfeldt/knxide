//! cEMI decoding never panics, and anything that decodes re-encodes to an equal frame.
#![no_main]

use knxide_core::Cemi;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(frame) = Cemi::decode(data) {
        let bytes = frame.to_vec().expect("decoded frame must re-encode");
        assert_eq!(Cemi::decode(&bytes), Ok(frame));
    }
});

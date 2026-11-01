//! DPT decoding never panics for any type and payload; decoded values re-encode.
#![no_main]

use knxide_core::Payload;
use knxide_core::dpt::{DptId, DptValue};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&selector, rest)) = data.split_first() else { return };
    let ids = [
        "1.001", "2.001", "3.007", "5", "5.001", "5.003", "6", "7", "8", "9.001", "10.001",
        "11.001", "12", "13", "14", "16.000", "16.001", "17.001", "18.001", "20.102", "232.600",
    ];
    let id: DptId = ids[usize::from(selector) % ids.len()].parse().unwrap();
    let payload = if selector & 0x80 != 0 {
        Payload::Small(rest.first().copied().unwrap_or(0))
    } else {
        Payload::Bytes(rest.to_vec())
    };
    if let Ok(value) = id.decode(&payload) {
        // Floats may lose precision through scaling; everything else must re-encode exactly.
        let encoded = id.encode(&value).expect("decoded value must re-encode");
        if !matches!(value, DptValue::Float(_)) {
            assert_eq!(id.decode(&encoded), Ok(value));
        }
    }
});

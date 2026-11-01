//! Parsers for addresses, DPT ids and DPT values never panic on arbitrary text.
#![no_main]

use knxide_core::dpt::DptId;
use knxide_core::{GroupAddress, IndividualAddress};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| {
    let _ = text.parse::<GroupAddress>();
    let _ = text.parse::<IndividualAddress>();
    let _ = text.parse::<DptId>();
    for id in ["1.001", "2.001", "3.007", "9.001", "10.001", "11.001", "18.001", "20.102", "232.600"] {
        let _ = id.parse::<DptId>().unwrap().parse_value(text);
    }
});

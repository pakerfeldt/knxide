//! KNXnet/IP decoding never panics, and anything that decodes re-encodes to an equal frame.
#![no_main]

use knxide_ip::Frame;
use knxide_ip::frame::frame_len;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = frame_len(data);
    if let Ok(frame) = Frame::decode(data) {
        let bytes = frame.to_vec().expect("decoded frame must re-encode");
        assert_eq!(Frame::decode(&bytes), Ok(frame));
    }
});

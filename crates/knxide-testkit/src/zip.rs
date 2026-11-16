//! A ZIP writer for tests: stored entries only, enough to make a project archive.

/// CRC-32 as ZIP uses it (polynomial `0xEDB88320`, bit by bit; test data is small).
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                0xEDB8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// An archive with `files` (path, content) as stored entries, in the given order.
pub fn stored(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut directory = Vec::new();
    for (name, content) in files {
        let offset = out.len() as u32;
        let mut common = Vec::new();
        common.extend(20u16.to_le_bytes()); // version needed
        common.extend(0x0800u16.to_le_bytes()); // names are UTF-8
        common.extend(0u16.to_le_bytes()); // stored
        common.extend([0u8; 4]); // time, date
        common.extend(crc32(content).to_le_bytes());
        common.extend((content.len() as u32).to_le_bytes());
        common.extend((content.len() as u32).to_le_bytes());
        common.extend((name.len() as u16).to_le_bytes());
        common.extend(0u16.to_le_bytes()); // extra field length

        out.extend(0x0403_4B50u32.to_le_bytes());
        out.extend(&common);
        out.extend(name.as_bytes());
        out.extend(content);

        directory.extend(0x0201_4B50u32.to_le_bytes());
        directory.extend(20u16.to_le_bytes()); // version made by
        directory.extend(&common);
        directory.extend([0u8; 10]); // comment length, disk, attributes
        directory.extend(offset.to_le_bytes());
        directory.extend(name.as_bytes());
    }
    let offset = out.len() as u32;
    let count = files.len() as u16;
    out.extend(&directory);
    out.extend(0x0605_4B50u32.to_le_bytes());
    out.extend([0u8; 4]); // disk numbers
    out.extend(count.to_le_bytes());
    out.extend(count.to_le_bytes());
    out.extend((directory.len() as u32).to_le_bytes());
    out.extend(offset.to_le_bytes());
    out.extend(0u16.to_le_bytes()); // comment length
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn archive_shape() {
        let bytes = stored(&[("a.txt".to_string(), b"hello".to_vec())]);
        assert_eq!(&bytes[..4], b"PK\x03\x04");
        assert_eq!(&bytes[bytes.len() - 22..bytes.len() - 18], b"PK\x05\x06");
        assert_eq!(bytes.len(), 30 + 5 + 5 + 46 + 5 + 22);
    }
}

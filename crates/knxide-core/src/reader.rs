//! Bounds-checked byte cursor so decoders never index out of range.
//!
//! Shared with `knxide-ip`; not part of the stable API.

use crate::error::DecodeError;

/// Byte cursor over a borrowed slice.
pub struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    /// Start at the beginning of `buf`.
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    /// Read one octet.
    pub fn u8(&mut self) -> Result<u8, DecodeError> {
        let (&first, rest) = self.buf.split_first().ok_or(DecodeError::UnexpectedEnd)?;
        self.buf = rest;
        Ok(first)
    }

    /// Read a big-endian `u16`.
    pub fn u16(&mut self) -> Result<u16, DecodeError> {
        let [a, b] = self.array()?;
        Ok(u16::from_be_bytes([a, b]))
    }

    /// Read `N` octets.
    pub fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let (head, rest) = self
            .buf
            .split_first_chunk::<N>()
            .ok_or(DecodeError::UnexpectedEnd)?;
        self.buf = rest;
        Ok(*head)
    }

    /// Read `n` octets.
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let (head, rest) = self
            .buf
            .split_at_checked(n)
            .ok_or(DecodeError::UnexpectedEnd)?;
        self.buf = rest;
        Ok(head)
    }

    /// Read everything that is left.
    pub fn rest(&mut self) -> &'a [u8] {
        core::mem::take(&mut self.buf)
    }

    /// Octets left.
    pub fn remaining(&self) -> usize {
        self.buf.len()
    }
}

//! A bounds-checked reader over a partially received byte stream.

use std::ops::Range;

use crate::error::ProtocolErrorKind;

/// Why a parse step stopped.
#[derive(Debug)]
pub(crate) enum Stop {
    /// More bytes are needed; nothing has been consumed.
    Incomplete,
    Invalid(ProtocolErrorKind),
}

impl From<ProtocolErrorKind> for Stop {
    fn from(k: ProtocolErrorKind) -> Self {
        Stop::Invalid(k)
    }
}

pub(crate) type Parse<T> = Result<T, Stop>;

pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Returns the range of the next `n` bytes and skips them.
    pub fn take(&mut self, n: usize) -> Parse<Range<usize>> {
        let end = self.pos.checked_add(n).ok_or(Stop::Incomplete)?;
        if end > self.buf.len() {
            return Err(Stop::Incomplete);
        }
        let r = self.pos..end;
        self.pos = end;
        Ok(r)
    }

    pub fn bytes(&mut self, n: usize) -> Parse<&'a [u8]> {
        let r = self.take(n)?;
        Ok(&self.buf[r])
    }

    pub fn array<const N: usize>(&mut self) -> Parse<[u8; N]> {
        Ok(self.bytes(N)?.try_into().unwrap())
    }

    pub fn u8(&mut self) -> Parse<u8> {
        Ok(self.array::<1>()?[0])
    }

    pub fn u16(&mut self) -> Parse<u16> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    pub fn u32(&mut self) -> Parse<u32> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    pub fn i32(&mut self) -> Parse<i32> {
        Ok(i32::from_be_bytes(self.array()?))
    }
}

// #![no_std]
#![allow(clippy::uninlined_format_args)]

use core::fmt::{Debug, Display};
use embedded_io::{ErrorType, ReadExactError, SeekFrom};

mod active;
mod pads;
mod passive;

pub use pads::{Bank, SystemHandler};
pub use passive::{Event, Onset, Rd, Wav};

pub const GRAIN_LEN: usize = 512;

#[derive(Debug)]
pub enum Error<E: Debug> {
    BadFormat,
    DataNotFound,
    Other(E),
}

impl<E: Debug + Display> core::fmt::Display for Error<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadFormat => write!(f, "bad format"),
            Self::DataNotFound => write!(f, "data not found"),
            Self::Other(e) => write!(f, "{}", e),
        }
    }
}

impl<E: Debug + Display> core::error::Error for Error<E> {}

impl<E: Debug> From<E> for Error<E> {
    fn from(value: E) -> Self {
        Self::Other(value)
    }
}

pub trait FileHandler: ErrorType {
    type File;

    /// open file handle
    fn open(&mut self, path: &str) -> Result<Self::File, Self::Error>;

    /// clone file handle
    fn try_clone(&mut self, file: &Self::File) -> Result<Self::File, Self::Error>;

    /// close file
    fn close(&mut self, file: &Self::File) -> Result<(), Self::Error>;

    /// Read some bytes from this source into the specified buffer, returning how many bytes were read.
    ///
    /// If no bytes are currently available to read, this function blocks until at least one byte is available.
    ///
    /// If bytes are available, a non-zero amount of bytes is read to the beginning of `buf`, and the amount
    /// is returned. It is not guaranteed that *all* available bytes are returned, it is possible for the
    /// implementation to read an amount of bytes less than `buf.len()` while there are more bytes immediately
    /// available.
    ///
    /// If the reader is at end-of-file (EOF), `Ok(0)` is returned. There is no guarantee that a reader at EOF
    /// will always be so in the future, for example a reader can stop being at EOF if another process appends
    /// more bytes to the underlying file.
    ///
    /// If `buf.len() == 0`, `read` returns without blocking, with either `Ok(0)` or an error.
    /// The `Ok(0)` doesn't indicate EOF, unlike when called with a non-empty buffer.
    fn read(&mut self, file: &mut Self::File, buf: &mut [u8]) -> Result<usize, Self::Error>;

    /// Seek to an offset, in bytes, in a stream.
    fn seek(&mut self, file: &mut Self::File, pos: SeekFrom) -> Result<u64, Self::Error>;

    fn read_exact(
        &mut self,
        file: &mut Self::File,
        buf: &mut [u8],
    ) -> Result<(), ReadExactError<Self::Error>> {
        let mut slice = &mut buf[..];
        while !slice.is_empty() {
            let n = self.read(file, slice)?;
            if n == 0 {
                return Err(ReadExactError::UnexpectedEof);
            }
            slice = &mut slice[n..];
        }
        Ok(())
    }

    /// Returns the current seek position from the start of the stream.
    fn stream_position(&mut self, file: &mut Self::File) -> Result<u64, Self::Error> {
        self.seek(file, SeekFrom::Current(0))
    }
}

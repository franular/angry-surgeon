#![no_std]

#[cfg(feature = "std")]
extern crate alloc;
use embedded_io::{ErrorType, SeekFrom};

mod active;
mod pads;
mod passive;

pub use pads::{Bank, SystemHandler};
pub use passive::{Event, Onset, Phrase, Rd, Wav};

pub const GRAIN_LEN: usize = 256;

pub trait FileHandler: ErrorType {
    type File;
    // type IO<'a>: Read + Write + Seek where Self: 'a;

    // /// temporary embedded_io::* access to file
    // fn io<F, IO, T>(
    //     &mut self,
    //     file: &Self::File,
    //     f: F,
    // ) -> T
    // where
    //     F: FnMut(IO) -> T,
    //     IO: Read<Error = Self::Error> + Write<Error = Self::Error> + Seek<Error = Self::Error>;

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
    fn read(&mut self, file: &Self::File, buf: &mut [u8]) -> Result<usize, Self::Error>;

    /// Write a buffer into this writer, returning how many bytes were written.
    ///
    /// If the writer is not currently ready to accept more bytes (for example, its buffer is full),
    /// this function blocks until it is ready to accept least one byte.
    ///
    /// If it's ready to accept bytes, a non-zero amount of bytes is written from the beginning of `buf`, and the amount
    /// is returned. It is not guaranteed that *all* available buffer space is filled, i.e. it is possible for the
    /// implementation to write an amount of bytes less than `buf.len()` while the writer continues to be
    /// ready to accept more bytes immediately.
    ///
    /// Implementations must not return `Ok(0)` unless `buf` is empty. Situations where the
    /// writer is not able to accept more bytes must instead be indicated with an error,
    /// where the `ErrorKind` is `WriteZero`.
    ///
    /// If `buf` is empty, `write` returns without blocking, with either `Ok(0)` or an error.
    /// `Ok(0)` doesn't indicate an error.
    fn write(&mut self, file: &Self::File, buf: &[u8]) -> Result<usize, Self::Error>;

    /// Seek to an offset, in bytes, in a stream.
    fn seek(&mut self, file: &Self::File, pos: SeekFrom) -> Result<u64, Self::Error>;

    /// Returns the current seek position from the start of the stream.
    fn stream_position(&mut self, file: &Self::File) -> Result<u64, Self::Error> {
        self.seek(file, SeekFrom::Current(0))
    }
}

#![no_std]

#[cfg(feature = "std")]
extern crate alloc;
use core::future::Future;
use embedded_io_async::{Read, Seek, Write};

mod active;
mod pads;
mod passive;

pub use pads::{Bank, SystemHandler};
pub use passive::{Event, Onset, Phrase, Rd, Wav};

pub const GRAIN_LEN: usize = 512;

pub trait FileHandler {
    type File: Read + Write + Seek;

    fn open(
        &mut self,
        path: &str,
    ) -> impl Future<Output = Result<Self::File, <Self::File as embedded_io_async::ErrorType>::Error>>;

    fn try_clone(
        &mut self,
        file: &Self::File,
    ) -> impl Future<Output = Result<Self::File, <Self::File as embedded_io_async::ErrorType>::Error>>;
}

#![no_std]

use core::future::Future;

use embedded_io_async::{Read, Seek, Write};

mod active;
mod pads;
mod passive;

pub use pads::{Scene, SceneHandler};
pub use passive::{Event, Onset, Rd, Wav};

pub const SAMPLE_RATE: u16 = 48000;
pub const GRAIN_LEN: usize = 1024;

pub const PPQ: u8 = 24;
pub const STEP_DIV: u8 = 4;
pub const LOOP_DIV: u8 = 8;

pub const MAX_POOL_LEN: usize = 128;

pub trait FileHandler {
    type File: Read + Write + Seek;

    fn open(
        &mut self,
        path: &str,
    ) -> impl Future<Output = Result<Self::File, <Self::File as embedded_io_async::ErrorType>::Error>>
           + Send;

    fn try_clone(
        &mut self,
        file: &Self::File,
    ) -> impl Future<Output = Result<Self::File, <Self::File as embedded_io_async::ErrorType>::Error>>
           + Send;
}

#[derive(Copy, Clone, serde::Serialize, serde::Deserialize)]
pub struct Fraction {
    numerator: u8,
    denominator: u8,
}

impl Fraction {
    pub fn new(numerator: u8, denominator: u8) -> Self {
        Self {
            numerator,
            denominator,
        }
    }
}

impl From<Fraction> for f32 {
    fn from(value: Fraction) -> Self {
        value.numerator as f32 / value.denominator as f32
    }
}

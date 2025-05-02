//! read-only data types

use crate::{active, pads, FileHandler};

use embedded_io_async::{Read, Seek, Write};
use tinyrand::Rand;

#[cfg(not(feature = "std"))]
use heapless::{String, Vec};
#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use micromath::F32Ext;

#[cfg(feature = "std")]
extern crate alloc;
#[cfg(feature = "std")]
use alloc::{string::String, vec::Vec};

#[derive(Clone, serde::Deserialize)]
pub struct Rd<#[cfg(not(feature = "std"))] const ONSETS: usize> {
    pub tempo: Option<f32>,
    pub steps: Option<u16>,
    #[cfg(not(feature = "std"))]
    pub onsets: Vec<u64, ONSETS>,
    #[cfg(feature = "std")]
    pub onsets: Vec<u64>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Wav<#[cfg(not(feature = "std"))] const PATH: usize> {
    pub tempo: Option<f32>,
    pub steps: Option<u16>,
    #[cfg(not(feature = "std"))]
    pub path: String<PATH>,
    #[cfg(feature = "std")]
    pub path: String,
    /// pcm length in bytes
    pub len: u64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Onset<#[cfg(not(feature = "std"))] const PATH: usize> {
    #[cfg(not(feature = "std"))]
    pub wav: Wav<PATH>,
    #[cfg(feature = "std")]
    pub wav: Wav,
    pub start: u64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub enum Event {
    Sync,
    Hold { index: u8 },
    Loop { index: u8, len: super::Fraction },
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Stamped {
    pub event: Event,
    pub step: u16,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Phrase<const STEPS: usize> {
    pub events: heapless::Vec<Stamped, STEPS>,
    pub len: u16,
}

impl<const STEPS: usize> Phrase<STEPS> {
    #[allow(clippy::too_many_arguments)]
    pub async fn generate_active<
        const PADS: usize,
        #[cfg(not(feature = "std"))] const PATH: usize,
        IO: Read + Write + Seek,
    >(
        &self,
        active: &mut Option<active::Phrase<IO>>,
        step: u16,
        bias: f32,
        drift: f32,
        rand: &mut impl Rand,
        #[cfg(not(feature = "std"))] kit: &pads::Kit<PADS, STEPS, PATH>,
        #[cfg(feature = "std")] kit: &pads::Kit<PADS, STEPS>,
        fs: &mut impl FileHandler<File = IO>,
    ) -> Result<Option<active::Phrase<IO>>, IO::Error> {
        if let Some(active) = active.as_mut() {
            if self.events.first().is_some_and(|v| v.step == 0) {
                // phrase events start on first step
                if let Some(event_rem) = self
                    .generate_stamped(&mut active.active, 0, step, bias, drift, rand, kit, fs)
                    .await?
                {
                    active.next = 1;
                    active.event_rem = event_rem;
                    active.phrase_rem = self.len;
                }
            } else {
                // phrase events start after first step
                let event_rem = self.events.first().map(|v| v.step).unwrap_or(self.len);
                active
                    .active
                    .trans(&Event::Sync, step, bias, rand, kit, fs)
                    .await?;
                active.next = 0;
                active.event_rem = event_rem;
                active.phrase_rem = self.len;
            }
        } else if self.events.first().is_some_and(|v| v.step == 0) {
            // phrase events start on first step
            let mut active = active::Event::Sync;
            if let Some(event_rem) = self
                .generate_stamped(&mut active, 0, step, bias, drift, rand, kit, fs)
                .await?
            {
                return Ok(Some(active::Phrase {
                    next: 1,
                    event_rem,
                    phrase_rem: self.len,
                    active,
                }));
            }
        } else {
            // phrase events start after first step
            let event_rem = self.events.first().map(|v| v.step).unwrap_or(self.len);
            return Ok(Some(active::Phrase {
                next: 0,
                event_rem,
                phrase_rem: self.len,
                active: active::Event::Sync,
            }));
        }
        Ok(None)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn generate_stamped<
        const PADS: usize,
        #[cfg(not(feature = "std"))] const PATH: usize,
        IO: Read + Write + Seek,
    >(
        &self,
        active: &mut active::Event<IO>,
        index: usize,
        step: u16,
        bias: f32,
        drift: f32,
        rand: &mut impl Rand,
        #[cfg(not(feature = "std"))] kit: &pads::Kit<PADS, STEPS, PATH>,
        #[cfg(feature = "std")] kit: &pads::Kit<PADS, STEPS>,
        fs: &mut impl FileHandler<File = IO>,
    ) -> Result<Option<u16>, IO::Error> {
        let drift =
            rand.next_lim_usize(((drift * self.events.len() as f32 - 1.).round()) as usize + 1);
        let index = (index + drift) % self.events.len();
        let stamped = &self.events[index];
        let event_rem = self
            .events
            .get(index + 1)
            .map(|v| v.step)
            .unwrap_or(self.len)
            - stamped.step;
        active
            .trans(&stamped.event, step, bias, rand, kit, fs)
            .await?;
        Ok(Some(event_rem))
    }
}

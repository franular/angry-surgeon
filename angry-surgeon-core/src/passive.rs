//! read-only data types

use crate::{active, pads, FileHandler};

use tinyrand::Rand;

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use micromath::F32Ext;

extern crate alloc;

#[derive(Clone, serde::Deserialize)]
pub struct Rd {
    pub tempo: Option<f32>,
    pub steps: Option<u16>,
    pub onsets: alloc::vec::Vec<u64>,
}

impl Default for Rd {
    fn default() -> Self {
        Self {
            tempo: None,
            steps: None,
            onsets: alloc::vec![0],
        }
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Wav {
    pub tempo: Option<f32>,
    pub steps: Option<u16>,
    pub path: alloc::string::String,
    /// pcm length in bytes
    pub len: u64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Onset {
    pub wav: Wav,
    pub start: u64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub enum Event {
    Sync,
    Hold { index: u8 },
    Loop { index: u8, len: u16 },
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
    pub fn generate_active<'d, const PADS: usize, F: FileHandler>(
        &'d self,
        active: &'d mut Option<active::Phrase<F>>,
        step: u16,
        bank: &pads::Bank<PADS, STEPS>,
        kit_index: usize,
        kit_drift: f32,
        phrase_drift: f32,
        rand: &mut impl Rand,
        fs: &mut F,
    ) -> Result<Option<active::Phrase<F>>, F::Error> {
        if let Some(active) = active.as_mut() {
            if self.events.first().is_some_and(|v| v.step == 0) {
                // phrase events start on first step
                if let Some(event_rem) = self.generate_stamped(
                    &mut active.active,
                    0,
                    step,
                    bank,
                    kit_index,
                    kit_drift,
                    phrase_drift,
                    rand,
                    fs,
                )? {
                    active.next = 1;
                    active.event_rem = event_rem;
                    active.phrase_rem = self.len;
                }
            } else {
                // phrase events start after first step
                let event_rem = self.events.first().map(|v| v.step).unwrap_or(self.len);
                active
                    .active
                    .trans(&Event::Sync, step, bank, kit_index, kit_drift, rand, fs)?;
                active.next = 0;
                active.event_rem = event_rem;
                active.phrase_rem = self.len;
            }
        } else if self.events.first().is_some_and(|v| v.step == 0) {
            // phrase events start on first step
            let mut active = active::Event::Sync;
            if let Some(event_rem) = self.generate_stamped(
                &mut active,
                0,
                step,
                bank,
                kit_index,
                kit_drift,
                phrase_drift,
                rand,
                fs,
            )? {
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
    pub fn generate_stamped<const PADS: usize, F: FileHandler>(
        &self,
        active: &mut active::Event<F>,
        index: usize,
        step: u16,
        bank: &pads::Bank<PADS, STEPS>,
        kit_index: usize,
        kit_drift: f32,
        phrase_drift: f32,
        rand: &mut impl Rand,
        fs: &mut F,
    ) -> Result<Option<u16>, F::Error> {
        let drift = phrase_drift * self.events.len() as f32;
        let drift = rand.next_lim_usize(drift as usize + 1)
            + rand.next_bool(tinyrand::Probability::new(drift.fract() as f64)) as usize;
        let index = (index + drift) % self.events.len();
        let stamped = &self.events[index];
        let event_rem = self
            .events
            .get(index + 1)
            .map(|v| v.step)
            .unwrap_or(self.len)
            - stamped.step;
        active.trans(&stamped.event, step, bank, kit_index, kit_drift, rand, fs)?;
        Ok(Some(event_rem))
    }
}

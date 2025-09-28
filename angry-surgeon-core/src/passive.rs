//! read-only data types

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use micromath::F32Ext;
use tinyrand::Rand;

extern crate alloc;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Rd {
    pub steps: Option<u16>,
    pub onsets: alloc::vec::Vec<u64>,
}

impl Default for Rd {
    fn default() -> Self {
        Self {
            steps: None,
            onsets: alloc::vec![0],
        }
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Wav {
    pub steps: Option<u16>,
    pub path: alloc::string::String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Onset {
    pub wav: Wav,
    pub start: u64,
}

#[derive(Copy, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Event {
    Sync,
    Hold { index: u8 },
    Loop { index: u8, len: u16 },
}

#[derive(Copy, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct Step {
    pub event: Option<Event>,
    pub reverse: bool,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Phrase<const STEPS: usize> {
    #[serde(with = "serde_arrays")]
    pub(crate) steps: [Step; STEPS],
    pub(crate) len: u16,
}

impl<const STEPS: usize> Phrase<STEPS> {
    pub(crate) fn generate_step(&self, step_index: u16, phrase_drift: f32, rand: &mut impl Rand) -> Step {
        let drift = phrase_drift * self.len as f32;
        let drift = rand.next_lim_usize(drift as usize + 1)
            + rand.next_bool(tinyrand::Probability::new(drift.fract() as f64)) as usize;
        let index = STEPS - self.len as usize + (step_index as usize + drift) % self.len as usize;
        self.steps[index]
    }
}

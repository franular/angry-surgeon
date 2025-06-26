//! stateful data types

use crate::{pads, passive, FileHandler};
use core::mem::MaybeUninit;
use heapless::{Deque, Vec};
use tinyrand::Rand;

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use micromath::F32Ext;

pub struct Wav<F: FileHandler> {
    pub tempo: Option<f32>,
    pub steps: Option<u16>,
    pub file: F::File,
    pub len: u64,
}

impl<F: FileHandler> Wav<F> {
    pub fn pos(&mut self, fs: &mut F) -> Result<u64, F::Error> {
        Ok(fs.stream_position(&self.file)? - 44)
    }

    pub fn seek(&mut self, offset: i64, fs: &mut F) -> Result<(), F::Error> {
        fs.seek(
            &self.file,
            embedded_io::SeekFrom::Start(44 + offset.rem_euclid(self.len as i64) as u64),
        )?;
        Ok(())
    }
}

pub struct Onset<F: FileHandler> {
    /// source onset index
    pub index: u8,
    pub pan: f32,
    pub wav: Wav<F>,
    pub start: u64,
}

pub enum Event<F: FileHandler> {
    Sync,
    Hold(Onset<F>, u16),
    Loop(Onset<F>, u16, u16),
}

impl<F: FileHandler> Event<F> {
    #[allow(clippy::too_many_arguments)]
    pub fn trans<const PADS: usize, const STEPS: usize>(
        &mut self,
        input: &passive::Event,
        step: u16,
        bank: &pads::Bank<PADS, STEPS>,
        kit_index: usize,
        kit_drift: f32,
        rand: &mut impl Rand,
        fs: &mut F,
    ) -> Result<(), F::Error> {
        match input {
            passive::Event::Sync => {
                if let Event::Hold(onset, ..) | Event::Loop(onset, ..) = self {
                    // close old file
                    fs.close(&onset.wav.file)?;
                }
                *self = Event::Sync;
            }
            passive::Event::Hold { index } => {
                match self {
                    Event::Loop(onset, ..) => {
                        // recast event variant with same Onset
                        let uninit: &mut MaybeUninit<Onset<F>> =
                            unsafe { core::mem::transmute(onset) };
                        let mut onset = unsafe {
                            core::mem::replace(uninit, MaybeUninit::uninit()).assume_init()
                        };
                        // i don't know either, girl
                        onset.wav.file = fs.try_clone(&onset.wav.file)?;
                        *self = Event::Hold(onset, step);
                    }
                    Event::Hold(onset, ..) => {
                        if let Some(kit) = bank.generate_kit(kit_index, kit_drift, rand) {
                            // close old file and replace onset
                            if let Some(new) =
                                kit.onset_seek(Some(&onset.wav.file), *index, pads::Kit::<PADS>::generate_pan(*index), fs)?
                            {
                                *self = Event::Hold(new, step);
                            }
                        }
                    }
                    _ => {
                        if let Some(kit) = bank.generate_kit(kit_index, kit_drift, rand) {
                            // replace onset; no old file to close
                            if let Some(onset) =
                                kit.onset_seek(None, *index, pads::Kit::<PADS>::generate_pan(*index), fs)?
                            {
                                *self = Event::Hold(onset, step);
                            }
                        }
                    }
                }
            }
            passive::Event::Loop { index, len } => {
                match self {
                    Event::Hold(onset, step) | Event::Loop(onset, step, ..) => {
                        if onset.index == *index {
                            // recast event variant with same Onset
                            let uninit: &mut MaybeUninit<Onset<F>> =
                                unsafe { core::mem::transmute(onset) };
                            let mut onset = unsafe {
                                core::mem::replace(uninit, MaybeUninit::uninit()).assume_init()
                            };
                            // i don't know either, girl
                            onset.wav.file = fs.try_clone(&onset.wav.file)?;
                            *self = Event::Loop(onset, *step, *len);
                        } else if let Some(kit) = bank.generate_kit(kit_index, kit_drift, rand) {
                            // close old file and replace onset
                            if let Some(new) =
                                kit.onset(Some(&onset.wav.file), *index, pads::Kit::<PADS>::generate_pan(*index), fs)?
                            {
                                *self = Event::Loop(new, *step, *len);
                            }
                        }
                    }
                    _ => {
                        if let Some(kit) = bank.generate_kit(kit_index, kit_drift, rand) {
                            // replace onset; no old file to close
                            if let Some(onset) =
                                kit.onset(None, *index, pads::Kit::<PADS>::generate_pan(*index), fs)?
                            {
                                *self = Event::Loop(onset, step, *len);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

pub struct Input<F: FileHandler> {
    pub active: Event<F>,
    pub buffer: Option<passive::Event>,
}

impl<F: FileHandler> Input<F> {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            active: Event::Sync,
            buffer: None,
        }
    }
}

pub struct Phrase<F: FileHandler> {
    /// next event index (sans drift)
    pub next: usize,
    /// remaining steps in event
    pub event_rem: u16,
    /// remaining steps in phrase
    pub phrase_rem: u16,
    /// active event (last consumed)
    pub active: Event<F>,
}

pub struct Record<const STEPS: usize, F: FileHandler> {
    /// running bounded event queue
    events: Deque<passive::Stamped, STEPS>,
    /// baked events
    buffer: Vec<passive::Stamped, STEPS>,
    /// trimmed source phrase, if any
    pub phrase: Option<passive::Phrase<STEPS>>,
    /// active phrase, if any
    pub active: Option<Phrase<F>>,
}

impl<const STEPS: usize, F: FileHandler> Record<STEPS, F> {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            events: Deque::new(),
            buffer: Vec::new(),
            phrase: None,
            active: None,
        }
    }

    pub fn push(&mut self, event: passive::Event, step: u16) {
        // remove steps beyond max phrase len
        while self
            .events
            .front()
            .is_some_and(|v| step - v.step > STEPS as u16)
        {
            self.events.pop_front();
        }
        let _ = self.events.push_back(passive::Stamped { event, step });
    }

    pub fn bake(&mut self, step: u16) {
        self.buffer = self
            .events
            .iter()
            .flat_map(|v| {
                Some(passive::Stamped {
                    event: v.event.clone(),
                    step: (v.step + STEPS as u16).checked_sub(step)?,
                })
            })
            .collect::<Vec<_, STEPS>>();
    }

    pub fn trim(&mut self, len: u16) {
        let events = self
            .buffer
            .iter()
            .flat_map(|v| {
                Some(passive::Stamped {
                    event: v.event.clone(),
                    step: (v.step + len).checked_sub(STEPS as u16)?,
                })
            })
            .collect::<Vec<_, STEPS>>();
        self.phrase = Some(passive::Phrase { events, len });
    }

    #[allow(clippy::too_many_arguments)]
    pub fn generate_phrase<const PADS: usize>(
        &mut self,
        step: u16,
        bank: &pads::Bank<PADS, STEPS>,
        kit_index: usize,
        kit_drift: f32,
        phrase_drift: f32,
        rand: &mut impl Rand,
        fs: &mut F,
    ) -> Result<(), F::Error> {
        if let Some(phrase) = self.phrase.as_mut() {
            if let Some(phrase) = phrase.generate_active(
                &mut self.active,
                step,
                bank,
                kit_index,
                kit_drift,
                phrase_drift,
                rand,
                fs,
            )? {
                self.active = Some(phrase);
            }
        }
        Ok(())
    }

    pub fn take(&mut self) -> Option<(passive::Phrase<STEPS>, Phrase<F>)> {
        self.buffer.clear();
        Some((self.phrase.take()?, self.active.take()?))
    }
}

pub struct Pool<const PHRASES: usize, F: FileHandler> {
    /// next phrase index (sans drift)
    pub next: usize,
    /// base phrase sequence
    pub phrases: Vec<u8, PHRASES>,
    /// pad index of source phrase, if any
    pub index: Option<u8>,
    /// active phrase, if any
    pub active: Option<Phrase<F>>,
}

impl<const PHRASES: usize, F: FileHandler> Pool<PHRASES, F> {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            next: 0,
            phrases: Vec::new(),
            index: None,
            active: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn generate_phrase<'d, const PADS: usize, const STEPS: usize>(
        &'d mut self,
        step: u16,
        bank: &'d pads::Bank<PADS, STEPS>,
        kit_index: usize,
        kit_drift: f32,
        phrase_drift: f32,
        rand: &mut impl Rand,
        fs: &mut F,
    ) -> Result<(), F::Error> {
        if self.phrases.is_empty() {
            self.next = 0;
            self.active = None;
        } else {
            let index = {
                // FIXME: use independent phrase_drift instead of same "stamped_drift"?
                let drift = rand.next_lim_usize(
                    ((phrase_drift * self.phrases.len() as f32 - 1.).round()) as usize + 1,
                );
                let index = (self.next + drift) % self.phrases.len();
                self.phrases[index]
            };
            self.index = Some(index);
            if let Some(phrase) = &bank.phrases[index as usize] {
                if let Some(phrase) = phrase.generate_active(
                    &mut self.active,
                    step,
                    bank,
                    kit_index,
                    kit_drift,
                    phrase_drift,
                    rand,
                    fs,
                )? {
                    self.active = Some(phrase);
                }
            }
            self.next = (self.next + 1) % self.phrases.len();
        }
        Ok(())
    }
}

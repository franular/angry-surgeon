//! stateful data types

use std::mem::MaybeUninit;

use crate::{pads, passive, Error, FileHandler};
use embedded_io::SeekFrom;
use tinyrand::Rand;

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use micromath::F32Ext;

#[derive(Clone)]
pub(crate) struct Wav<F: FileHandler> {
    pub steps: Option<u16>,
    pub file: F::File,
    pub pcm_start: u64,
    pub pcm_len: u64,
    pub sample_rate: u32,
}

impl<F: FileHandler> Wav<F> {
    pub fn pos(&mut self, fs: &mut F) -> Result<u64, F::Error> {
        Ok(fs.stream_position(&mut self.file)? - self.pcm_start)
    }

    pub fn seek(&mut self, offset: i64, fs: &mut F) -> Result<(), F::Error> {
        fs.seek(
            &mut self.file,
            SeekFrom::Start(self.pcm_start + offset.rem_euclid(self.pcm_len as i64) as u64),
        )
        .map(|_| ())
    }

    // read that loops without crossfade as fallback
    pub fn read(&mut self, mut bytes: &mut [u8], fs: &mut F) -> Result<(), F::Error> {
        while !bytes.is_empty() {
            let len = bytes.len().min((self.pcm_len - self.pos(fs)?) as usize);
            let n = fs.read(&mut self.file, &mut bytes[..len])?;
            if n == 0 {
                self.seek(0, fs)?;
            }
            bytes = &mut bytes[n..];
        }
        Ok(())
    }
}

pub(crate) struct Onset<F: FileHandler> {
    /// pad index of source onset
    pub index: u8,
    pub pan: f32,
    pub wav: Wav<F>,
    pub start: u64,
}

pub(crate) enum Event<F: FileHandler> {
    Sync,
    Hold {
        onset: Onset<F>,
        tick: i16,
    },
    Loop {
        onset: Onset<F>,
        tick: i16,
        len: u16,
    },
}

impl<F: FileHandler> Event<F> {
    #[allow(clippy::too_many_arguments)]
    pub fn trans<const PADS: usize, const STEPS: usize>(
        &mut self,
        input: &passive::Event,
        bank: &pads::Bank<PADS, STEPS>,
        kit_index: u8,
        kit_drift: f32,
        grain: &mut pads::GrainReader,
        rand: &mut impl Rand,
        fs: &mut F,
    ) -> Result<(), Error<F::Error>> {
        match input {
            passive::Event::Sync => {
                if let Event::Hold { onset, .. } | Event::Loop { onset, .. } = self {
                    grain.fade(Some(&mut onset.wav), fs)?;
                    // close old file
                    fs.close(&onset.wav.file)?;
                    *self = Event::Sync;
                }
            }
            passive::Event::Hold { index } => {
                match self {
                    Event::Sync => {
                        if let Some(kit) = bank.generate_kit(kit_index, kit_drift, rand) {
                            grain.fade(None, fs)?;
                            // replace onset; no old file to close
                            if let Some(onset) = kit.onset_seek(
                                None,
                                *index,
                                pads::Kit::<PADS>::generate_pan(*index),
                                fs,
                            )? {
                                *self = Event::Hold { onset, tick: 0 };
                            }
                        }
                    }
                    Event::Hold { onset, .. } => {
                        if let Some(kit) = bank.generate_kit(kit_index, kit_drift, rand) {
                            grain.fade(Some(&mut onset.wav), fs)?;
                            // close old file and replace onset
                            if let Some(onset) = kit.onset_seek(
                                Some(&onset.wav.file),
                                *index,
                                pads::Kit::<PADS>::generate_pan(*index),
                                fs,
                            )? {
                                *self = Event::Hold { onset, tick: 0 };
                            }
                        }
                    }
                    Event::Loop { onset, .. } => {
                        // recast event variant with same onset
                        let uninit: &mut MaybeUninit<Onset<F>> =
                            unsafe { core::mem::transmute(onset) };
                        let mut onset = unsafe {
                            core::mem::replace(uninit, MaybeUninit::uninit()).assume_init()
                        };
                        // i don't know either, girl
                        onset.wav.file = fs.try_clone(&onset.wav.file)?;
                        *self = Event::Hold { onset, tick: 0 };
                    }
                }
            }
            passive::Event::Loop { index, len } => {
                match self {
                    Event::Sync => {
                        if let Some(kit) = bank.generate_kit(kit_index, kit_drift, rand) {
                            grain.fade(None, fs)?;
                            // replace onset; no old file to close
                            if let Some(onset) = kit.onset_seek(
                                None,
                                *index,
                                pads::Kit::<PADS>::generate_pan(*index),
                                fs,
                            )? {
                                *self = Event::Loop {
                                    onset,
                                    tick: 0,
                                    len: *len,
                                };
                            }
                        }
                    }
                    Event::Hold { onset, tick } | Event::Loop { onset, tick, .. } => {
                        if onset.index == *index {
                            // recast event variant with same Onset
                            let uninit: &mut MaybeUninit<Onset<F>> =
                                unsafe { core::mem::transmute(onset) };
                            let mut onset = unsafe {
                                core::mem::replace(uninit, MaybeUninit::uninit()).assume_init()
                            };
                            // i don't know either, girl
                            onset.wav.file = fs.try_clone(&onset.wav.file)?;
                            *self = Event::Loop {
                                onset,
                                tick: *tick,
                                len: *len,
                            };
                        } else if let Some(kit) = bank.generate_kit(kit_index, kit_drift, rand) {
                            grain.fade(Some(&mut onset.wav), fs)?;
                            // close old file and replace onset
                            if let Some(onset) = kit.onset_seek(
                                Some(&onset.wav.file),
                                *index,
                                pads::Kit::<PADS>::generate_pan(*index),
                                fs,
                            )? {
                                *self = Event::Loop {
                                    onset,
                                    tick: *tick,
                                    len: *len,
                                };
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// active event and reverse
pub(crate) struct Active<F: FileHandler> {
    pub event: Event<F>,
    pub reverse: bool,
}

impl<F: FileHandler> Default for Active<F> {
    fn default() -> Self {
        Self {
            event: Event::Sync,
            reverse: false,
        }
    }
}

impl<F: FileHandler> Active<F> {
    pub fn non_sync(&mut self) -> Option<&mut Event<F>> {
        if !matches!(self.event, Event::Sync) {
            Some(&mut self.event)
        } else {
            None
        }
    }

    /// in-/decrement tick w/ `self.reverse` ^ `xor_reverse`
    /// the seeks are done in BankHandler::tick() for single crossfade
    pub fn tick(&mut self, xor_reverse: bool, ticks_per_step: u16) {
        match &mut self.event {
            Event::Sync => (),
            Event::Hold { tick, .. } => {
                if self.reverse ^ xor_reverse {
                    *tick -= ticks_per_step as i16;
                } else {
                    *tick += ticks_per_step as i16;
                }
            }
            Event::Loop { tick, .. } => {
                if self.reverse ^ xor_reverse {
                    *tick -= ticks_per_step as i16;
                } else {
                    *tick += ticks_per_step as i16;
                }
            }
        }
    }
}

pub(crate) struct Input<F: FileHandler> {
    pub buffer: passive::Step,
    pub active: Active<F>,
}

impl<F: FileHandler> Default for Input<F> {
    fn default() -> Self {
        Self {
            buffer: passive::Step::default(),
            active: Active::default(),
        }
    }
}

impl<F: FileHandler> Input<F> {
    #[allow(clippy::too_many_arguments)]
    pub fn tick<const PADS: usize, const STEPS: usize>(
        &mut self,
        ticks_per_step: u16,
        bank: &pads::Bank<PADS, STEPS>,
        kit_index: u8,
        kit_drift: f32,
        grain: &mut pads::GrainReader,
        rand: &mut impl Rand,
        fs: &mut F,
    ) -> Result<Option<passive::Event>, Error<F::Error>> {
        self.active.reverse = self.buffer.reverse;
        if let Some(event) = self.buffer.event.take() {
            self.active
                .event
                .trans(&event, bank, kit_index, kit_drift, grain, rand, fs)?;
            return Ok(Some(event));
        } else {
            self.active.tick(false, ticks_per_step);
        }
        Ok(None)
    }
}

/// running phrase reading from **last** passive::Phrase.len steps of source
/// passive::Phrase
pub(crate) struct Phrase<F: FileHandler> {
    /// step index sans drift
    pub step_index: u16,
    pub active: Active<F>,
}

pub(crate) struct Record<const STEPS: usize, F: FileHandler> {
    /// running step queue
    queue: heapless::HistoryBuffer<passive::Step, STEPS>,
    /// trimmed source phrase, if any
    pub source_phrase: Option<passive::Phrase<STEPS>>,
    /// active phrase, if any
    pub active_phrase: Option<Phrase<F>>,
}

impl<const STEPS: usize, F: FileHandler> Default for Record<STEPS, F> {
    fn default() -> Self {
        Self {
            queue: heapless::HistoryBuffer::new(),
            source_phrase: None,
            active_phrase: None,
        }
    }
}

impl<const STEPS: usize, F: FileHandler> Record<STEPS, F> {
    #[allow(clippy::too_many_arguments)]
    pub fn tick<const PADS: usize>(
        &mut self,
        xor_reverse: bool,
        ticks_per_step: u16,
        bank: &pads::Bank<PADS, STEPS>,
        kit_index: u8,
        kit_drift: f32,
        phrase_drift: f32,
        grain: &mut pads::GrainReader,
        rand: &mut impl Rand,
        fs: &mut F,
    ) -> Result<Option<passive::Event>, Error<F::Error>> {
        if let Some(source_phrase) = self.source_phrase.as_ref() {
            if let Some(active_phrase) = self.active_phrase.as_mut() {
                // increment step
                active_phrase.step_index = (active_phrase.step_index + 1) % source_phrase.len;
                let step =
                    source_phrase.generate_step(active_phrase.step_index, phrase_drift, rand);

                active_phrase.active.reverse = step.reverse;
                // process event
                if let Some(ref event) = step.event {
                    active_phrase
                        .active
                        .event
                        .trans(event, bank, kit_index, kit_drift, grain, rand, fs)?;
                    return Ok(Some(*event));
                } else {
                    active_phrase.active.tick(xor_reverse, ticks_per_step);
                }
            } else {
                // start active phrase from empty
                let step = source_phrase.generate_step(0, phrase_drift, rand);
                let mut event = Event::Sync;
                let ret = if let Some(ref source) = step.event {
                    event.trans(source, bank, kit_index, kit_drift, grain, rand, fs)?;
                    Some(*source)
                } else {
                    None
                };
                self.active_phrase = Some(Phrase {
                    step_index: 0,
                    active: Active {
                        event,
                        reverse: step.reverse,
                    },
                });
                return Ok(ret);
            }
        }
        Ok(None)
    }

    pub fn push(&mut self, step: passive::Step) {
        self.queue.write(step);
    }

    pub fn trim(&mut self, len: u16) {
        if let Some(phrase) = self.source_phrase.as_mut() {
            phrase.len = len;
        } else {
            self.save();
        }
        self.active_phrase = None;
    }

    pub fn take(&mut self) -> Option<passive::Phrase<STEPS>> {
        self.active_phrase = None;
        self.source_phrase.take()
    }

    fn save(&mut self) {
        let mut steps = [passive::Step::default(); STEPS];
        let (front, back) = self.queue.as_slices();
        if !front.is_empty() {
            steps[..front.len()].copy_from_slice(front);
        }
        if !back.is_empty() {
            steps[front.len()..][..back.len()].copy_from_slice(back);
        }
        self.source_phrase = Some(passive::Phrase {
            steps,
            len: self.queue.len() as u16,
        });
    }
}

pub(crate) struct Sequence<const PHRASES: usize, F: FileHandler> {
    /// sequence index sans drift
    phrase_index: u16,
    /// sequence of source phrase indices
    phrases: heapless::HistoryBuffer<u8, PHRASES>,
    /// pad index of source phrase, if any
    source_phrase: Option<u8>,
    /// active phrase, if any
    pub active_phrase: Option<Phrase<F>>,
}

impl<const PHRASES: usize, F: FileHandler> Default for Sequence<PHRASES, F> {
    fn default() -> Self {
        Self {
            phrase_index: 0,
            phrases: heapless::HistoryBuffer::new(),
            source_phrase: None,
            active_phrase: None,
        }
    }
}

impl<const PHRASES: usize, F: FileHandler> Sequence<PHRASES, F> {
    #[allow(clippy::too_many_arguments)]
    pub fn tick<const PADS: usize, const STEPS: usize>(
        &mut self,
        xor_reverse: bool,
        ticks_per_step: u16,
        bank: &pads::Bank<PADS, STEPS>,
        kit_index: u8,
        kit_drift: f32,
        phrase_drift: f32,
        grain: &mut pads::GrainReader,
        rand: &mut impl Rand,
        fs: &mut F,
    ) -> Result<Option<passive::Event>, Error<F::Error>> {
        if let Some(active_phrase) = self.active_phrase.as_mut() {
            let source_phrase = self
                .source_phrase
                .and_then(|v| bank.phrases[v as usize].as_ref());

            let source_phrase = if source_phrase.is_some_and(|v| active_phrase.step_index < v.len) {
                // increment step
                active_phrase.step_index += 1;
                source_phrase.unwrap()
            } else if let Some(source_phrase) = Self::try_increment_phrase(
                &mut self.phrase_index,
                &self.phrases,
                &mut self.source_phrase,
                bank,
                phrase_drift,
                rand,
            ) {
                // incremented phrase
                active_phrase.step_index %= source_phrase.len;
                source_phrase
            } else {
                self.active_phrase = None;
                return Ok(None);
            };
            // process step
            let step = source_phrase.generate_step(active_phrase.step_index, phrase_drift, rand);
            active_phrase.active.reverse = step.reverse;
            if let Some(ref event) = step.event {
                active_phrase
                    .active
                    .event
                    .trans(event, bank, kit_index, kit_drift, grain, rand, fs)?;
                return Ok(Some(*event));
            } else {
                active_phrase.active.tick(xor_reverse, ticks_per_step);
            }
        } else if let Some(source_phrase) = Self::try_increment_phrase(
            &mut self.phrase_index,
            &self.phrases,
            &mut self.source_phrase,
            bank,
            phrase_drift,
            rand,
        ) {
            // start active phrase from empty
            let step = source_phrase.generate_step(0, phrase_drift, rand);
            let mut event = Event::Sync;
            let ret = if let Some(ref source) = step.event {
                event.trans(source, bank, kit_index, kit_drift, grain, rand, fs)?;
                Some(*source)
            } else {
                None
            };
            self.active_phrase = Some(Phrase {
                step_index: 0,
                active: Active {
                    event,
                    reverse: step.reverse,
                },
            });
            return Ok(ret);
        } else {
            self.active_phrase = None;
        }
        Ok(None)
    }

    pub fn clear(&mut self) {
        self.phrase_index = 0;
        self.phrases.clear();
        self.source_phrase = None;
    }

    pub fn push(&mut self, index: u8) {
        self.phrases.write(index);
    }

    /// associated method to appease borrow rules
    fn try_increment_phrase<'d, const PADS: usize, const STEPS: usize>(
        phrase_index: &mut u16,
        phrases: &heapless::HistoryBuffer<u8, PHRASES>,
        source_phrase: &mut Option<u8>,
        bank: &'d pads::Bank<PADS, STEPS>,
        phrase_drift: f32,
        rand: &mut impl Rand,
    ) -> Option<&'d passive::Phrase<STEPS>> {
        // try increment phrase
        let phrase_count = phrases
            .oldest_ordered()
            .filter(|v| bank.phrases[**v as usize].is_some())
            .count();
        if phrase_count != 0 {
            *phrase_index = (*phrase_index + 1) % phrase_count as u16;
            *source_phrase = {
                let drift = phrase_drift * phrase_count as f32;
                let drift = rand.next_lim_usize(drift as usize + 1)
                    + rand.next_bool(tinyrand::Probability::new(drift.fract() as f64)) as usize;
                let index = (*phrase_index as usize + drift) % phrase_count;
                phrases
                    .oldest_ordered()
                    .cycle()
                    .filter(|v| bank.phrases[**v as usize].is_some())
                    .nth(index)
                    .copied()
            };
            return source_phrase.and_then(|v| bank.phrases[v as usize].as_ref());
        }
        None
    }
}

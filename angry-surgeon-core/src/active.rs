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
    pub tempo: Option<f32>,
    pub steps: Option<u16>,
    pub file: F::File,
    pub pcm_start: u64,
    pub pcm_len: u64,
    pub seek_to: Option<i64>,
}

impl<F: FileHandler> Wav<F> {
    pub fn pos(&mut self, fs: &mut F) -> Result<u64, F::Error> {
        Ok(fs.stream_position(&mut self.file)? - self.pcm_start)
    }

    pub fn force_seek(&mut self, offset: i64, fs: &mut F) -> Result<(), F::Error> {
        fs.seek(
            &mut self.file,
            SeekFrom::Start(self.pcm_start - offset.rem_euclid(self.pcm_len as i64) as u64),
        )
        .map(|_| ())
    }

    pub fn push_seek(&mut self, offset: i64) {
        self.seek_to = Some(offset);
    }

    pub fn flush_seek(&mut self, fs: &mut F) -> Result<(), F::Error> {
        if let Some(offset) = self.seek_to.take() {
            self.force_seek(offset, fs)?;
        }
        Ok(())
    }

    pub fn read(&mut self, bytes: &mut [u8], fs: &mut F) -> Result<(), F::Error> {
        let mut slice = bytes;
        while !slice.is_empty() {
            let len = slice.len().min((self.pcm_len - self.pos(fs)?) as usize);
            let n = fs.read(&mut self.file, &mut slice[..len])?;
            if n == 0 {
                self.force_seek(0, fs)?;
            }
            slice = &mut slice[n..];
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
        tick: u16,
    },
    Loop {
        onset: Onset<F>,
        tick: u16,
        len: u16,
    },
}

impl<F: FileHandler> Event<F> {
    pub fn trans<const PADS: usize, const STEPS: usize>(
        &mut self,
        input: &passive::Event,
        bank: &pads::Bank<PADS, STEPS>,
        kit_index: u8,
        kit_drift: f32,
        rand: &mut impl Rand,
        fs: &mut F,
    ) -> Result<(), Error<F::Error>> {
        match input {
            passive::Event::Sync => {
                if let Event::Hold { onset, .. } | Event::Loop { onset, .. } = self {
                    // close old file
                    fs.close(&onset.wav.file)?;
                }
                *self = Event::Sync;
            }
            passive::Event::Hold { index } => {
                match self {
                    Event::Sync => {
                        if let Some(kit) = bank.generate_kit(kit_index, kit_drift, rand) {
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
                            // replace onset; no old file to close
                            if let Some(onset) = kit.onset(
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
                            // close old file and replace onset
                            if let Some(onset) = kit.onset(
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
    /// reverse start tick
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

    /// in-/decrement tick w/ `self.reverse` ^ `xor_reverse`, push seek to sync to
    /// tick offset from onset
    pub fn tick(&mut self, xor_reverse: bool, loop_div: f32) {
        match &mut self.event {
            Event::Sync => (),
            Event::Hold { onset, tick } => {
                if self.reverse ^ xor_reverse {
                    *tick -= 1;
                } else {
                    *tick += 1;
                }
                let wav = &mut onset.wav;
                if let Some(steps) = wav.steps {
                    let offset = (wav.pcm_len as f32 / steps as f32 * *tick as f32) as i64 & !1;
                    wav.push_seek(onset.start as i64 * 2 + offset);
                }
            }
            Event::Loop { onset, tick, len } => {
                if self.reverse ^ xor_reverse {
                    *tick -= 1;
                } else {
                    *tick += 1;
                }
                let wav = &mut onset.wav;
                if let Some(steps) = wav.steps {
                    let offset = (wav.pcm_len as f32 / steps as f32
                        * (*tick as f32).rem_euclid(*len as f32 / loop_div))
                        as i64
                        & !1;
                    wav.push_seek(onset.start as i64 * 2 + offset);
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
    pub fn tick<const PADS: usize, const STEPS: usize>(
        &mut self,
        loop_div: f32,
        bank: &pads::Bank<PADS, STEPS>,
        kit_index: u8,
        kit_drift: f32,
        rand: &mut impl Rand,
        fs: &mut F,
    ) -> Result<Option<passive::Event>, Error<F::Error>> {
        if let Some(event) = self.buffer.event.take() {
            self.active
                .event
                .trans(&event, bank, kit_index, kit_drift, rand, fs)?;
            return Ok(Some(event));
        } else {
            self.active.tick(false, loop_div);
        }
        Ok(None)
    }
}

/// running phrase reading from **last** `step_count` steps of active
pub(crate) struct Phrase<F: FileHandler> {
    /// step index sans drift
    pub step_index: u16,
    /// phrase length
    pub step_count: u16,
    pub active: Active<F>,
}

pub(crate) struct Record<const STEPS: usize, F: FileHandler> {
    /// running step queue
    queue: heapless::Deque<passive::Step, STEPS>,
    /// trimmed source phrase, if any
    pub source_phrase: Option<passive::Phrase<STEPS>>,
    /// active phrase, if any
    pub active_phrase: Option<Phrase<F>>,
}

impl<const STEPS: usize, F: FileHandler> Default for Record<STEPS, F> {
    fn default() -> Self {
        Self {
            queue: heapless::Deque::new(),
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
        loop_div: f32,
        bank: &pads::Bank<PADS, STEPS>,
        kit_index: u8,
        kit_drift: f32,
        phrase_drift: f32,
        rand: &mut impl Rand,
        fs: &mut F,
    ) -> Result<Option<passive::Event>, Error<F::Error>> {
        if let Some(source_phrase) = self.source_phrase.as_ref() {
            if let Some(active_phrase) = self.active_phrase.as_mut() {
                // increment step
                active_phrase.step_index =
                    (active_phrase.step_index + 1) % active_phrase.step_count;
                let step =
                    source_phrase.generate_step(active_phrase.step_index, phrase_drift, rand);

                active_phrase.active.reverse = step.reverse;
                // process event
                if let Some(ref event) = step.event {
                    active_phrase
                        .active
                        .event
                        .trans(event, bank, kit_index, kit_drift, rand, fs)?;
                    return Ok(Some(*event));
                } else {
                    active_phrase.active.tick(xor_reverse, loop_div);
                }
            } else {
                // start active phrase from empty
                let step = source_phrase.generate_step(0, phrase_drift, rand);
                let mut event = Event::Sync;
                let ret = if let Some(ref source) = step.event {
                    event.trans(source, bank, kit_index, kit_drift, rand, fs)?;
                    Some(*source)
                } else {
                    None
                };
                self.active_phrase = Some(Phrase {
                    step_index: 0,
                    step_count: source_phrase.len,
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
        let _ = self.queue.push_back(step);
    }

    pub fn save(&mut self) {
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

    pub fn trim(&mut self, len: u16) {
        if let Some(phrase) = self.source_phrase.as_mut() {
            phrase.len = len;
        }
    }
}

pub(crate) struct Sequence<const PHRASES: usize, F: FileHandler> {
    /// sequence index sans drift
    phrase_index: u16,
    /// sequence of source phrase indices
    phrases: heapless::Vec<u8, PHRASES>,
    /// pad index of source phrase, if any
    source_phrase: Option<u8>,
    /// active phrase, if any
    pub active_phrase: Option<Phrase<F>>,
}

impl<const PHRASES: usize, F: FileHandler> Default for Sequence<PHRASES, F> {
    fn default() -> Self {
        Self {
            phrase_index: 0,
            phrases: heapless::Vec::new(),
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
        loop_div: f32,
        bank: &pads::Bank<PADS, STEPS>,
        kit_index: u8,
        kit_drift: f32,
        phrase_drift: f32,
        rand: &mut impl Rand,
        fs: &mut F,
    ) -> Result<Option<passive::Event>, Error<F::Error>> {
        if let Some(active_phrase) = self.active_phrase.as_mut() {
            let source_phrase = if let Some(source_phrase) = self
                .source_phrase
                .and_then(|v| bank.phrases[v as usize].as_ref())
            {
                if active_phrase.step_index >= active_phrase.step_count {
                    if let Some(source_phrase) = Self::try_increment_phrase(
                        &mut self.phrase_index,
                        &self.phrases,
                        &mut self.source_phrase,
                        bank,
                        phrase_drift,
                        rand,
                    ) {
                        // incremented phrase
                        active_phrase.step_count = source_phrase.len;
                        active_phrase.step_index %= active_phrase.step_count;
                        source_phrase
                    } else {
                        self.active_phrase = None;
                        return Ok(None);
                    }
                } else {
                    // increment step
                    active_phrase.step_index += 1;
                    source_phrase
                }
            } else if let Some(source_phrase) = Self::try_increment_phrase(
                &mut self.phrase_index,
                &self.phrases,
                &mut self.source_phrase,
                bank,
                phrase_drift,
                rand,
            ) {
                // incremented phrase
                active_phrase.step_count = source_phrase.len;
                active_phrase.step_index %= active_phrase.step_count;
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
                    .trans(event, bank, kit_index, kit_drift, rand, fs)?;
                return Ok(Some(*event));
            } else {
                active_phrase.active.tick(xor_reverse, loop_div);
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
                event.trans(source, bank, kit_index, kit_drift, rand, fs)?;
                Some(*source)
            } else {
                None
            };
            self.active_phrase = Some(Phrase {
                step_index: 0,
                step_count: source_phrase.len,
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

    /// associated method to appease borrow rules
    fn try_increment_phrase<'d, const PADS: usize, const STEPS: usize>(
        phrase_index: &mut u16,
        phrases: &heapless::Vec<u8, PHRASES>,
        source_phrase: &mut Option<u8>,
        bank: &'d pads::Bank<PADS, STEPS>,
        phrase_drift: f32,
        rand: &mut impl Rand,
    ) -> Option<&'d passive::Phrase<STEPS>> {
        // try increment phrase
        let phrase_count = phrases
            .iter()
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
                    .iter()
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

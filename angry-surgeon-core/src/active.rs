//! stateful data types

use crate::{pads, passive, FileHandler};
use core::mem::MaybeUninit;
use embedded_io_async::{Read, Seek, Write};
use heapless::{Deque, Vec};
use tinyrand::Rand;

pub struct Wav<IO: Read + Write + Seek> {
    pub tempo: Option<f32>,
    pub steps: Option<u16>,
    pub file: IO,
    pub len: u64,
}

impl<IO: Read + Write + Seek> Wav<IO> {
    pub async fn pos(&mut self) -> Result<u64, IO::Error> {
        Ok(self.file.stream_position().await? - 44)
    }

    pub async fn seek(&mut self, offset: i64) -> Result<(), IO::Error> {
        self.file
            .seek(embedded_io_async::SeekFrom::Start(
                44 + (offset.rem_euclid(self.len as i64) as u64),
            ))
            .await?;
        Ok(())
    }
}

pub struct Onset<IO: Read + Write + Seek> {
    /// source onset index
    pub index: u8,
    pub pan: f32,
    pub wav: Wav<IO>,
    pub start: u64,
}

pub enum Event<IO: Read + Write + Seek> {
    Sync,
    Hold(Onset<IO>, u16),
    Loop(Onset<IO>, u16, crate::Fraction),
}

impl<IO: Read + Write + Seek> Event<IO> {
    pub async fn trans<const PADS: usize, const N: usize>(
        &mut self,
        input: &passive::Event,
        step: u16,
        bias: f32,
        rand: &mut impl Rand,
        kit: &pads::Kit<PADS, N>,
        fs: &mut impl FileHandler<File = IO>,
    ) -> Result<(), IO::Error> {
        match input {
            passive::Event::Sync => {
                *self = Event::Sync;
            }
            passive::Event::Hold { index } => {
                if let Event::Loop(onset, ..) = self {
                    // recast event variant with same Onset
                    let uninit: &mut MaybeUninit<Onset<IO>> =
                        unsafe { core::mem::transmute(onset) };
                    let mut onset =
                        unsafe { core::mem::replace(uninit, MaybeUninit::uninit()).assume_init() };
                    // i don't know either, girl
                    onset.wav.file = fs.try_clone(&onset.wav.file).await?;
                    *self = Event::Hold(onset, step);
                } else if let Some(alt) = kit.generate_alt(*index, bias, rand) {
                    let onset = kit
                        .onset_seek(*index, alt, pads::Kit::<PADS, N>::generate_pan(*index), fs)
                        .await?;
                    *self = Event::Hold(onset, step);
                }
            }
            passive::Event::Loop { index, len } => {
                match self {
                    Event::Hold(onset, step) | Event::Loop(onset, step, ..)
                        if onset.index == *index =>
                    {
                        // recast event variant with same Onset
                        let uninit: &mut MaybeUninit<Onset<IO>> =
                            unsafe { core::mem::transmute(onset) };
                        let mut onset = unsafe {
                            core::mem::replace(uninit, MaybeUninit::uninit()).assume_init()
                        };
                        // i don't know either, girl
                        onset.wav.file = fs.try_clone(&onset.wav.file).await?;
                        *self = Event::Loop(onset, *step, *len);
                    }
                    _ => {
                        if let Some(alt) = kit.generate_alt(*index, bias, rand) {
                            let onset = kit
                                .onset(*index, alt, pads::Kit::<PADS, N>::generate_pan(*index), fs)
                                .await?;
                            *self = Event::Loop(onset, step, *len);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

pub struct Input<IO: Read + Write + Seek> {
    pub active: Event<IO>,
    pub buffer: Option<passive::Event>,
}

impl<IO: Read + Write + Seek> Input<IO> {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            active: Event::Sync,
            buffer: None,
        }
    }
}

pub struct Phrase<IO: Read + Write + Seek> {
    /// next event index (sans drift)
    pub next: usize,
    /// remaining steps in event
    pub event_rem: u16,
    /// remaining steps in phrase
    pub phrase_rem: u16,
    /// active event (last consumed)
    pub active: Event<IO>,
}

pub struct Record<const STEPS: usize, IO: Read + Write + Seek> {
    /// running bounded event queue
    events: Deque<passive::Stamped, STEPS>,
    /// baked events
    buffer: Vec<passive::Stamped, STEPS>,
    /// trimmed source phrase, if any
    pub phrase: Option<passive::Phrase<STEPS>>,
    /// active phrase, if any
    pub active: Option<Phrase<IO>>,
}

impl<const STEPS: usize, IO: Read + Write + Seek> Record<STEPS, IO> {
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

    pub async fn generate_phrase<const PADS: usize>(
        &mut self,
        step: u16,
        bias: f32,
        drift: f32,
        rand: &mut impl Rand,
        kit: &pads::Kit<PADS, STEPS>,
        fs: &mut impl FileHandler<File = IO>,
    ) -> Result<(), IO::Error> {
        if let Some(phrase) = self.phrase.as_mut() {
            if let Some(phrase) = phrase
                .generate_active(&mut self.active, step, bias, drift, rand, kit, fs)
                .await?
            {
                self.active = Some(phrase);
            }
        }
        Ok(())
    }

    pub fn take(&mut self) -> Option<(passive::Phrase<STEPS>, Phrase<IO>)> {
        self.buffer.clear();
        Some((self.phrase.take()?, self.active.take()?))
    }
}

pub struct Pool<IO: Read + Write + Seek> {
    /// next phrase index (sans drift)
    pub next: usize,
    /// base phrase sequence
    pub phrases: Vec<u8, { crate::MAX_POOL_LEN }>,
    /// pad index of source phrase, if any
    pub index: Option<u8>,
    /// active phrase, if any
    pub active: Option<Phrase<IO>>,
}

impl<IO: Read + Write + Seek> Pool<IO> {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            next: 0,
            phrases: Vec::new(),
            index: None,
            active: None,
        }
    }

    pub async fn generate_phrase<const PADS: usize, const N: usize>(
        &mut self,
        step: u16,
        bias: f32,
        drift: f32,
        rand: &mut impl Rand,
        kit: &pads::Kit<PADS, N>,
        fs: &mut impl FileHandler<File = IO>,
    ) -> Result<(), IO::Error> {
        if self.phrases.is_empty() {
            self.next = 0;
            self.active = None;
        } else {
            let index = {
                // FIXME: use independent phrase_drift instead of same "stamped_drift"?
                let drift = rand.next_lim_usize(
                    ((drift * self.phrases.len() as f32 - 1.).round()) as usize + 1,
                );
                let index = (self.next + drift) % self.phrases.len();
                self.phrases[index]
            };
            self.index = Some(index);
            if let Some(phrase) = &kit.pads[index as usize].phrase {
                if let Some(phrase) = phrase
                    .generate_active(&mut self.active, step, bias, drift, rand, kit, fs)
                    .await?
                {
                    self.active = Some(phrase);
                }
            }
            self.next = (self.next + 1) % self.phrases.len();
        }
        Ok(())
    }
}

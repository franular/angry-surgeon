//! main logic-to-audio driver

use crate::{active, passive, Error, FileHandler};
use embedded_io::ReadExactError;
use tinyrand::Rand;

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use micromath::F32Ext;

/// upcoming source events; consumes input event buffer
macro_rules! sources {
    ($bank_hdlr:expr) => {
        [
            &Some($bank_hdlr.input.buffer),
            &$bank_hdlr.record.next_step(),
            &$bank_hdlr.sequence.next_step(&$bank_hdlr.bank),
        ]
    };
}

macro_rules! actives {
    (ref $bank_hdlr:expr) => {
        [
            Some(&$bank_hdlr.input.active),
            $bank_hdlr.record.active_phrase.as_ref().map(|v| &v.active),
            $bank_hdlr
                .sequence
                .active_phrase
                .as_ref()
                .map(|v| &v.active),
        ]
    };
    (mut $bank_hdlr:expr) => {
        [
            Some(&mut $bank_hdlr.input.active),
            $bank_hdlr
                .record
                .active_phrase
                .as_mut()
                .map(|v| &mut v.active),
            $bank_hdlr
                .sequence
                .active_phrase
                .as_mut()
                .map(|v| &mut v.active),
        ]
    };
}

macro_rules! active_from {
    ($bank_hdlr:expr) => {
        if let Some(event) = actives!($bank_hdlr)
            .into_iter()
            .flat_map(|v| v.and_then(|v| v.non_sync()))
            .next()
        {
            event
        } else {
            &mut active::Event::Sync
        }
    };
}

struct GrainReader<const MAX_LEN: usize> {
    /// sample buffer
    samples: [i16; MAX_LEN],
    /// i16 sample index
    index: f32,
    bounds: core::ops::Range<usize>,
}

impl<const MAX_LEN: usize> GrainReader<MAX_LEN> {
    fn new() -> Self {
        Self {
            samples: [0; MAX_LEN],
            index: 0.,
            bounds: 0..0,
        }
    }

    fn is_zero_xing(sample_a: i16, sample_b: i16) -> bool {
        sample_a.signum() != sample_b.signum()
    }

    // refill buffer; return pre-read position
    fn fill<F: FileHandler>(
        &mut self,
        wav: &mut active::Wav<F>,
        fs: &mut F,
    ) -> Result<u64, F::Error> {
        wav.flush_seek(fs)?;
        let init_pos = wav.pos(fs)?;

        let bytes = bytemuck::cast_slice_mut(&mut self.samples[..]);
        wav.read(bytes, fs)?;
        // find zero xings about new grain
        let start = self
            .samples
            .windows(2)
            .enumerate()
            .find(|(_, w)| Self::is_zero_xing(w[0], w[1]))
            .map(|(i, _)| i)
            .unwrap_or(0);
        let end = self
            .samples
            .windows(2)
            .enumerate()
            .rev()
            .find(|(_, w)| Self::is_zero_xing(w[0], w[1]))
            .map(|(i, _)| i)
            .unwrap_or(MAX_LEN - 1);
        self.bounds = start..end;

        Ok(init_pos)
    }

    fn read_interpolated<F: FileHandler>(
        &mut self,
        stretch: f32,
        pitch: f32,
        reverse: bool,
        wav: &mut active::Wav<F>,
        fs: &mut F,
    ) -> Result<f32, F::Error> {
        let init_bounds = self.bounds.clone();

        if self.bounds.is_empty() || self.index >= self.bounds.end as f32 {
            let init_pos = self.fill(wav, fs)?;
            wav.force_seek(
                init_pos as i64 + (self.bounds.len() as f32 * stretch) as i64 * 2,
                fs,
            )?;
            self.index = self.index - init_bounds.end as f32 + self.bounds.start as f32;
        } else if self.index < self.bounds.start as f32 {
            let init_pos = self.fill(wav, fs)?;
            wav.force_seek(
                init_pos as i64 - (self.bounds.len() as f32 * stretch) as i64 * 2,
                fs,
            )?;
            self.index = self.index - init_bounds.start as f32 + self.bounds.end as f32;
        }
        // read sample with linear interpolation
        let word_a =
            self.samples[self.index as usize] as f32 / i16::MAX as f32 * (1. - self.index.fract());
        let word_b =
            self.samples[self.index as usize + 1] as f32 / i16::MAX as f32 * self.index.fract();

        if reverse {
            self.index -= pitch;
        } else {
            self.index += pitch;
        }

        Ok(word_a + word_b)
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Kit<const PADS: usize> {
    #[serde(with = "serde_arrays")]
    onsets: [Option<passive::Onset>; PADS],
}

impl<const PADS: usize> Default for Kit<PADS> {
    fn default() -> Self {
        Self {
            onsets: core::array::from_fn(|_| None),
        }
    }
}

impl<const PADS: usize> Kit<PADS> {
    pub fn generate_pan(index: impl Into<usize>) -> f32 {
        index.into() as f32 / PADS as f32 - 0.5
    }

    pub fn onset<F: FileHandler>(
        &self,
        to_close: Option<&F::File>,
        index: u8,
        pan: f32,
        fs: &mut F,
    ) -> Result<Option<active::Onset<F>>, Error<F::Error>> {
        if let Some(source) = self.onsets[index as usize].as_ref() {
            Ok(Some(Self::onset_inner(source, to_close, index, pan, fs)?))
        } else {
            Ok(None)
        }
    }

    pub fn onset_seek<F: FileHandler>(
        &self,
        to_close: Option<&F::File>,
        index: u8,
        pan: f32,
        fs: &mut F,
    ) -> Result<Option<active::Onset<F>>, Error<F::Error>> {
        if let Some(source) = self.onsets[index as usize].as_ref() {
            let mut onset = Self::onset_inner(source, to_close, index, pan, fs)?;
            onset.wav.force_seek(source.start as i64 * 2, fs)?;
            Ok(Some(onset))
        } else {
            Ok(None)
        }
    }

    fn onset_inner<F: FileHandler>(
        source: &passive::Onset,
        to_close: Option<&F::File>,
        index: u8,
        pan: f32,
        fs: &mut F,
    ) -> Result<active::Onset<F>, Error<F::Error>> {
        if let Some(file) = to_close {
            fs.close(file)?;
        }
        let mut file = fs.open(&source.wav.path)?;
        // parse wav looking for `data` subchunk
        let (pcm_start, pcm_len) = loop {
            let mut id = [0u8; 4];
            fs.read_exact(&mut file, &mut id).map_err(|e| match e {
                ReadExactError::UnexpectedEof => Error::DataNotFound,
                ReadExactError::Other(e) => Error::Other(e),
            })?;
            if &id[..] == b"RIFF" {
                fs.seek(&mut file, embedded_io::SeekFrom::Current(4))?;
                let mut data = [0u8; 4];
                fs.read_exact(&mut file, &mut data).map_err(|e| match e {
                    ReadExactError::UnexpectedEof => Error::DataNotFound,
                    ReadExactError::Other(e) => Error::Other(e),
                })?;
                if &data[..] != b"WAVE" {
                    return Err(Error::BadFormat);
                }
            } else if &id[..] == b"data" {
                let mut size = [0u8; 4];
                fs.read_exact(&mut file, &mut size).map_err(|e| match e {
                    ReadExactError::UnexpectedEof => Error::DataNotFound,
                    ReadExactError::Other(e) => Error::Other(e),
                })?;
                let pcm_start = fs.stream_position(&mut file)?;
                let pcm_len = u32::from_le_bytes(size) as u64;
                break (pcm_start, pcm_len);
            } else {
                let mut size = [0u8; 4];
                fs.read_exact(&mut file, &mut size).map_err(|e| match e {
                    ReadExactError::UnexpectedEof => Error::DataNotFound,
                    ReadExactError::Other(e) => Error::Other(e),
                })?;
                let chunk_len = u32::from_le_bytes(size) as i64;
                fs.seek(&mut file, embedded_io::SeekFrom::Current(chunk_len))?;
            }
        };
        let wav = active::Wav {
            tempo: source.wav.tempo,
            steps: source.wav.steps,
            file,
            pcm_start,
            pcm_len,
            seek_to: None,
        };
        Ok(active::Onset {
            index,
            pan,
            wav,
            start: source.start,
        })
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Bank<const PADS: usize, const STEPS: usize> {
    #[serde(with = "serde_arrays")]
    kits: [Option<Kit<PADS>>; PADS],
    #[serde(with = "serde_arrays")]
    pub phrases: [Option<passive::Phrase<STEPS>>; PADS],
}

impl<const PADS: usize, const STEPS: usize> Default for Bank<PADS, STEPS> {
    fn default() -> Self {
        Self {
            kits: core::array::from_fn(|_| None),
            phrases: core::array::from_fn(|_| None),
        }
    }
}

impl<const PADS: usize, const STEPS: usize> Bank<PADS, STEPS> {
    /// find first non-None kit, if any, at `drift` indices from base `index`
    pub fn generate_kit(
        &self,
        mut index: u8,
        drift: f32,
        rand: &mut impl Rand,
    ) -> Option<&Kit<PADS>> {
        if self.kits.iter().all(|v| v.is_none()) {
            return None;
        }
        let drift = drift * self.kits.len() as f32;
        let mut drift = rand.next_lim_usize(drift as usize + 1)
            + rand.next_bool(tinyrand::Probability::new(drift.fract() as f64)) as usize;
        loop {
            while self.kits[index as usize].is_none() {
                index = (index + 1) % self.kits.len() as u8;
            }
            if drift == 0 {
                return self.kits[index as usize].as_ref();
            }
            drift -= 1;
            index += 1;
        }
    }
}

pub struct Mod<T: Copy + core::ops::Mul> {
    pub base: T,
    pub offset: T,
}

impl<T: Copy + core::ops::Mul> Mod<T> {
    pub fn new(base: T, offset: T) -> Self {
        Self { base, offset }
    }

    pub fn net(&self) -> T::Output {
        self.base * self.offset
    }
}

pub struct BankHandler<const PADS: usize, const STEPS: usize, const PHRASES: usize, F: FileHandler>
{
    quant: bool,
    tempo: f32,
    step_div: u16,
    pub loop_div: Mod<f32>,

    pub gain: f32,
    pub width: f32,
    pub pitch: Mod<f32>,

    pub bank: Bank<PADS, STEPS>,
    pub kit_index: u8,
    pub kit_drift: f32,
    pub phrase_drift: f32,

    input: active::Input<F>,
    record: active::Record<STEPS, F>,
    sequence: active::Sequence<PHRASES, F>,
    reader: GrainReader<{ crate::GRAIN_LEN }>,
}

impl<const PADS: usize, const STEPS: usize, const PHRASES: usize, F: FileHandler>
    BankHandler<PADS, STEPS, PHRASES, F>
{
    fn new(step_div: u16) -> Self {
        Self {
            quant: false,
            tempo: 0.,
            step_div,
            loop_div: Mod::new(8., 1.),

            gain: 0.5,
            width: 0.5,
            pitch: Mod::new(1., 1.),

            bank: Bank::default(),
            kit_index: 0,
            kit_drift: 0.,
            phrase_drift: 0.,

            input: active::Input::default(),
            record: active::Record::default(),
            sequence: active::Sequence::default(),
            reader: GrainReader::new(),
        }
    }

    pub fn read_attenuated<T: core::ops::AddAssign + From<f32>>(
        &mut self,
        fs: &mut F,
        buffer: &mut [T],
        channels: usize,
        sample_rate: u16,
    ) -> Result<(), F::Error> {
        let reverse = self.reverse();
        let event = if let Some(event) = actives!(mut self)
            .into_iter()
            .find_map(|v| v.and_then(|v| v.non_sync()))
        {
            event
        } else {
            &mut active::Event::Sync
        };

        if let active::Event::Hold { onset, .. } = event {
            Self::read_grain::<T>(
                self.tempo,
                self.step_div,
                self.gain,
                self.width,
                self.pitch.net(),
                reverse,
                onset,
                &mut self.reader,
                fs,
                buffer,
                channels,
            )
        } else if let active::Event::Loop { onset, len, .. } = event {
            let wav = &mut onset.wav;
            if self.tempo > 0. {
                // all in bytes
                let pos = wav.pos(fs)?;
                let start = onset.start * 2;
                let len = if let Some(steps) = wav.steps {
                    (*len as f32 / self.loop_div.net() * wav.pcm_len as f32 / steps as f32) as u64
                        & !1
                } else {
                    (*len as f32 / self.loop_div.net() * sample_rate as f32 * 60. / self.tempo
                        * self.loop_div.net()) as u64
                        * 2
                };
                let end = start + len;
                if pos > end || pos < start && pos + wav.pcm_len > end {
                    // always loop over len/loop_div steps **after** onset
                    if reverse {
                        wav.push_seek(end as i64);
                    } else {
                        wav.push_seek(start as i64);
                    }
                }
            }
            Self::read_grain::<T>(
                self.tempo,
                self.step_div,
                self.gain,
                self.width,
                self.pitch.net(),
                reverse,
                onset,
                &mut self.reader,
                fs,
                buffer,
                channels,
            )
        } else {
            Ok(())
        }
    }

    fn read_grain<T: core::ops::AddAssign + From<f32>>(
        tempo: f32,
        step_div: u16,
        gain: f32,
        width: f32,
        pitch: f32,
        reverse: bool,
        onset: &mut active::Onset<F>,
        reader: &mut GrainReader<{ crate::GRAIN_LEN }>,
        fs: &mut F,
        buffer: &mut [T],
        channels: usize,
    ) -> Result<(), F::Error> {
        let stretch = onset
            .wav
            .tempo
            .map(|v| tempo * step_div as f32 / v / pitch)
            .unwrap_or(1.);
        // FIXME: support alternative channel counts?
        assert!(channels == 2);
        for i in 0..buffer.len() / channels {
            let sample = reader.read_interpolated(stretch, pitch, reverse, &mut onset.wav, fs)?;
            let l = sample * (1. + width * ((onset.pan - 0.5).abs() - 1.)) * gain;
            let r = sample * (1. + width * ((onset.pan + 0.5).abs() - 1.)) * gain;
            buffer[i * channels] += T::from(l);
            buffer[i * channels + 1] += T::from(r);
        }
        Ok(())
    }

    pub fn assign_onset(&mut self, pad_index: u8, onset: passive::Onset) {
        self.bank.kits[self.kit_index as usize]
            .get_or_insert_default()
            .onsets[pad_index as usize] = Some(onset);
    }

    fn tick(&mut self, rand: &mut impl Rand, fs: &mut F) -> Result<(), Error<F::Error>> {
        self.quant = true;
        let input_event = self
            .input
            .tick(&self.bank, self.kit_index, self.kit_drift, rand, fs)?;
        let record_event = self.record.tick(
            self.input.active.reverse,
            &self.bank,
            self.kit_index,
            self.kit_drift,
            self.phrase_drift,
            rand,
            fs,
        )?;
        let sequence_event = self.sequence.tick(
            self.input.active.reverse,
            &self.bank,
            self.kit_index,
            self.kit_drift,
            self.phrase_drift,
            rand,
            fs,
        )?;
        let event = input_event.or(record_event).or(sequence_event);
        self.record.push(passive::Step {
            event,
            reverse: self.reverse(),
        });
        for active in actives!(mut self).into_iter().flatten() {
            // sync all actives with clock
            match &mut active.event {
                active::Event::Sync => (),
                active::Event::Hold { onset, tick } => {
                    let wav = &mut onset.wav;
                    if let Some(steps) = wav.steps {
                        let offset = (wav.pcm_len as f32 / steps as f32 * *tick as f32) as i64 & !1;
                        wav.push_seek(onset.start as i64 * 2 + offset);
                    }
                }
                active::Event::Loop { onset, tick, len } => {
                    let wav = &mut onset.wav;
                    if let Some(steps) = wav.steps {
                        let offset = (wav.pcm_len as f32 / steps as f32
                            * (*tick as f32).rem_euclid(*len as f32 / self.loop_div.net()))
                            as i64
                            & !1;
                        wav.push_seek(onset.start as i64 * 2 + offset);
                    }
                }
            }
        }
        Ok(())
    }

    fn stop(&mut self) {
        todo!()
    }

    pub fn push_reverse(&mut self, reverse: bool) {
        self.input.buffer.reverse = reverse;
    }

    pub fn force_event(
        &mut self,
        event: passive::Event,
        rand: &mut impl Rand,
        fs: &mut F,
    ) -> Result<(), Error<F::Error>> {
        self.input.active.event.trans(
            &event,
            &self.bank,
            self.kit_index,
            self.kit_drift,
            rand,
            fs,
        )?;
        Ok(())
    }

    fn reverse(&self) -> bool {
        self.input.active.reverse
            ^ self
                .record
                .active_phrase
                .as_ref()
                .map(|v| v.active.reverse)
                .or(self
                    .sequence
                    .active_phrase
                    .as_ref()
                    .map(|v| v.active.reverse))
                .unwrap_or_default()
    }
}

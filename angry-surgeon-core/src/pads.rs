//! main logic-to-audio driver

use crate::{active, passive, Error, FileHandler};
use embedded_io::ReadExactError;
use tinyrand::Rand;

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use micromath::F32Ext;

macro_rules! actives_mut {
    ($bank_hdlr:expr) => {
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

/// grain length in frames
pub const GRAIN_LEN: usize = 1024;
/// crossfade length in frames
const FADE_LEN: usize = 256;

#[derive(PartialEq)]
enum FadeState {
    None,
    Primed,
    Fading,
}

struct Fade {
    buffer: [i16; FADE_LEN + 1],
    state: FadeState,
}

impl Fade {
    fn new() -> Self {
        Self {
            buffer: [0; FADE_LEN + 1],
            state: FadeState::None,
        }
    }
}

pub(crate) struct GrainReader {
    buffer: [i16; GRAIN_LEN + 1], // +1 frame for interpolation
    window: [f32; FADE_LEN + 1], // for crossfade
    tail: Fade,
    head: Fade,
    index: f32,
}

impl GrainReader {
    fn new() -> Self {
        let window = core::array::from_fn(|i| {
            0.5 - 0.5 * f32::cos(core::f32::consts::PI * i as f32 / FADE_LEN as f32)
        });
        Self {
            buffer: [0; GRAIN_LEN + 1],
            window,
            tail: Fade::new(),
            head: Fade::new(),
            index: 0.,
        }
    }

    pub fn fade<F: FileHandler>(
        &mut self,
        wav: Option<&mut active::Wav<F>>,
        fs: &mut F,
    ) -> Result<(), F::Error> {
        Self::fade_inner(
            &mut self.tail,
            &mut self.head,
            wav,
            fs,
        )
    }

    fn fade_inner<F: FileHandler>(
        tail: &mut Fade,
        head: &mut Fade,
        wav: Option<&mut active::Wav<F>>,
        fs: &mut F,
    ) -> Result<(), F::Error> {
        if let Some(wav) = wav {
            let end_pos = wav.pos(fs)?;
            if tail.state == FadeState::None {
                tail.state = FadeState::Primed;
                let bytes = bytemuck::cast_slice_mut(&mut tail.buffer);
                wav.read(bytes, fs)?;
            }
            wav.seek(
                end_pos as i64 - GRAIN_LEN as i64 * 2 - FADE_LEN as i64 * 2,
                fs,
            )?;
            if head.state == FadeState::None {
                head.state = FadeState::Primed;
                let bytes = bytemuck::cast_slice_mut(&mut head.buffer);
                wav.read(bytes, fs)?;
            }
            wav.seek(end_pos as i64, fs)?; // this is probably redundant
        } else {
            if tail.state == FadeState::None {
                tail.state = FadeState::Primed;
                tail.buffer.fill(0);
            }
            if head.state == FadeState::None {
                head.state = FadeState::Primed;
                head.buffer.fill(0);
            }
        }
        Ok(())
    }

    /// looping read with crossfade at eof
    fn fill<F: FileHandler>(
        &mut self,
        wav: &mut active::Wav<F>,
        fs: &mut F,
    ) -> Result<(), F::Error> {
        let mut slice = bytemuck::cast_slice_mut(&mut self.buffer[..]);
        while !slice.is_empty() {
            let len = slice.len().min((wav.pcm_len - wav.pos(fs)?) as usize);
            let n = fs.read(&mut wav.file, &mut slice[..len])?;
            if n == 0 {
                // rewind to start/end with crossfade
                Self::fade_inner(
                    &mut self.tail,
                    &mut self.head,
                    Some(wav),
                    fs,
                )?;
                wav.seek(0, fs)?;
            }
            slice = &mut slice[n..];
        }
        Ok(())
    }

    fn sample(&mut self, index: usize) -> f32 {
        if self.tail.state == FadeState::Fading {
            if index < FADE_LEN {
                return self.buffer[index] as f32 / i16::MAX as f32 * self.window[index]
                    + self.tail.buffer[index] as f32 / i16::MAX as f32 * (1. - self.window[index]);
            }
            self.tail.state = FadeState::None;
        }
        if self.head.state == FadeState::Fading {
            if index >= GRAIN_LEN - FADE_LEN {
                let transposed = index + FADE_LEN - GRAIN_LEN;
                return self.buffer[index] as f32 / i16::MAX as f32 * (1. - self.window[transposed])
                    + self.head.buffer[transposed] as f32 / i16::MAX as f32 * (self.window[transposed]);
            }
            self.head.state = FadeState::None;
        }
        self.buffer[index] as f32 / i16::MAX as f32
    }

    fn read_interpolated<F: FileHandler>(
        &mut self,
        speed: f32,
        reverse: bool,
        len: Option<f32>,
        onset: &mut active::Onset<F>,
        fs: &mut F,
    ) -> Result<f32, F::Error> {
        let wav = &mut onset.wav;
        // handle loop
        if let (Some(len), Some(steps)) = (len, wav.steps) {
            // all in bytes
            let pos = wav.pos(fs)?;
            let start = onset.start * 2;
            let len = (len * wav.pcm_len as f32 / steps as f32) as u64 & !1;
            let end = start + len;
            if pos > end || pos < start && pos + wav.pcm_len > end {
                Self::fade_inner(
                    &mut self.tail,
                    &mut self.head,
                    Some(wav),
                    fs,
                )?;
                // always loop over len/loop_div steps **after** onset
                if reverse {
                    wav.seek(end as i64, fs)?;
                } else {
                    wav.seek(start as i64, fs)?;
                }
            }
        }
        // handle grain refill
        if self.index as i64 >= GRAIN_LEN as i64 {
            let seek_to = wav.pos(fs)? as i64 + GRAIN_LEN as i64 * 2;
            self.fill(wav, fs)?;
            wav.seek(seek_to, fs)?;
            if self.tail.state == FadeState::Primed {
                self.tail.state = FadeState::Fading;
                self.head.state = FadeState::None;
            }
            // wrap to [0, GRAIN_LEN)
            self.index %= GRAIN_LEN as f32;
        } else if (self.index as i64) < 0 {
            let seek_to = wav.pos(fs)? as i64 - GRAIN_LEN as i64 * 2;
            wav.seek(seek_to, fs)?; // seek here so start of an onset is sought back from
            self.fill(wav, fs)?;
            wav.seek(seek_to, fs)?;
            if self.head.state == FadeState::Primed {
                self.head.state = FadeState::Fading;
                self.tail.state = FadeState::None;
            }
            // wrap to [0, GRAIN_LEN)
            self.index = self.index.rem_euclid(GRAIN_LEN as f32);
        }
        // linear interpolation
        // let word_a = self.sample(self.index as usize + 1);
        // let word_b = 0.;
        let word_a = self.sample(self.index as usize) * (1. - self.index.fract());
        let word_b = self.sample(self.index as usize + 1) * self.index.fract();
        if reverse {
            self.index -= speed;
        } else {
            self.index += speed;
        }
        Ok(word_a + word_b)
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Kit<const PADS: usize> {
    #[serde(with = "serde_arrays")]
    pub onsets: [Option<passive::Onset>; PADS],
}

impl<const PADS: usize> Default for Kit<PADS> {
    fn default() -> Self {
        Self {
            onsets: core::array::from_fn(|_| None),
        }
    }
}

impl<const PADS: usize> Kit<PADS> {
    pub(crate) fn generate_pan(index: impl Into<usize>) -> f32 {
        index.into() as f32 / PADS as f32 - 0.5
    }

    pub(crate) fn onset_seek<F: FileHandler>(
        &self,
        to_close: Option<&F::File>,
        index: u8,
        pan: f32,
        fs: &mut F,
    ) -> Result<Option<active::Onset<F>>, Error<F::Error>> {
        if let Some(source) = self.onsets[index as usize].as_ref() {
            let mut onset = Self::onset_inner(source, to_close, index, pan, fs)?;
            onset.wav.seek(source.start as i64 * 2, fs)?;
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
        let re_err = |e| match e {
            ReadExactError::UnexpectedEof => Error::DataNotFound,
            ReadExactError::Other(e) => Error::Other(e),
        };
        let assert = |b: bool| if !b { Err(Error::BadFormat) } else { Ok(()) };
        // parse wav looking for metadata and `data` subchunk
        let mut pcm_start = 0;
        let mut pcm_len = 0;
        let mut sample_rate = 0;
        let mut essential_chunks_parsed = 0;
        while essential_chunks_parsed < 3 {
            let mut id = [0u8; 4];
            fs.read_exact(&mut file, &mut id).map_err(re_err)?;
            if &id[..] == b"RIFF" {
                fs.seek(&mut file, embedded_io::SeekFrom::Current(4))?;
                let mut data = [0u8; 4];
                fs.read_exact(&mut file, &mut data).map_err(re_err)?;
                assert(&data[..] == b"WAVE")?;
                essential_chunks_parsed += 1;
            } else if &id[..] == b"fmt " {
                let mut data32 = [0u8; 4];
                let mut data16 = [0u8; 2];
                fs.read_exact(&mut file, &mut data32).map_err(re_err)?;
                assert(u32::from_le_bytes(data32) == 16)?; // `fmt ` chunk size
                fs.read_exact(&mut file, &mut data16).map_err(re_err)?;
                assert(u16::from_le_bytes(data16) == 1)?; // pcm integer format
                fs.read_exact(&mut file, &mut data16).map_err(re_err)?;
                assert(u16::from_le_bytes(data16) == 1)?; // 1 channel
                fs.read_exact(&mut file, &mut data32).map_err(re_err)?;
                sample_rate = u32::from_le_bytes(data32);
                fs.seek(&mut file, embedded_io::SeekFrom::Current(6))?;
                fs.read_exact(&mut file, &mut data16).map_err(re_err)?;
                assert(u16::from_le_bytes(data16) == 16)?; // 16 bits/sample
                essential_chunks_parsed += 1;
            } else if &id[..] == b"data" {
                let mut size = [0u8; 4];
                fs.read_exact(&mut file, &mut size).map_err(re_err)?;
                pcm_start = fs.stream_position(&mut file)?;
                pcm_len = u32::from_le_bytes(size) as u64;
                essential_chunks_parsed += 1;
            } else {
                let mut size = [0u8; 4];
                fs.read_exact(&mut file, &mut size).map_err(re_err)?;
                let chunk_len = u32::from_le_bytes(size) as i64;
                fs.seek(&mut file, embedded_io::SeekFrom::Current(chunk_len))?;
            }
        }
        let wav = active::Wav {
            steps: source.wav.steps,
            file,
            pcm_start,
            pcm_len,
            sample_rate,
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
pub struct Bank<const PADS: usize, const STEPS: usize> {
    #[serde(with = "serde_arrays")]
    pub kits: [Option<Kit<PADS>>; PADS],
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
    pub(crate) fn generate_kit(
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
    ticks_per_step: u16,
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
    grain: GrainReader,
}

impl<const PADS: usize, const STEPS: usize, const PHRASES: usize, F: FileHandler>
    BankHandler<PADS, STEPS, PHRASES, F>
{
    fn new(ticks_per_step: u16) -> Self {
        Self {
            quant: false,
            tempo: 0.,
            ticks_per_step,
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
            grain: GrainReader::new(),
        }
    }

    pub fn assign_onset(&mut self, pad_index: u8, onset: passive::Onset) {
        self.bank.kits[self.kit_index as usize]
            .get_or_insert_default()
            .onsets[pad_index as usize] = Some(onset);
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
            &mut self.grain,
            rand,
            fs,
        )?;
        Ok(())
    }

    pub fn push_event(
        &mut self,
        event: passive::Event,
        rand: &mut impl Rand,
        fs: &mut F,
    ) -> Result<(), Error<F::Error>> {
        if self.quant {
            self.input.buffer.event = Some(event);
        } else {
            self.force_event(event, rand, fs)?;
        }
        Ok(())
    }

    pub fn push_reverse(&mut self, reverse: bool) {
        if self.quant {
            self.input.buffer.reverse = reverse;
        } else {
            self.input.active.reverse = reverse;
        }
    }

    pub fn trim_record(&mut self, len: u16) {
        self.record.trim(len);
    }

    pub fn take_record(&mut self, index: Option<u8>) {
        if let Some(source) = self.record.take() {
            if let Some(index) = index {
                self.bank.phrases[index as usize] = Some(source);
                self.sequence.clear();
                self.sequence.push(index);
            }
        }
    }

    pub fn clear_sequence(&mut self) {
        self.sequence.clear();
    }

    pub fn push_sequence(&mut self, index: u8) {
        self.sequence.push(index);
    }

    fn read_attenuated<T: core::ops::AddAssign + From<f32>>(
        &mut self,
        fs: &mut F,
        buffer: &mut [T],
        channels: usize,
        sample_rate: u32,
    ) -> Result<(), F::Error> {
        let reverse = self.reverse();
        let event = if let Some(event) = actives_mut!(self)
            .into_iter()
            .find_map(|v| v.and_then(|v| v.non_sync()))
        {
            event
        } else {
            &mut active::Event::Sync
        };

        let (len, onset) = match event {
            active::Event::Sync => (None, None),
            active::Event::Hold { onset, .. } => (None, Some(onset)),
            active::Event::Loop { onset, len, .. } => (Some(*len as f32 * self.ticks_per_step as f32 / self.loop_div.net()), Some(onset)),
        };
        let speed = if let Some(ref onset) = onset {
            self.pitch.net() * onset.wav.sample_rate as f32 / sample_rate as f32
        } else {
            self.pitch.net()
        };
        Self::read_grain::<T>(
            self.gain,
            self.width,
            speed,
            reverse,
            len,
            onset,
            &mut self.grain,
            fs,
            buffer,
            channels,
        )
    }

    /// associated method to appease borrow rules
    #[allow(clippy::too_many_arguments)]
    fn read_grain<T: core::ops::AddAssign + From<f32>>(
        gain: f32,
        width: f32,
        speed: f32,
        reverse: bool,
        len: Option<f32>,
        onset: Option<&mut active::Onset<F>>,
        grain: &mut GrainReader,
        fs: &mut F,
        buffer: &mut [T],
        channels: usize,
    ) -> Result<(), F::Error> {
        // FIXME: support alternative channel counts?
        assert!(channels == 2, "currently only stereo output is supported");
        // FIXME: play tails of sound with no onset active
        // requires maintainance of onset data with GrainReader.tail!head for sample
        // rate and pan (both of which should also be accounted for when fading
        // between samples anyhow)
        if let Some(onset) = onset {
            for i in 0..buffer.len() / channels {
                let sample = grain.read_interpolated(speed, reverse, len, onset, fs)?;
                let l = sample * (1. + width * ((onset.pan - 0.5).abs() - 1.)) * gain;
                let r = sample * (1. + width * ((onset.pan + 0.5).abs() - 1.)) * gain;
                buffer[i * channels] += T::from(l);
                buffer[i * channels + 1] += T::from(r);
            }
        }
        Ok(())
    }

    fn tick(&mut self, rand: &mut impl Rand, fs: &mut F) -> Result<(), Error<F::Error>> {
        self.quant = true;
        let input_event = self.input.tick(
            self.ticks_per_step,
            &self.bank,
            self.kit_index,
            self.kit_drift,
            &mut self.grain,
            rand,
            fs,
        )?;
        let record_event = self.record.tick(
            self.input.active.reverse,
            self.ticks_per_step,
            &self.bank,
            self.kit_index,
            self.kit_drift,
            self.phrase_drift,
            &mut self.grain,
            rand,
            fs,
        )?;
        let sequence_event = self.sequence.tick(
            self.input.active.reverse,
            self.ticks_per_step,
            &self.bank,
            self.kit_index,
            self.kit_drift,
            self.phrase_drift,
            &mut self.grain,
            rand,
            fs,
        )?;
        let event = input_event.or(record_event).or(sequence_event);
        self.record.push(passive::Step {
            event,
            reverse: self.reverse(),
        });
        if event.is_none() {
            // sync audible active, if any, with clock (with crossfade)
            if let Some(event) = actives_mut!(self)
                .into_iter()
                .find_map(|v| v.and_then(|v| v.non_sync()))
            {
                match event {
                    active::Event::Sync => unreachable!(),
                    active::Event::Hold { onset, tick } => {
                        let wav = &mut onset.wav;
                        if let Some(steps) = wav.steps {
                            self.grain.fade(Some(wav), fs)?;
                            let offset =
                                (wav.pcm_len as f32 / steps as f32 * *tick as f32) as i64 & !1;
                            wav.seek(onset.start as i64 * 2 + offset, fs)?;
                        }
                    }
                    active::Event::Loop { onset, tick, len } => {
                        let wav = &mut onset.wav;
                        if let Some(steps) = wav.steps {
                            self.grain.fade(Some(wav), fs)?;
                            let offset = (wav.pcm_len as f32 / steps as f32
                                * (*tick as f32).rem_euclid(
                                    *len as f32 * self.ticks_per_step as f32 / self.loop_div.net(),
                                )) as i64
                                & !1;
                            wav.seek(onset.start as i64 * 2 + offset, fs)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn stop(&mut self) {
        self.quant = false;
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

pub struct SystemHandler<
    const BANKS: usize,
    const PADS: usize,
    const STEPS: usize,
    const PHRASES: usize,
    R: Rand,
    F: FileHandler,
> {
    pub banks: [BankHandler<PADS, STEPS, PHRASES, F>; BANKS],
    pub rand: R,
    pub fs: F,
}

impl<
        const BANKS: usize,
        const PADS: usize,
        const STEPS: usize,
        const PHRASES: usize,
        R: Rand,
        F: FileHandler,
    > SystemHandler<BANKS, PADS, STEPS, PHRASES, R, F>
{
    pub fn new(ticks_per_step: u16, rand: R, fs: F) -> Self {
        Self {
            banks: core::array::from_fn(|_| BankHandler::new(ticks_per_step)),
            rand,
            fs,
        }
    }

    pub fn read_all<T: core::ops::AddAssign + From<f32>>(
        &mut self,
        buffer: &mut [T],
        channels: usize,
        sample_rate: u32,
    ) -> Result<(), Error<F::Error>> {
        for bank in self.banks.iter_mut() {
            bank.read_attenuated(&mut self.fs, buffer, channels, sample_rate)?;
        }
        Ok(())
    }

    pub fn tick(&mut self) -> Result<(), Error<F::Error>> {
        for bank in self.banks.iter_mut() {
            bank.tick(&mut self.rand, &mut self.fs)?;
        }
        Ok(())
    }

    pub fn stop(&mut self) {
        for bank in self.banks.iter_mut() {
            bank.stop();
        }
    }

    pub fn assign_tempo(&mut self, tempo: f32) {
        for bank in self.banks.iter_mut() {
            bank.tempo = tempo;
        }
    }
}

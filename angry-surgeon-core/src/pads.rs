//! main logic-to-audio driver

use crate::{active, passive, FileHandler, OpenError};
use embedded_io::ReadExactError;
use tinyrand::Rand;

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use micromath::F32Ext;

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
        let start = self.samples.windows(2).enumerate().find(|(_, w)| Self::is_zero_xing(w[0], w[1])).map(|(i, _)| i).unwrap_or(0);
        let end = self.samples.windows(2).enumerate().rev().find(|(_, w)| Self::is_zero_xing(w[0], w[1])).map(|(i, _)| i).unwrap_or(MAX_LEN - 1);
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
            wav.force_seek(init_pos as i64 + (self.bounds.len() as f32 * stretch) as i64 * 2, fs)?;
            self.index = self.index - init_bounds.end as f32 + self.bounds.start as f32;
        } else if self.index < self.bounds.start as f32 {
            let init_pos = self.fill(wav, fs)?;
            wav.force_seek(init_pos as i64 - (self.bounds.len() as f32 * stretch) as i64 * 2, fs)?;
            self.index = self.index - init_bounds.start as f32 + self.bounds.end as f32;
        }
        // read sample with linear interpolation
        let word_a = self.samples[self.index as usize] as f32 / i16::MAX as f32 * (1. - self.index.fract());
        let word_b = self.samples[self.index as usize + 1] as f32 / i16::MAX as f32 * self.index.fract();

        if reverse {
            self.index -= pitch;
        } else {
            self.index += pitch;
        }

        Ok(word_a + word_b)
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Kit<const PADS: usize> {
    #[serde(with = "serde_arrays")]
    pub onsets: [Option<passive::Onset>; PADS],
}

impl<const PADS: usize> Kit<PADS> {
    pub fn generate_pan(index: impl Into<usize>) -> f32 {
        index.into() as f32 / PADS as f32 - 0.5
    }

    pub fn onset<F: FileHandler>(
        &self,
        to_close: Option<&F::File>,
        index: impl Into<usize> + Copy,
        pan: f32,
        fs: &mut F,
    ) -> Result<Option<active::Onset<F>>, OpenError<F::Error>> {
        if let Some(source) = self.onsets[index.into()].as_ref() {
            Ok(Some(Self::onset_inner(source, to_close, index, pan, fs)?))
        } else {
            Ok(None)
        }
    }

    pub fn onset_seek<F: FileHandler>(
        &self,
        to_close: Option<&F::File>,
        index: impl Into<usize> + Copy,
        pan: f32,
        fs: &mut F,
    ) -> Result<Option<active::Onset<F>>, OpenError<F::Error>> {
        if let Some(source) = self.onsets[index.into()].as_ref() {
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
        index: impl Into<usize> + Copy,
        pan: f32,
        fs: &mut F,
    ) -> Result<active::Onset<F>, OpenError<F::Error>> {
        if let Some(file) = to_close {
            fs.close(file)?;
        }
        let mut file = fs.open(&source.wav.path)?;
        // parse wav looking for `data` subchunk
        let (pcm_start, pcm_len) = loop {
            let mut id = [0u8; 4];
            fs.read_exact(&mut file, &mut id).map_err(|e| match e {
                ReadExactError::UnexpectedEof => OpenError::DataNotFound,
                ReadExactError::Other(e) => OpenError::Other(e),
            })?;
            if &id[..] == b"RIFF" {
                fs.seek(&mut file, embedded_io::SeekFrom::Current(4))?;
                let mut data = [0u8; 4];
                fs.read_exact(&mut file, &mut data).map_err(|e| match e {
                    ReadExactError::UnexpectedEof => OpenError::DataNotFound,
                    ReadExactError::Other(e) => OpenError::Other(e),
                })?;
                if &data[..] != b"WAVE" {
                    return Err(OpenError::BadFormat);
                }
            } else if &id[..] == b"data" {
                let mut size = [0u8; 4];
                fs.read_exact(&mut file, &mut size).map_err(|e| match e {
                    ReadExactError::UnexpectedEof => OpenError::DataNotFound,
                    ReadExactError::Other(e) => OpenError::Other(e),
                })?;
                let pcm_start = fs.stream_position(&mut file)?;
                let pcm_len = u32::from_le_bytes(size) as u64;
                break (pcm_start, pcm_len);
            } else {
                let mut size = [0u8; 4];
                fs.read_exact(&mut file, &mut size).map_err(|e| match e {
                    ReadExactError::UnexpectedEof => OpenError::DataNotFound,
                    ReadExactError::Other(e) => OpenError::Other(e),
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
            index: index.into() as u8,
            pan,
            wav,
            start: source.start,
        })
    }
}

impl<const PADS: usize> Default for Kit<PADS> {
    fn default() -> Self {
        Self {
            onsets: core::array::from_fn(|_| None),
        }
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Bank<const PADS: usize, const STEPS: usize> {
    #[serde(with = "serde_arrays")]
    pub kits: [Option<Kit<PADS>>; PADS],
    #[serde(with = "serde_arrays")]
    pub phrases: [Option<passive::Phrase<STEPS>>; PADS],
}

impl<const PADS: usize, const STEPS: usize> Bank<PADS, STEPS> {
    /// find first non-None kit, if any, at `drift` indices from base `index`
    pub fn generate_kit(
        &self,
        mut index: usize,
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
            while self.kits[index].is_none() {
                index = (index + 1) % self.kits.len();
            }
            if drift == 0 {
                return self.kits[index].as_ref();
            }
            drift -= 1;
            index += 1;
        }
    }
}

impl<const PADS: usize, const STEPS: usize> Default for Bank<PADS, STEPS> {
    fn default() -> Self {
        Self {
            kits: core::array::from_fn(|_| None),
            phrases: core::array::from_fn(|_| None),
        }
    }
}

pub struct BankHandler<const PADS: usize, const STEPS: usize, const PHRASES: usize, F: FileHandler>
{
    quant: bool,
    clock: f32,
    tempo: f32,
    pub step_div: u16,

    pub gain: f32,
    pub width: f32,
    pub speed: Mod<f32>,
    pub loop_div: Mod<f32>,
    pub phrase_drift: f32,
    pub kit_drift: f32,

    pub kit_index: usize,
    pub bank: Bank<PADS, STEPS>,

    reverse: Option<f32>,
    input: active::Input<F>,
    record: active::Record<STEPS, F>,
    pool: active::Pool<PHRASES, F>,
    reader: GrainReader<{ crate::GRAIN_LEN }>,
}

impl<const PADS: usize, const STEPS: usize, const PHRASES: usize, F: FileHandler>
    BankHandler<PADS, STEPS, PHRASES, F>
{
    fn new(step_div: u16, loop_div: f32) -> Self {
        Self {
            quant: false,
            clock: 0.,
            tempo: 0.,
            step_div,

            gain: 1.,
            width: 1.,
            speed: Mod::new(1., 1.),
            loop_div: Mod::new(loop_div, 1.),
            phrase_drift: 0.,
            kit_drift: 0.,

            kit_index: 0,
            bank: Bank::default(),

            reverse: None,
            input: active::Input::new(),
            record: active::Record::new(),
            pool: active::Pool::new(),
            reader: GrainReader::new(),
        }
    }

    pub fn read_attenuated<const SAMPLE_RATE: u16, T: core::ops::AddAssign + From<f32>>(
        &mut self,
        fs: &mut F,
        buffer: &mut [T],
        channels: usize,
    ) -> Result<(), F::Error> {
        let active = if !matches!(self.input.active, active::Event::Sync) {
            &mut self.input.active
        } else if self
            .record
            .active
            .as_ref()
            .is_some_and(|v| !matches!(v.active, active::Event::Sync))
        {
            &mut self.record.active.as_mut().unwrap().active
        } else if self
            .pool
            .active
            .as_ref()
            .is_some_and(|v| !matches!(v.active, active::Event::Sync))
        {
            &mut self.pool.active.as_mut().unwrap().active
        } else {
            &mut active::Event::Sync
        };
        if let active::Event::Hold(onset, ..) = active {
            return Self::read_grain::<T>(
                self.tempo,
                self.step_div,
                self.gain,
                self.speed.net(),
                self.width,
                self.reverse.is_some(),
                onset,
                &mut self.reader,
                fs,
                buffer,
                channels,
            );
        } else if let active::Event::Loop(onset, _, num) = active {
            let wav = &mut onset.wav;
            if self.tempo > 0. {
                let pos = wav.pos(fs)?;
                let len = if let Some(steps) = wav.steps {
                    (*num as f32 / self.loop_div.net() * wav.pcm_len as f32 / steps as f32) as u64 & !1
                } else {
                    (*num as f32 / self.loop_div.net() * SAMPLE_RATE as f32 * 60. / self.tempo
                        * self.loop_div.net()) as u64
                        & !1
                };
                let end = onset.start * 2 + len;
                if pos > end || pos < onset.start && pos + wav.pcm_len > end {
                    if self.reverse.is_some() {
                        wav.push_seek(end as i64);
                    } else {
                        wav.push_seek(onset.start as i64 * 2);
                    }
                }
            }
            return Self::read_grain::<T>(
                self.tempo,
                self.step_div,
                self.gain,
                self.speed.net(),
                self.width,
                self.reverse.is_some(),
                onset,
                &mut self.reader,
                fs,
                buffer,
                channels,
            );
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn read_grain<T: core::ops::AddAssign + From<f32>>(
        tempo: f32,
        step_div: u16,
        gain: f32,
        speed: f32,
        width: f32,
        reverse: bool,
        onset: &mut active::Onset<F>,
        reader: &mut GrainReader<{ crate::GRAIN_LEN }>,
        fs: &mut F,
        buffer: &mut [T],
        channels: usize,
    ) -> Result<(), F::Error> {
        let stretch = onset.wav.tempo.map(|v| tempo * step_div as f32 / v / speed).unwrap_or(1.);
        // FIXME: support alternative channel counts?
        assert!(channels == 2);
        for i in 0..buffer.len() / channels {
            let sample = reader.read_interpolated(stretch, speed, reverse, &mut onset.wav, fs)?;
            let l = sample * (1. + width * ((onset.pan - 0.5).abs() - 1.)) * gain;
            let r = sample * (1. + width * ((onset.pan + 0.5).abs() - 1.)) * gain;
            buffer[i * channels] += T::from(l);
            buffer[i * channels + 1] += T::from(r);
        }
        Ok(())
    }

    pub fn assign_reverse(&mut self, reverse: bool) {
        if reverse {
            self.reverse = Some(self.clock);
        } else {
            self.reverse = None;
        }
    }

    pub fn assign_onset(
        &mut self,
        index: u8,
        onset: passive::Onset,
    ) {
        self.bank.kits[self.kit_index]
            .get_or_insert_default()
            .onsets[index as usize] = Some(onset);
    }

    pub fn clock(&mut self, fs: &mut F, rand: &mut impl Rand) -> Result<(), OpenError<F::Error>> {
        if let Some(input) = self.input.buffer.take() {
            self.process_input(fs, rand, input)?;
        } else {
            // sync all actives with clock
            let actives = [
                Some(&mut self.input.active),
                self.record.active.as_mut().map(|v| &mut v.active),
                self.pool.active.as_mut().map(|v| &mut v.active),
            ];
            for active in actives.into_iter().flatten() {
                match active {
                    active::Event::Hold(onset, step) => {
                        let wav = &mut onset.wav;
                        if let Some(steps) = wav.steps {
                            let clock = self.reverse.unwrap_or(self.clock);
                            let offset = (wav.pcm_len as f32 / steps as f32 * (clock - *step as f32))
                                as i64
                                & !1;
                            wav.push_seek(onset.start as i64 * 2 + offset);
                        }
                    }
                    active::Event::Loop(onset, step, num) => {
                        let wav = &mut onset.wav;
                        if let Some(steps) = wav.steps {
                            let clock = self.reverse.unwrap_or(self.clock);
                            let offset = (wav.pcm_len as f32 / steps as f32
                                * ((clock - *step as f32)
                                    .rem_euclid(*num as f32 / self.loop_div.net())))
                                as i64
                                & !1;
                            wav.push_seek(onset.start as i64 * 2 + offset);
                        }
                    }
                    _ => (),
                }
            }
        }
        self.tick_phrases(fs, rand)?;
        if let Some(clock) = self.reverse.as_mut() {
            *clock -= 1.;
        }
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(clock) = self.reverse.as_mut() {
            *clock = 0.;
        }
    }

    pub fn force_event(
        &mut self,
        fs: &mut F,
        rand: &mut impl Rand,
        event: passive::Event,
    ) -> Result<(), OpenError<F::Error>> {
        self.input.active.trans(
            &event,
            self.clock as u16,
            &self.bank,
            self.kit_index,
            self.kit_drift,
            rand,
            fs,
        )?;
        Ok(())
    }

    pub fn push_event(
        &mut self,
        fs: &mut F,
        rand: &mut impl Rand,
        event: passive::Event,
    ) -> Result<(), OpenError<F::Error>> {
        if self.quant {
            self.input.buffer = Some(event);
        } else {
            self.process_input(fs, rand, event)?;
        }
        Ok(())
    }

    pub fn take_record(&mut self, index: Option<u8>) {
        if let Some((phrase, active)) = self.record.take() {
            if let Some(index) = index {
                self.bank.phrases[index as usize] = Some(phrase);
                self.pool.next = 1;
                self.pool.phrases.clear();
                let _ = self.pool.phrases.push(index);
                self.pool.index = Some(index);
                self.pool.active = Some(active);
            }
        }
    }

    pub fn bake_record(
        &mut self,
        fs: &mut F,
        rand: &mut impl Rand,
        len: u16,
    ) -> Result<(), OpenError<F::Error>> {
        if self.record.active.is_none() {
            self.record.bake(self.clock as u16);
        }
        self.record.trim(len);
        self.record.generate_phrase(
            self.clock as u16,
            &self.bank,
            self.kit_index,
            self.kit_drift,
            self.phrase_drift,
            rand,
            fs,
        )?;
        Ok(())
    }

    pub fn clear_pool(&mut self) {
        self.pool.next = 0;
        self.pool.phrases.clear();
        if let Some(active) = self.pool.active.as_mut() {
            active.phrase_rem = 0;
        }
    }

    pub fn push_pool(&mut self, index: u8) {
        let _ = self.pool.phrases.push(index);
    }

    fn process_input(
        &mut self,
        fs: &mut F,
        rand: &mut impl Rand,
        event: passive::Event,
    ) -> Result<(), OpenError<F::Error>> {
        self.input.active.trans(
            &event,
            self.clock as u16,
            &self.bank,
            self.kit_index,
            self.kit_drift,
            rand,
            fs,
        )?;
        self.record.push(event, self.clock as u16);
        if let Some(reverse) = &mut self.reverse {
            *reverse = self.clock;
        }
        Ok(())
    }

    fn tick_phrases(&mut self, fs: &mut F, rand: &mut impl Rand) -> Result<(), OpenError<F::Error>> {
        // advance record phrase, if any
        if let Some(active::Phrase {
            next,
            event_rem,
            phrase_rem,
            active,
        }) = self.record.active.as_mut()
        {
            *event_rem = event_rem.saturating_sub(1);
            *phrase_rem = phrase_rem.saturating_sub(1);
            if *phrase_rem == 0 {
                // generate next phrase from record
                self.record.generate_phrase(
                    self.clock as u16,
                    &self.bank,
                    self.kit_index,
                    self.kit_drift,
                    self.phrase_drift,
                    rand,
                    fs,
                )?;
            } else if *event_rem == 0 {
                // generate next event from record
                if let Some(phrase) = self.record.phrase.as_mut() {
                    if let Some(rem) = phrase.generate_stamped(
                        active,
                        *next,
                        self.clock as u16,
                        &self.bank,
                        self.kit_index,
                        self.kit_drift,
                        self.phrase_drift,
                        rand,
                        fs,
                    )? {
                        *next += 1;
                        *event_rem = rem;
                    }
                }
            }
        }
        // advance pool phrase, if any
        if let Some(active::Phrase {
            next,
            event_rem,
            phrase_rem,
            active,
        }) = self.pool.active.as_mut()
        {
            *event_rem = event_rem.saturating_sub(1);
            *phrase_rem = phrase_rem.saturating_sub(1);
            if *phrase_rem == 0 {
                // generate next phrase from pool
                self.pool.generate_phrase(
                    self.clock as u16,
                    &self.bank,
                    self.kit_index,
                    self.kit_drift,
                    self.phrase_drift,
                    rand,
                    fs,
                )?;
            } else if *event_rem == 0 {
                // generate next event from pool
                if let Some(phrase) = self
                    .pool
                    .index
                    .and_then(|v| self.bank.phrases[v as usize].as_ref())
                {
                    if let Some(rem) = phrase.generate_stamped(
                        active,
                        *next,
                        self.clock as u16,
                        &self.bank,
                        self.kit_index,
                        self.kit_drift,
                        self.phrase_drift,
                        rand,
                        fs,
                    )? {
                        *next += 1;
                        *event_rem = rem;
                    }
                }
            }
        } else if !self.pool.phrases.is_empty() {
            // generate first phrase from pool
            self.pool.generate_phrase(
                self.clock as u16,
                &self.bank,
                self.kit_index,
                self.kit_drift,
                self.phrase_drift,
                rand,
                fs,
            )?;
        }
        Ok(())
    }
}

pub struct SystemHandler<
    const BANKS: usize,
    const PADS: usize,
    const STEPS: usize,
    const PHRASES: usize,
    F: FileHandler,
    R: Rand,
> {
    pub fs: F,
    pub rand: R,
    pub banks: [BankHandler<PADS, STEPS, PHRASES, F>; BANKS],
}

impl<
        const BANKS: usize,
        const PADS: usize,
        const STEPS: usize,
        const PHRASES: usize,
        F: FileHandler,
        R: Rand,
    > SystemHandler<BANKS, PADS, STEPS, PHRASES, F, R>
{
    pub fn new(fs: F, rand: R, step_div: u16, loop_div: f32) -> Self {
        // oh rust, why won't you let me use generics in const operations
        assert_eq!(STEPS, 2usize.pow(PADS as u32 - 1));
        Self {
            fs,
            rand,
            banks: core::array::from_fn(|_| BankHandler::new(step_div, loop_div)),
        }
    }

    pub fn read_all<const SAMPLE_RATE: u16, T: core::ops::AddAssign + From<f32>>(
        &mut self,
        buffer: &mut [T],
        channels: usize,
    ) -> Result<(), F::Error> {
        for bank in self.banks.iter_mut() {
            bank.read_attenuated::<SAMPLE_RATE, T>(&mut self.fs, buffer, channels)?;
        }
        Ok(())
    }

    pub fn tick(&mut self) -> Result<(), OpenError<F::Error>> {
        for bank in self.banks.iter_mut() {
            bank.quant = true;
            bank.clock(&mut self.fs, &mut self.rand)?;
            bank.clock += 1.;
        }
        Ok(())
    }

    pub fn stop(&mut self) {
        for bank in self.banks.iter_mut() {
            bank.quant = false;
            bank.stop();
            bank.clock = 0.;
        }
    }

    pub fn assign_tempo(&mut self, tempo: f32) {
        for bank in self.banks.iter_mut() {
            bank.tempo = tempo;
        }
    }
}

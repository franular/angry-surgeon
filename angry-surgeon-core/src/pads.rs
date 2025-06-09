//! main logic-to-audio driver

use crate::{active, passive, FileHandler};
use embedded_io_async::{Read, Seek, Write};
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

struct GrainReader<const LEN: usize> {
    // byte buffer
    buffer: [u8; LEN],
    // i16 sample index
    index: f32,
}

impl<const LEN: usize> GrainReader<LEN> {
    fn new() -> Self {
        Self {
            buffer: [0; LEN],
            index: 0.,
        }
    }

    async fn fill_backwards<IO: Read + Write + Seek>(
        &mut self,
        wav: &mut active::Wav<IO>,
    ) -> Result<(), IO::Error> {
        // grain len in samples
        let grain_len = self.buffer.len() / 2 - 1;
        while self.index < 0. {
            // seek back two grains (in bytes)
            let pos = wav.pos().await?;
            wav.seek(pos as i64 - 4 * grain_len as i64).await?;
            // refill buffer backwards
            let mut slice = &mut self.buffer[..];
            while !slice.is_empty() {
                let n = wav.file.read(slice).await?;
                if n == 0 {
                    wav.seek(0).await?;
                }
                slice = &mut slice[n..];
            }
            // seek back -2 from extra word read for interpolation
            let pos = wav.pos().await?;
            wav.seek(pos as i64 - 2).await?;

            self.index += grain_len as f32;
        }
        Ok(())
    }

    async fn fill_forwards<IO: Read + Write + Seek>(
        &mut self,
        wav: &mut active::Wav<IO>,
    ) -> Result<(), IO::Error> {
        // grain len in samples
        let grain_len = self.buffer.len() / 2 - 1;
        while self.index as usize >= grain_len {
            // refill buffer forwards
            let mut slice = &mut self.buffer[..];
            while !slice.is_empty() {
                let n = wav.file.read(slice).await?;
                if n == 0 {
                    wav.seek(0).await?;
                }
                slice = &mut slice[n..];
            }
            // seek back -2 from extra word read for interpolation
            let pos = wav.pos().await?;
            wav.seek(pos as i64 - 2).await?;

            self.index -= grain_len as f32;
        }
        Ok(())
    }

    async fn read_interpolated<IO: Read + Write + Seek>(
        &mut self,
        wav: &mut active::Wav<IO>,
        speed: f32,
    ) -> Result<f32, IO::Error> {
        // update buffer if necessary
        self.fill_backwards(wav).await?;
        self.fill_forwards(wav).await?;

        // read sample with linear interpolation
        let mut i16_buffer = [0u8; 2];
        i16_buffer.copy_from_slice(&self.buffer[self.index as usize * 2..][0..2]);
        let word_a =
            i16::from_le_bytes(i16_buffer) as f32 / i16::MAX as f32 * (1. - self.index.fract());
        i16_buffer.copy_from_slice(&self.buffer[self.index as usize * 2..][2..4]);
        let word_b = i16::from_le_bytes(i16_buffer) as f32 / i16::MAX as f32 * self.index.fract();
        let interpolated = word_a + word_b;
        self.index += speed;
        Ok(interpolated)
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

    pub async fn onset<IO: Read + Write + Seek>(
        &self,
        index: impl Into<usize> + Copy,
        pan: f32,
        fs: &mut impl FileHandler<File = IO>,
    ) -> Result<Option<active::Onset<IO>>, IO::Error> {
        if let Some(passive::Onset { wav, start, .. }) = self.onsets[index.into()].as_ref() {
            let wav = active::Wav {
                tempo: wav.tempo,
                steps: wav.steps,
                file: fs.open(&wav.path).await?,
                len: wav.len,
            };
            Ok(Some(active::Onset {
                index: index.into() as u8,
                pan,
                wav,
                start: *start,
            }))
        } else {
            Ok(None)
        }
    }

    pub async fn onset_seek<IO: Read + Write + Seek>(
        &self,
        index: impl Into<usize> + Copy,
        pan: f32,
        fs: &mut impl FileHandler<File = IO>,
    ) -> Result<Option<active::Onset<IO>>, IO::Error> {
        if let Some(passive::Onset { wav, start, .. }) = self.onsets[index.into()].as_ref() {
            let mut wav = active::Wav {
                tempo: wav.tempo,
                steps: wav.steps,
                file: fs.open(&wav.path).await?,
                len: wav.len,
            };
            wav.seek(*start as i64).await?;
            Ok(Some(active::Onset {
                index: index.into() as u8,
                pan,
                wav,
                start: *start,
            }))
        } else {
            Ok(None)
        }
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
    pub kits: [Kit<PADS>; PADS],
    #[serde(with = "serde_arrays")]
    pub phrases: [Option<passive::Phrase<STEPS>>; PADS],
}

impl<const PADS: usize, const STEPS: usize> Bank<PADS, STEPS> {
    pub fn generate_kit(&self, index: usize, drift: f32, rand: &mut impl Rand) -> &Kit<PADS> {
        let drift = drift * self.kits.len() as f32;
        let drift = rand.next_lim_usize(drift as usize)
            + rand.next_bool(tinyrand::Probability::new(drift.fract() as f64)) as usize;
        &self.kits[(index + drift) % self.kits.len()]
    }
}

impl<const PADS: usize, const STEPS: usize> Default for Bank<PADS, STEPS> {
    fn default() -> Self {
        Self {
            kits: core::array::from_fn(|_| Kit::default()),
            phrases: core::array::from_fn(|_| None),
        }
    }
}

pub struct BankHandler<
    const PADS: usize,
    const STEPS: usize,
    const PHRASES: usize,
    IO: Read + Write + Seek,
> {
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
    input: active::Input<IO>,
    record: active::Record<STEPS, IO>,
    pool: active::Pool<PHRASES, IO>,
    reader: GrainReader<{ crate::GRAIN_LEN + 2 }>,
}

impl<const PADS: usize, const STEPS: usize, const PHRASES: usize, IO: Read + Write + Seek>
    BankHandler<PADS, STEPS, PHRASES, IO>
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

    pub async fn read_attenuated<const SAMPLE_RATE: u16, T: core::ops::AddAssign + From<f32>>(
        &mut self,
        buffer: &mut [T],
        channels: usize,
    ) -> Result<(), IO::Error> {
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
        if self.tempo > 0. {
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
                    buffer,
                    channels,
                )
                .await;
            } else if let active::Event::Loop(onset, _, num) = active {
                let wav = &mut onset.wav;
                let pos = wav.pos().await?;
                let len = if let Some(steps) = wav.steps {
                    (*num as f32 / self.loop_div.net() * wav.len as f32 / steps as f32) as u64 & !1
                } else {
                    (*num as f32 / self.loop_div.net() * SAMPLE_RATE as f32 * 60. / self.tempo * self.loop_div.net())
                        as u64
                        & !1
                };
                let end = onset.start + len;
                if pos > end || pos < onset.start && pos + wav.len > end {
                    if self.reverse.is_some() {
                        wav.seek(end as i64).await?;
                    } else {
                        wav.seek(onset.start as i64).await?;
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
                    buffer,
                    channels,
                )
                .await;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn read_grain<T: core::ops::AddAssign + From<f32>>(
        tempo: f32,
        step_div: u16,
        gain: f32,
        speed: f32,
        width: f32,
        reverse: bool,
        onset: &mut active::Onset<IO>,
        reader: &mut GrainReader<{ crate::GRAIN_LEN + 2 }>,
        buffer: &mut [T],
        channels: usize,
    ) -> Result<(), IO::Error> {
        let mut speed = if let Some(t) = onset.wav.tempo {
            tempo * step_div as f32 / t * speed
        } else {
            speed
        };
        if reverse {
            speed *= -1.;
        }
        // FIXME: support alternative channel counts?
        assert!(channels == 2);
        for i in 0..buffer.len() / channels {
            let sample = reader.read_interpolated(&mut onset.wav, speed).await?;
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

    pub async fn assign_onset(
        &mut self,
        fs: &mut impl FileHandler<File = IO>,
        rand: &mut impl Rand,
        index: u8,
        onset: passive::Onset,
    ) -> Result<(), IO::Error> {
        self.bank.kits[self.kit_index].onsets[index as usize] = Some(onset);
        self.input
            .active
            .trans(
                &passive::Event::Hold { index },
                self.clock as u16,
                &self.bank,
                self.kit_index,
                self.kit_drift,
                rand,
                fs,
            )
            .await?;
        Ok(())
    }

    pub async fn clock(
        &mut self,
        fs: &mut impl FileHandler<File = IO>,
        rand: &mut impl Rand,
    ) -> Result<(), IO::Error> {
        if let Some(input) = self.input.buffer.take() {
            self.process_input(fs, rand, input).await?;
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
                            let offset = (wav.len as f32 / steps as f32 * (clock - *step as f32))
                                as i64
                                & !1;
                            wav.seek(onset.start as i64 + offset).await?;
                        }
                    }
                    active::Event::Loop(onset, step, num) => {
                        let wav = &mut onset.wav;
                        if let Some(steps) = wav.steps {
                            let clock = self.reverse.unwrap_or(self.clock);
                            let offset = (wav.len as f32 / steps as f32
                                * ((clock - *step as f32).rem_euclid(*num as f32 / self.loop_div.net())))
                                as i64
                                & !1;
                            wav.seek(onset.start as i64 + offset).await?;
                        }
                    }
                    _ => (),
                }
            }
        }
        self.tick_phrases(fs, rand).await?;
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

    pub async fn force_event(
        &mut self,
        fs: &mut impl FileHandler<File = IO>,
        rand: &mut impl Rand,
        event: passive::Event,
    ) -> Result<(), IO::Error> {
        self.input
            .active
            .trans(
                &event,
                self.clock as u16,
                &self.bank,
                self.kit_index,
                self.kit_drift,
                rand,
                fs,
            )
            .await?;
        Ok(())
    }

    pub async fn push_event(
        &mut self,
        fs: &mut impl FileHandler<File = IO>,
        rand: &mut impl Rand,
        event: passive::Event,
    ) -> Result<(), IO::Error> {
        if self.quant {
            self.input.buffer = Some(event);
        } else {
            self.process_input(fs, rand, event).await?;
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

    pub async fn bake_record(
        &mut self,
        fs: &mut impl FileHandler<File = IO>,
        rand: &mut impl Rand,
        len: u16,
    ) -> Result<(), IO::Error> {
        if self.record.active.is_none() {
            self.record.bake(self.clock as u16);
        }
        self.record.trim(len);
        self.record
            .generate_phrase(
                self.clock as u16,
                &self.bank,
                self.kit_index,
                self.kit_drift,
                self.phrase_drift,
                rand,
                fs,
            )
            .await?;
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

    async fn process_input(
        &mut self,
        fs: &mut impl FileHandler<File = IO>,
        rand: &mut impl Rand,
        event: passive::Event,
    ) -> Result<(), IO::Error> {
        self.input
            .active
            .trans(
                &event,
                self.clock as u16,
                &self.bank,
                self.kit_index,
                self.kit_drift,
                rand,
                fs,
            )
            .await?;
        self.record.push(event, self.clock as u16);
        if let Some(reverse) = &mut self.reverse {
            *reverse = self.clock;
        }
        Ok(())
    }

    async fn tick_phrases(
        &mut self,
        fs: &mut impl FileHandler<File = IO>,
        rand: &mut impl Rand,
    ) -> Result<(), IO::Error> {
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
                self.record
                    .generate_phrase(
                        self.clock as u16,
                        &self.bank,
                        self.kit_index,
                        self.kit_drift,
                        self.phrase_drift,
                        rand,
                        fs,
                    )
                    .await?;
            } else if *event_rem == 0 {
                // generate next event from record
                if let Some(phrase) = self.record.phrase.as_mut() {
                    if let Some(rem) = phrase
                        .generate_stamped(
                            active,
                            *next,
                            self.clock as u16,
                            &self.bank,
                            self.kit_index,
                            self.kit_drift,
                            self.phrase_drift,
                            rand,
                            fs,
                        )
                        .await?
                    {
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
                self.pool
                    .generate_phrase(
                        self.clock as u16,
                        &self.bank,
                        self.kit_index,
                        self.kit_drift,
                        self.phrase_drift,
                        rand,
                        fs,
                    )
                    .await?;
            } else if *event_rem == 0 {
                // generate next event from pool
                if let Some(phrase) = self
                    .pool
                    .index
                    .and_then(|v| self.bank.phrases[v as usize].as_ref())
                {
                    if let Some(rem) = phrase
                        .generate_stamped(
                            active,
                            *next,
                            self.clock as u16,
                            &self.bank,
                            self.kit_index,
                            self.kit_drift,
                            self.phrase_drift,
                            rand,
                            fs,
                        )
                        .await?
                    {
                        *next += 1;
                        *event_rem = rem;
                    }
                }
            }
        } else if !self.pool.phrases.is_empty() {
            // generate first phrase from pool
            self.pool
                .generate_phrase(
                    self.clock as u16,
                    &self.bank,
                    self.kit_index,
                    self.kit_drift,
                    self.phrase_drift,
                    rand,
                    fs,
                )
                .await?;
        }
        Ok(())
    }
}

pub struct SystemHandler<
    const BANKS: usize,
    const PADS: usize,
    const STEPS: usize,
    const PHRASES: usize,
    IO: Read + Write + Seek,
> {
    pub banks: [BankHandler<PADS, STEPS, PHRASES, IO>; BANKS],
}

impl<
        const BANKS: usize,
        const PADS: usize,
        const STEPS: usize,
        const PHRASES: usize,
        IO: Read + Write + Seek,
    > SystemHandler<BANKS, PADS, STEPS, PHRASES, IO>
{
    pub fn new(step_div: u16, loop_div: f32) -> Self {
        // oh rust, why won't you let me use generics in const operations
        assert_eq!(STEPS, 2usize.pow(PADS as u32 - 1));
        Self {
            banks: core::array::from_fn(|_| BankHandler::new(step_div, loop_div)),
        }
    }

    pub async fn tick(
        &mut self,
        fs: &mut impl FileHandler<File = IO>,
        rand: &mut impl Rand,
    ) -> Result<(), IO::Error> {
        for bank in self.banks.iter_mut() {
            bank.quant = true;
            bank.clock(fs, rand).await?;
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

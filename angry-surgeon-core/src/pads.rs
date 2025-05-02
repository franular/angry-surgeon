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

    async fn fill_backwards<IO: Read + Write + Seek>(&mut self, wav: &mut active::Wav<IO>) -> Result<(), IO::Error> {
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

    async fn fill_forwards<IO: Read + Write + Seek>(&mut self, wav: &mut active::Wav<IO>) -> Result<(), IO::Error> {
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

    async fn read_interpolated<IO: Read + Write + Seek>(&mut self, wav: &mut active::Wav<IO>, speed: f32) -> Result<f32, IO::Error> {
        // update buffer if necessary
        self.fill_backwards(wav).await?;
        self.fill_forwards(wav).await?;

        // read sample with linear interpolation
        let mut i16_buffer = [0u8; 2];
        i16_buffer.copy_from_slice(&self.buffer[self.index as usize * 2..][0..2]);
        let word_a = i16::from_le_bytes(i16_buffer) as f32 / i16::MAX as f32 * (1. - self.index.fract());
        i16_buffer.copy_from_slice(&self.buffer[self.index as usize * 2..][2..4]);
        let word_b = i16::from_le_bytes(i16_buffer) as f32 / i16::MAX as f32 * self.index.fract();
        let interpolated = word_a + word_b;
        self.index += speed;
        Ok(interpolated)
    }
}

macro_rules! maybe_path {
    ($($path:ident)?) => {
        #[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
        pub struct Pad<const STEPS: usize$(, const $path: usize)?> {
            pub onsets: [Option<passive::Onset$(<$path>)?>; 2],
            pub phrase: Option<passive::Phrase<STEPS>>,
        }

        #[derive(Clone, serde::Serialize, serde::Deserialize)]
        pub struct Kit<const PADS: usize, const STEPS: usize$(, const $path: usize)?> {
            #[serde(with = "serde_arrays")]
            pub pads: [Pad<STEPS$(, $path)?>; PADS],
        }

        impl<const PADS: usize, const STEPS: usize$(, const $path: usize)?> Kit<PADS, STEPS$(, $path)?> {
            pub fn generate_pan(index: impl Into<usize>) -> f32 {
                index.into() as f32 / PADS as f32 - 0.5
            }

            pub async fn onset<IO: Read + Write + Seek>(
                &self,
                index: impl Into<usize> + Copy,
                alt: bool,
                pan: f32,
                fs: &mut impl FileHandler<File = IO>,
            ) -> Result<active::Onset<IO>, IO::Error> {
                let passive::Onset { wav, start, .. } = self.pads[index.into()].onsets[alt as usize]
                    .as_ref()
                    .unwrap();
                let wav = active::Wav {
                    tempo: wav.tempo,
                    steps: wav.steps,
                    file: fs.open(&wav.path).await?,
                    len: wav.len,
                };
                Ok(active::Onset {
                    index: index.into() as u8,
                    pan,
                    wav,
                    start: *start,
                })
            }

            pub async fn onset_seek<IO: Read + Write + Seek>(
                &self,
                index: impl Into<usize> + Copy,
                alt: bool,
                pan: f32,
                fs: &mut impl FileHandler<File = IO>,
            ) -> Result<active::Onset<IO>, IO::Error> {
                let passive::Onset { wav, start, .. } = self.pads[index.into()].onsets[alt as usize]
                    .as_ref()
                    .unwrap();
                let mut wav = active::Wav {
                    tempo: wav.tempo,
                    steps: wav.steps,
                    file: fs.open(&wav.path).await?,
                    len: wav.len,
                };
                wav.seek(*start as i64).await?;
                Ok(active::Onset {
                    index: index.into() as u8,
                    pan,
                    wav,
                    start: *start,
                })
            }

            pub fn generate_alt(
                &self,
                index: impl Into<usize>,
                bias: f32,
                rand: &mut impl Rand,
            ) -> Option<bool> {
                match self.pads[index.into()].onsets {
                    [None, None] => None,
                    [Some(_), None] => Some(false),
                    [None, Some(_)] => Some(true),
                    [Some(_), Some(_)] => Some(rand.next_bool(tinyrand::Probability::new(bias as f64))),
                }
            }
        }

        impl<const PADS: usize, const STEPS: usize$(, const $path: usize)?> Default for Kit<PADS, STEPS$(, $path)?> {
            fn default() -> Self {
                Self {
                    pads: core::array::from_fn(|_| Pad::default()),
                }
            }
        }

        /// separate for serde reasons
        #[derive(serde::Serialize, serde::Deserialize)]
        pub struct Bank<const PADS: usize, const STEPS: usize$(, const $path: usize)?> {
            #[serde(with = "serde_arrays")]
            pub kits: [Kit<PADS, STEPS$(, $path)?>; PADS],
        }

        impl<const PADS: usize, const STEPS: usize$(, const $path: usize)?> Default for Bank<PADS, STEPS$(, $path)?> {
            fn default() -> Self {
                Self {
                    kits: core::array::from_fn(|_| Kit::default()),
                }
            }
        }

        #[derive(serde::Serialize, serde::Deserialize)]
        pub struct Scene<const BANKS: usize, const PADS: usize, const STEPS: usize$(, const $path: usize)?> {
            #[serde(with = "serde_arrays")]
            pub banks: [Bank<PADS, STEPS$(, $path)?>; BANKS],
        }

        impl<const BANKS: usize, const PADS: usize, const STEPS: usize$(, const $path: usize)?> Default for Scene<BANKS, PADS, STEPS$(, $path)?> {
            fn default() -> Self {
                Self {
                    banks: core::array::from_fn(|_| Bank::default()),
                }
            }
        }

        pub struct BankHandler<const PADS: usize, const STEPS: usize, const PHRASES: usize$(, const $path: usize)?, IO: Read + Write + Seek> {
            quant: bool,
            clock: f32,
            tempo: f32,
            pub step_div: u16,
            pub loop_div: u16,

            pub gain: f32,
            pub speed: Mod<f32>,
            pub drift: f32,
            pub bias: f32,
            pub width: f32,

            pub kit: Kit<PADS, STEPS$(, $path)?>,
            reverse: Option<f32>,
            input: active::Input<IO>,
            record: active::Record<STEPS, IO>,
            pool: active::Pool<PHRASES, IO>,
            reader: GrainReader<{crate::GRAIN_LEN + 2}>,
        }

        impl<const PADS: usize, const STEPS: usize, const PHRASES: usize$(, const $path: usize)?, IO: Read + Write + Seek>
            BankHandler<PADS, STEPS, PHRASES$(, $path)?, IO>
        {
            fn new(step_div: u16, loop_div: u16) -> Self {
                Self {
                    quant: false,
                    clock: 0.,
                    tempo: 0.,
                    step_div,
                    loop_div,

                    gain: 1.,
                    speed: Mod::new(1., 1.),
                    drift: 0.,
                    bias: 0.,
                    width: 1.,

                    kit: Kit::default(),
                    reverse: None,
                    input: active::Input::new(),
                    record: active::Record::new(),
                    pool: active::Pool::new(),
                    reader: GrainReader::new(),
                }
            }

            pub async fn read_attenuated<const SAMPLE_RATE: usize, T: core::ops::AddAssign + From<f32>>(
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
                    } else if let active::Event::Loop(onset, _, len) = active {
                        let wav = &mut onset.wav;
                        let pos = wav.pos().await?;
                        let len = if let Some(steps) = wav.steps {
                            (f32::from(*len) * wav.len as f32 / steps as f32) as u64 & !1
                        } else {
                            (f32::from(*len) * SAMPLE_RATE as f32 * 60. / self.tempo
                                * self.loop_div as f32) as u64
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
                reader: &mut GrainReader<{crate::GRAIN_LEN + 2}>,
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
                alt: bool,
                onset: passive::Onset$(<$path>)?,
            ) -> Result<(), IO::Error> {
                self.kit.pads[index as usize].onsets[alt as usize] = Some(onset);
                self.input
                    .active
                    .trans(
                        &passive::Event::Hold { index },
                        self.clock as u16,
                        self.bias,
                        rand,
                        &self.kit,
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
                            active::Event::Loop(onset, step, len) => {
                                let wav = &mut onset.wav;
                                if let Some(steps) = wav.steps {
                                    let clock = self.reverse.unwrap_or(self.clock);
                                    let offset = (wav.len as f32 / steps as f32
                                        * ((clock - *step as f32).rem_euclid(f32::from(*len))))
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
                    .trans(&event, self.clock as u16, self.bias, rand, &self.kit, fs)
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
                        self.kit.pads[index as usize].phrase = Some(phrase);
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
                        self.bias,
                        self.drift,
                        rand,
                        &self.kit,
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
                    .trans(&event, self.clock as u16, self.bias, rand, &self.kit, fs)
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
                                self.bias,
                                self.drift,
                                rand,
                                &self.kit,
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
                                    self.bias,
                                    self.drift,
                                    rand,
                                    &self.kit,
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
                                self.bias,
                                self.drift,
                                rand,
                                &self.kit,
                                fs,
                            )
                            .await?;
                    } else if *event_rem == 0 {
                        // generate next event from pool
                        if let Some(phrase) = self
                            .pool
                            .index
                            .and_then(|v| self.kit.pads[v as usize].phrase.as_ref())
                        {
                            if let Some(rem) = phrase
                                .generate_stamped(
                                    active,
                                    *next,
                                    self.clock as u16,
                                    self.bias,
                                    self.drift,
                                    rand,
                                    &self.kit,
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
                            self.bias,
                            self.drift,
                            rand,
                            &self.kit,
                            fs,
                        )
                        .await?;
                }
                Ok(())
            }
        }

        pub struct SceneHandler<const BANKS: usize, const PADS: usize, const STEPS: usize, const PHRASES: usize$(, const $path: usize)?, IO: Read + Write + Seek> {
            pub scene: Scene<BANKS, PADS, STEPS$(,$path)?>,
            pub banks: [BankHandler<PADS, STEPS, PHRASES$(, $path)?, IO>; BANKS],
        }

        impl<const BANKS: usize, const PADS: usize, const STEPS: usize, const PHRASES: usize$(, const $path: usize)?, IO: Read + Write + Seek>
            SceneHandler<BANKS, PADS, STEPS, PHRASES$(, $path)?, IO>
        {
            pub fn new(step_div: u16, loop_div: u16) -> Self {
                // oh rust, why won't you let me use generics in const operations
                assert_eq!(STEPS, 2usize.pow(PADS as u32 - 1));
                Self {
                    scene: Scene::default(),
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
    }
}

#[cfg(not(feature = "std"))]
maybe_path!(PATH);

#[cfg(feature = "std")]
maybe_path!();

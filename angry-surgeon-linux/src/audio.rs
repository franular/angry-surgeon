use angry_surgeon_core::{Event, Onset};
use color_eyre::Result;
use cpal::{FromSample, SizedSample};
use std::{io::{Read, Seek, Write}, sync::mpsc::Receiver};
use tinyrand::Seeded;

pub const SAMPLE_RATE: u32 = 48000;
pub const PPQ: u16 = 24;
pub const LINES_PER_STEP: u16 = 4;

pub const BANK_COUNT: usize = 2;
pub const PAD_COUNT: usize = 8;
pub const MAX_PHRASE_COUNT: usize = 128;
pub const MAX_PHRASE_LEN: usize = 2usize.pow(PAD_COUNT as u32 - 1);

#[derive(Copy, Clone)]
pub enum Bank {
    A,
    B,
}

pub enum Cmd {
    LoadOneshot(std::fs::File),
    StopOneshot,
    AssignGainOneshot(f32),

    Tick,
    Stop,
    AssignTempo(f32),
    OffsetPitch(f32),
    Bank(Bank, BankCmd),
}

pub enum BankCmd {
    AssignGain(f32),
    AssignWidth(f32),
    AssignPitch(f32),
    AssignRoll(f32),
    AssignKitDrift(f32),
    AssignPhraseDrift(f32),

    SaveBank(std::fs::File),
    LoadBank(Box<angry_surgeon_core::Bank<PAD_COUNT, MAX_PHRASE_LEN>>),
    LoadKit(u8),
    AssignOnset(u8, Box<Onset>),

    ForceEvent(Event),
    PushEvent(Event),
    PushReverse(bool),
    TrimRecord(u16),
    TakeRecord(Option<u8>),
    ClearSequence,
    PushSequence(u8),
}

pub struct Oneshot<const LEN: usize> {
    file: Option<std::fs::File>,
    /// sample buffer
    bytes: [u8; LEN],
    index: usize,
    rem: u64,
    sample_rate: u32,
    gain: f32,
}

impl<const LEN: usize> Oneshot<LEN> {
    fn new() -> Self {
        Self {
            file: None,
            bytes: [0; LEN],
            index: 0,
            rem: 0,
            sample_rate: 0,
            gain: 1.,
        }
    }

    fn load(&mut self, mut file: Option<std::fs::File>) -> Result<()> {
        if let Some(file) = file.as_mut() {
            let assert = |b: bool| if !b {
                Err(color_eyre::Report::msg("bad .wav"))
            } else {
                Ok(())
            };
            // parse wav looking for metadata and `data` subchunk
            let mut pcm_start = 0;
            let mut essential_chunks_parsed = 0;
            while essential_chunks_parsed < 3 {
                let mut id = [0u8; 4];
                file.read_exact(&mut id)?;
                if &id[..] == b"RIFF" {
                    file.seek_relative(4)?;
                    let mut data = [0u8; 4];
                    file.read_exact(&mut data)?;
                    if &data[..] != b"WAVE" {
                        return Err(color_eyre::Report::msg("bad .wav"));
                    }
                    essential_chunks_parsed += 1;
                } else if &id[..] == b"fmt " {
                    let mut data32 = [0u8; 4];
                    let mut data16 = [0u8; 2];
                    file.read_exact(&mut data32)?;
                    assert(u32::from_le_bytes(data32) == 16)?; // `fmt ` chunk size
                    file.read_exact(&mut data16)?;
                    assert(u16::from_le_bytes(data16) == 1)?; // pcm integer format
                    file.read_exact(&mut data16)?;
                    assert(u16::from_le_bytes(data16) == 1)?; // 1 channel
                    file.read_exact(&mut data32)?;
                    self.sample_rate = u32::from_le_bytes(data32);
                    file.seek_relative(6)?;
                    file.read_exact(&mut data16)?;
                    assert(u16::from_le_bytes(data16) == 16)?; // 16 bits/sample
                    essential_chunks_parsed += 1;
                } else if &id[..] == b"data" {
                    let mut size = [0u8; 4];
                    file.read_exact(&mut size)?;
                    pcm_start = file.stream_position()?;
                    let pcm_len = u32::from_le_bytes(size) as u64;
                    self.rem = pcm_start + pcm_len;
                    essential_chunks_parsed += 1;
                } else {
                    let mut size = [0u8; 4];
                    file.read_exact(&mut size)?;
                    let chunk_len = u32::from_le_bytes(size) as i64;
                    file.seek_relative(chunk_len)?;
                }
            };
            file.seek(std::io::SeekFrom::Start(pcm_start))?;
        }
        self.file = file;
        Ok(())
    }

    fn fill(&mut self) -> Result<(), std::io::Error> {
        if let Some(file) = self.file.as_mut() {
            if (self.index + 1) * 2 >= LEN || self.rem == 0 {
                // refill buffer
                self.index %= LEN / 2 - 1;
                self.rem = self.rem.saturating_sub(LEN as u64);
                let mut slice = &mut self.bytes[..];
                while !slice.is_empty() {
                    let len = slice.len().min(self.rem as usize);
                    let n = file.read(&mut slice[..len])?;
                    if n == 0 {
                        self.file = None;
                        return Ok(());
                    }
                    slice = &mut slice[n..];
                    self.rem += n as u64;
                }
            }
        }
        Ok(())
    }

    fn read_attenuated<T: core::ops::AddAssign + From<f32>>(
        &mut self,
        buffer: &mut [T],
        channels: usize,
    ) -> Result<(), std::io::Error> {
        // TODO: support other channel counts?
        assert!(channels == 2);
        for i in 0..buffer.len() / channels {
            // update buffer if necessary
            self.fill()?;
            if self.rem == 0 {
                return Ok(());
            }
            let mut i16_buffer = [0u8; 2];
            i16_buffer.copy_from_slice(&self.bytes[self.index * 2..][0..2]);
            let word = i16::from_le_bytes(i16_buffer) as f32 / i16::MAX as f32 * self.gain;
            self.index += 1;
            self.rem -= 2;

            buffer[i * 2] += T::from(word);
            buffer[i * 2 + 1] += T::from(word);
        }
        Ok(())
    }
}

pub struct SystemHandler {
    system: angry_surgeon_core::SystemHandler<
        BANK_COUNT,
        PAD_COUNT,
        MAX_PHRASE_LEN,
        MAX_PHRASE_COUNT,
        tinyrand::Wyrand,
        crate::fs::LinuxFileHandler,
    >,
    oneshot: Oneshot<{ angry_surgeon_core::GRAIN_LEN * 2 }>,
    cmd_rx: Receiver<Cmd>,
}

impl SystemHandler {
    pub fn new(cmd_rx: Receiver<Cmd>) -> Result<Self> {
        Ok(Self {
            system: angry_surgeon_core::SystemHandler::new(
                LINES_PER_STEP,
                tinyrand::Wyrand::seed(0xf2aa),
                crate::fs::LinuxFileHandler {},
            ),
            oneshot: Oneshot::new(),
            cmd_rx,
        })
    }

    pub fn tick<T>(&mut self, buffer: &mut [T], channels: usize) -> Result<()>
    where
        T: SizedSample + FromSample<f32>,
    {
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            match cmd {
                Cmd::LoadOneshot(file) => self.oneshot.load(Some(file))?,
                Cmd::StopOneshot => self.oneshot.load(None)?,
                Cmd::AssignGainOneshot(v) => self.oneshot.gain = v,

                Cmd::Tick => self.system.tick()?,
                Cmd::Stop => self.system.stop(),
                Cmd::AssignTempo(v) => self.system.assign_tempo(v),
                Cmd::OffsetPitch(v) => {
                    for bank in self.system.banks.iter_mut() {
                        bank.pitch.offset = v;
                    }
                }
                Cmd::Bank(bank, cmd) => {
                    let bank_h = &mut self.system.banks[bank as u8 as usize];
                    match cmd {
                        BankCmd::AssignGain(v) => bank_h.gain = v,
                        BankCmd::AssignWidth(v) => bank_h.width = v,
                        BankCmd::AssignPitch(v) => bank_h.pitch.base = v,
                        BankCmd::AssignRoll(v) => bank_h.loop_div.base = v,
                        BankCmd::AssignKitDrift(v) => bank_h.kit_drift = v,
                        BankCmd::AssignPhraseDrift(v) => bank_h.phrase_drift = v,

                        BankCmd::SaveBank(mut file) => {
                            let json = serde_json::to_string_pretty(&bank_h.bank)?;
                            write!(file, "{}", json)?;
                        }
                        BankCmd::LoadBank(bank) => bank_h.bank = *bank,
                        BankCmd::LoadKit(index) => bank_h.kit_index = index,
                        BankCmd::AssignOnset(index, onset) => bank_h.assign_onset(
                            index,
                            *onset,
                        ),
                        BankCmd::ForceEvent(event) => {
                            bank_h.force_event(event, &mut self.system.rand, &mut self.system.fs)?
                        }
                        BankCmd::PushEvent(event) => {
                            bank_h.push_event(event, &mut self.system.rand, &mut self.system.fs)?
                        }
                        BankCmd::PushReverse(reverse) => bank_h.push_reverse(reverse),
                        BankCmd::TrimRecord(len) => bank_h.trim_record(len),
                        BankCmd::TakeRecord(index) => bank_h.take_record(index),
                        BankCmd::ClearSequence => bank_h.clear_sequence(),
                        BankCmd::PushSequence(index) => bank_h.push_sequence(index),
                    }
                }
            }
        }
        buffer.fill(T::EQUILIBRIUM);
        let f32_buffer: &mut [f32] = unsafe { core::mem::transmute(buffer) };
        self.oneshot.read_attenuated(f32_buffer, channels)?;
        self.system.read_all(f32_buffer, channels, SAMPLE_RATE)?;
        Ok(())
    }
}

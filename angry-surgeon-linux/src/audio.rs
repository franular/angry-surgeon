use angry_surgeon_core::{Event, Onset};
use color_eyre::Result;
use cpal::{FromSample, SizedSample};
use std::{io::{Read, Seek, Write}, sync::mpsc::Receiver};
use tinyrand::Seeded;

pub const SAMPLE_RATE: u16 = 48000;
pub const PPQ: u16 = 24;
pub const STEP_DIV: u16 = 4;

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

    Clock,
    Stop,
    AssignTempo(f32),
    OffsetSpeed(f32),
    Bank(Bank, BankCmd),
}

pub enum BankCmd {
    AssignGain(f32),
    AssignWidth(f32),
    AssignSpeed(f32),
    AssignRoll(f32),
    AssignKitDrift(f32),
    AssignPhraseDrift(f32),
    AssignReverse(bool),

    SaveBank(std::fs::File),
    LoadBank(Box<angry_surgeon_core::Bank<PAD_COUNT, MAX_PHRASE_LEN>>),
    LoadKit(u8),
    AssignOnset(u8, Box<Onset>),

    ForceEvent(Event),
    PushEvent(Event),
    TakeRecord(Option<u8>),
    BakeRecord(u16),
    ClearPool,
    PushPool(u8),
}

pub struct Oneshot<const LEN: usize> {
    file: Option<std::fs::File>,
    /// byte buffer
    buffer: [u8; LEN],
    /// i16 sample index
    index: usize,
    /// samples to read
    rem: usize,
    gain: f32,
}

impl<const LEN: usize> Oneshot<LEN> {
    fn new() -> Self {
        Self {
            file: None,
            buffer: [0; LEN],
            index: 0,
            rem: 0,
            gain: 1.,
        }
    }

    fn load(&mut self, mut file: Option<std::fs::File>) -> Result<()> {
        if let Some(file) = file.as_mut() {
            file.seek(std::io::SeekFrom::Start(44))?;
        }
        self.file = file;
        self.index = 0;
        self.rem = 0;
        Ok(())
    }

    fn fill(
        &mut self,
    ) -> Result<(), std::io::Error> {
        if let Some(file) = self.file.as_mut() {
            if self.rem == 0 {
                // refill buffer
                self.index %= self.buffer.len() / 2;
                let mut slice = &mut self.buffer[..];
                while !slice.is_empty() {
                    let n = file.read(slice)?;
                    if n == 0 {
                        self.file = None;
                        return Ok(());
                    }
                    slice = &mut slice[n..];
                    self.rem += n;
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
            i16_buffer.copy_from_slice(&self.buffer[self.index * 2..][0..2]);
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
        crate::fs::LinuxFileHandler,
        tinyrand::Wyrand,
    >,
    oneshot: Oneshot<{ angry_surgeon_core::GRAIN_LEN }>,
    cmd_rx: Receiver<Cmd>,
}

impl SystemHandler {
    pub fn new(cmd_rx: Receiver<Cmd>) -> Result<Self> {
        Ok(Self {
            system: angry_surgeon_core::SystemHandler::new(
                crate::fs::LinuxFileHandler {},
                tinyrand::Wyrand::seed(0xf2aa),
                STEP_DIV,
                8.,
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

                Cmd::Clock => self.system.tick()?,
                Cmd::Stop => self.system.stop(),
                Cmd::AssignTempo(v) => self.system.assign_tempo(v),
                Cmd::OffsetSpeed(v) => {
                    for bank in self.system.banks.iter_mut() {
                        bank.speed.offset = v;
                    }
                }
                Cmd::Bank(bank, cmd) => {
                    let bank_h = &mut self.system.banks[bank as u8 as usize];
                    match cmd {
                        BankCmd::AssignGain(v) => bank_h.gain = v,
                        BankCmd::AssignWidth(v) => bank_h.width = v,
                        BankCmd::AssignSpeed(v) => bank_h.speed.base = v,
                        BankCmd::AssignRoll(v) => bank_h.loop_div.base = v,
                        BankCmd::AssignKitDrift(v) => bank_h.kit_drift = v,
                        BankCmd::AssignPhraseDrift(v) => bank_h.phrase_drift = v,
                        BankCmd::AssignReverse(v) => bank_h.assign_reverse(v),

                        BankCmd::SaveBank(mut file) => {
                            let json = serde_json::to_string_pretty(&bank_h.bank)?;
                            write!(file, "{}", json)?;
                        }
                        BankCmd::LoadBank(bank) => bank_h.bank = *bank,
                        BankCmd::LoadKit(index) => bank_h.kit_index = index as usize,
                        BankCmd::AssignOnset(index, onset) => bank_h.assign_onset(
                            index,
                            *onset,
                        ),
                        BankCmd::ForceEvent(event) => {
                            bank_h.force_event(&mut self.system.fs, &mut self.system.rand, event)?
                        }
                        BankCmd::PushEvent(event) => {
                            bank_h.push_event(&mut self.system.fs, &mut self.system.rand, event)?
                        }
                        BankCmd::TakeRecord(index) => bank_h.take_record(index),
                        BankCmd::BakeRecord(len) => {
                            bank_h.bake_record(&mut self.system.fs, &mut self.system.rand, len)?
                        }
                        BankCmd::ClearPool => bank_h.clear_pool(),
                        BankCmd::PushPool(index) => bank_h.push_pool(index),
                    }
                }
            }
        }
        buffer.fill(T::EQUILIBRIUM);
        let f32_buffer: &mut [f32] = unsafe { core::mem::transmute(buffer) };
        self.system
            .read_all::<SAMPLE_RATE, _>(f32_buffer, channels)?;
        self.oneshot.read_attenuated(f32_buffer, channels)?;
        Ok(())
    }
}

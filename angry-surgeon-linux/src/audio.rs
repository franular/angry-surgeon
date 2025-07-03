use angry_surgeon_core::{Event, Onset};
use color_eyre::Result;
use cpal::{FromSample, SizedSample};
use std::{io::Write, sync::mpsc::Receiver};
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

pub struct SystemHandler {
    system: angry_surgeon_core::SystemHandler<
        BANK_COUNT,
        PAD_COUNT,
        MAX_PHRASE_LEN,
        MAX_PHRASE_COUNT,
        crate::fs::LinuxFileHandler,
        tinyrand::Wyrand,
    >,
    cmd_rx: Receiver<Cmd>,
}

impl SystemHandler {
    pub fn new(cmd_rx: Receiver<Cmd>) -> Self {
        Self {
            system: angry_surgeon_core::SystemHandler::new(
                crate::fs::LinuxFileHandler {},
                tinyrand::Wyrand::seed(0xf2aa),
                STEP_DIV,
                8.,
            ),
            cmd_rx,
        }
    }

    pub fn tick<T>(&mut self, buffer: &mut [T], channels: usize) -> Result<()>
    where
        T: SizedSample + FromSample<f32>,
    {
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            match cmd {
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
                            &mut self.system.fs,
                            &mut self.system.rand,
                            index,
                            *onset,
                        )?,
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
            .read_all::<SAMPLE_RATE, f32>(f32_buffer, channels)?;
        Ok(())
    }
}

use crate::input::{STEP_DIV, LOOP_DIV};

use angry_surgeon_core::{Event, FileHandler, Onset, Scene, SceneHandler};

use std::{io::Write, sync::mpsc::Receiver};
use color_eyre::Result;
use cpal::{FromSample, SizedSample};
use embedded_io_adapters::futures_03::FromFutures;
use futures::io::AllowStdIo;
use tinyrand::Seeded;

pub const SAMPLE_RATE: usize = 48000;
pub const BANK_COUNT: usize = 2;
pub const PAD_COUNT: usize = 8;
pub const MAX_PHRASE_COUNT: usize = 128;
pub const MAX_PHRASE_LEN: usize = 2usize.pow(PAD_COUNT as u32 - 1);

pub type File = <LinuxFileHandler as FileHandler>::File;

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
    SaveScene(std::fs::File),
    LoadScene(Box<Scene<BANK_COUNT, PAD_COUNT, MAX_PHRASE_LEN, File>>),
    Bank(Bank, BankCmd),
}

pub enum BankCmd {
    AssignGain(f32),
    AssignSpeed(f32),
    AssignDrift(f32),
    AssignBias(f32),
    AssignWidth(f32),
    AssignReverse(bool),

    AssignKit(u8),
    LoadKit(u8),
    AssignOnset(u8, bool, Box<Onset<File>>),

    ForceEvent(Event),
    PushEvent(Event),
    TakeRecord(Option<u8>),
    BakeRecord(u16),
    ClearPool,
    PushPool(u8),
}

pub struct LinuxFileHandler;

impl FileHandler for LinuxFileHandler {
    type File = FromFutures<AllowStdIo<std::fs::File>>;

    async fn try_clone(
        &mut self,
        file: &Self::File,
    ) -> Result<Self::File, <Self::File as embedded_io_async::ErrorType>::Error> {
        Ok(FromFutures::new(AllowStdIo::new(file.inner().get_ref().try_clone()?)))
    }
}

pub struct AudioHandler {
    rand: tinyrand::Wyrand,
    scene: SceneHandler<BANK_COUNT, PAD_COUNT, MAX_PHRASE_LEN, MAX_PHRASE_COUNT, FromFutures<AllowStdIo<std::fs::File>>>,
    cmd_rx: Receiver<Cmd>,
}

impl AudioHandler {
    pub fn new(cmd_rx: Receiver<Cmd>) -> Self {
        Self {
            rand: tinyrand::Wyrand::seed(0),
            // FIXME: make mutable loop div
            scene: SceneHandler::new(STEP_DIV as u16, LOOP_DIV as u16),
            cmd_rx,
        }
    }

    pub async fn tick<T>(&mut self, buffer: &mut [T], channels: usize) -> Result<()>
    where
        T: SizedSample + FromSample<f32>,
    {
        let mut fs = LinuxFileHandler;
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            match cmd {
                Cmd::Clock => self.scene.tick(&mut fs, &mut self.rand).await?,
                Cmd::Stop => self.scene.stop(),
                Cmd::AssignTempo(v) => self.scene.assign_tempo(v),
                Cmd::OffsetSpeed(v) => for bank in self.scene.banks.iter_mut() {
                    bank.speed.offset = v;
                }
                Cmd::SaveScene(mut v) => {
                    let json = serde_json::to_string_pretty(&self.scene.scene)?;
                    write!(v, "{}", json)?;
                }
                Cmd::LoadScene(v) => self.scene.scene = *v,
                Cmd::Bank(bank, cmd) => {
                    let kits = &mut self.scene.scene.banks[bank as u8 as usize].kits;
                    let bank = &mut self.scene.banks[bank as u8 as usize];
                    match cmd {
                        BankCmd::AssignGain(v) => bank.gain = v,
                        BankCmd::AssignSpeed(v) => bank.speed.base = v,
                        BankCmd::AssignDrift(v) => bank.drift = v,
                        BankCmd::AssignBias(v) => bank.bias = v,
                        BankCmd::AssignWidth(v) => bank.width = v,
                        BankCmd::AssignReverse(v) => bank.assign_reverse(v),
                        BankCmd::AssignKit(index) => kits[index as usize] = bank.kit.clone(),
                        BankCmd::LoadKit(index) => bank.kit = kits[index as usize].clone(),
                        BankCmd::AssignOnset(index, alt, onset) => bank.assign_onset(&mut fs, &mut self.rand, index, alt, *onset).await?,
                        BankCmd::ForceEvent(event) => bank.force_event(&mut fs, &mut self.rand, event).await?,
                        BankCmd::PushEvent(event) => bank.push_event(&mut fs, &mut self.rand, event).await?,
                        BankCmd::TakeRecord(index) => bank.take_record(index),
                        BankCmd::BakeRecord(len) => bank.bake_record(&mut fs, &mut self.rand, len).await?,
                        BankCmd::ClearPool => bank.clear_pool(),
                        BankCmd::PushPool(index) => bank.push_pool(index),
                    }
                }
            }
        }
        buffer.fill(T::EQUILIBRIUM);
        let f32_buffer: &mut [f32] = unsafe { core::mem::transmute(buffer) };
        for bank in self.scene.banks.iter_mut() {
            bank.read_attenuated::<SAMPLE_RATE, f32>(f32_buffer, channels).await?;
        }
        Ok(())
    }
}

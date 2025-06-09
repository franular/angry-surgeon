use crate::fs::hw::{Dir, File};
use angry_surgeon_core::{Event, Onset, SystemHandler, GRAIN_LEN};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use tinyrand::Seeded;

pub mod hw;

pub const SAMPLE_RATE: u16 = 48000;
pub const BANK_COUNT: usize = 2;
pub const PAD_COUNT: usize = 8;
pub const MAX_PHRASE_COUNT: usize = 16;
pub const MAX_PHRASE_LEN: usize = 2usize.pow(PAD_COUNT as u32 - 1);

/// pulses per quarter
pub const PPQ: u16 = 2;
/// steps per quarter
pub const STEP_DIV: u16 = 4;

#[derive(Copy, Clone)]
pub enum Bank {
    A,
    B,
}

pub enum Cmd<'d> {
    Clock,
    AssignTempo(f32),
    Bank(Bank, BankCmd<'d>),
}

pub enum BankCmd<'d> {
    AssignGain(f32),
    AssignWidth(f32),
    AssignSpeed(f32),
    OffsetSpeed(f32),
    AssignRoll(f32),
    OffsetRoll(f32),
    AssignPhraseDrift(f32),
    AssignKitDrift(f32),
    AssignReverse(bool),

    SaveBank(embassy_sync::channel::DynamicSender<'d, crate::input::Bank>),
    LoadBank(embassy_sync::channel::DynamicReceiver<'d, crate::input::Bank>),
    LoadKit(u8),
    AssignOnset(u8, Onset),

    ForceEvent(Event),
    PushEvent(Event),
    TakeRecord(Option<u8>),
    BakeRecord(u16),
    ClearPool,
    PushPool(u8),
}

async fn parse_cmd<'d>(
    fs: &mut crate::fs::hw::SdmmcFileHandler<'d>,
    sys_hdlr: &mut SystemHandler<
        BANK_COUNT,
        PAD_COUNT,
        MAX_PHRASE_LEN,
        MAX_PHRASE_COUNT,
        File<'d>,
    >,
    rand: &mut tinyrand::Wyrand,
    cmd: Cmd<'d>,
) -> Result<(), <File<'d> as embedded_io_async::ErrorType>::Error> {
    match cmd {
        Cmd::Clock => sys_hdlr.tick(fs, rand).await?,
        Cmd::AssignTempo(v) => sys_hdlr.assign_tempo(v),
        Cmd::Bank(bank, cmd) => {
            let bank_hdlr = &mut sys_hdlr.banks[bank as u8 as usize];
            match cmd {
                BankCmd::AssignGain(v) => bank_hdlr.gain = v,
                BankCmd::AssignWidth(v) => bank_hdlr.width = v,
                BankCmd::AssignSpeed(v) => bank_hdlr.speed.base = v,
                BankCmd::OffsetSpeed(v) => bank_hdlr.speed.offset = v,
                BankCmd::AssignRoll(v) => bank_hdlr.loop_div.base = v,
                BankCmd::OffsetRoll(v) => bank_hdlr.loop_div.offset = v,
                BankCmd::AssignPhraseDrift(v) => bank_hdlr.phrase_drift = v,
                BankCmd::AssignKitDrift(v) => bank_hdlr.kit_drift = v,
                BankCmd::AssignReverse(v) => bank_hdlr.assign_reverse(v),

                BankCmd::SaveBank(tx) => tx.send(bank_hdlr.bank.clone()).await,
                BankCmd::LoadBank(rx) => bank_hdlr.bank = rx.receive().await,
                BankCmd::LoadKit(index) => bank_hdlr.kit_index = index as usize,
                BankCmd::AssignOnset(index, onset) => {
                    bank_hdlr.assign_onset(fs, rand, index, onset).await?
                }

                BankCmd::ForceEvent(event) => bank_hdlr.force_event(fs, rand, event).await?,
                BankCmd::PushEvent(event) => bank_hdlr.push_event(fs, rand, event).await?,
                BankCmd::TakeRecord(index) => bank_hdlr.take_record(index),
                BankCmd::BakeRecord(len) => bank_hdlr.bake_record(fs, rand, len).await?,
                BankCmd::ClearPool => bank_hdlr.clear_pool(),
                BankCmd::PushPool(index) => bank_hdlr.push_pool(index),
            }
        }
    }
    Ok(())
}

#[embassy_executor::task]
pub async fn system_handler(
    root: Dir<'static>,
    mut system: SystemHandler<BANK_COUNT, PAD_COUNT, MAX_PHRASE_LEN, MAX_PHRASE_COUNT, File<'static>>,
    mut grain_tx: embassy_sync::zerocopy_channel::Sender<'static, NoopRawMutex, [u16; GRAIN_LEN]>,
    cmd_rx: embassy_sync::channel::DynamicReceiver<'static, Cmd<'static>>,
) {
    use embassy_futures::select::*;

    let mut fs = crate::fs::hw::SdmmcFileHandler::new(root);
    let mut rand = tinyrand::Wyrand::seed(0xf2aa);
    loop {
        match select(grain_tx.send(), cmd_rx.receive()).await {
            Either::First(u16_buffer) => {
                let mut f32_buffer = [0f32; GRAIN_LEN];
                for bank in system.banks.iter_mut() {
                    bank.read_attenuated::<SAMPLE_RATE, f32>(&mut f32_buffer, 2)
                        .await
                        .unwrap();
                }
                for i in 0..u16_buffer.len() {
                    u16_buffer[i] = (f32_buffer[i] * i16::MAX as f32) as i16 as u16;
                }
                grain_tx.send_done();
            }
            Either::Second(cmd) => parse_cmd(&mut fs, &mut system, &mut rand, cmd)
                .await
                .unwrap(),
        }
    }
}

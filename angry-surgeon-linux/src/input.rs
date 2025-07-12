use crate::{audio, tui};
use audio::{Bank, MAX_PHRASE_LEN, PAD_COUNT, PPQ, STEP_DIV};

use angry_surgeon_core::{Event, Onset, Wav};
use color_eyre::Result;
use midly::{live::LiveEvent, MidiMessage};
use std::{
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, Sender},
};

macro_rules! audio_bank_cmd {
    ($bank:expr,$cmd:ident) => {
        audio::Cmd::Bank($bank, audio::BankCmd::$cmd)
    };
    ($bank:expr,$cmd:ident,$($params:tt)+) => {
        audio::Cmd::Bank($bank, audio::BankCmd::$cmd($($params)+))
    };
}

macro_rules! tui_bank_cmd {
    ($bank:expr,$cmd:ident) => {
        tui::Cmd::Bank($bank, tui::BankCmd::$cmd)
    };
    ($bank:expr,$cmd:ident,$($params:tt)+) => {
        tui::Cmd::Bank($bank, tui::BankCmd::$cmd($($params)+))
    };
}

macro_rules! dec {
    ($index:expr,$count:expr) => {
        if *$index == 0 {
            *$index = $count - 1;
        } else {
            *$index -= 1;
        }
    };
}

macro_rules! inc {
    ($index:expr,$count:expr) => {
        if *$index == $count - 1 {
            *$index = 0;
        } else {
            *$index += 1;
        }
    };
}

macro_rules! paths {
    ($parent:expr,$iter:expr,$ext:expr) => {{
        let mut paths: Vec<Box<Path>> = Vec::new();
        if let Some(parent) = $parent {
            if !parent.to_str().unwrap().is_empty() {
                paths.push(parent.to_path_buf().into_boxed_path())
            }
        }
        for entry in $iter.filter_map(|v| v.ok()) {
            let path = entry.path();
            if entry.metadata()?.is_dir()
                || path.extension().is_some_and(|v| v.to_str() == Some($ext))
            {
                paths.push(path.into_boxed_path());
            }
        }
        paths.sort();
        paths
    }};
}

macro_rules! to_fs {
    ($parent:expr,$names:expr,$index:expr) => {{
        let mut strings = [const { String::new() }; tui::FILE_COUNT];
        if !$names.is_empty() {
            for i in 0..tui::FILE_COUNT {
                let index = ($index as isize + i as isize - tui::FILE_COUNT as isize / 2)
                    .rem_euclid($names.len() as isize) as usize;
                if $parent == Some($names[index].as_ref()) {
                    strings[i] = "..".to_string();
                } else {
                    strings[i] = $names[index]
                        .file_name()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_string();
                }
            }
        }
        strings
    }};
    ($name:expr) => {
        $name.file_name().unwrap().to_str().unwrap().to_string()
    };
}

mod keys {
    pub const KIT_A: u8 = 48;
    pub const HOLD_A: u8 = 49;
    pub const REVERSE_A: u8 = 50;
    pub const SHIFT_A: u8 = 51;
    pub const BANK_A: core::ops::Range<u8> = 52..60;

    pub const BANK_B: core::ops::Range<u8> = 60..68;
    pub const SHIFT_B: u8 = 68;
    pub const REVERSE_B: u8 = 69;
    pub const HOLD_B: u8 = 70;
    pub const KIT_B: u8 = 71;

    pub const OPEN: u8 = 72;
}

mod ctrl {
    pub const GAIN_ONESHOT: u8 = 83;

    pub const GAIN_A: u8 = 102;
    pub const SPEED_A: u8 = 103;
    pub const DRIFT_A: u8 = 28;

    pub const GAIN_B: u8 = 105;
    pub const SPEED_B: u8 = 106;
    pub const DRIFT_B: u8 = 29;
}

pub enum Cmd {
    Deafen(bool),
}

#[derive(PartialEq)]
enum BankState {
    Mangle,
    LoadKit,
    BakeRecord,
    BuildPool { cleared: bool },
}

enum Preshift {
    None,
    Primed,
    FromLess,
    FromMore,
}

struct Knob {
    /// base and shift
    values: [Option<u8>; 2],
    preshift: Preshift,
}

impl Knob {
    fn new() -> Self {
        Self {
            values: [None; 2],
            preshift: Preshift::None,
        }
    }

    /// sets value if returned from shift discontinuity; return true if set
    fn maybe_set(&mut self, value: u8, shift: bool) -> bool {
        if let Some(mine) = self.values[shift as usize].as_mut() {
            match self.preshift {
                Preshift::None => {
                    if value == *mine {
                        false
                    } else {
                        *mine = value;
                        true
                    }
                }
                Preshift::Primed => {
                    if value < mine.saturating_sub(1) {
                        self.preshift = Preshift::FromLess;
                    } else if value > mine.saturating_add(1) {
                        self.preshift = Preshift::FromMore;
                    } else {
                        self.preshift = Preshift::None;
                    }
                    false
                }
                Preshift::FromLess => {
                    if value == *mine {
                        self.preshift = Preshift::None;
                        false
                    } else if value >= mine.saturating_sub(1) {
                        self.preshift = Preshift::None;
                        *mine = value;
                        true
                    } else {
                        false
                    }
                }
                Preshift::FromMore => {
                    if value == *mine {
                        self.preshift = Preshift::None;
                        false
                    } else if value <= mine.saturating_add(1) {
                        self.preshift = Preshift::None;
                        *mine = value;
                        true
                    } else {
                        false
                    }
                }
            }
        } else {
            self.values[shift as usize] = Some(value);
            true
        }
    }
}

struct BankHandler {
    bank: Bank,

    gain: Knob,
    speed: Knob,
    drift: Knob,

    downs: Vec<u8>,
    shift: bool,
    reverse: bool,
    hold: bool,

    state: BankState,
}

impl BankHandler {
    fn new(bank: Bank) -> Self {
        Self {
            bank,

            gain: Knob::new(),
            speed: Knob::new(),
            drift: Knob::new(),

            downs: Vec::new(),
            shift: false,
            reverse: false,
            hold: false,

            state: BankState::Mangle,
        }
    }

    fn shift(&mut self, shift: bool) {
        self.shift = shift;
        self.gain.preshift = Preshift::Primed;
        self.speed.preshift = Preshift::Primed;
        self.drift.preshift = Preshift::Primed;
    }

    fn gain(&mut self, value: u8, audio_tx: &mut Sender<audio::Cmd>) -> Result<()> {
        if self.gain.maybe_set(value, self.shift) {
            let cmd = if self.shift {
                audio_bank_cmd!(self.bank, AssignWidth, value as f32 / 127.)
            } else {
                audio_bank_cmd!(self.bank, AssignGain, value as f32 / 127.)
            };
            audio_tx.send(cmd)?;
        }
        Ok(())
    }

    fn speed(&mut self, value: u8, audio_tx: &mut Sender<audio::Cmd>) -> Result<()> {
        if self.speed.maybe_set(value, self.shift) {
            let cmd = if self.shift {
                audio_bank_cmd!(self.bank, AssignRoll, value as f32 / 127. * 8.)
            } else {
                audio_bank_cmd!(self.bank, AssignSpeed, value as f32 / 127. * 2.)
            };
            audio_tx.send(cmd)?;
        }
        Ok(())
    }

    fn drift(&mut self, value: u8, audio_tx: &mut Sender<audio::Cmd>) -> Result<()> {
        if self.speed.maybe_set(value, self.shift) {
            let cmd = if self.shift {
                audio_bank_cmd!(self.bank, AssignPhraseDrift, value as f32 / 127.)
            } else {
                audio_bank_cmd!(self.bank, AssignKitDrift, value as f32 / 127.)
            };
            audio_tx.send(cmd)?;
        }
        Ok(())
    }

    fn reverse_up(
        &mut self,
        audio_tx: &mut Sender<audio::Cmd>,
        tui_tx: &mut Sender<tui::Cmd>,
    ) -> Result<()> {
        match self.state {
            BankState::Mangle => {
                self.reverse = false;
                audio_tx.send(audio_bank_cmd!(self.bank, AssignReverse, false))?;
            }
            BankState::BakeRecord => {
                // exit record
                self.state = BankState::Mangle;
                audio_tx.send(audio_bank_cmd!(
                    self.bank,
                    TakeRecord,
                    self.downs.first().copied()
                ))?;
                tui_tx.send(tui_bank_cmd!(self.bank, Mangle))?;
            }
            _ => (),
        }
        Ok(())
    }

    fn reverse_down(
        &mut self,
        audio_tx: &mut Sender<audio::Cmd>,
        tui_tx: &mut Sender<tui::Cmd>,
    ) -> Result<()> {
        if self.state == BankState::Mangle {
            if self.shift {
                // init record
                self.state = BankState::BakeRecord;
                self.hold = false;
                if self.downs.is_empty() {
                    audio_tx.send(audio_bank_cmd!(self.bank, PushEvent, Event::Sync))?;
                }
                audio_tx.send(audio_bank_cmd!(
                    self.bank,
                    BakeRecord,
                    MAX_PHRASE_LEN as u16
                ))?;
                tui_tx.send(tui_bank_cmd!(
                    self.bank,
                    BakeRecord,
                    None,
                    audio::MAX_PHRASE_LEN as u16
                ))?;
            } else {
                self.reverse = true;
                audio_tx.send(audio_bank_cmd!(self.bank, AssignReverse, true))?;
            }
        }
        Ok(())
    }

    fn hold_up(
        &mut self,
        audio_tx: &mut Sender<audio::Cmd>,
        tui_tx: &mut Sender<tui::Cmd>,
    ) -> Result<()> {
        if let BankState::BuildPool { cleared } = self.state {
            // exit build pool
            if !cleared {
                audio_tx.send(audio_bank_cmd!(self.bank, ClearPool))?;
                tui_tx.send(tui_bank_cmd!(self.bank, ClearPool))?;
            }
            self.state = BankState::Mangle;
            tui_tx.send(tui_bank_cmd!(self.bank, Mangle))?;
        }
        Ok(())
    }

    fn hold_down(
        &mut self,
        audio_tx: &mut Sender<audio::Cmd>,
        tui_tx: &mut Sender<tui::Cmd>,
    ) -> Result<()> {
        if self.state == BankState::Mangle {
            if self.shift {
                // init build pool
                self.state = BankState::BuildPool { cleared: false };
                tui_tx.send(tui_bank_cmd!(self.bank, PushPool, None))?;
            } else {
                self.hold = !self.hold;
                if !self.hold && self.downs.is_empty() {
                    audio_tx.send(audio_bank_cmd!(self.bank, PushEvent, Event::Sync))?;
                }
            }
        }
        Ok(())
    }

    fn kit_up(&mut self, tui_tx: &mut Sender<tui::Cmd>) -> Result<()> {
        if self.state == BankState::LoadKit {
            // exit load kit
            self.state = BankState::Mangle;
            tui_tx.send(tui_bank_cmd!(self.bank, Mangle))?;
        }
        Ok(())
    }

    fn kit_down(
        &mut self,
        audio_tx: &mut Sender<audio::Cmd>,
        tui_tx: &mut Sender<tui::Cmd>,
    ) -> Result<()> {
        if self.state == BankState::Mangle {
            if self.shift {
                // save bank
                let mut index = 0;
                while std::fs::exists(format!("banks/bank{}.bd", index))? {
                    index += 1;
                }
                audio_tx.send(audio_bank_cmd!(
                    self.bank,
                    SaveBank,
                    std::fs::File::create_new(format!("banks/bank{}.bd", index))?
                ))?;
                tui_tx.send(tui::Cmd::Log(format!("saved to ./banks/bank{}.bd!", index)))?;
            } else {
                // init load kit
                self.state = BankState::LoadKit;
                tui_tx.send(tui_bank_cmd!(self.bank, LoadKit, None))?;
            }
        }
        Ok(())
    }

    fn pad_up(
        &mut self,
        audio_tx: &mut Sender<audio::Cmd>,
        tui_tx: &mut Sender<tui::Cmd>,
    ) -> Result<()> {
        match self.state {
            BankState::Mangle => {
                if !self.hold {
                    self.pad_input(audio_tx)?;
                }
            }
            BankState::BakeRecord => {
                let len = if self.downs.len() > 1 {
                    self.binary_offset(self.downs[0])
                } else {
                    MAX_PHRASE_LEN as u16
                };
                audio_tx.send(audio_bank_cmd!(self.bank, BakeRecord, len))?;
                tui_tx.send(tui_bank_cmd!(
                    self.bank,
                    BakeRecord,
                    self.downs.first().copied(),
                    len
                ))?;
            }
            _ => (),
        }
        Ok(())
    }

    fn pad_down(
        &mut self,
        audio_tx: &mut Sender<audio::Cmd>,
        tui_tx: &mut Sender<tui::Cmd>,
    ) -> Result<()> {
        match &mut self.state {
            BankState::Mangle => self.pad_input(audio_tx)?,
            BankState::LoadKit => {
                audio_tx.send(audio_bank_cmd!(self.bank, LoadKit, self.downs[0]))?;
                tui_tx.send(tui_bank_cmd!(
                    self.bank,
                    LoadKit,
                    self.downs.first().copied()
                ))?;
            }
            BankState::BakeRecord => {
                let len = if self.downs.len() > 1 {
                    self.binary_offset(self.downs[0])
                } else {
                    MAX_PHRASE_LEN as u16
                };
                audio_tx.send(audio_bank_cmd!(self.bank, BakeRecord, len))?;
                tui_tx.send(tui_bank_cmd!(
                    self.bank,
                    BakeRecord,
                    self.downs.first().copied(),
                    len
                ))?;
            }
            BankState::BuildPool { cleared } => {
                if !*cleared {
                    *cleared = true;
                    audio_tx.send(audio_bank_cmd!(self.bank, ClearPool))?;
                    tui_tx.send(tui_bank_cmd!(self.bank, ClearPool))?;
                }
                audio_tx.send(audio_bank_cmd!(
                    self.bank,
                    PushPool,
                    *self.downs.last().unwrap()
                ))?;
            }
        }
        Ok(())
    }

    fn pad_input(&mut self, audio_tx: &mut Sender<audio::Cmd>) -> Result<()> {
        if let Some(&index) = self.downs.first() {
            if self.downs.len() > 1 {
                // init loop start
                let len = self.binary_offset(index);
                audio_tx.send(audio_bank_cmd!(
                    self.bank,
                    PushEvent,
                    Event::Loop { index, len }
                ))?;
            } else {
                // init loop stop | jump
                audio_tx.send(audio_bank_cmd!(self.bank, PushEvent, Event::Hold { index }))?;
            }
        } else {
            // init sync
            audio_tx.send(audio_bank_cmd!(self.bank, PushEvent, Event::Sync))?;
        }
        Ok(())
    }

    fn binary_offset(&self, index: u8) -> u16 {
        self.downs
            .iter()
            .skip(1)
            .map(|v| {
                v.checked_sub(index + 1)
                    .unwrap_or(v + PAD_COUNT as u8 - 1 - index)
            })
            .fold(0u16, |acc, v| acc | (1 << v))
    }
}

struct Context {
    dir: Box<Path>,
    paths: Vec<Box<Path>>,
    file_index: usize,
}

enum GlobalState {
    Yield,
    LoadBd {
        bank: audio::Bank,
    },
    LoadRd,
    LoadOnset {
        rd: angry_surgeon_core::Rd,
        onset_index: usize,
    },
}

pub struct InputHandler {
    bank_a: BankHandler,
    bank_b: BankHandler,

    bd_cx: Option<Context>,
    rd_cx: Option<Context>,
    banks_maybe_focus: Option<audio::Bank>,

    deafen: bool,
    clock: u16,
    last_step: Option<std::time::Instant>,
    state: GlobalState,

    audio_tx: Sender<audio::Cmd>,
    tui_tx: Sender<tui::Cmd>,
    cmd_rx: Receiver<Cmd>,
}

impl InputHandler {
    pub fn new(audio_tx: Sender<audio::Cmd>, tui_tx: Sender<tui::Cmd>, cmd_rx: Receiver<Cmd>) -> Self {
        Self {
            bank_a: BankHandler::new(Bank::A),
            bank_b: BankHandler::new(Bank::B),

            bd_cx: None,
            rd_cx: None,
            banks_maybe_focus: None,

            deafen: false,
            clock: 0,
            last_step: None,
            state: GlobalState::Yield,

            audio_tx,
            tui_tx,
            cmd_rx,
        }
    }

    pub fn push_midi(&mut self, message: &[u8]) -> Result<()> {
        match self.cmd_rx.try_recv() {
            Ok(cmd) => match cmd {
                Cmd::Deafen(deafen) => self.deafen = deafen,
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => (),
            Err(e) => Err(e)?,
        }
        if !self.deafen {
            match LiveEvent::parse(message)? {
                LiveEvent::Midi { message, .. } => {
                    match message {
                        MidiMessage::NoteOff { key, .. } => self.note_off(key.as_int())?,
                        MidiMessage::NoteOn { key, .. } => self.note_on(key.as_int())?,
                        MidiMessage::Controller { controller, value } => {
                            self.controller(controller.as_int(), value.as_int())?
                        }
                        MidiMessage::PitchBend { bend } => {
                            // affect both banks
                            self.audio_tx
                                .send(audio::Cmd::OffsetSpeed(1. - bend.as_f32()))?;
                        }
                        _ => (),
                    }
                }
                LiveEvent::Realtime(midly::live::SystemRealtime::TimingClock) => self.timing_clock()?,
                LiveEvent::Realtime(midly::live::SystemRealtime::Stop) => self.stop()?,
                _ => (),
            }
        }
        Ok(())
    }

    fn note_off(&mut self, key: u8) -> Result<()> {
        match key {
            keys::SHIFT_A => {
                self.bank_a.shift(false);
                // unfocus for load bd
                if self.bank_b.shift {
                    self.banks_maybe_focus = Some(Bank::B);
                } else {
                    self.banks_maybe_focus = None;
                }
            }
            keys::SHIFT_B => {
                self.bank_b.shift(false);
                // unfocus for load bd
                if self.bank_a.shift {
                    self.banks_maybe_focus = Some(Bank::A);
                } else {
                    self.banks_maybe_focus = None;
                }
            }
            keys::REVERSE_A => {
                if let GlobalState::Yield = self.state {
                    self.bank_a
                        .reverse_up(&mut self.audio_tx, &mut self.tui_tx)?;
                }
            }
            keys::REVERSE_B => {
                if let GlobalState::Yield = self.state {
                    self.bank_b
                        .reverse_up(&mut self.audio_tx, &mut self.tui_tx)?;
                }
            }
            keys::HOLD_A => {
                if let GlobalState::Yield = self.state {
                    self.bank_a.hold_up(&mut self.audio_tx, &mut self.tui_tx)?;
                }
            }
            keys::HOLD_B => {
                if let GlobalState::Yield = self.state {
                    self.bank_b.hold_up(&mut self.audio_tx, &mut self.tui_tx)?;
                }
            }
            keys::KIT_A => {
                if let GlobalState::Yield = self.state {
                    self.bank_a.kit_up(&mut self.tui_tx)?;
                }
            }
            keys::KIT_B => {
                if let GlobalState::Yield = self.state {
                    self.bank_b.kit_up(&mut self.tui_tx)?;
                }
            }
            _ if keys::BANK_A.contains(&key) => {
                let index = keys::BANK_A.start + PAD_COUNT as u8 - 1 - key; // flipped
                self.bank_a.downs.retain(|&v| v != index);
                match self.state {
                    GlobalState::Yield => {
                        self.bank_a.pad_up(&mut self.audio_tx, &mut self.tui_tx)?
                    }
                    GlobalState::LoadOnset { .. } => {
                        self.audio_tx
                            .send(audio_bank_cmd!(Bank::A, ForceEvent, Event::Sync))?
                    }
                    _ => {
                        self.state = GlobalState::Yield;
                        self.audio_tx.send(audio_bank_cmd!(Bank::A, ForceEvent, Event::Sync))?;
                        self.tui_tx.send(tui::Cmd::Yield)?;
                    }
                }
                self.tui_tx
                    .send(tui_bank_cmd!(Bank::A, Pad, index, false))?;
            }
            _ if keys::BANK_B.contains(&key) => {
                let index = key - keys::BANK_B.start;
                self.bank_b.downs.retain(|&v| v != index);
                match self.state {
                    GlobalState::Yield => {
                        self.bank_b.pad_up(&mut self.audio_tx, &mut self.tui_tx)?
                    }
                    GlobalState::LoadOnset { .. } => {
                        self.audio_tx
                            .send(audio_bank_cmd!(Bank::B, ForceEvent, Event::Sync))?
                    }
                    _ => {
                        self.state = GlobalState::Yield;
                        self.audio_tx.send(audio_bank_cmd!(Bank::B, ForceEvent, Event::Sync))?;
                        self.tui_tx.send(tui::Cmd::Yield)?;
                    }
                }
                self.tui_tx
                    .send(tui_bank_cmd!(Bank::B, Pad, index, false))?;
            }
            _ => (),
        }
        Ok(())
    }

    fn note_on(&mut self, key: u8) -> Result<()> {
        match key {
            keys::OPEN => self.open()?,
            keys::SHIFT_A => {
                self.bank_a.shift(true);
                self.banks_maybe_focus = Some(Bank::A);
                if let GlobalState::LoadBd { bank } = &mut self.state {
                    *bank = Bank::A;
                }
            }
            keys::SHIFT_B => {
                self.bank_b.shift(true);
                self.banks_maybe_focus = Some(Bank::B);
                if let GlobalState::LoadBd { bank } = &mut self.state {
                    *bank = Bank::B;
                }
            }
            keys::REVERSE_A => {
                if let GlobalState::Yield = self.state {
                    self.bank_a
                        .reverse_down(&mut self.audio_tx, &mut self.tui_tx)?;
                }
            }
            keys::REVERSE_B => {
                if let GlobalState::Yield = self.state {
                    self.bank_b
                        .reverse_down(&mut self.audio_tx, &mut self.tui_tx)?;
                } else {
                    self.decrement()?;
                }
            }
            keys::HOLD_A => {
                if let GlobalState::Yield = self.state {
                    self.bank_a
                        .hold_down(&mut self.audio_tx, &mut self.tui_tx)?;
                }
            }
            keys::HOLD_B => {
                if let GlobalState::Yield = self.state {
                    self.bank_b
                        .hold_down(&mut self.audio_tx, &mut self.tui_tx)?;
                }
            }
            keys::KIT_A => {
                if let GlobalState::Yield = self.state {
                    self.bank_a.kit_down(&mut self.audio_tx, &mut self.tui_tx)?;
                }
            }
            keys::KIT_B => {
                if let GlobalState::Yield = self.state {
                    self.bank_b.kit_down(&mut self.audio_tx, &mut self.tui_tx)?;
                } else {
                    self.increment()?;
                }
            }
            _ if keys::BANK_A.contains(&key) => {
                let index = keys::BANK_A.start + PAD_COUNT as u8 - 1 - key; // flipped
                // let index = PAD_COUNT as u8 - (key - keys::BANK_A.start); // flipped
                self.bank_a.downs.push(index);
                match &mut self.state {
                    GlobalState::Yield => self.bank_a.pad_down(&mut self.audio_tx, &mut self.tui_tx)?,
                    GlobalState::LoadOnset { rd, onset_index } => {
                        let cx = self.rd_cx.as_ref().unwrap();
                        let path = &cx.paths[cx.file_index];
                        if let Ok(meta) = std::fs::metadata(path) {
                            // assign onset to pad
                            let onset = Onset {
                                wav: Wav {
                                    tempo: rd.tempo,
                                    steps: rd.steps,
                                    path: path.to_str().unwrap().to_string(),
                                    len: meta.len() - 44,
                                },
                                start: rd.onsets[*onset_index],
                            };
                            self.audio_tx.send(audio_bank_cmd!(
                                Bank::A,
                                AssignOnset,
                                index,
                                Box::new(onset)
                            ))?;
                            self.audio_tx.send(audio_bank_cmd!(Bank::A, ForceEvent, Event::Hold { index }))?;
                        } else {
                            self.tui_tx
                                .send(tui::Cmd::Log("no wav found".to_string()))?;
                        }
                    }
                    _ => {
                        self.state = GlobalState::Yield;
                        self.bank_a.pad_down(&mut self.audio_tx, &mut self.tui_tx)?;
                        self.tui_tx.send(tui::Cmd::Yield)?;
                    }
                }
                self.tui_tx.send(tui_bank_cmd!(Bank::A, Pad, index, true))?;
            }
            _ if keys::BANK_B.contains(&key) => {
                let index = key - keys::BANK_B.start;
                self.bank_b.downs.push(index);
                match &mut self.state {
                    GlobalState::Yield => self.bank_b.pad_down(&mut self.audio_tx, &mut self.tui_tx)?,
                    GlobalState::LoadOnset { rd, onset_index } => {
                        let cx = self.rd_cx.as_ref().unwrap();
                        let path = &cx.paths[cx.file_index].with_extension("wav");
                        if let Ok(meta) = std::fs::metadata(path) {
                            // assign onset to pad
                            let onset = Onset {
                                wav: Wav {
                                    tempo: rd.tempo,
                                    steps: rd.steps,
                                    path: path.to_str().unwrap().to_string(),
                                    len: meta.len() - 44,
                                },
                                start: rd.onsets[*onset_index],
                            };
                            self.audio_tx.send(audio_bank_cmd!(
                                Bank::B,
                                AssignOnset,
                                index,
                                Box::new(onset)
                            ))?;
                            self.audio_tx.send(audio_bank_cmd!(Bank::B, ForceEvent, Event::Hold { index }))?;
                        } else {
                            self.tui_tx
                                .send(tui::Cmd::Log("no wav found".to_string()))?;
                        }
                    }
                    _ => {
                        self.state = GlobalState::Yield;
                        self.bank_b.pad_down(&mut self.audio_tx, &mut self.tui_tx)?;
                        self.tui_tx.send(tui::Cmd::Yield)?;
                    }
                }
                self.tui_tx.send(tui_bank_cmd!(Bank::B, Pad, index, true))?;
            }
            _ => (),
        }
        Ok(())
    }

    fn controller(&mut self, controller: u8, value: u8) -> Result<()> {
        match controller {
            ctrl::GAIN_ONESHOT => {
                self.audio_tx.send(audio::Cmd::AssignGainOneshot(value as f32 / 127.))?;
            }
            ctrl::GAIN_A => {
                self.bank_a.gain(value, &mut self.audio_tx)?;
            }
            ctrl::GAIN_B => {
                self.bank_b.gain(value, &mut self.audio_tx)?;
            }
            ctrl::SPEED_A => {
                self.bank_a.speed(value, &mut self.audio_tx)?;
            }
            ctrl::SPEED_B => {
                self.bank_b.speed(value, &mut self.audio_tx)?;
            }
            ctrl::DRIFT_A => {
                self.bank_a.drift(value, &mut self.audio_tx)?;
            }
            ctrl::DRIFT_B => {
                self.bank_b.drift(value, &mut self.audio_tx)?;
            }
            _ => (),
        }
        Ok(())
    }

    fn timing_clock(&mut self) -> Result<()> {
        // affect both banks
        if self.clock == 0 {
            let now = std::time::Instant::now();
            if let Some(delta) = self.last_step {
                let ioi = now.duration_since(delta);
                let tempo = 60. / ioi.as_secs_f32() / STEP_DIV as f32;
                self.audio_tx.send(audio::Cmd::AssignTempo(tempo))?;
            }
            self.last_step = Some(now);
            self.audio_tx.send(audio::Cmd::Clock)?;
            self.tui_tx.send(tui::Cmd::Clock)?;
        }
        self.clock = (self.clock + 1) % (PPQ / STEP_DIV);
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        // affect both banks
        self.clock = 0;
        self.last_step = None;
        self.audio_tx.send(audio::Cmd::Stop)?;
        self.audio_tx.send(audio_bank_cmd!(Bank::A, ClearPool))?;
        self.audio_tx.send(audio_bank_cmd!(Bank::B, ClearPool))?;
        self.tui_tx.send(tui::Cmd::Stop)?;
        Ok(())
    }

    fn open(&mut self) -> Result<()> {
        match &self.state {
            GlobalState::Yield => {
                if let Some(bank) = self.banks_maybe_focus.take() {
                    // trans load bd
                    if let Some(cx) = &mut self.bd_cx {
                        // recall dir
                        let paths = paths!(cx.dir.parent(), std::fs::read_dir(&cx.dir)?, "bd");
                        self.tui_tx
                            .send(tui::Cmd::LoadBd(to_fs!(cx.dir.parent(), paths, cx.file_index)))?;
                        cx.paths = paths;
                        self.state = GlobalState::LoadBd { bank };
                    } else if let Ok(dir) = std::fs::read_dir("banks") {
                        // open ./banks
                        let paths = paths!(Some(Path::new("")), dir, "bd");
                        self.tui_tx.send(tui::Cmd::LoadBd(to_fs!(Some(Path::new("")), paths, 0)))?;
                        self.bd_cx = Some(Context {
                            dir: PathBuf::from("banks").into_boxed_path(),
                            file_index: 0,
                            paths,
                        });
                        self.state = GlobalState::LoadBd { bank };
                    } else {
                        self.tui_tx
                            .send(tui::Cmd::Log("no ./banks found".to_string()))?;
                    }
                } else {
                    // trans load rd
                    if let Some(cx) = &mut self.rd_cx {
                        // recall dir
                        let paths = paths!(cx.dir.parent(), std::fs::read_dir(&cx.dir)?, "wav");
                        self.tui_tx
                            .send(tui::Cmd::LoadRd(to_fs!(cx.dir.parent(), paths, cx.file_index)))?;
                        cx.paths = paths;
                        self.state = GlobalState::LoadRd;
                    } else if let Ok(dir) = std::fs::read_dir("onsets") {
                        // open ./onsets
                        let paths = paths!(Some(Path::new("")), dir, "wav");
                        self.tui_tx.send(tui::Cmd::LoadRd(to_fs!(Some(Path::new("")), paths, 0)))?;
                        self.rd_cx = Some(Context {
                            dir: PathBuf::from("onsets").into_boxed_path(),
                            file_index: 0,
                            paths,
                        });
                        self.state = GlobalState::LoadRd;
                    } else {
                        self.tui_tx
                            .send(tui::Cmd::Log("no ./onsets found".to_string()))?;
                    }
                }
            }
            GlobalState::LoadBd { bank } => {
                let cx = self.bd_cx.as_ref().unwrap();
                let path = &cx.paths[cx.file_index];
                if let Ok(entry) = std::fs::metadata(path) {
                    if entry.is_dir() {
                        // open dir
                        let paths = paths!(path.parent(), std::fs::read_dir(path)?, "bd");
                        self.tui_tx.send(tui::Cmd::LoadBd(to_fs!(path.parent(), paths, 0)))?;
                        self.bd_cx = Some(Context {
                            dir: path.clone(),
                            paths,
                            file_index: 0,
                        });
                    } else if entry.is_file()
                        && path.extension().is_some_and(|v| v.to_str() == Some("bd"))
                    {
                        // load bd
                        let bytes = std::fs::read(path)?;
                        if let Ok(bd) = serde_json::from_slice::<
                            angry_surgeon_core::Bank<PAD_COUNT, MAX_PHRASE_LEN>,
                        >(&bytes)
                        {
                            self.tui_tx.send(tui_bank_cmd!(
                                *bank,
                                LoadBank,
                                tui::Bank::from_audio(&bd)
                            ))?;
                            self.audio_tx
                                .send(audio_bank_cmd!(*bank, LoadBank, Box::new(bd)))?;
                            self.tui_tx.send(tui::Cmd::Log(std::format!(
                                "load {}!",
                                cx.paths[cx.file_index].to_str().unwrap_or_default()
                            )))?;
                        } else {
                            self.tui_tx.send(tui::Cmd::Log("bad .bd".to_string()))?;
                        }
                    }
                } else {
                    self.tui_tx
                        .send(tui::Cmd::Log("bad fs entry".to_string()))?;
                }
            }
            GlobalState::LoadRd => {
                let cx = self.rd_cx.as_ref().unwrap();
                let path = &cx.paths[cx.file_index];
                if let Ok(entry) = std::fs::metadata(path) {
                    if entry.is_dir() {
                        // open dir
                        let paths = paths!(path.parent(), std::fs::read_dir(path)?, "wav");
                        self.tui_tx.send(tui::Cmd::LoadRd(to_fs!(path.parent(), paths, 0)))?;
                        self.rd_cx = Some(Context {
                            dir: path.clone(),
                            paths,
                            file_index: 0,
                        });
                    } else if entry.is_file()
                        && path.extension().is_some_and(|v| v.to_str() == Some("wav"))
                    {
                        // load rd or default (loop file)
                        if let Ok(bytes) = std::fs::read(path.with_extension("rd")) {
                            if let Ok(rd) = serde_json::from_slice::<angry_surgeon_core::Rd>(&bytes) {
                                self.tui_tx.send(tui::Cmd::LoadOnset {
                                    name: to_fs!(path),
                                    index: 0,
                                    count: rd.onsets.len(),
                                })?;
                                self.state = GlobalState::LoadOnset { rd, onset_index: 0 };
                            } else {
                                self.tui_tx.send(tui::Cmd::Log("bad .rd".to_string()))?;
                            }
                        } else {
                            let rd = angry_surgeon_core::Rd::default();
                            self.tui_tx.send(tui::Cmd::LoadOnset {
                                name: to_fs!(path),
                                index: 0,
                                count: rd.onsets.len(),
                            })?;
                            self.state = GlobalState::LoadOnset { rd, onset_index: 0 };
                        };
                    }
                } else {
                    self.tui_tx
                        .send(tui::Cmd::Log("bad fs entry".to_string()))?;
                }
            }
            GlobalState::LoadOnset { .. } => {
                let cx = self.rd_cx.as_ref().unwrap();
                self.tui_tx
                    .send(tui::Cmd::LoadRd(to_fs!(cx.dir.parent(), cx.paths, cx.file_index)))?;
                self.state = GlobalState::LoadRd;
            }
        }
        Ok(())
    }

    fn decrement(&mut self) -> Result<()> {
        match &mut self.state {
            GlobalState::LoadBd { .. } => {
                let cx = self.bd_cx.as_mut().unwrap();
                dec!(&mut cx.file_index, cx.paths.len());
                self.tui_tx
                    .send(tui::Cmd::LoadBd(to_fs!(cx.dir.parent(), cx.paths, cx.file_index)))?;
            }
            GlobalState::LoadRd => {
                let cx = self.rd_cx.as_mut().unwrap();
                dec!(&mut cx.file_index, cx.paths.len());
                self.tui_tx
                    .send(tui::Cmd::LoadRd(to_fs!(cx.dir.parent(), cx.paths, cx.file_index)))?;
            }
            GlobalState::LoadOnset { rd, onset_index } => {
                let cx = self.rd_cx.as_ref().unwrap();
                dec!(onset_index, rd.onsets.len());
                self.tui_tx.send(tui::Cmd::LoadOnset {
                    name: to_fs!(cx.paths[cx.file_index]),
                    index: *onset_index,
                    count: rd.onsets.len(),
                })?;
            }
            _ => (),
        }
        Ok(())
    }

    fn increment(&mut self) -> Result<()> {
        match &mut self.state {
            GlobalState::LoadBd { .. } => {
                let cx = self.bd_cx.as_mut().unwrap();
                inc!(&mut cx.file_index, cx.paths.len());
                self.tui_tx
                    .send(tui::Cmd::LoadBd(to_fs!(cx.dir.parent(), cx.paths, cx.file_index)))?;
            }
            GlobalState::LoadRd => {
                let cx = self.rd_cx.as_mut().unwrap();
                inc!(&mut cx.file_index, cx.paths.len());
                self.tui_tx
                    .send(tui::Cmd::LoadRd(to_fs!(cx.dir.parent(), cx.paths, cx.file_index)))?;
            }
            GlobalState::LoadOnset { rd, onset_index } => {
                let cx = self.rd_cx.as_ref().unwrap();
                inc!(onset_index, rd.onsets.len());
                self.tui_tx.send(tui::Cmd::LoadOnset {
                    name: to_fs!(cx.paths[cx.file_index]),
                    index: *onset_index,
                    count: rd.onsets.len(),
                })?;
            }
            _ => (),
        }
        Ok(())
    }
}

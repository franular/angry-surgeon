use crate::{audio, tui};
use audio::{BANK_COUNT, PAD_COUNT, MAX_PHRASE_LEN, Bank};
use angry_surgeon_core::{LOOP_DIV, PPQ, STEP_DIV, Event, Fraction, Onset, Rd, Scene, Wav};

use color_eyre::Result;
use midly::{live::LiveEvent, MidiMessage};
use std::{path::{Path, PathBuf}, sync::mpsc::Sender};

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

macro_rules! to_fs_at {
    ($paths:expr,$index:expr) => {
        {
            let mut strings = [const { String::new() }; tui::FILE_COUNT];
            if !$paths.is_empty() {
                for i in 0..tui::FILE_COUNT {
                    let index = ($index as isize + i as isize - tui::FILE_COUNT as isize / 2).rem_euclid($paths.len() as isize) as usize;
                    strings[i] = $paths[index]
                        .file_stem()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_string();
                }
            }
            strings
        }
    }
}

/**
    chords:
        Reverse\*: reverse bank playback
        Hold\*: toggle hold
        Kit\* + Pad\*: load pad's kit
        Shift\* + Reverse\*: init record
            bake phrase \*
            first Pad\*: assign phrase to pad
            more Pad\*s: phrase len
            release Reverse\*: take phrase, assign to first held pad, if any
        Shift\* + Hold\*: init build pool
            Pad\*s: push pads' phrase to pool
            release Hold\*: clear pool if unchanged
        Shift\* + Kit\* + Pad\*: save bank to pad's kit

        Global + HoldB: open onset fs
            KitB: decrement
            ShiftB: increment
            HoldB: into wav/dir
            in wav:
                KitB: decrement
                ShiftB: increment
                Pad\*: assign to first Pad\* onset
                Reverse\* + Pad\*: assign to second Pad\* onset
            release Global: exit fs
        Global + KitB: open scene fs
            KitB: decrement
            ShiftB: increment
            HoldB: load scene / into dir
            release Global: exit fs
        Global + ReverseB: save active scene to new .sd
*/
enum KeyCode {
    BankAOffset = 48,
    ShiftA = 56,
    ReverseA = 57,
    KitA = 58,
    HoldA = 59,

    Global = 60,

    KitB = 61,
    HoldB = 62,
    ShiftB = 63,
    ReverseB = 64,
    BankBOffset = 65,
}

enum CtrlCode {
    GainA = 23,
    SpeedA = 105,
    DriftA = 106,
    BiasA = 29,
    WidthA = 26,

    GainB = 83,
    SpeedB = 102,
    DriftB = 103,
    BiasB = 28,
    WidthB = 24,
}

enum GlobalState {
    Yield,
    Prime,
    LoadScene {
        paths: Vec<Box<Path>>,
        file_index: usize,
    },
    LoadWav {
        paths: Vec<Box<Path>>,
        file_index: usize,
    },
    AssignOnset {
        paths: Vec<Box<Path>>,
        file_index: usize,
        rd: Rd,
        onset_index: usize,
        alt: bool,
    }
}

enum BankState {
    LoadOnset,
    LoadKit,
    AssignKit,
    BakeRecord,
    BuildPool { cleared: bool },
}

struct BankHandler {
    bank: Bank,
    hold: bool,
    reverse: bool,
    downs: Vec<u8>,
    shift: bool,
    state: BankState,
}

impl BankHandler {
    fn new(bank: Bank) -> Self {
        Self {
            bank,
            hold: false,
            reverse: false,
            downs: Vec::new(),
            shift: false,
            state: BankState::LoadOnset,
        }
    }

    fn handle_reverse_up(&mut self, audio_tx: &mut Sender<audio::Cmd>, tui_tx: &mut Sender<tui::Cmd>) -> Result<()> {
        match self.state {
            BankState::LoadOnset => {
                self.reverse = false;
                audio_tx.send(audio_bank_cmd!(self.bank, AssignReverse, false))?;
            }
            BankState::BakeRecord => {
                // exit record
                self.state = BankState::LoadOnset;
                audio_tx.send(audio_bank_cmd!(self.bank, TakeRecord, self.downs.first().copied()))?;
                tui_tx.send(tui_bank_cmd!(self.bank, LoadOnset))?;
            }
            _  => (),
        }
        Ok(())
    }

    fn handle_reverse_down(&mut self, audio_tx: &mut Sender<audio::Cmd>, tui_tx: &mut Sender<tui::Cmd>) -> Result<()> {
        if let BankState::LoadOnset = self.state {
            if self.shift {
                // init record
                self.state = BankState::BakeRecord;
                self.hold = false;
                if self.downs.is_empty() {
                    audio_tx.send(audio_bank_cmd!(self.bank, PushEvent, Event::Sync))?;
                }
                audio_tx.send(audio_bank_cmd!(self.bank, BakeRecord, audio::MAX_PHRASE_LEN as u16))?;
                tui_tx.send(tui_bank_cmd!(self.bank, BakeRecord, None, audio::MAX_PHRASE_LEN as u16))?;
            } else {
                self.reverse = true;
                audio_tx.send(audio_bank_cmd!(self.bank, AssignReverse, true))?;
            }
        }
        Ok(())
    }

    fn handle_hold_up(&mut self, audio_tx: &mut Sender<audio::Cmd>, tui_tx: &mut Sender<tui::Cmd>) -> Result<()> {
        if let BankState::BuildPool { cleared } = self.state {
            // exit build pool
            if !cleared {
                audio_tx.send(audio_bank_cmd!(self.bank, ClearPool))?;
                tui_tx.send(tui_bank_cmd!(self.bank, ClearPool))?;
            }
            self.state = BankState::LoadOnset;
            tui_tx.send(tui_bank_cmd!(self.bank, LoadOnset))?;
        }
        Ok(())
    }

    fn handle_hold_down(&mut self, audio_tx: &mut Sender<audio::Cmd>, tui_tx: &mut Sender<tui::Cmd>) -> Result<()> {
        if let BankState::LoadOnset = self.state {
            if self.shift {
                // init build pool
                self.state = BankState::BuildPool { cleared: false };
                tui_tx.send(tui_bank_cmd!(self.bank, BuildPool))?;
            } else {
                self.hold = !self.hold;
                if !self.hold && self.downs.is_empty() {
                    audio_tx.send(audio_bank_cmd!(self.bank, PushEvent, Event::Sync))?;
                }
            }
        }
        Ok(())
    }

    fn handle_kit_up(&mut self, tui_tx: &mut Sender<tui::Cmd>) -> Result<()> {
        match self.state {
            BankState::LoadKit | BankState::AssignKit => {
                // exit load/assign kit
                self.state = BankState::LoadOnset;
                tui_tx.send(tui_bank_cmd!(self.bank, LoadOnset))?;
            }
            _ => (),
        }
        Ok(())
    }

    fn handle_kit_down(&mut self, tui_tx: &mut Sender<tui::Cmd>) -> Result<()> {
        if let BankState::LoadOnset = self.state {
            if self.shift {
                // init assign kit
                self.state = BankState::AssignKit;
                tui_tx.send(tui_bank_cmd!(self.bank, AssignKit, None))?;
            } else {
                // init load kit
                self.state = BankState::LoadKit;
                tui_tx.send(tui_bank_cmd!(self.bank, LoadKit, None))?;
            }
        }
        Ok(())
    }

    fn handle_pad_up(&mut self, audio_tx: &mut Sender<audio::Cmd>, tui_tx: &mut Sender<tui::Cmd>) -> Result<()> {
        match self.state {
            BankState::LoadOnset => if !self.hold {
                self.handle_pad_input(audio_tx)?;
            }
            BankState::BakeRecord => {
                let len = if self.downs.len() > 1 {
                    let index = self.downs[0];
                    self.downs.iter().skip(1).map(|v| {
                        v.checked_sub(index + 1).unwrap_or(v + PAD_COUNT as u8 - 1 - index)
                    })
                    .fold(0u8, |acc, v| acc | (1 << v)) as u16
                } else {
                    audio::MAX_PHRASE_LEN as u16
                };
                audio_tx.send(audio_bank_cmd!(self.bank, BakeRecord, len))?;
                tui_tx.send(tui_bank_cmd!(self.bank, BakeRecord, self.downs.first().copied(), len))?;
            }
            _ => (),
        }
        Ok(())
    }

    fn handle_pad_down(&mut self, audio_tx: &mut Sender<audio::Cmd>, tui_tx: &mut Sender<tui::Cmd>) -> Result<()> {
        match &mut self.state {
            BankState::LoadOnset => self.handle_pad_input(audio_tx)?,
            BankState::LoadKit => {
                audio_tx.send(audio_bank_cmd!(self.bank, LoadKit, self.downs[0]))?;
                tui_tx.send(tui_bank_cmd!(self.bank, LoadKit, self.downs.first().copied()))?;
            }
            BankState::AssignKit => {
                audio_tx.send(audio_bank_cmd!(self.bank, AssignKit, self.downs[0]))?;
                tui_tx.send(tui_bank_cmd!(self.bank, AssignKit, self.downs.first().copied()))?;
            }
            BankState::BakeRecord => {
                let len = if self.downs.len() > 1 {
                    let index = self.downs[0];
                    self.downs.iter().skip(1).map(|v| {
                        v.checked_sub(index + 1).unwrap_or(v + PAD_COUNT as u8 - 1 - index)
                    })
                    .fold(0u8, |acc, v| acc | (1 << v)) as u16
                } else {
                    audio::MAX_PHRASE_LEN as u16
                };
                audio_tx.send(audio_bank_cmd!(self.bank, BakeRecord, len))?;
                tui_tx.send(tui_bank_cmd!(self.bank, BakeRecord, self.downs.first().copied(), len))?;
            }
            BankState::BuildPool { cleared } => {
                if !*cleared {
                    *cleared = true;
                    audio_tx.send(audio_bank_cmd!(self.bank, ClearPool))?;
                    tui_tx.send(tui_bank_cmd!(self.bank, ClearPool))?;
                }
                audio_tx.send(audio_bank_cmd!(self.bank, PushPool, self.downs[0]))?;
            }
        }
        Ok(())
    }

    fn handle_pad_input(&mut self, audio_tx: &mut Sender<audio::Cmd>)-> Result<()> {
        if let Some(&index) = self.downs.first() {
            if self.downs.len() > 1 {
                // init loop start
                let numerator = self.downs.iter().skip(1).map(|v| {
                    v.checked_sub(index + 1).unwrap_or(v + PAD_COUNT as u8 - 1 - index)
                })
                .fold(0u8, |acc, v| acc | (1 << v));
                let len = Fraction::new(numerator, LOOP_DIV);
                audio_tx.send(audio_bank_cmd!(self.bank, PushEvent, Event::Loop { index, len }))?;
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
}

pub struct InputHandler {
    clock: u8,
    last_step: Option<std::time::Instant>,
    state: GlobalState,
    bank_a: BankHandler,
    bank_b: BankHandler,
    audio_tx: Sender<audio::Cmd>,
    tui_tx: Sender<tui::Cmd>,
}

impl InputHandler {
    pub fn new(audio_tx: Sender<audio::Cmd>, tui_tx: Sender<tui::Cmd>) -> Self {
        Self {
            clock: 0,
            last_step: None,
            state: GlobalState::Yield,
            bank_a: BankHandler::new(Bank::A),
            bank_b: BankHandler::new(Bank::B),
            audio_tx,
            tui_tx,
        }
    }

    pub fn push(&mut self, message: &[u8]) -> Result<()> {
        match LiveEvent::parse(message)? {
            LiveEvent::Midi { message, .. } => {
                match message {
                    MidiMessage::NoteOff { key, .. } => self.parse_note_off(key.as_int())?,
                    MidiMessage::NoteOn { key, .. } => self.parse_note_on(key.as_int())?,
                    MidiMessage::Controller { controller, value } => self.parse_controller(controller.as_int(), value.as_int())?,
                    MidiMessage::PitchBend { bend } => {
                        // affect both banks
                        self.audio_tx.send(audio::Cmd::OffsetSpeed(bend.as_f32() + 1.))?;
                    }
                    _ => (),
                }
            }
            LiveEvent::Realtime(midly::live::SystemRealtime::TimingClock) => self.timing_clock()?,
            LiveEvent::Realtime(midly::live::SystemRealtime::Stop) => self.stop()?,
            _ => (),
        }
        Ok(())
    }

    fn parse_note_off(&mut self, key: u8) -> Result<()> {
        match key {
            v if v == KeyCode::Global as u8 => {
                self.state = GlobalState::Yield;
                self.tui_tx.send(tui::Cmd::Yield)?;
            }
            v if v == KeyCode::ShiftA as u8 => self.bank_a.shift = false,
            v if v == KeyCode::ShiftB as u8 => self.bank_b.shift = false,
            v if v == KeyCode::ReverseA as u8 => if let GlobalState::Yield = self.state {
                self.bank_a.handle_reverse_up(&mut self.audio_tx, &mut self.tui_tx)?;
            }
            v if v == KeyCode::ReverseB as u8 => match &mut self.state {
                GlobalState::Yield => {
                    self.bank_b.handle_reverse_up(&mut self.audio_tx, &mut self.tui_tx)?;
                }
                GlobalState::AssignOnset { paths, file_index, rd, onset_index, alt } => {
                    *alt = false;
                    let name = paths[*file_index].file_stem().unwrap().to_str().unwrap().to_string();
                    self.tui_tx.send(tui::Cmd::AssignOnset { name, index: *onset_index, count: rd.onsets.len(), alt: *alt })?;
                }
                _ => (),
            }
            v if v == KeyCode::HoldA as u8 => if let GlobalState::Yield = self.state {
                self.bank_a.handle_hold_up(&mut self.audio_tx, &mut self.tui_tx)?;
            }
            v if v == KeyCode::HoldB as u8 => if let GlobalState::Yield = self.state {
                self.bank_b.handle_hold_up(&mut self.audio_tx, &mut self.tui_tx)?;
            }
            v if v == KeyCode::KitA as u8 => if let GlobalState::Yield = self.state {
                self.bank_a.handle_kit_up(&mut self.tui_tx)?;
            }
            v if v == KeyCode::KitB as u8 => if let GlobalState::Yield = self.state {
                self.bank_b.handle_kit_up(&mut self.tui_tx)?;
            }
            v if (KeyCode::BankAOffset as u8..KeyCode::BankAOffset as u8 + PAD_COUNT as u8).contains(&v) => {
                let index = PAD_COUNT as u8 - 1 - (v - KeyCode::BankAOffset as u8);
                self.bank_a.downs.retain(|&v| v != index);
                self.tui_tx.send(tui_bank_cmd!(Bank::A, Pad, index, false))?;
                match self.state {
                    GlobalState::Yield => self.bank_a.handle_pad_up(&mut self.audio_tx, &mut self.tui_tx)?,
                    GlobalState::AssignOnset { .. } => self.audio_tx.send(audio_bank_cmd!(Bank::A, ForceEvent, Event::Sync))?,
                    _ => (),
                }
            }
            v if (KeyCode::BankBOffset as u8..KeyCode::BankBOffset as u8 + PAD_COUNT as u8).contains(&v) => {
                let index = v - KeyCode::BankBOffset as u8;
                self.bank_b.downs.retain(|&v| v != index);
                self.tui_tx.send(tui_bank_cmd!(Bank::B, Pad, index, false))?;
                match self.state {
                    GlobalState::Yield => self.bank_b.handle_pad_up(&mut self.audio_tx, &mut self.tui_tx)?,
                    GlobalState::AssignOnset { .. } => self.audio_tx.send(audio_bank_cmd!(Bank::B, ForceEvent, Event::Sync))?,
                    _ => (),
                }
            }
            _ => (),
        }
        Ok(())
    }

    fn parse_note_on(&mut self, key: u8) -> Result<()> {
        match key {
            v if v == KeyCode::Global as u8 => self.state = GlobalState::Prime,
            v if v == KeyCode::ShiftA as u8 => self.bank_a.shift = true,
            v if v == KeyCode::ShiftB as u8 => self.bank_b.shift = true,
            v if v == KeyCode::ReverseA as u8 => if let GlobalState::Yield = self.state {
                self.bank_a.handle_reverse_down(&mut self.audio_tx, &mut self.tui_tx)?;
            }
            v if v == KeyCode::ReverseB as u8 => match &mut self.state {
                GlobalState::Yield => self.bank_b.handle_reverse_down(&mut self.audio_tx, &mut self.tui_tx)?,
                GlobalState::Prime => {
                    // save active scene for both banks
                    let mut index = 0;
                    let mut file = std::fs::File::create_new(format!("scenes/scenes{}.sd", index));
                    while file.is_err() {
                        index += 1;
                        file = std::fs::File::create_new(format!("scenes/scenes{}.sd", index));
                    }
                    self.audio_tx.send(audio::Cmd::SaveScene(file?))?;
                    self.tui_tx.send(tui::Cmd::SaveScene(format!("scenes/scenes{}.sd", index)))?;
                }
                GlobalState::LoadScene { paths, file_index } => {
                    // increment file index
                    *file_index = (*file_index as isize + 1).rem_euclid(paths.len() as isize) as usize;
                    self.tui_tx.send(tui::Cmd::LoadScene(to_fs_at!(paths, *file_index)))?;
                }
                GlobalState::LoadWav { paths, file_index } => {
                    // increment file index
                    *file_index = (*file_index as isize + 1).rem_euclid(paths.len() as isize) as usize;
                    self.tui_tx.send(tui::Cmd::LoadWav(to_fs_at!(paths, *file_index)))?;
                }
                GlobalState::AssignOnset { paths, file_index, rd, onset_index, alt } => {
                    // increment onset index
                    *onset_index = (*onset_index as isize + 1).rem_euclid(rd.onsets.len() as isize) as usize;
                    let name = paths[*file_index].file_stem().unwrap().to_str().unwrap().to_string();
                    self.tui_tx.send(tui::Cmd::AssignOnset { name, index: *onset_index, count: rd.onsets.len(), alt: *alt })?;
                }
            }
            v if v == KeyCode::HoldA as u8 => if let GlobalState::Yield = self.state {
                self.bank_a.handle_hold_down(&mut self.audio_tx, &mut self.tui_tx)?;
            }
            v if v == KeyCode::HoldB as u8 => match &mut self.state {
                GlobalState::Yield => self.bank_b.handle_hold_down(&mut self.audio_tx, &mut self.tui_tx)?,
                GlobalState::Prime => {
                    // open onset dir
                    let mut paths = std::fs::read_dir("onsets")?
                        .flat_map(|v| Some(v.ok()?.path().into_boxed_path()))
                        .filter(|v| v.extension().unwrap() == "wav" || v.is_dir())
                        .collect::<Vec<_>>();
                    paths.sort();
                    self.tui_tx.send(tui::Cmd::LoadWav(to_fs_at!(paths, 0)))?;
                    self.state = GlobalState::LoadWav {
                        paths,
                        file_index: 0,
                    }
                }
                GlobalState::LoadScene { paths, file_index } => {
                    if paths.is_empty() {
                        self.state = GlobalState::Yield;
                        self.tui_tx.send(tui::Cmd::Yield)?;
                    } else {
                        let path = &paths[*file_index];
                        if path.is_dir() {
                            // enter subdirectory
                            let mut paths = if path.parent().unwrap() == PathBuf::from("") {
                                // in ./scenes; don't include ".."
                                Vec::new()
                            } else {
                                // in subdirectory; include ".."
                                vec![path.parent().unwrap().into()]
                            };
                            paths.extend(std::fs::read_dir(path)?
                                .flat_map(|v| Some(v.ok()?.path().into_boxed_path()))
                                .filter(|v| v.extension().unwrap() == "sd" || v.is_dir())
                            );
                            paths.sort();
                            self.tui_tx.send(tui::Cmd::LoadScene(to_fs_at!(paths, 0)))?;
                            self.state = GlobalState::LoadScene { paths, file_index: 0 };
                        } else {
                            // load scene
                            let sd_string = std::fs::read_to_string(&paths[*file_index])?;
                            let scene: Scene<BANK_COUNT, PAD_COUNT, MAX_PHRASE_LEN> = serde_json::from_str(&sd_string)?;
                            self.tui_tx.send(tui::Cmd::AssignScene(Box::new(tui::Scene::from_audio(&scene))))?;
                            self.audio_tx.send(audio::Cmd::LoadScene(Box::new(scene)))?;
                        }
                    }
                }
                GlobalState::LoadWav { ref paths, file_index } => {
                    if paths.is_empty() {
                        self.state = GlobalState::Yield;
                        self.tui_tx.send(tui::Cmd::Yield)?;
                    } else {
                        let path = &paths[*file_index];
                        if path.is_dir() {
                            // enter subdirectory
                            let mut paths = if path.parent().unwrap() == PathBuf::from("") {
                                // in ./scenes; don't include ".."
                                Vec::new()
                            } else {
                                // in subdirectory; include ".."
                                vec![path.parent().unwrap().into()]
                            };
                            paths.extend(std::fs::read_dir(path)?
                                .flat_map(|v| Some(v.ok()?.path().into_boxed_path()))
                                .filter(|v| v.extension().unwrap() == "wav" || v.is_dir())
                            );
                            paths.sort();
                            self.tui_tx.send(tui::Cmd::LoadWav(to_fs_at!(paths, 0)))?;
                            self.state = GlobalState::LoadWav { paths, file_index: 0 };
                        } else {
                            // enter onset selection
                            let rd_string = std::fs::read_to_string(path.with_extension("rd"))?;
                            let rd: Rd = serde_json::from_str(&rd_string)?;
                            let name = path.file_stem().unwrap().to_str().unwrap().to_string();
                            self.tui_tx.send(tui::Cmd::AssignOnset { name, index: 0, count: rd.onsets.len(), alt: false })?;
                            self.state = GlobalState::AssignOnset {
                                paths: paths.clone(),
                                file_index: *file_index,
                                rd,
                                onset_index: 0,
                                alt: false,
                            };
                        }
                    }
                }
                GlobalState::AssignOnset { paths, file_index, .. } => {
                    // exit onset selection, return to dir
                    self.tui_tx.send(tui::Cmd::LoadWav(to_fs_at!(paths, *file_index)))?;
                    self.state = GlobalState::LoadWav { paths: paths.clone(), file_index: 0 };
                }
            }
            v if v == KeyCode::KitA as u8 => if let GlobalState::Yield = self.state {
                self.bank_a.handle_kit_down(&mut self.tui_tx)?;
            }
            v if v == KeyCode::KitB as u8 => match &mut self.state {
                GlobalState::Yield => self.bank_b.handle_kit_down(&mut self.tui_tx)?,
                GlobalState::Prime => {
                    // open scene dir
                    let mut paths = std::fs::read_dir("scenes")?
                        .flat_map(|v| Some(v.ok()?.path().into_boxed_path()))
                        .filter(|v| v.extension().unwrap() == "sd" || v.is_dir())
                        .collect::<Vec<_>>();
                    paths.sort();
                    self.tui_tx.send(tui::Cmd::LoadScene(to_fs_at!(paths, 0)))?;
                    self.state = GlobalState::LoadScene {
                        paths,
                        file_index: 0,
                    };
                }
                GlobalState::LoadScene { paths, file_index } => {
                    // decrement file index
                    *file_index = (*file_index as isize - 1).rem_euclid(paths.len() as isize) as usize;
                    self.tui_tx.send(tui::Cmd::LoadScene(to_fs_at!(paths, *file_index)))?;
                }
                GlobalState::LoadWav { paths, file_index } => {
                    // decrement file index
                    *file_index = (*file_index as isize - 1).rem_euclid(paths.len() as isize) as usize;
                    self.tui_tx.send(tui::Cmd::LoadWav(to_fs_at!(paths, *file_index)))?;
                }
                GlobalState::AssignOnset { paths, file_index, rd, onset_index, alt } => {
                    // decrement onset index
                    *onset_index = (*onset_index as isize - 1).rem_euclid(rd.onsets.len() as isize) as usize;
                    let name = paths[*file_index].file_stem().unwrap().to_str().unwrap().to_string();
                    self.tui_tx.send(tui::Cmd::AssignOnset { name, index: *onset_index, count: rd.onsets.len(), alt: *alt })?;
                }
            }
            v if (KeyCode::BankAOffset as u8..KeyCode::BankAOffset as u8 + PAD_COUNT as u8).contains(&v) => {
                let index = PAD_COUNT as u8 - 1 - (v - KeyCode::BankAOffset as u8);
                self.bank_a.downs.push(index);
                self.tui_tx.send(tui_bank_cmd!(Bank::A, Pad, index, true))?;
                match &self.state {
                    GlobalState::Yield => {
                        self.bank_a.handle_pad_down(&mut self.audio_tx, &mut self.tui_tx)?;
                    }
                    GlobalState::AssignOnset { paths, file_index, rd, onset_index, alt } => {
                        // assign onset to pad
                        let len = std::fs::metadata(&paths[*file_index])?.len() - 44;
                        let start = rd.onsets[*onset_index];
                        let wav = Wav {
                            tempo: rd.tempo,
                            steps: rd.steps,
                            path: paths[*file_index].to_str().unwrap().to_string().into_boxed_str(),
                            len,
                        };
                        let onset = Onset { wav, start };
                        self.audio_tx.send(audio_bank_cmd!(Bank::A, AssignOnset, index, *alt, Box::new(onset)))?;
                    }
                    _ => (),
                }
            }
            v if (KeyCode::BankBOffset as u8..KeyCode::BankBOffset as u8 + PAD_COUNT as u8).contains(&v) => {
                let index = v - KeyCode::BankBOffset as u8;
                self.bank_b.downs.push(index);
                self.tui_tx.send(tui_bank_cmd!(Bank::B, Pad, index, true))?;
                match &self.state {
                    GlobalState::Yield => {
                        self.bank_b.handle_pad_down(&mut self.audio_tx, &mut self.tui_tx)?;
                    }
                    GlobalState::AssignOnset { paths, file_index, rd, onset_index, alt } => {
                        // assign onset to pad
                        let len = std::fs::metadata(&paths[*file_index])?.len() - 44;
                        let start = rd.onsets[*onset_index];
                        let wav = Wav {
                            tempo: rd.tempo,
                            steps: rd.steps,
                            path: paths[*file_index].to_str().unwrap().to_string().into_boxed_str(),
                            len,
                        };
                        let onset = Onset { wav, start };
                        self.audio_tx.send(audio_bank_cmd!(Bank::B, AssignOnset, index, *alt, Box::new(onset)))?;
                    }
                    _ => (),
                }
            }
            _ => (),
        }
        Ok(())
    }

    fn parse_controller(&mut self, controller: u8, value: u8) -> Result<()> {
        match controller {
            v if v == CtrlCode::GainA as u8 => {
                self.audio_tx.send(audio_bank_cmd!(Bank::A, AssignGain, value as f32 / 127. * 2.))?;
            }
            v if v == CtrlCode::SpeedA as u8 => {
                self.audio_tx.send(audio_bank_cmd!(Bank::A, AssignSpeed, value as f32 / 127. * 2.))?;
            }
            v if v == CtrlCode::DriftA as u8 => {
                self.audio_tx.send(audio_bank_cmd!(Bank::A, AssignDrift, value as f32 / 127.))?;
                self.tui_tx.send(tui_bank_cmd!(Bank::A, AssignDrift, value))?;
            }
            v if v == CtrlCode::BiasA as u8 => {
                self.audio_tx.send(audio_bank_cmd!(Bank::A, AssignBias, value as f32 / 127.))?;
                self.tui_tx.send(tui_bank_cmd!(Bank::A, AssignBias, value))?;
            }
            v if v == CtrlCode::WidthA as u8 => {
                self.audio_tx.send(audio_bank_cmd!(Bank::A, AssignWidth, value as f32 / 127.))?;
            }
            v if v == CtrlCode::GainB as u8 => {
                self.audio_tx.send(audio_bank_cmd!(Bank::B, AssignGain, value as f32 / 127. * 2.))?;
            }
            v if v == CtrlCode::SpeedB as u8 => {
                self.audio_tx.send(audio_bank_cmd!(Bank::B, AssignSpeed, value as f32 / 127. * 2.))?;
            }
            v if v == CtrlCode::DriftB as u8 => {
                self.audio_tx.send(audio_bank_cmd!(Bank::B, AssignDrift, value as f32 / 127.))?;
                self.tui_tx.send(tui_bank_cmd!(Bank::B, AssignDrift, value))?;
            }
            v if v == CtrlCode::BiasB as u8 => {
                self.audio_tx.send(audio_bank_cmd!(Bank::B, AssignBias, value as f32 / 127.))?;
                self.tui_tx.send(tui_bank_cmd!(Bank::B, AssignBias, value))?;
            }
            v if v == CtrlCode::WidthB as u8 => {
                self.audio_tx.send(audio_bank_cmd!(Bank::B, AssignWidth, value as f32 / 127.))?;
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
        self.last_step = None;
        self.clock = 0;
        self.audio_tx.send(audio::Cmd::Stop)?;
        self.tui_tx.send(tui::Cmd::Stop)?;
        Ok(())
    }
}

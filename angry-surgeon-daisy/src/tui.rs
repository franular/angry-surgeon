use crate::audio::{BANK_COUNT, MAX_PHRASE_LEN, PAD_COUNT};
use heapless::{String, Vec};

pub const FILE_COUNT: usize = 5;
pub const ROWS: usize = 6;
pub const COLS: usize = 21;

#[derive(Default)]
pub struct Kit {
    pub onsets: [bool; PAD_COUNT],
}

#[derive(Default)]
pub struct Bank {
    pub kits: [Option<Kit>; PAD_COUNT],
    pub phrases: [bool; PAD_COUNT],
}

#[derive(Default)]
pub struct Sd {
    pub banks: [Bank; BANK_COUNT],
}

impl Sd {
    pub fn from_audio(sd: &crate::input::Sd) -> Self {
        let mut ret = Self::default();
        for (rbank, wbank) in sd.banks.iter().zip(ret.banks.iter_mut()) {
            for (rkit, wkit) in rbank.kits.iter().zip(wbank.kits.iter_mut()) {
                for i in 0..rkit.onsets.len() {
                    if rkit.onsets[i].is_some() {
                        wkit.get_or_insert_default().onsets[i] = true;
                    }
                }
            }
            for i in 0..rbank.phrases.len() {
                if rbank.phrases[i].is_some() {
                    wbank.phrases[i] = true;
                }
            }
        }
        ret
    }
}

pub enum Cmd {
    Clock,
    Stop,
    Yield,
    AssignScene(Sd),
    SaveScene(String<COLS>),
    LoadScene([String<COLS>; FILE_COUNT]),
    LoadWav([String<COLS>; FILE_COUNT]),
    AssignOnset {
        name: String<COLS>,
        index: usize,
        count: usize,
    },
    Bank(crate::audio::Bank, BankCmd),
}

pub enum BankCmd {
    Pad(u8, bool),
    LoadOnset,
    AssignPhraseDrift(u8),
    AssignKitDrift(u8),
    LoadKit(Option<u8>),
    ClearOnset(Option<u8>),
    BakeRecord(Option<u8>, u16),
    ClearPool,
    BuildPool,
}

enum GlobalState {
    Yield,
    LoadScene {
        paths: [String<COLS>; FILE_COUNT],
    },
    LoadWav {
        paths: [String<COLS>; FILE_COUNT],
    },
    AssignOnset {
        name: String<COLS>,
        index: usize,
        count: usize,
    },
}

enum BankState {
    LoadOnset,
    LoadKit { index: Option<u8> },
    ClearOnset { index: Option<u8> },
    BakeRecord { index: Option<u8>, len: u16 },
    BuildPool,
}

struct BankHandler {
    phrase_drift: u8,
    kit_drift: u8,
    kit_index: usize,
    bank: Bank,
    downs: Vec<u8, PAD_COUNT>,
    pool: Vec<u8, MAX_PHRASE_LEN>,
    state: BankState,
}

impl BankHandler {
    fn new() -> Self {
        Self {
            phrase_drift: 0,
            kit_drift: 0,
            kit_index: 0,
            bank: Bank::default(),
            downs: Vec::new(),
            pool: Vec::new(),
            state: BankState::LoadOnset,
        }
    }

    fn cmd(&mut self, cmd: BankCmd) {
        match cmd {
            BankCmd::Pad(index, down) => self.pad(index, down),
            BankCmd::LoadOnset => self.load_onset(),
            BankCmd::AssignPhraseDrift(v) => self.phrase_drift = v,
            BankCmd::AssignKitDrift(v) => self.kit_drift = v,
            BankCmd::LoadKit(index) => self.load_kit(index),
            BankCmd::ClearOnset(index) => self.clear_onset(index),
            BankCmd::BakeRecord(index, len) => self.state = BankState::BakeRecord { index, len },
            BankCmd::BuildPool => self.state = BankState::BuildPool,
            BankCmd::ClearPool => self.pool.clear(),
        }
    }

    fn pad(&mut self, index: u8, down: bool) {
        if down {
            self.downs.push(index);
            if let BankState::BuildPool = &mut self.state {
                self.pool.push(index);
            }
        } else {
            self.downs.retain(|v| *v != index);
        }
    }

    fn load_onset(&mut self) {
        if let BankState::BakeRecord {
            index: Some(index), ..
        } = self.state
        {
            self.bank.phrases[index as usize] = true;
        }
        self.state = BankState::LoadOnset;
    }

    fn load_kit(&mut self, index: Option<u8>) {
        if let Some(index) = index {
            self.kit_index = index as usize;
        }
        self.state = BankState::LoadKit { index };
    }

    fn clear_onset(&mut self, index: Option<u8>) {
        if let Some(index) = index {
            if let Some(kit) = &mut self.bank.kits[self.kit_index] {
                kit.onsets[index as usize] = false;
            }
        }
        self.state = BankState::ClearOnset { index };
    }
}

pub struct TuiHandler {
    clock: bool,
    state: GlobalState,
    banks: [BankHandler; BANK_COUNT],
}

#[embassy_executor::task]
pub async fn tui_handler(
    mut tui_handler: TuiHandler,
    mut cmd_rx: embassy_sync::channel::DynamicReceiver<'static, Cmd>,
) {
    loop {
        match cmd_rx.receive().await {
            Cmd::Clock => {}
            Cmd::Stop => {}
            Cmd::Yield => {}
            Cmd::AssignScene(sd) => {}
            Cmd::SaveScene(fs) => {}
            Cmd::LoadScene(fs) => {}
            Cmd::LoadWav(fs) => {}
            Cmd::AssignOnset { name, index, count } => {}
            Cmd::Bank(bank, cmd) => {
                todo!()
            }
        }
    }
}

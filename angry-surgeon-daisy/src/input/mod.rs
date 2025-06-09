use core::str::FromStr;

use crate::{
    audio::{self, MAX_PHRASE_LEN, PAD_COUNT},
    fs::hw::Dir,
    tui,
};
use angry_surgeon_core::{Event, Onset, Wav};
use embassy_stm32::gpio::Level;
use embassy_sync::{
    blocking_mutex::raw::NoopRawMutex,
    channel::{DynamicReceiver, DynamicSender},
};
use embedded_io_async::Write;

pub mod analog;
pub mod digital;
pub mod i2c;
pub mod touch;

macro_rules! audio_bank_cmd {
    ($bank:expr,$cmd:ident) => {
        audio::Cmd::Bank($bank, audio::BankCmd::$cmd)
    };
    ($bank:expr,$cmd:ident,$($params:tt)+) => {
        audio::Cmd::Bank($bank, audio::BankCmd::$cmd($($params)+))
    };
}
use audio_bank_cmd;

macro_rules! tui_bank_cmd {
    ($bank:expr,$cmd:ident) => {
        tui::Cmd::Bank($bank, tui::BankCmd::$cmd)
    };
    ($bank:expr,$cmd:ident,$($params:tt)+) => {
        tui::Cmd::Bank($bank, tui::BankCmd::$cmd($($params)+))
    };
}
use tui_bank_cmd;

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

macro_rules! log {
    ($tx:expr,$($arg:tt)*) => {
        let text = heapless::String::from_str(&alloc::format!($($arg)*)).unwrap();
        $tx.send(tui::Cmd::Log(text)).await;
    }
}

macro_rules! names {
    ($dir:expr,$ext:expr) => {{
        let mut names = alloc::vec::Vec::new();
        let mut iter = $dir.iter();
        while let Some(Ok(entry)) = iter.next().await {
            let name =
                alloc::string::String::from_utf16(entry.long_file_name_as_ucs2_units().unwrap())
                    .unwrap();
            if entry.is_dir() || name.ends_with($ext) {
                names.push(name);
            }
        }
        names
    }};
}

macro_rules! to_fs {
    ($names:expr,$index:expr,$ext:expr) => {{
        let mut strings = [const { heapless::String::new() }; tui::FILE_COUNT];
        if !$names.is_empty() {
            for i in 0..tui::FILE_COUNT {
                let index = ($index as isize + i as isize - tui::FILE_COUNT as isize / 2);
                if (0..$names.len() as isize).contains(&index) {
                    let name = &$names[index as usize];
                    let string_len = strings[i].len();
                    unsafe {
                        if name.len() > string_len {
                            strings[i]
                                .as_bytes_mut()
                                .copy_from_slice(name[..string_len].as_bytes());
                            if name.ends_with($ext) {
                                strings[i].as_bytes_mut()[string_len - $ext.len()..]
                                    .copy_from_slice($ext.as_bytes());
                            }
                        } else {
                            strings[i].as_bytes_mut()[..name.len()]
                                .copy_from_slice(name.as_bytes());
                        }
                    }
                }
            }
        }
        strings
    }};
    ($name:expr,$ext:expr) => {{
        let mut string = heapless::String::new();
        let string_len = string.len();
        unsafe {
            if $name.len() > string_len {
                string
                    .as_bytes_mut()
                    .copy_from_slice($name[..string_len].as_bytes());
                if $name.ends_with($ext) {
                    string.as_bytes_mut()[string_len - $ext.len()..]
                        .copy_from_slice($ext.as_bytes());
                }
            } else {
                string.as_bytes_mut()[..$name.len()].copy_from_slice($name.as_bytes());
            }
        }
        string
    }};
}

pub type Bank = angry_surgeon_core::Bank<PAD_COUNT, MAX_PHRASE_LEN>;
type BdChannel = embassy_sync::channel::Channel<NoopRawMutex, Bank, 1>;

async fn ancestors(path: &mut alloc::string::String, parent: &Dir<'_>) {
    if let Ok(entry) = parent.open_meta("..").await {
        alloc::boxed::Box::pin(ancestors(path, &entry.to_dir())).await;
        path.push_str(
            &alloc::string::String::from_utf16(entry.long_file_name_as_ucs2_units().unwrap())
                .unwrap(),
        );
    }
}

// mpr121 electrode index of modifiers
enum Index {
    BankOffset = 0,
    Shift = 8,
    Reverse = 9,
    Hold = 10,
    Kit = 11,
}

struct Context<'d> {
    dir: Dir<'d>,
    file_index: usize,
    names: alloc::vec::Vec<alloc::string::String>,
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

#[derive(PartialEq)]
enum BankState {
    LoadOnset,
    LoadKit,
    BakeRecord,
    BuildPool { cleared: bool },
}

struct BankHandler {
    bank: audio::Bank,
    hold: bool,
    reverse: bool,
    downs: heapless::Vec<u8, { audio::PAD_COUNT }>,
    shift: bool,
    state: BankState,
}

impl BankHandler {
    fn new(bank: audio::Bank) -> Self {
        Self {
            bank,
            hold: false,
            reverse: false,
            downs: heapless::Vec::new(),
            shift: false,
            state: BankState::LoadOnset,
        }
    }

    async fn reverse_up(
        &mut self,
        audio_tx: &DynamicSender<'_, audio::Cmd<'_>>,
        tui_tx: &DynamicSender<'_, tui::Cmd>,
    ) {
        match self.state {
            BankState::LoadOnset => {
                self.reverse = false;
                audio_tx
                    .send(audio_bank_cmd!(self.bank, AssignReverse, false))
                    .await;
            }
            BankState::BakeRecord => {
                // exit record
                self.state = BankState::LoadOnset;
                audio_tx
                    .send(audio_bank_cmd!(
                        self.bank,
                        TakeRecord,
                        self.downs.first().copied()
                    ))
                    .await;
                tui_tx.send(tui_bank_cmd!(self.bank, LoadOnset)).await;
            }
            _ => (),
        }
    }

    async fn reverse_down(
        &mut self,
        audio_tx: &DynamicSender<'_, audio::Cmd<'_>>,
        tui_tx: &DynamicSender<'_, tui::Cmd>,
    ) {
        if self.state == BankState::LoadOnset {
            if self.shift {
                // init record
                self.state = BankState::BakeRecord;
                self.hold = false;
                if self.downs.is_empty() {
                    audio_tx
                        .send(audio_bank_cmd!(self.bank, PushEvent, Event::Sync))
                        .await;
                }
                audio_tx
                    .send(audio_bank_cmd!(
                        self.bank,
                        BakeRecord,
                        audio::MAX_PHRASE_LEN as u16
                    ))
                    .await;
                tui_tx
                    .send(tui_bank_cmd!(
                        self.bank,
                        BakeRecord,
                        None,
                        audio::MAX_PHRASE_LEN as u16
                    ))
                    .await;
            } else {
                self.reverse = true;
                audio_tx
                    .send(audio_bank_cmd!(self.bank, AssignReverse, true))
                    .await;
            }
        }
    }

    async fn hold_up(
        &mut self,
        audio_tx: &DynamicSender<'_, audio::Cmd<'_>>,
        tui_tx: &DynamicSender<'_, tui::Cmd>,
    ) {
        if let BankState::BuildPool { cleared } = self.state {
            // exit build pool
            if !cleared {
                audio_tx.send(audio_bank_cmd!(self.bank, ClearPool)).await;
                tui_tx.send(tui_bank_cmd!(self.bank, ClearPool)).await;
            }
            self.state = BankState::LoadOnset;
            tui_tx.send(tui_bank_cmd!(self.bank, LoadOnset)).await;
        }
    }

    async fn hold_down(
        &mut self,
        audio_tx: &DynamicSender<'_, audio::Cmd<'_>>,
        tui_tx: &DynamicSender<'_, tui::Cmd>,
    ) {
        if self.state == BankState::LoadOnset {
            if self.shift {
                // init build pool
                self.state = BankState::BuildPool { cleared: false };
                tui_tx.send(tui_bank_cmd!(self.bank, PushPool, None)).await;
            } else {
                self.hold = !self.hold;
                if !self.hold && self.downs.is_empty() {
                    audio_tx
                        .send(audio_bank_cmd!(self.bank, PushEvent, Event::Sync))
                        .await;
                }
            }
        }
    }

    async fn kit_up(&mut self, tui_tx: &DynamicSender<'_, tui::Cmd>) {
        match self.state {
            BankState::LoadKit => {
                // exit load kit
                self.state = BankState::LoadOnset;
                tui_tx.send(tui_bank_cmd!(self.bank, LoadOnset)).await;
            }
            _ => (),
        }
    }

    async fn kit_down(&mut self, tui_tx: &DynamicSender<'_, tui::Cmd>) {
        if self.state == BankState::LoadOnset && !self.shift {
            // init load kit
            self.state = BankState::LoadKit;
            tui_tx.send(tui_bank_cmd!(self.bank, LoadKit, None)).await;
        }
        // bank save handled in InputHandler
    }

    async fn pad_up(
        &mut self,
        audio_tx: &DynamicSender<'_, audio::Cmd<'_>>,
        tui_tx: &DynamicSender<'_, tui::Cmd>,
    ) {
        match self.state {
            BankState::LoadOnset => {
                if !self.hold {
                    self.pad_input(audio_tx).await;
                }
            }
            BankState::BakeRecord => {
                let len = if self.downs.len() > 1 {
                    self.binary_offset(self.downs[0])
                } else {
                    audio::MAX_PHRASE_LEN as u16
                };
                audio_tx
                    .send(audio_bank_cmd!(self.bank, BakeRecord, len))
                    .await;
                tui_tx
                    .send(tui_bank_cmd!(
                        self.bank,
                        BakeRecord,
                        self.downs.first().copied(),
                        len
                    ))
                    .await;
            }
            _ => (),
        }
    }

    async fn pad_down(
        &mut self,
        audio_tx: &DynamicSender<'_, audio::Cmd<'_>>,
        tui_tx: &DynamicSender<'_, tui::Cmd>,
    ) {
        match &mut self.state {
            BankState::LoadOnset => self.pad_input(audio_tx).await,
            BankState::LoadKit => {
                audio_tx
                    .send(audio_bank_cmd!(self.bank, LoadKit, self.downs[0]))
                    .await;
                tui_tx
                    .send(tui_bank_cmd!(
                        self.bank,
                        LoadKit,
                        self.downs.first().copied()
                    ))
                    .await;
            }
            BankState::BakeRecord => {
                let len = if self.downs.len() > 1 {
                    self.binary_offset(self.downs[0])
                } else {
                    audio::MAX_PHRASE_LEN as u16
                };
                audio_tx
                    .send(audio_bank_cmd!(self.bank, BakeRecord, len))
                    .await;
                tui_tx
                    .send(tui_bank_cmd!(
                        self.bank,
                        BakeRecord,
                        self.downs.first().copied(),
                        len
                    ))
                    .await;
            }
            BankState::BuildPool { cleared } => {
                if !*cleared {
                    *cleared = true;
                    audio_tx.send(audio_bank_cmd!(self.bank, ClearPool)).await;
                    tui_tx.send(tui_bank_cmd!(self.bank, ClearPool)).await;
                }
                audio_tx
                    .send(audio_bank_cmd!(
                        self.bank,
                        PushPool,
                        *self.downs.last().unwrap()
                    ))
                    .await;
                tui_tx
                    .send(tui_bank_cmd!(
                        self.bank,
                        PushPool,
                        self.downs.last().copied()
                    ))
                    .await;
            }
        }
    }

    async fn pad_input(&mut self, audio_tx: &DynamicSender<'_, audio::Cmd<'_>>) {
        if let Some(&index) = self.downs.first() {
            if self.downs.len() > 1 {
                // init loop start
                let len = self.binary_offset(index);
                audio_tx
                    .send(audio_bank_cmd!(
                        self.bank,
                        PushEvent,
                        Event::Loop { index, len }
                    ))
                    .await;
            } else {
                // init loop stop | jump
                audio_tx
                    .send(audio_bank_cmd!(self.bank, PushEvent, Event::Hold { index }))
                    .await;
            }
        } else {
            // init sync
            audio_tx
                .send(audio_bank_cmd!(self.bank, PushEvent, Event::Sync))
                .await;
        }
    }

    fn binary_offset(&self, index: u8) -> u16 {
        self.downs
            .iter()
            .skip(1)
            .map(|v| {
                v.checked_sub(index + 1)
                    .unwrap_or(v + audio::PAD_COUNT as u8 - 1 - index)
            })
            .fold(0u16, |acc, v| acc | (1 << v))
    }
}

pub struct InputHandler<'d> {
    root: Dir<'d>,
    onsets_cx: Option<Context<'d>>,
    banks_cx: Option<Context<'d>>,
    banks_maybe_focus: Option<audio::Bank>,
    state: GlobalState,
    bank_a: BankHandler,
    bank_b: BankHandler,
}

impl<'d> InputHandler<'d> {
    pub fn new(root: Dir<'d>) -> Self {
        Self {
            root,
            onsets_cx: None,
            banks_cx: None,
            banks_maybe_focus: None,
            state: GlobalState::Yield,
            bank_a: BankHandler::new(audio::Bank::A),
            bank_b: BankHandler::new(audio::Bank::B),
        }
    }

    /// save bank to new file
    async fn save_bank(
        &self,
        bank: audio::Bank,
        bank_ch: &'static BdChannel,
        audio_tx: &DynamicSender<'_, audio::Cmd<'_>>,
        tui_tx: &DynamicSender<'_, tui::Cmd>,
    ) {
        audio_tx
            .send(audio_bank_cmd!(bank, SaveBank, bank_ch.dyn_sender()))
            .await;
        let bd = bank_ch.receive().await;
        if let Ok(bytes) = postcard::to_allocvec(&bd) {
            let mut index = 0;
            while self
                .root
                .exists(&alloc::format!("banks/bank{}.bd", index))
                .await
                .unwrap()
            {
                index += 1;
            }
            let mut bd_file = self
                .root
                .create_file(&alloc::format!("banks/bank{}.bd", index))
                .await
                .unwrap();
            bd_file.write_all(&bytes).await.unwrap();

            let name = heapless::String::from_str(&alloc::format!("bank{}", index)).unwrap();
            tui_tx.send(tui::Cmd::Log(name)).await;
        }
    }

    /// load target dir/.bd/.rd depending on state
    async fn open(
        &mut self,
        bank_ch: &'static BdChannel,
        audio_tx: &DynamicSender<'_, audio::Cmd<'_>>,
        tui_tx: &DynamicSender<'_, tui::Cmd>,
    ) {
        match &self.state {
            GlobalState::Yield => {
                if let Some(bank) = self.banks_maybe_focus.take() {
                    // trans load bd
                    if let Some(cx) = &mut self.banks_cx {
                        // recall dir
                        let names = names!(cx.dir, ".bd");
                        tui_tx.send(tui::Cmd::LoadBd(to_fs!(names, cx.file_index, ".bd"))).await;
                        cx.names = names;
                        self.state = GlobalState::LoadBd { bank };
                    } else if let Ok(dir) = self.root.open_dir("banks").await {
                        // open /banks
                        let names = names!(dir, ".bd");
                        tui_tx.send(tui::Cmd::LoadBd(to_fs!(names, 0, ".bd"))).await;
                        self.banks_cx = Some(Context {
                            dir,
                            file_index: 0,
                            names,
                        });
                        self.state = GlobalState::LoadBd { bank };
                    } else {
                        log!(tui_tx, "no /banks found");
                    }
                } else {
                    // trans load rd
                    if let Some(cx) = &mut self.onsets_cx {
                        // recall dir
                        let names = names!(cx.dir, ".rd");
                        tui_tx.send(tui::Cmd::LoadRd(to_fs!(names, cx.file_index, ".rd"))).await;
                        cx.names = names;
                        self.state = GlobalState::LoadRd;
                    } else if let Ok(dir) = self.root.open_dir("onsets").await {
                        // open /onsets
                        let names = names!(dir, ".rd");
                        tui_tx.send(tui::Cmd::LoadRd(to_fs!(names, 0, ".rd"))).await;
                        self.onsets_cx = Some(Context {
                            dir,
                            file_index: 0,
                            names,
                        });
                        self.state = GlobalState::LoadRd;
                    } else {
                        log!(tui_tx, "no /onsets found");
                    }
                }
            }
            GlobalState::LoadBd { bank } => {
                let cx = self.banks_cx.as_ref().unwrap();
                if let Ok(entry) = cx.dir.open_meta(&cx.names[cx.file_index]).await {
                    if entry.is_dir() {
                        // open dir
                        let dir = entry.to_dir();
                        let names = names!(dir, ".bd");
                        tui_tx.send(tui::Cmd::LoadBd(to_fs!(names, 0, ".bd"))).await;
                        self.banks_cx = Some(Context {
                            dir,
                            file_index: 0,
                            names,
                        });
                    } else if entry.is_file() && cx.names[cx.file_index].ends_with(".bd") {
                        // load bd
                        let mut bd_file = entry.to_file();
                        let mut reader = crate::fs::BufReader::new(&mut bd_file);
                        let mut bytes = alloc::vec::Vec::new();
                        while let Ok(Some(c)) = reader.next().await {
                            bytes.push(c);
                        }
                        if let Ok(bd) = postcard::from_bytes::<Bank>(&bytes) {
                            tui_tx
                                .send(tui_bank_cmd!(*bank, LoadBank, tui::Bank::from_audio(&bd)))
                                .await;
                            bank_ch.send(bd).await;
                            audio_tx
                                .send(audio_bank_cmd!(*bank, LoadBank, bank_ch.dyn_receiver()))
                                .await;
                            log!(tui_tx, "load {}!", cx.names[cx.file_index]);
                        } else {
                            log!(tui_tx, "bad .bd");
                        }
                    }
                } else {
                    log!(tui_tx, "bad fs entry");
                }
            }
            GlobalState::LoadRd => {
                let cx = self.onsets_cx.as_ref().unwrap();
                if let Ok(entry) = cx.dir.open_meta(&cx.names[cx.file_index]).await {
                    if entry.is_dir() {
                        // open dir
                        let dir = entry.to_dir();
                        let names = names!(dir, ".rd");
                        tui_tx.send(tui::Cmd::LoadRd(to_fs!(names, 0, ".rd"))).await;
                        self.onsets_cx = Some(Context {
                            dir,
                            file_index: 0,
                            names,
                        });
                    } else if entry.is_file() && cx.names[cx.file_index].ends_with(".rd") {
                        // load rd
                        let mut rd_file = entry.to_file();
                        let mut reader = crate::fs::BufReader::new(&mut rd_file);
                        let mut bytes = alloc::vec::Vec::new();
                        while let Ok(Some(c)) = reader.next().await {
                            bytes.push(c);
                        }
                        if let Ok(rd) = postcard::from_bytes::<angry_surgeon_core::Rd>(&bytes) {
                            let name = cx.names[cx.file_index].clone();
                            tui_tx
                                .send(tui::Cmd::LoadOnset {
                                    name: to_fs!(name, ".rd"),
                                    index: 0,
                                    count: rd.onsets.len(),
                                })
                                .await;
                            self.state = GlobalState::LoadOnset { rd, onset_index: 0 };
                        } else {
                            log!(tui_tx, "bad .rd");
                        }
                    }
                } else {
                    log!(tui_tx, "bad fs entry");
                }
            }
            GlobalState::LoadOnset { .. } => self.state = GlobalState::LoadRd,
        }
    }

    async fn decrement(&mut self, tui_tx: &DynamicSender<'_, tui::Cmd>) {
        match &mut self.state {
            GlobalState::LoadBd { .. } => {
                let cx = self.banks_cx.as_mut().unwrap();
                dec!(&mut cx.file_index, cx.names.len());
                tui_tx
                    .send(tui::Cmd::LoadBd(to_fs!(cx.names, cx.file_index, ".bd")))
                    .await;
            }
            GlobalState::LoadRd => {
                let cx = self.onsets_cx.as_mut().unwrap();
                dec!(&mut cx.file_index, cx.names.len());
                tui_tx
                    .send(tui::Cmd::LoadRd(to_fs!(cx.names, cx.file_index, ".rd")))
                    .await;
            }
            GlobalState::LoadOnset { rd, onset_index } => {
                let cx = self.onsets_cx.as_ref().unwrap();
                dec!(onset_index, rd.onsets.len());
                tui_tx
                    .send(tui::Cmd::LoadOnset {
                        name: to_fs!(cx.names[cx.file_index], ".rd"),
                        index: *onset_index,
                        count: rd.onsets.len(),
                    })
                    .await;
            }
            _ => (),
        }
    }

    async fn increment(&mut self, tui_tx: &DynamicSender<'_, tui::Cmd>) {
        match &mut self.state {
            GlobalState::LoadBd { .. } => {
                let cx = self.banks_cx.as_mut().unwrap();
                inc!(&mut cx.file_index, cx.names.len());
                tui_tx
                    .send(tui::Cmd::LoadBd(to_fs!(cx.names, cx.file_index, ".bd")))
                    .await;
            }
            GlobalState::LoadRd => {
                let cx = self.onsets_cx.as_mut().unwrap();
                inc!(&mut cx.file_index, cx.names.len());
                tui_tx
                    .send(tui::Cmd::LoadRd(to_fs!(cx.names, cx.file_index, ".rd")))
                    .await;
            }
            GlobalState::LoadOnset { rd, onset_index } => {
                let cx = self.onsets_cx.as_ref().unwrap();
                inc!(onset_index, rd.onsets.len());
                tui_tx
                    .send(tui::Cmd::LoadOnset {
                        name: to_fs!(cx.names[cx.file_index], ".rd"),
                        index: *onset_index,
                        count: rd.onsets.len(),
                    })
                    .await;
            }
            _ => (),
        }
    }

    async fn touch_up(
        &mut self,
        bank: audio::Bank,
        index: u8,
        shift_tx: &DynamicSender<'static, (audio::Bank, bool)>,
        audio_tx: &DynamicSender<'_, audio::Cmd<'_>>,
        tui_tx: &DynamicSender<'_, tui::Cmd>,
    ) {
        let my_bank = match bank {
            audio::Bank::A => &mut self.bank_a,
            audio::Bank::B => &mut self.bank_b,
        };
        if index == Index::Shift as u8 {
            my_bank.shift = false;
            // unfocus for load bd
            if !self.bank_a.shift && !self.bank_b.shift {
                self.banks_maybe_focus = None;
            }
            shift_tx.send((bank, false)).await;
        } else {
            if matches!(
                self.state,
                GlobalState::LoadBd { .. } | GlobalState::LoadRd
            ) {
                tui_tx.send(tui::Cmd::Yield).await;
                self.state = GlobalState::Yield;
            }
            if (0..Index::BankOffset as u8).contains(&index) {
                my_bank.downs.retain(|&i| i != index);
                tui_tx.send(tui_bank_cmd!(bank, Pad, index, false)).await;
                if let GlobalState::LoadOnset { .. } = self.state {
                    audio_tx
                        .send(audio_bank_cmd!(bank, ForceEvent, Event::Sync))
                        .await;
                } else {
                    my_bank.pad_up(audio_tx, tui_tx).await;
                }
            } else if index == Index::Reverse as u8 {
                my_bank.reverse_up(audio_tx, tui_tx).await;
            } else if index == Index::Hold as u8 {
                my_bank.hold_up(audio_tx, tui_tx).await;
            } else if index == Index::Kit as u8 {
                my_bank.kit_up(tui_tx).await;
            }
        }
    }

    async fn touch_down(
        &mut self,
        bank: audio::Bank,
        index: u8,
        bank_ch: &'static BdChannel,
        shift_tx: &DynamicSender<'static, (audio::Bank, bool)>,
        audio_tx: &DynamicSender<'_, audio::Cmd<'_>>,
        tui_tx: &DynamicSender<'_, tui::Cmd>,
    ) {
        let my_bank = match bank {
            audio::Bank::A => &mut self.bank_a,
            audio::Bank::B => &mut self.bank_b,
        };
        if index == Index::Shift as u8 {
            my_bank.shift = true;
            // focus for load bd
            self.banks_maybe_focus = Some(bank);
            shift_tx.send((bank, true)).await;
        } else if index == Index::Kit as u8 {
            if let GlobalState::LoadBd { bank: b } = &mut self.state {
                // target bank for load bank
                *b = bank;
            } else if my_bank.shift {
                self.save_bank(bank, bank_ch, audio_tx, tui_tx).await;
            } else {
                my_bank.kit_down(tui_tx).await;
            }
        } else {
            if matches!(self.state, GlobalState::LoadRd | GlobalState::LoadBd { .. }) {
                tui_tx.send(tui::Cmd::Yield).await;
                self.state = GlobalState::Yield;
            }
            if (0..Index::BankOffset as u8).contains(&index) {
                let _ = my_bank.downs.push(index);
                tui_tx.send(tui_bank_cmd!(bank, Pad, index, true)).await;
                if let GlobalState::LoadOnset { rd, onset_index } = &mut self.state {
                    let cx = self.onsets_cx.as_ref().unwrap();
                    let name = &cx.names[cx.file_index];
                    // get full wav path from rd name
                    let mut path = alloc::string::String::new();
                    ancestors(&mut path, &cx.dir).await;
                    path.push_str(&alloc::format!("{}wav", &name[..name.len() - 2]));
                    if let Ok(meta) = cx
                        .dir
                        .open_meta(&alloc::format!("{}wav", &name[..name.len() - 2]))
                        .await
                    {
                        // assign onset to pad
                        let onset = Onset {
                            wav: Wav {
                                tempo: rd.tempo,
                                steps: rd.steps,
                                path: path.clone(),
                                len: meta.len() - 44,
                            },
                            start: rd.onsets[*onset_index],
                        };
                        audio_tx
                            .send(audio_bank_cmd!(bank, AssignOnset, index, onset))
                            .await;
                    } else {
                        log!(tui_tx, "no wav found");
                    }
                } else {
                    my_bank.pad_down(audio_tx, tui_tx).await;
                }
            } else if index == Index::Reverse as u8 {
                my_bank.reverse_down(audio_tx, tui_tx).await;
            } else if index == Index::Hold as u8 {
                my_bank.hold_down(audio_tx, tui_tx).await;
            }
        }
    }
}

#[embassy_executor::task]
pub async fn input(
    mut input: InputHandler<'static>,
    mut mpr121_a: touch::Mpr121<
        'static,
        i2c::Ref<
            'static,
            NoopRawMutex,
            embassy_stm32::i2c::I2c<'static, embassy_stm32::mode::Async>,
        >,
    >,
    mut mpr121_b: touch::Mpr121<
        'static,
        i2c::Ref<
            'static,
            NoopRawMutex,
            embassy_stm32::i2c::I2c<'static, embassy_stm32::mode::Async>,
        >,
    >,
    shift_tx: DynamicSender<'static, (audio::Bank, bool)>,
    audio_tx: DynamicSender<'static, audio::Cmd<'static>>,
    tui_tx: DynamicSender<'static, tui::Cmd>,
    encoder_sw_rx: DynamicReceiver<'static, Level>,
    encoder_rx: DynamicReceiver<'static, digital::Direction>,
) {
    use embassy_futures::select::*;

    static BANK_CH: static_cell::StaticCell<
        embassy_sync::channel::Channel<NoopRawMutex, Bank, 1>,
    > = static_cell::StaticCell::new();
    let bank_ch = BANK_CH.init_with(embassy_sync::channel::Channel::new);

    let mut last_touched_a = 0u16;
    let mut last_touched_b = 0u16;
    loop {
        match select4(
            encoder_sw_rx.receive(),
            encoder_rx.receive(),
            mpr121_a.wait_for_touched(),
            mpr121_b.wait_for_touched(),
        )
        .await
        {
            Either4::First(level) => {
                if level == Level::Low {
                    input.open(bank_ch, &audio_tx, &tui_tx).await;
                }
            }
            Either4::Second(direction) => match direction {
                digital::Direction::Counterclockwise => input.decrement(&tui_tx).await,
                digital::Direction::Clockwise => input.increment(&tui_tx).await,
            },
            Either4::Third(curr_touched) => {
                let curr_touched = curr_touched.unwrap();
                for index in 0..12 {
                    let curr = (curr_touched >> index) & 1;
                    let last = (last_touched_a >> index) & 1;
                    if curr != last {
                        if curr == 0 {
                            input
                                .touch_up(audio::Bank::A, index, &shift_tx, &audio_tx, &tui_tx)
                                .await;
                        } else {
                            input
                                .touch_down(
                                    audio::Bank::A,
                                    index,
                                    bank_ch,
                                    &shift_tx,
                                    &audio_tx,
                                    &tui_tx,
                                )
                                .await;
                        }
                    }
                }
                last_touched_a = curr_touched;
            }
            Either4::Fourth(curr_touched) => {
                let curr_touched = curr_touched.unwrap();
                for index in 0..12 {
                    let curr = (curr_touched >> index) & 1;
                    let last = (last_touched_b >> index) & 1;
                    if curr != last {
                        if curr == 0 {
                            input
                                .touch_up(audio::Bank::B, index, &shift_tx, &audio_tx, &tui_tx)
                                .await;
                        } else {
                            input
                                .touch_down(
                                    audio::Bank::B,
                                    index,
                                    bank_ch,
                                    &shift_tx,
                                    &audio_tx,
                                    &tui_tx,
                                )
                                .await;
                        }
                    }
                }
                last_touched_b = curr_touched;
            }
        }
    }
}

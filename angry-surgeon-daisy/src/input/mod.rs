use crate::{
    audio::{self, BANK_COUNT, MAX_PHRASE_LEN, PAD_COUNT},
    fs::hw::Dir,
    tui,
};
use alloc::string::ToString;
use angry_surgeon_core::{Event, Fraction, Onset, Wav};
use embassy_stm32::gpio::Level;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::DynamicSender};
use embassy_time::WithTimeout;
use embedded_io_async::Write;

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

macro_rules! tui_bank_cmd {
    ($bank:expr,$cmd:ident) => {
        tui::Cmd::Bank($bank, tui::BankCmd::$cmd)
    };
    ($bank:expr,$cmd:ident,$($params:tt)+) => {
        tui::Cmd::Bank($bank, tui::BankCmd::$cmd($($params)+))
    };
}

pub type Sd<'d> = angry_surgeon_core::Sd<BANK_COUNT, PAD_COUNT, MAX_PHRASE_LEN>;

// mpr121 electrode index of modifiers
enum Index {
    BankOffset = 0,
    Shift = 8,
    Reverse = 9,
    Hold = 10,
    Kit = 11,
}

#[allow(clippy::large_enum_variant)]
enum GlobalState<'d> {
    Yield,
    Prime,
    LoadScene {
        dir: Dir<'d>,
        file_index: usize,
        file_count: usize,
        path: alloc::string::String,
    },
    LoadWav {
        dir: Dir<'d>,
        file_index: usize,
        file_count: usize,
        path: alloc::string::String,
    },
    AssignOnset {
        dir: Dir<'d>,
        file_index: usize,
        file_count: usize,
        path: alloc::string::String,
        rd: angry_surgeon_core::Rd,
        onset_index: usize,
    },
}

enum BankState {
    LoadOnset,
    LoadKit,
    ClearOnset,
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
        if let BankState::LoadOnset = self.state {
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
        if let BankState::LoadOnset = self.state {
            if self.shift {
                // init build pool
                self.state = BankState::BuildPool { cleared: false };
                tui_tx.send(tui_bank_cmd!(self.bank, BuildPool)).await;
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
            BankState::LoadKit | BankState::ClearOnset => {
                // exit load kit/clear onset
                self.state = BankState::LoadOnset;
                tui_tx.send(tui_bank_cmd!(self.bank, LoadOnset)).await;
            }
            _ => (),
        }
    }

    async fn kit_down(&mut self, tui_tx: &DynamicSender<'_, tui::Cmd>) {
        if let BankState::LoadOnset = self.state {
            if self.shift {
                // init clear onset
                self.state = BankState::ClearOnset;
                tui_tx
                    .send(tui_bank_cmd!(self.bank, ClearOnset, None))
                    .await;
            } else {
                // init load kit
                self.state = BankState::LoadKit;
                tui_tx.send(tui_bank_cmd!(self.bank, LoadKit, None)).await;
            }
        }
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
            BankState::ClearOnset => {
                audio_tx
                    .send(audio_bank_cmd!(self.bank, ClearOnset, self.downs[0]))
                    .await;
                tui_tx
                    .send(tui_bank_cmd!(
                        self.bank,
                        ClearOnset,
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
                    .send(audio_bank_cmd!(self.bank, PushPool, self.downs[0]))
                    .await;
            }
        }
    }

    async fn pad_input(&mut self, audio_tx: &DynamicSender<'_, audio::Cmd<'_>>) {
        if let Some(&index) = self.downs.first() {
            if self.downs.len() > 1 {
                // init loop start
                let numerator = self.binary_offset(index);
                let len = Fraction::new(numerator, audio::LOOP_DIV);
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
    clock: u16,
    last_step: Option<embassy_time::Instant>,
    state: GlobalState<'d>,
    bank_a: BankHandler,
    bank_b: BankHandler,
}

impl<'d> InputHandler<'d> {
    pub fn new() -> Self {
        Self {
            clock: 0,
            last_step: None,
            state: GlobalState::Yield,
            bank_a: BankHandler::new(audio::Bank::A),
            bank_b: BankHandler::new(audio::Bank::B),
        }
    }

    async fn file_count(dir: &Dir<'d>) -> usize {
        let mut iter = dir.iter();
        let mut file_count = 0;
        while let Some(Ok(entry)) = iter.next().await {
            let name = core::str::from_utf8(entry.short_file_name_as_bytes()).unwrap();
            if entry.is_dir() || entry.is_file() && name.ends_with("RD\0") {
                file_count += 1;
            }
        }
        file_count
    }

    fn decrement(index: &mut usize, count: usize) {
        if *index == 0 {
            *index = count - 1;
        } else {
            *index -= 1;
        }
    }

    fn increment(index: &mut usize, count: usize) {
        if *index == count - 1 {
            *index = 0;
        } else {
            *index += 1;
        }
    }

    // async fn load_scene(&self, source: Scene<'d>) -> Result<Scene<'d>, crate::fs::hw::Error<'d>> {
    //     let mut scene = angry_surgeon_core::Scene::default();
    //     for (rbank, wbank) in source.banks.into_iter().zip(scene.banks.iter_mut()) {
    //         for (rpad, wpad) in rbank.pads.into_iter().zip(wbank.pads.iter_mut()) {
    //             for (rgroup, wgroup) in rpad.kit.onsets.into_iter().zip(wpad.kit.onsets.iter_mut()) {
    //                 for (ronset, wonset) in rgroup.into_iter().zip(wgroup.iter_mut()) {
    //                     if let Some(ronset) = ronset {
    //                         let len = self.root.open_meta(&ronset.wav.path).await.unwrap().len() - 44;
    //                         // parse rd
    //                         let path = ronset.wav.path.strip_suffix("wav").unwrap().to_string() + "rd";
    //                         if let Ok(mut rd_file) = self.root.open_file(&path).await {
    //                             let mut reader = crate::fs::BufReader::new(&mut rd_file);
    //                             let mut bytes: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    //                             while let Ok(Some(c)) = reader.next().await {
    //                                 bytes.push(c);
    //                             }
    //                             if let Ok(rd) = postcard::from_bytes::<angry_surgeon_core::Rd>(&bytes[..]) {
    //                                 *wonset = Some(Onset {
    //                                     wav: Wav {
    //                                         tempo: rd.tempo,
    //                                         steps: rd.steps,
    //                                         path: ronset.wav.path,
    //                                         len,
    //                                     },
    //                                     start: ronset.start,
    //                                 });
    //                             }
    //                         }
    //                     }
    //                 }
    //             }
    //             wpad.phrase = rpad.phrase;
    //         }
    //     }
    //     Ok(scene)
    // }

    // async fn save_scene(&self, scene: Scene<'d>) -> Result<, crate::fs::hw::Error<'d>> {
    //     let mut sd = crate::fs::Sd::default();
    //     for (rbank, wbank) in scene.banks.into_iter().zip(sd.banks.iter_mut()) {
    //         for (rpad, wpad) in rbank.pads.into_iter().zip(wbank.pads.iter_mut()) {
    //             for (rgroup, wgroup) in rpad.kit.onsets.into_iter().zip(wpad.kit.onsets.iter_mut()) {
    //                 for (ronset, wonset) in rgroup.into_iter().zip(wgroup.iter_mut()) {
    //                     if let Some(ronset) = ronset {
    //                     }
    //                 }
    //             }
    //         }
    //     }
    //     Ok(sd)
    // }

    // async fn entry_open(dir: &Dir<'d>, mut index: usize, count: usize) -> Result<(), Error<'d>> {
    //     let mut iter = dir.iter();
    //     while let Some(Ok(entry)) = iter.next().await {
    //         let name = core::str::from_utf8(entry.short_file_name_as_bytes()).unwrap();
    //         if entry.is_dir() {
    //             index += 1;
    //             if index == count {
    //                 return Ok(Entry::Dir(entry.to_dir()));
    //             }
    //         } else if entry.is_file() && name.ends_with("RD\0") {
    //             index += 1;
    //             if index == count {
    //                 let mut file = entry.to_file();
    //                 let mut reader = crate::fs::BufReader::new(&mut file);
    //                 let mut parser = crate::fs::RdParser::new();
    //                 while let Some(c) = reader.next().await? {
    //                     parser.parse(c);
    //                 }
    //                 let rd = parser.take();
    //                 return Ok(Entry::Rd(rd))
    //             }
    //         }
    //     }
    //     // FIXME: actually return dir or rd data
    //     Ok(())
    // }
}

#[embassy_executor::task]
pub async fn input(
    root: Dir<'static>,
    mut input: InputHandler<'static>,
    mut scenes_sw: digital::Debounce<'static>,
    mut onsets_sw: digital::Debounce<'static>,
    mut encoder: digital::Encoder<'static>,
    mut mpr121: touch::Mpr121<
        'static,
        i2c::Ref<
            'static,
            NoopRawMutex,
            embassy_stm32::i2c::I2c<'static, embassy_stm32::mode::Async>,
        >,
    >,
    audio_tx: DynamicSender<'static, audio::Cmd<'static>>,
    tui_tx: DynamicSender<'static, tui::Cmd>,
) {
    use embassy_futures::select::*;

    static SCENE_CH: static_cell::StaticCell<embassy_sync::channel::Channel<NoopRawMutex, Sd, 1>> =
        static_cell::StaticCell::new();
    let scene_ch = SCENE_CH.init_with(embassy_sync::channel::Channel::new);

    let mut last_touched = 0u16;
    loop {
        match select4(
            scenes_sw.wait_for_any_edge(),
            onsets_sw.wait_for_any_edge(),
            encoder.wait_for_direction(),
            mpr121.wait_for_touched(),
        )
        .await
        {
            Either4::First(level) => {
                if level == Level::Low {
                    if scenes_sw
                        .wait_for_any_edge()
                        .with_timeout(embassy_time::Duration::from_secs(2))
                        .await
                        .is_err()
                    {
                        // save scene to new file
                        audio_tx
                            .send(crate::audio::Cmd::SaveScene(scene_ch.dyn_sender()))
                            .await;
                        let sd = scene_ch.receive().await;
                        if let Ok(bytes) = postcard::to_allocvec(&sd) {
                            let mut index = 0;
                            while root
                                .exists(&alloc::format!("scenes/scene{}.sd", index))
                                .await
                                .unwrap()
                            {
                                index += 1;
                            }
                            let mut sd_file = root
                                .create_file(&alloc::format!("scenes/scene{}.sd", index))
                                .await
                                .unwrap();
                            sd_file.write_all(&bytes).await.unwrap();
                        }
                    } else {
                        // load scene
                        match input.state {
                            GlobalState::LoadScene {
                                ref dir,
                                file_index,
                                ref path,
                                ..
                            } => {
                                // find entry
                                let mut iter = dir.iter();
                                let mut i = 0;
                                while let Some(Ok(entry)) = iter.next().await {
                                    let name = alloc::string::String::from_utf16(
                                        entry.long_file_name_as_ucs2_units().unwrap(),
                                    )
                                    .unwrap();
                                    if entry.is_dir() {
                                        i += 1;
                                        if i == file_index {
                                            // open found dir
                                            let dir = entry.to_dir();
                                            let file_count = InputHandler::file_count(&dir).await;
                                            let path = if name == ".." {
                                                path.rsplit_once('/').unwrap().0.to_string()
                                            } else {
                                                path.clone() + "/" + &name
                                            };
                                            input.state = GlobalState::LoadScene {
                                                dir,
                                                file_index: 0,
                                                file_count,
                                                path,
                                            };
                                            break;
                                        }
                                    } else if entry.is_file() && name.ends_with(".sd") {
                                        i += 1;
                                        if i == file_index {
                                            // load found scene
                                            let mut sd_file = entry.to_file();
                                            let mut reader =
                                                crate::fs::BufReader::new(&mut sd_file);
                                            let mut bytes: alloc::vec::Vec<u8> =
                                                alloc::vec::Vec::new();
                                            while let Ok(Some(c)) = reader.next().await {
                                                bytes.push(c);
                                            }
                                            if let Ok(sd) = postcard::from_bytes::<Sd>(&bytes) {
                                                scene_ch.send(sd).await;
                                                audio_tx
                                                    .send(crate::audio::Cmd::LoadScene(
                                                        scene_ch.dyn_receiver(),
                                                    ))
                                                    .await;
                                            }
                                            break;
                                        }
                                    }
                                }
                            }
                            _ => {
                                if let Ok(dir) = root.open_dir("scenes").await {
                                    let file_count = InputHandler::file_count(&root).await;
                                    input.state = GlobalState::LoadScene {
                                        dir,
                                        file_index: 0,
                                        file_count,
                                        path: "scenes".to_string(),
                                    };
                                } else {
                                    // TODO: display helpful error message
                                }
                            }
                        }
                    }
                }
            }
            Either4::Second(level) => {
                if level == Level::Low {
                    match input.state {
                        GlobalState::LoadWav {
                            ref dir,
                            file_index,
                            file_count,
                            ref path,
                        } => {
                            // find entry
                            let mut iter = dir.iter();
                            let mut i = 0;
                            while let Some(Ok(entry)) = iter.next().await {
                                let name =
                                    core::str::from_utf8(entry.short_file_name_as_bytes()).unwrap();
                                if entry.is_dir() {
                                    i += 1;
                                    if i == file_index {
                                        // open found dir
                                        let dir = entry.to_dir();
                                        let file_count = InputHandler::file_count(&dir).await;
                                        let path = if name == ".." {
                                            path.rsplit_once('/').unwrap().0.to_string()
                                        } else {
                                            path.clone() + "/" + &name
                                        };
                                        input.state = GlobalState::LoadWav {
                                            dir,
                                            file_index: 0,
                                            file_count,
                                            path,
                                        };
                                        break;
                                    }
                                } else if entry.is_file() && name.ends_with(".rd") {
                                    i += 1;
                                    if i == file_index {
                                        // open found rd
                                        let mut rd_file = entry.to_file();
                                        let mut reader = crate::fs::BufReader::new(&mut rd_file);
                                        let mut bytes: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
                                        while let Ok(Some(c)) = reader.next().await {
                                            bytes.push(c);
                                        }
                                        // wav's filename from rd's
                                        let name =
                                            path.rsplit_once('.').unwrap().0.to_string() + "wav";
                                        if let Ok(rd) =
                                            postcard::from_bytes::<angry_surgeon_core::Rd>(&bytes)
                                        {
                                            input.state = GlobalState::AssignOnset {
                                                dir: dir.clone(),
                                                file_index,
                                                file_count,
                                                path: path.clone() + "/" + &name,
                                                rd,
                                                onset_index: 0,
                                            }
                                        }
                                        break;
                                    }
                                }
                            }
                        }
                        GlobalState::AssignOnset {
                            dir,
                            file_index,
                            file_count,
                            path,
                            ..
                        } => {
                            // exit wav
                            input.state = GlobalState::LoadWav {
                                dir: dir.clone(),
                                file_index,
                                file_count,
                                path: path.rsplit_once('/').unwrap().0.to_string(),
                            };
                        }
                        _ => {
                            if let Ok(dir) = root.open_dir("onsets").await {
                                let file_count = InputHandler::file_count(&root).await;
                                input.state = GlobalState::LoadWav {
                                    dir,
                                    file_index: 0,
                                    file_count,
                                    path: "onsets".to_string(),
                                };
                            } else {
                                // TODO: display helpful error message
                            }
                        }
                    }
                }
            }
            Either4::Third(direction) => match direction {
                digital::Direction::Counterclockwise => match &mut input.state {
                    GlobalState::LoadScene {
                        file_index,
                        file_count,
                        ..
                    } => {
                        InputHandler::decrement(file_index, *file_count);
                    }
                    GlobalState::LoadWav {
                        file_index,
                        file_count,
                        ..
                    } => {
                        InputHandler::decrement(file_index, *file_count);
                    }
                    GlobalState::AssignOnset {
                        rd, onset_index, ..
                    } => {
                        InputHandler::decrement(onset_index, rd.onsets.len());
                    }
                    _ => (),
                },
                digital::Direction::Clockwise => match &mut input.state {
                    GlobalState::LoadScene {
                        file_index,
                        file_count,
                        ..
                    } => {
                        InputHandler::increment(file_index, *file_count);
                    }
                    GlobalState::LoadWav {
                        file_index,
                        file_count,
                        ..
                    } => {
                        InputHandler::increment(file_index, *file_count);
                    }
                    GlobalState::AssignOnset {
                        rd, onset_index, ..
                    } => {
                        InputHandler::increment(onset_index, rd.onsets.len());
                    }
                    _ => (),
                },
            },
            Either4::Fourth(curr_touched) => {
                let curr_touched = curr_touched.unwrap();
                for index in 0..12 {
                    let curr = (curr_touched >> index) & 1;
                    let last = (last_touched >> index) & 1;
                    if curr != last {
                        if curr == 0 {
                            match index {
                                i if (0..Index::BankOffset as u8).contains(&i) => {
                                    input.bank_a.downs.retain(|&i| i != index);
                                    tui_tx
                                        .send(tui_bank_cmd!(audio::Bank::A, Pad, index, false))
                                        .await;
                                    match input.state {
                                        GlobalState::Yield => {
                                            input.bank_a.pad_up(&audio_tx, &tui_tx).await
                                        }
                                        GlobalState::AssignOnset { .. } => {
                                            audio_tx
                                                .send(audio_bank_cmd!(
                                                    audio::Bank::A,
                                                    ForceEvent,
                                                    Event::Sync
                                                ))
                                                .await
                                        }
                                        _ => (),
                                    }
                                }
                                i if i == Index::Shift as u8 => input.bank_a.shift = false,
                                i if i == Index::Reverse as u8 => {
                                    if let GlobalState::Yield = input.state {
                                        input.bank_a.reverse_up(&audio_tx, &tui_tx).await;
                                    }
                                }
                                i if i == Index::Hold as u8 => {
                                    if let GlobalState::Yield = input.state {
                                        input.bank_a.hold_up(&audio_tx, &tui_tx).await;
                                    }
                                }
                                i if i == Index::Kit as u8 => {
                                    if let GlobalState::Yield = input.state {
                                        input.bank_a.kit_up(&tui_tx).await;
                                    }
                                }
                                _ => unreachable!(),
                            }
                        } else {
                            match index {
                                i if (0..Index::BankOffset as u8).contains(&i) => {
                                    input.bank_a.downs.push(index).unwrap();
                                    tui_tx
                                        .send(tui_bank_cmd!(audio::Bank::A, Pad, index, true))
                                        .await;
                                    match &mut input.state {
                                        GlobalState::Yield => {
                                            input.bank_a.pad_down(&audio_tx, &tui_tx).await
                                        }
                                        GlobalState::AssignOnset {
                                            path,
                                            rd,
                                            onset_index,
                                            ..
                                        } => {
                                            if let Ok(meta) = root.open_meta(&path).await {
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
                                                    .send(audio_bank_cmd!(
                                                        audio::Bank::A,
                                                        AssignOnset,
                                                        index,
                                                        onset
                                                    ))
                                                    .await;
                                            } else {
                                                // TODO: display helpful error message
                                            }
                                        }
                                        _ => (),
                                    }
                                }
                                i if i == Index::Shift as u8 => input.bank_a.shift = true,
                                i if i == Index::Reverse as u8 => {
                                    if let GlobalState::Yield = input.state {
                                        input.bank_a.reverse_down(&audio_tx, &tui_tx).await;
                                    }
                                }
                                i if i == Index::Hold as u8 => {
                                    if let GlobalState::Yield = input.state {
                                        input.bank_a.hold_down(&audio_tx, &tui_tx).await;
                                    }
                                }
                                i if i == Index::Kit as u8 => {
                                    if let GlobalState::Yield = input.state {
                                        input.bank_a.kit_down(&tui_tx).await;
                                    }
                                }
                                _ => unreachable!(),
                            }
                        }
                    }
                }
                last_touched = curr_touched;
            }
        }
    }
}

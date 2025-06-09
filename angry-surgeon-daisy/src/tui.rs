use crate::audio::{BANK_COUNT, PAD_COUNT};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embedded_graphics::{mono_font, pixelcolor::BinaryColor, text, Drawable};
use heapless::{Deque, String, Vec};
use ssd1306::mode::DisplayConfigAsync;

macro_rules! format6 {
    ($filler:expr,$($arg:tt)*) => {{
        let mut text = [$filler; 6];
        let bytes = alloc::format!($($arg)*).into_bytes();
        let len = bytes.len().min(text.len());
        text[..len].copy_from_slice(&bytes[..len]);
        text
    }}
}

macro_rules! format14 {
    ($filler:expr,$($arg:tt)*) => {{
        let mut text = [$filler; 14];
        let bytes = alloc::format!($($arg)*).into_bytes();
        let len = bytes.len().min(text.len());
        text[..len].copy_from_slice(&bytes[..len]);
        text
    }}
}

macro_rules! write_pads {
    ($text:expr,$a:expr,$b:expr) => {
        for i in 0..2 {
            $text[ROWS - 2 + i] = {
                let mut text = [0u8; 14];
                for j in 0..4 {
                    text[1 + j] = $a[4 * i + j];
                    text[9 + j] = $b[4 * i + j];
                }
                Some(Text::base(text))
            }
        }
    };
    ($text:expr,$pads:expr) => {
        for i in 0..2 {
            $text[ROWS - 2 + i] = {
                let mut text = [0u8; 6];
                for j in 0..4 {
                    text[1 + j] = $pads[4 * i + j];
                }
                Some(Text::base(text))
            };
        }
    };
}

macro_rules! draw_border {
    ($display:expr,$offset:expr,$width:expr) => {
        let mut border = [[b' '; $width]; ROWS];
        border[0] = [1; $width];
        border[ROWS - 1] = [1; $width];
        for i in [0, $width - 1] {
            for b in border.iter_mut().map(|v| &mut v[i]) {
                *b = 2;
            }
        }
        border[0][0] = 5;
        border[0][$width - 1] = 8;
        border[ROWS - 1][0] = 11;
        border[ROWS - 1][$width - 1] = 14;
        for i in 0..border.len() {
            text::Text::with_text_style(
                core::str::from_utf8(&border[i]).unwrap(),
                ($offset, (i as i32 * 8)).into(),
                BASE_CHAR_STYLE,
                TEXT_STYLE,
            )
            .draw($display)
            .unwrap();
        }
    };
}

pub const FILE_COUNT: usize = 5;
const ROWS: usize = 8;
const COLS: usize = 16;
const TEXT_STYLE: text::TextStyle = text::TextStyleBuilder::new()
    .alignment(text::Alignment::Left)
    .baseline(text::Baseline::Top)
    .build();
const BASE_CHAR_STYLE: mono_font::MonoTextStyle<BinaryColor> =
    mono_font::MonoTextStyleBuilder::new()
        .font(&ibm437::IBM437_8X8_REGULAR)
        .text_color(BinaryColor::On)
        .background_color(BinaryColor::Off)
        .build();
const INVERT_CHAR_STYLE: mono_font::MonoTextStyle<BinaryColor> =
    mono_font::MonoTextStyleBuilder::new()
        .font(&ibm437::IBM437_8X8_REGULAR)
        .text_color(BinaryColor::Off)
        .background_color(BinaryColor::On)
        .build();

type Ssd1306<'d> = ssd1306::Ssd1306Async<
    ssd1306::prelude::I2CInterface<
        crate::input::i2c::Ref<
            'd,
            NoopRawMutex,
            embassy_stm32::i2c::I2c<'d, embassy_stm32::mode::Async>,
        >,
    >,
    ssd1306::size::DisplaySize128x64,
    ssd1306::mode::BufferedGraphicsModeAsync<ssd1306::size::DisplaySize128x64>,
>;

struct Text<const N: usize> {
    string: [u8; N],
    invert: bool,
}

impl<const N: usize> Text<N> {
    fn base(string: [u8; N]) -> Self {
        Self {
            string,
            invert: false,
        }
    }

    fn invert(string: [u8; N]) -> Self {
        Self {
            string,
            invert: true,
        }
    }
}

#[derive(Default)]
pub struct Kit {
    pub onsets: [bool; PAD_COUNT],
}

#[derive(Default)]
pub struct Bank {
    pub kits: [Option<Kit>; PAD_COUNT],
    pub phrases: [bool; PAD_COUNT],
}

impl Bank {
    pub fn from_audio(bank: &crate::input::Bank) -> Self {
        let mut ret = Self::default();
        for (rkit, wkit) in bank.kits.iter().zip(ret.kits.iter_mut()) {
            for i in 0..rkit.onsets.len() {
                if rkit.onsets[i].is_some() {
                    wkit.get_or_insert_default().onsets[i] = true;
                }
            }
        }
        for i in 0..bank.phrases.len() {
            if bank.phrases[i].is_some() {
                ret.phrases[i] = true;
            }
        }
        ret
    }
}

pub enum Cmd {
    Yield,
    Log(String<COLS>),
    LoadBd([String<COLS>; FILE_COUNT]),
    LoadRd([String<COLS>; FILE_COUNT]),
    LoadOnset {
        name: String<COLS>,
        index: usize,
        count: usize,
    },
    Bank(crate::audio::Bank, BankCmd),
}

pub enum BankCmd {
    Pad(u8, bool),
    LoadOnset,
    AssignGain(f32),
    AssignWidth(f32),
    AssignSpeed(f32),
    AssignRoll(f32),
    AssignKitDrift(f32),
    AssignPhraseDrift(f32),

    LoadBank(Bank),
    LoadKit(Option<u8>),

    BakeRecord(Option<u8>, u16),
    ClearPool,
    PushPool(Option<u8>),
}

enum GlobalState {
    Yield,
    LoadBd {
        paths: [String<COLS>; FILE_COUNT],
    },
    LoadRd {
        paths: [String<COLS>; FILE_COUNT],
    },
    LoadOnset {
        name: String<COLS>,
        index: usize,
        count: usize,
    },
}

enum BankState {
    LoadOnset,
    LoadKit { index: Option<u8> },
    BakeRecord { index: Option<u8>, len: u16 },
    PushPool { index: Option<u8> },
}

struct BankHandler {
    gain: f32,
    width: f32,
    speed: f32,
    roll: f32,
    kit_drift: f32,
    phrase_drift: f32,
    kit_index: usize,
    bank: Bank,
    downs: Vec<u8, PAD_COUNT>,
    pool: Deque<u8, { COLS / 2 - 2 }>,
    state: BankState,
}

impl BankHandler {
    fn new() -> Self {
        Self {
            gain: 0.,
            width: 0.,
            speed: 0.,
            roll: 0.,
            kit_drift: 0.,
            phrase_drift: 0.,
            kit_index: 0,
            bank: Bank::default(),
            downs: Vec::new(),
            pool: Deque::new(),
            state: BankState::LoadOnset,
        }
    }

    fn cmd(&mut self, cmd: BankCmd) {
        match cmd {
            BankCmd::Pad(index, down) => self.pad(index, down),
            BankCmd::LoadOnset => self.load_onset(),
            BankCmd::AssignGain(v) => self.gain = v,
            BankCmd::AssignWidth(v) => self.width = v,
            BankCmd::AssignSpeed(v) => self.speed = v,
            BankCmd::AssignRoll(v) => self.roll = v,
            BankCmd::AssignPhraseDrift(v) => self.phrase_drift = v,
            BankCmd::AssignKitDrift(v) => self.kit_drift = v,

            BankCmd::LoadBank(bank) => self.bank = bank,
            BankCmd::LoadKit(index) => self.load_kit(index),

            BankCmd::BakeRecord(index, len) => self.state = BankState::BakeRecord { index, len },
            BankCmd::PushPool(index) => self.push_pool(index),
            BankCmd::ClearPool => self.pool.clear(),
        }
    }

    fn pad(&mut self, index: u8, down: bool) {
        if down {
            let _ = self.downs.push(index);
            if let BankState::PushPool { .. } = &mut self.state {
                let _ = self.pool.push_back(index);
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

    fn push_pool(&mut self, index: Option<u8>) {
        if let Some(index) = index {
            if self.bank.phrases[index as usize] {
                let _ = self.pool.push_back(index);
            }
        }
        self.state = BankState::PushPool { index };
    }

    fn render(&self) -> [Option<Text<6>>; ROWS] {
        let mut text = core::array::from_fn(|_| None);
        match self.state {
            BankState::LoadOnset => self.render_load_onset(&mut text),
            BankState::LoadKit { index } => self.render_load_kit(&mut text, index),
            BankState::BakeRecord { index, len } => self.render_bake_record(&mut text, index, len),
            BankState::PushPool { index } => self.render_push_pool(&mut text, index),
        }
        text
    }

    fn render_load_onset(&self, text: &mut [Option<Text<6>>; ROWS]) {
        let mut pads: [u8; PAD_COUNT] = core::array::from_fn(|i| {
            if self.downs.contains(&(i as u8)) {
                b'.'
            } else {
                b' '
            }
        });
        if let Some(index) = self.downs.first() {
            pads[*index as usize] = b'@';
        }
        text[0] = Some(Text::base(format6!(b' ', "g {:04}", self.gain)));
        text[1] = Some(Text::base(format6!(b' ', "w {:04}", self.width)));
        text[2] = Some(Text::base(format6!(b' ', "s {:04}", self.speed)));
        text[3] = Some(Text::base(format6!(b' ', "r {:04}", self.roll)));
        text[4] = Some(Text::base(format6!(b' ', "k {:04}", self.kit_drift)));
        text[5] = Some(Text::base(format6!(b' ', "p {:04}", self.phrase_drift)));
        write_pads!(text, pads);
    }

    fn render_load_kit(&self, text: &mut [Option<Text<6>>; ROWS], index: Option<u8>) {
        let mut pads: [u8; PAD_COUNT] = core::array::from_fn(|i| {
            if self.bank.kits[i].is_some() {
                b'k'
            } else {
                b' '
            }
        });
        if let Some(index) = index {
            pads[index as usize] = b'@';
        }
        text[0] = Some(Text::base(format6!(1, "kit")));
        write_pads!(text, pads);
    }

    fn render_bake_record(&self, text: &mut [Option<Text<6>>; ROWS], index: Option<u8>, len: u16) {
        let mut pads: [u8; PAD_COUNT] = core::array::from_fn(|i| {
            if self.downs.contains(&(i as u8)) {
                b'.'
            } else {
                b' '
            }
        });
        if let Some(index) = index {
            pads[index as usize] = b'@';
        }
        text[0] = Some(Text::base(format6!(1, "loop")));
        text[2] = Some(Text::base(format6!(1, "ln: {:02x}", len)));
        write_pads!(text, pads);
    }

    fn render_push_pool(&self, text: &mut [Option<Text<6>>; ROWS], index: Option<u8>) {
        let mut pads: [u8; PAD_COUNT] =
            core::array::from_fn(|i| if self.bank.phrases[i] { b'p' } else { b' ' });
        if let Some(index) = index {
            pads[index as usize] = b'@';
        }
        let mut pool = [b' '; 6];
        for (r, w) in self.pool.iter().zip(pool.iter_mut().rev()) {
            *w = b'0' + *r;
        }
        text[0] = Some(Text::base(format6!(1, "seq")));
        text[2] = Some(Text::base(pool));
        write_pads!(text, pads);
    }
}

pub struct TuiHandler {
    log: Option<(embassy_time::Ticker, String<COLS>)>,
    state: GlobalState,
    banks: [BankHandler; BANK_COUNT],
}

impl TuiHandler {
    pub fn new() -> Self {
        Self {
            log: None,
            state: GlobalState::Yield,
            banks: core::array::from_fn(|_| BankHandler::new()),
        }
    }

    fn parse(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Log(msg) => {
                self.log = Some((
                    embassy_time::Ticker::every(embassy_time::Duration::from_millis(2)),
                    msg,
                ))
            }
            Cmd::Yield => {
                self.state = GlobalState::Yield;
                for bank in self.banks.iter_mut() {
                    bank.state = BankState::LoadOnset;
                }
            }
            Cmd::LoadBd(paths) => self.state = GlobalState::LoadBd { paths },
            Cmd::LoadRd(paths) => self.state = GlobalState::LoadRd { paths },
            Cmd::LoadOnset { name, index, count } => {
                self.state = GlobalState::LoadOnset { name, index, count }
            }
            Cmd::Bank(bank, cmd) => {
                if let BankCmd::Pad(index, true) = cmd {
                    if let GlobalState::LoadOnset { .. } = self.state {
                        let kit_index = self.banks[bank as u8 as usize].kit_index;
                        self.banks[bank as u8 as usize].bank.kits[kit_index]
                            .get_or_insert_default()
                            .onsets[index as usize] = true;
                    }
                }
                self.banks[bank as u8 as usize].cmd(cmd);
            }
        }
    }

    async fn render(&mut self, display: &mut Ssd1306<'_>) {
        if let GlobalState::Yield = self.state {
            for i in 0..self.banks.len() {
                draw_border!(display, (i * COLS / BANK_COUNT) as i32, COLS / BANK_COUNT);
                // draw bank
                let lines = self.banks[i].render();
                for i in 0..lines.len() {
                    if let Some(line) = &lines[i] {
                        let char_style = if line.invert {
                            INVERT_CHAR_STYLE
                        } else {
                            BASE_CHAR_STYLE
                        };
                        text::Text::with_text_style(
                            core::str::from_utf8(&line.string).unwrap(),
                            (8 + (i * COLS / BANK_COUNT) as i32, (i as i32 * 8)).into(),
                            char_style,
                            TEXT_STYLE,
                        )
                        .draw(display)
                        .unwrap();
                    }
                }
            }
        } else {
            draw_border!(display, 0, COLS);
            // draw global
            let mut lines = core::array::from_fn(|_| None);
            match &self.state {
                GlobalState::Yield => unreachable!(),
                GlobalState::LoadBd { paths } => self.render_load_bd(&mut lines, paths),
                GlobalState::LoadRd { paths } => self.render_load_rd(&mut lines, paths),
                GlobalState::LoadOnset { name, index, count } => {
                    self.render_assign_onset(&mut lines, name, *index, *count)
                }
            };
            for i in 0..lines.len() {
                if let Some(line) = &lines[i] {
                    let char_style = if line.invert {
                        INVERT_CHAR_STYLE
                    } else {
                        BASE_CHAR_STYLE
                    };
                    text::Text::with_text_style(
                        core::str::from_utf8(&line.string).unwrap(),
                        (8, (i as i32 * 8)).into(),
                        char_style,
                        TEXT_STYLE,
                    )
                    .draw(display)
                    .unwrap();
                }
            }
        }
        if let Some((_, msg)) = &self.log {
            text::Text::with_text_style(
                core::str::from_utf8(&format14!(1, "{}", msg.as_str())).unwrap(),
                (0, 0).into(),
                BASE_CHAR_STYLE,
                TEXT_STYLE,
            )
            .draw(display)
            .unwrap();
        }
    }

    fn render_load_bd(&self, text: &mut [Option<Text<14>>; ROWS], paths: &[String<COLS>]) {
        let pads: [[u8; PAD_COUNT]; BANK_COUNT] = core::array::from_fn(|i| {
            let mut ret = core::array::from_fn(|j| {
                if self.banks[i].downs.contains(&(j as u8)) {
                    b'.'
                } else if self.banks[i].bank.kits[j].is_some() {
                    b'k'
                } else {
                    b' '
                }
            });
            if let Some(index) = self.banks[i].downs.first() {
                ret[*index as usize] = b'@';
            }
            ret
        });
        text[0] = Some(Text::base(format14!(1, "load bank")));
        for i in 0..paths.len() {
            text[1 + i] = Some(Text::base(format14!(b' ', "{}", paths[i])));
        }
        text[1 + FILE_COUNT / 2].as_mut().unwrap().invert = true;
        write_pads!(text, pads[0], pads[1]);
    }

    fn render_load_rd(&self, text: &mut [Option<Text<14>>; ROWS], paths: &[String<COLS>]) {
        let pads: [[u8; PAD_COUNT]; BANK_COUNT] = core::array::from_fn(|i| {
            let mut ret = core::array::from_fn(|j| {
                if self.banks[i].downs.contains(&(j as u8)) {
                    b'.'
                } else if self.banks[i].bank.kits[self.banks[i].kit_index]
                    .as_ref()
                    .is_some_and(|v| v.onsets[j])
                {
                    b'o'
                } else {
                    b' '
                }
            });
            if let Some(index) = self.banks[i].downs.first() {
                ret[*index as usize] = b'@';
            }
            ret
        });
        text[0] = Some(Text::base(format14!(1, "load wav")));
        for i in 0..paths.len() {
            text[1 + i] = Some(Text::base(format14!(b' ', "{}", paths[i])));
        }
        text[1 + FILE_COUNT / 2].as_mut().unwrap().invert = true;
        write_pads!(text, pads[0], pads[1]);
    }

    fn render_assign_onset(
        &self,
        text: &mut [Option<Text<14>>; ROWS],
        name: &str,
        index: usize,
        count: usize,
    ) {
        let pads: [[u8; PAD_COUNT]; BANK_COUNT] = core::array::from_fn(|i| {
            let mut ret = core::array::from_fn(|j| {
                if self.banks[i].downs.contains(&(j as u8)) {
                    b'.'
                } else if self.banks[i].bank.kits[self.banks[i].kit_index]
                    .as_ref()
                    .is_some_and(|v| v.onsets[j])
                {
                    b'o'
                } else {
                    b' '
                }
            });
            if let Some(index) = self.banks[i].downs.first() {
                ret[*index as usize] = b'@';
            }
            ret
        });
        text[0] = Some(Text::base(format14!(1, "assign onset")));
        text[1 + FILE_COUNT / 2] = Some(Text::invert(format14!(b' ', "{}", name)));
        text[1 + FILE_COUNT / 2 + 1] = Some(Text::base(format14!(
            b' ',
            ">>{:03}/{:03}",
            index + 1,
            count
        )));
        write_pads!(text, pads[0], pads[1]);
    }
}

#[embassy_executor::task]
pub async fn tui_handler(
    mut tui_hdlr: TuiHandler,
    mut display: Ssd1306<'static>,
    cmd_rx: embassy_sync::channel::DynamicReceiver<'static, Cmd>,
) {
    use embassy_futures::select::*;

    display.init().await.unwrap();
    loop {
         // render display
         display.clear_buffer();
         tui_hdlr.render(&mut display).await;
         display.flush().await.unwrap();
         // parse cmd
         if let Some((ticker, _)) = &mut tui_hdlr.log {
             match select(ticker.next(), cmd_rx.receive()).await {
                 Either::First(()) => tui_hdlr.log = None,
                 Either::Second(cmd) => tui_hdlr.parse(cmd),
             }
         } else {
             tui_hdlr.parse(cmd_rx.receive().await);
         }
    }
}

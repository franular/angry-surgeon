use crate::audio::{MAX_PHRASE_COUNT, MAX_PHRASE_LEN, PAD_COUNT};

use color_eyre::eyre::Result;
use crossterm::event::{self, KeyCode, KeyEvent, KeyEventKind};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Flex, Layout, Rect},
    style::Stylize,
    text::{Line, Text},
    widgets::{Block, Padding, Paragraph, Widget, Wrap},
    DefaultTerminal, Frame,
};
use std::{path::Path, sync::mpsc::{Receiver, Sender}};

pub const FILE_COUNT: usize = 5;
const LOG_DURATION: std::time::Duration = std::time::Duration::from_millis(1000);

pub enum Cmd {
    Log(String),
    Clock,
    Stop,
    Yield,
    LoadBd([String; FILE_COUNT]),
    LoadRd([String; FILE_COUNT]),
    LoadOnset {
        name: String,
        index: usize,
        count: usize,
    },
    Bank(crate::audio::Bank, BankCmd),
}

pub enum BankCmd {
    Pad(u8, bool),
    LoadBank(Bank),
    Mangle,
    LoadKit(Option<u8>),
    BakeRecord(Option<u8>, u16),
    ClearPool,
    PushPool(Option<u8>),
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
    pub fn from_audio(bank: &angry_surgeon_core::Bank<PAD_COUNT, MAX_PHRASE_LEN>) -> Self {
        let mut ret = Self::default();
        for (rkit, wkit) in bank
            .kits
            .iter()
            .zip(ret.kits.iter_mut())
            .filter_map(|(r, w)| Some((r.as_ref()?, w)))
        {
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

enum GlobalState {
    Yield,
    LoadBd {
        paths: [String; FILE_COUNT],
    },
    LoadRd {
        paths: [String; FILE_COUNT],
    },
    LoadOnset {
        name: String,
        index: usize,
        count: usize,
    },
}

enum BankState {
    Mangle,
    LoadKit { index: Option<u8> },
    BakeRecord { index: Option<u8>, len: u16 },
    PushPool { index: Option<u8> },
}

struct BankHandler {
    kit_index: usize,
    bank: Bank,
    downs: heapless::Vec<u8, PAD_COUNT>,
    pool: heapless::Deque<u8, MAX_PHRASE_COUNT>,
    state: BankState,
}

impl BankHandler {
    fn new() -> Self {
        Self {
            kit_index: 0,
            bank: Bank::default(),
            downs: heapless::Vec::new(),
            pool: heapless::Deque::new(),
            state: BankState::Mangle,
        }
    }

    fn cmd(&mut self, cmd: BankCmd) {
        match cmd {
            BankCmd::Pad(index, down) => self.pad(index, down),
            BankCmd::LoadBank(bank) => self.bank = bank,
            BankCmd::Mangle => self.mangle(),
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

    fn mangle(&mut self) {
        if let BankState::BakeRecord {
            index: Some(index), ..
        } = self.state
        {
            self.bank.phrases[index as usize] = true;
        }
        self.state = BankState::Mangle;
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

    fn render(&self, flex: Flex, area: Rect, buf: &mut Buffer) {
        match self.state {
            BankState::Mangle => self.render_mangle(flex, area, buf),
            BankState::LoadKit { index } => self.render_load_kit(index, flex, area, buf),
            BankState::BakeRecord { index, len } => {
                self.render_bake_record(index, len, flex, area, buf)
            }
            BankState::PushPool { index } => self.render_pool(index, area, buf),
        }
    }

    fn render_mangle(&self, flex: Flex, area: Rect, buf: &mut Buffer) {
        let [area] = Layout::horizontal(vec![Constraint::Max(14)])
            .flex(flex)
            .areas(area);
        // render pads
        let mut pads: [_; PAD_COUNT] = core::array::from_fn(|i| {
            if self.downs.contains(&(i as u8)) {
                '.'
            } else {
                ' '
            }
        });
        if let Some(index) = self.downs.first() {
            pads[*index as usize] = '@';
        }
        Paragraph::new(Text::raw(String::from_iter(pads)))
            .block(Block::bordered().bold().padding(Padding::horizontal(4)))
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_load_kit(&self, index: Option<u8>, flex: Flex, area: Rect, buf: &mut Buffer) {
        let [area] = Layout::horizontal(vec![Constraint::Max(14)])
            .flex(flex)
            .areas(area);
        // render pads
        let mut pads: [_; PAD_COUNT] = core::array::from_fn(|i| {
            if self.bank.kits[i].is_some() || self.bank.phrases[i] {
                'k'
            } else {
                ' '
            }
        });
        if let Some(index) = index {
            pads[index as usize] = '@';
        }
        Paragraph::new(Text::raw(String::from_iter(pads)).centered())
            .block(
                Block::bordered()
                    .bold()
                    .title(" load kit ")
                    .padding(Padding::horizontal(4)),
            )
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_bake_record(
        &self,
        index: Option<u8>,
        len: u16,
        flex: Flex,
        area: Rect,
        buf: &mut Buffer,
    ) {
        let [area] = Layout::horizontal(vec![Constraint::Max(16)])
            .flex(flex)
            .areas(area);
        let [pad_area, len_area] = Layout::vertical(Constraint::from_maxes([3, 2]))
            .flex(Flex::SpaceBetween)
            .areas(area);
        // render border
        Block::bordered()
            .bold()
            .title(" bake record ")
            .render(area, buf);
        {
            // render pads
            let mut text: [_; PAD_COUNT] = core::array::from_fn(|i| {
                if self.downs.contains(&(i as u8)) {
                    '.'
                } else {
                    ' '
                }
            });
            if let Some(index) = index {
                text[index as usize] = '@';
            }
            Paragraph::new(Text::raw(String::from_iter(text)).centered())
                .block(Block::new().padding(Padding::new(6, 6, 1, 0)))
                .wrap(Wrap { trim: false })
                .render(pad_area, buf);
        }
        // render length
        Paragraph::new(Text::raw(format!("length: {:>3}", len)).left_aligned())
            .block(Block::new().padding(Padding::new(2, 2, 0, 1)))
            .wrap(Wrap { trim: false })
            .render(len_area, buf);
    }

    fn render_pool(&self, index: Option<u8>, area: Rect, buf: &mut Buffer) {
        let [pad_area, pool_area] =
            Layout::horizontal(vec![Constraint::Min(8), Constraint::Percentage(100)]).areas(area);
        let [_, arrow_area] = Layout::horizontal(Constraint::from_maxes([7, 2]))
            .flex(Flex::Start)
            .areas(area);
        // render border
        Block::bordered()
            .bold()
            .title(" build pool ")
            .render(area, buf);
        {
            // render pads
            let mut text: [_; PAD_COUNT] =
                core::array::from_fn(|i| if self.bank.phrases[i] { 'p' } else { ' ' });
            if let Some(index) = index {
                text[index as usize] = '@';
            }
            Paragraph::new(Text::raw(String::from_iter(text)).centered())
                .block(Block::new().padding(Padding::new(2, 2, 1, 0)))
                .wrap(Wrap { trim: false })
                .render(pad_area, buf);
        }
        // render pool
        Paragraph::new(Text::raw(format!("{:?}", self.pool)).left_aligned())
            .block(Block::new().padding(Padding::new(2, 2, 1, 0)))
            .wrap(Wrap { trim: false })
            .render(pool_area, buf);
        // render arrow
        Paragraph::new(Text::raw(">>"))
            .block(Block::new().padding(Padding::new(0, 0, 1, 0)))
            .render(arrow_area, buf);
    }
}

struct Oneshots {
    paths: Vec<Box<Path>>,
    index: Option<usize>,
}

impl Oneshots {
    fn new() -> Self {
        Self { paths: Vec::new(), index: None }
    }

    fn open(&mut self, dir: impl AsRef<Path>) -> Result<()> {
        self.index = None;
        self.paths.clear();
        for entry in std::fs::read_dir(dir)?.filter_map(|v| v.ok()) {
            let path = entry.path();
            if entry.metadata()?.is_file() && path.extension().is_some_and(|v| v.to_str() == Some("wav")) {
                self.paths.push(path.into_boxed_path());
            }
        }
        self.paths.sort();
        Ok(())
    }
}

pub struct TuiHandler {
    oneshots: Oneshots,

    bank_a: BankHandler,
    bank_b: BankHandler,

    deafen: bool,
    log: Option<(std::time::Instant, String)>,
    clock: bool,
    state: GlobalState,

    audio_tx: Sender<crate::audio::Cmd>,
    input_tx: Sender<crate::input::Cmd>,
}

impl TuiHandler {
    pub fn new(audio_tx: Sender<crate::audio::Cmd>, input_tx: Sender<crate::input::Cmd>) -> Result<Self> {
        Ok(Self {
            oneshots: Oneshots::new(),

            bank_a: BankHandler::new(),
            bank_b: BankHandler::new(),

            deafen: false,
            log: None,
            clock: false,
            state: GlobalState::Yield,

            audio_tx,
            input_tx,
        })
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal, input_rx: Receiver<Cmd>) -> Result<()> {
        terminal.draw(|frame| self.draw(frame))?;
        loop {
            let mut flush = false;
            if let Some((start, ..)) = &self.log {
                if start.elapsed() >= LOG_DURATION {
                    self.log = None;
                    flush = true;
                }
            };
            if crossterm::event::poll(std::time::Duration::from_millis(16))? {
                if self.kbd()? {
                    break;
                }
                flush = true;
            }
            match input_rx.try_recv() {
                Ok(cmd) => {
                    self.cmd(cmd);
                    flush = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => (),
                Err(e) => Err(e)?,
            }
            if flush {
                terminal.draw(|frame| self.draw(frame))?;
            }
        }
        Ok(())
    }

    /// returns true if should exit
    fn kbd(&mut self) -> Result<bool> {
        match event::read()? {
            event::Event::Key(KeyEvent {
                code: KeyCode::Char('q'),
                kind: KeyEventKind::Press,
                ..
            }) => {
                return Ok(true);
            }
            event::Event::Key(KeyEvent {
                code: KeyCode::Char('1'),
                kind: KeyEventKind::Press,
                ..
            }) => {
                self.oneshots.open("oneshots/1")?;
                self.log = Some((std::time::Instant::now(), "open ./oneshots/1".to_string()));
            }
            event::Event::Key(KeyEvent {
                code: KeyCode::Char('2'),
                kind: KeyEventKind::Press,
                ..
            }) => {
                self.oneshots.open("oneshots/2")?;
                self.log = Some((std::time::Instant::now(), "open ./oneshots/2".to_string()));
            }
            event::Event::Key(KeyEvent {
                code: KeyCode::Char('3'),
                kind: KeyEventKind::Press,
                ..
            }) => {
                self.oneshots.open("oneshots/3")?;
                self.log = Some((std::time::Instant::now(), "open ./oneshots/3".to_string()));
            }
            event::Event::Key(KeyEvent {
                code: KeyCode::Char('4'),
                kind: KeyEventKind::Press,
                ..
            }) => {
                self.oneshots.open("oneshots/4")?;
                self.log = Some((std::time::Instant::now(), "open ./oneshots/4".to_string()));
            }
            event::Event::Key(KeyEvent {
                code: KeyCode::Char(' '),
                kind: KeyEventKind::Press,
                ..
            }) => if !self.oneshots.paths.is_empty() {
                if let Some(i) = self.oneshots.index.as_mut() {
                    if *i < self.oneshots.paths.len() - 1 {
                        *i += 1;
                    } else {
                        self.oneshots.index = None;
                    }
                } else {
                    self.oneshots.index = Some(0);
                }
                if let Some(index) = self.oneshots.index {
                    self.audio_tx.send(crate::audio::Cmd::LoadOneshot(std::fs::File::open(self.oneshots.paths[index].clone())?))?;
                    self.log = Some((std::time::Instant::now(), format!("oneshot {:>3}/{:>3}", index, self.oneshots.paths.len())));
                } else {
                    self.audio_tx.send(crate::audio::Cmd::StopOneshot)?;
                    self.log = Some((std::time::Instant::now(), "oneshots exhausted".to_string()));
                }
            }
            event::Event::Key(KeyEvent {
                code: KeyCode::Enter,
                kind: KeyEventKind::Press,
                ..
            }) => {
                self.deafen = !self.deafen;
                self.input_tx.send(crate::input::Cmd::Deafen(self.deafen))?;
            }
            _ => (),
        }
        Ok(false)
    }

    fn cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Log(msg) => self.log = Some((std::time::Instant::now(), msg)),
            Cmd::Clock => self.clock = !self.clock,
            Cmd::Stop => self.clock = false,
            Cmd::Yield => {
                self.state = GlobalState::Yield;
                self.bank_a.state = BankState::Mangle;
                self.bank_b.state = BankState::Mangle;
            }
            Cmd::LoadBd(paths) => self.state = GlobalState::LoadBd { paths },
            Cmd::LoadRd(paths) => self.state = GlobalState::LoadRd { paths },
            Cmd::LoadOnset { name, index, count } => {
                self.state = GlobalState::LoadOnset { name, index, count }
            }
            Cmd::Bank(bank, cmd) => {
                let my_bank = match bank {
                    crate::audio::Bank::A => &mut self.bank_a,
                    crate::audio::Bank::B => &mut self.bank_b,
                };
                if let BankCmd::Pad(index, true) = cmd {
                    if let GlobalState::LoadOnset { .. } = self.state {
                        let kit_index = my_bank.kit_index;
                        my_bank.bank.kits[kit_index].get_or_insert_default().onsets
                            [index as usize] = true;
                    }
                }
                my_bank.cmd(cmd);
            }
        }
    }

    fn draw(&self, frame: &mut Frame) {
        frame.render_widget(self, frame.area());
    }

    fn render_log(&self, area: Rect, buf: &mut Buffer) {
        if let Some((_, msg)) = &self.log {
            Paragraph::new(Text::raw(msg)).centered().render(area, buf);
        }
    }

    fn render_clock(&self, area: Rect, buf: &mut Buffer) {
        let [left, right] = Layout::horizontal(Constraint::from_maxes([11, 11]))
            .flex(Flex::Center)
            .areas(area);
        if self.clock {
            Block::new().reversed().render(right, buf);
        } else {
            Block::new().reversed().render(left, buf);
        }
    }

    fn render_load_bd(&self, paths: &[String; FILE_COUNT], area: Rect, buf: &mut Buffer) {
        let [pad_area, fs_area] =
            Layout::horizontal(vec![Constraint::Min(8), Constraint::Percentage(100)]).areas(area);
        let [_, arrow_area] = Layout::horizontal(Constraint::from_maxes([7, 2]))
            .flex(Flex::Start)
            .areas(area);
        let [a_area, b_area] = Layout::vertical(Constraint::from_maxes([3, 3]))
            .flex(Flex::SpaceBetween)
            .areas(pad_area);
        // render border
        Block::bordered().bold().render(pad_area, buf);
        // render bank a
        {
            let mut text: [_; PAD_COUNT] = core::array::from_fn(|i| {
                if self.bank_a.bank.kits[i].is_some() {
                    'k'
                } else {
                    ' '
                }
            });
            if let Some(index) = self.bank_a.downs.first() {
                text[*index as usize] = '@';
            }
            Paragraph::new(Text::raw(String::from_iter(text)).centered())
                .block(Block::new().bold().padding(Padding::new(2, 2, 1, 0)))
                .wrap(Wrap { trim: false })
                .render(a_area, buf);
        }
        // render bank b
        {
            let mut text: [_; PAD_COUNT] = core::array::from_fn(|i| {
                if self.bank_b.bank.kits[i].is_some() {
                    'k'
                } else {
                    ' '
                }
            });
            if let Some(index) = self.bank_b.downs.first() {
                text[*index as usize] = '@';
            }
            Paragraph::new(Text::raw(String::from_iter(text)).centered())
                .block(Block::new().bold().padding(Padding::new(2, 2, 0, 1)))
                .wrap(Wrap { trim: false })
                .render(b_area, buf);
        }
        // render fs
        {
            let text = if paths.iter().any(|v| !v.is_empty()) {
                let mut lines = paths.clone().map(Line::raw).to_vec();
                let mid = lines.len() / 2;
                lines[mid] = lines[mid].clone().reversed();
                Text::from(lines.to_vec())
            } else {
                Text::raw("no files found </3")
            };
            Paragraph::new(text)
                .left_aligned()
                .block(
                    Block::bordered()
                        .title(" load bank ")
                        .padding(Padding::horizontal(1)),
                )
                .render(fs_area, buf);
        }
        // render arrow
        Paragraph::new(Text::raw("<<"))
            .block(Block::new().padding(Padding::new(0, 0, FILE_COUNT as u16 / 2 + 1, 0)))
            .render(arrow_area, buf);
    }

    fn render_load_rd(&self, paths: &[String; FILE_COUNT], area: Rect, buf: &mut Buffer) {
        let [pad_area, fs_area] =
            Layout::horizontal(vec![Constraint::Min(8), Constraint::Percentage(100)]).areas(area);
        let [_, arrow_area] = Layout::horizontal(Constraint::from_maxes([7, 2]))
            .flex(Flex::Start)
            .areas(area);
        let [a_area, b_area] = Layout::vertical(Constraint::from_maxes([3, 3]))
            .flex(Flex::SpaceBetween)
            .areas(pad_area);
        // render border
        Block::bordered().bold().render(pad_area, buf);
        // render bank a
        {
            let mut text: [_; PAD_COUNT] = core::array::from_fn(|i| {
                if self.bank_a.bank.kits[self.bank_a.kit_index]
                    .as_ref()
                    .is_some_and(|v| v.onsets[i])
                {
                    'o'
                } else {
                    ' '
                }
            });
            if let Some(index) = self.bank_a.downs.first() {
                text[*index as usize] = '@';
            }
            Paragraph::new(Text::raw(String::from_iter(text)).centered())
                .block(Block::new().bold().padding(Padding::new(2, 2, 1, 0)))
                .wrap(Wrap { trim: false })
                .render(a_area, buf);
        }
        // render bank b
        {
            let mut text: [_; PAD_COUNT] = core::array::from_fn(|i| {
                if self.bank_b.bank.kits[self.bank_b.kit_index]
                    .as_ref()
                    .is_some_and(|v| v.onsets[i])
                {
                    'o'
                } else {
                    ' '
                }
            });
            if let Some(index) = self.bank_b.downs.first() {
                text[*index as usize] = '@';
            }
            Paragraph::new(Text::raw(String::from_iter(text)).centered())
                .block(Block::new().bold().padding(Padding::new(2, 2, 0, 1)))
                .wrap(Wrap { trim: false })
                .render(b_area, buf);
        }
        // render fs
        {
            let text = if paths.iter().any(|v| !v.is_empty()) {
                let mut lines = paths.clone().map(Line::raw).to_vec();
                let mid = lines.len() / 2;
                lines[mid] = lines[mid].clone().reversed();
                Text::from(lines.to_vec())
            } else {
                Text::raw("no files found </3")
            };
            Paragraph::new(text)
                .left_aligned()
                .block(
                    Block::bordered()
                        .title(" load sample ")
                        .padding(Padding::horizontal(1)),
                )
                .render(fs_area, buf);
        }
        // render arrow
        Paragraph::new(Text::raw("<<"))
            .block(Block::new().padding(Padding::new(0, 0, FILE_COUNT as u16 / 2 + 1, 0)))
            .render(arrow_area, buf);
    }

    fn render_load_onset(
        &self,
        name: &str,
        index: usize,
        count: usize,
        area: Rect,
        buf: &mut Buffer,
    ) {
        let [pad_area, onset_area] =
            Layout::horizontal(vec![Constraint::Min(8), Constraint::Percentage(100)]).areas(area);
        let [_, arrow_area] = Layout::horizontal(Constraint::from_maxes([7, 2]))
            .flex(Flex::Start)
            .areas(area);
        let [a_area, b_area] = Layout::vertical(Constraint::from_maxes([3, 3]))
            .flex(Flex::SpaceBetween)
            .areas(pad_area);
        // render border
        Block::bordered().bold().render(pad_area, buf);
        // render bank a
        {
            let mut text: [_; PAD_COUNT] = core::array::from_fn(|i| {
                if self.bank_a.bank.kits[self.bank_a.kit_index]
                    .as_ref()
                    .is_some_and(|v| v.onsets[i])
                {
                    'o'
                } else {
                    ' '
                }
            });
            if let Some(index) = self.bank_a.downs.first() {
                text[*index as usize] = '@';
            }
            Paragraph::new(Text::raw(String::from_iter(text)).centered())
                .block(Block::new().bold().padding(Padding::new(2, 2, 1, 0)))
                .wrap(Wrap { trim: false })
                .render(a_area, buf);
        }
        // render bank b
        {
            let mut text: [_; PAD_COUNT] = core::array::from_fn(|i| {
                if self.bank_b.bank.kits[self.bank_b.kit_index]
                    .as_ref()
                    .is_some_and(|v| v.onsets[i])
                {
                    'o'
                } else {
                    ' '
                }
            });
            if let Some(index) = self.bank_b.downs.first() {
                text[*index as usize] = '@';
            }
            Paragraph::new(Text::raw(String::from_iter(text)).centered())
                .block(Block::new().bold().padding(Padding::new(2, 2, 0, 1)))
                .wrap(Wrap { trim: false })
                .render(b_area, buf);
        }
        // render onset
        {
            let mut lines: [_; FILE_COUNT] = core::array::from_fn(|_| Line::raw(""));
            lines[FILE_COUNT / 2] = Line::raw(name).reversed();
            lines[FILE_COUNT - 1] = Line::raw(format!("{:>3}/{:>3}", index, count));
            Paragraph::new(Text::from(lines.to_vec()))
                .left_aligned()
                .block(
                    Block::bordered()
                        .title(" load onset ")
                        .padding(Padding::horizontal(1)),
                )
                .render(onset_area, buf);
        }
        // render arrow
        Paragraph::new(Text::raw("<<"))
            .block(Block::new().padding(Padding::new(0, 0, FILE_COUNT as u16, 0)))
            .render(arrow_area, buf);
    }
}

impl Widget for &TuiHandler {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let [area] = Layout::vertical(vec![Constraint::Max(FILE_COUNT as u16 + 5)])
            .flex(Flex::Center)
            .areas(area);
        let [clock_area, area, log_area] =
            Layout::vertical(Constraint::from_maxes([2, FILE_COUNT as u16 + 2, 1]))
                .flex(Flex::Center)
                .areas(area);
        self.render_log(log_area, buf);
        self.render_clock(clock_area, buf);
        match &self.state {
            GlobalState::Yield => {
                let [a_area, b_area] = Layout::horizontal(Constraint::from_percentages([50, 50]))
                    .flex(Flex::Center)
                    .areas(area);
                self.bank_a.render(Flex::End, a_area, buf);
                self.bank_b.render(Flex::Start, b_area, buf);
            }
            GlobalState::LoadBd { paths } => self.render_load_bd(paths, area, buf),
            GlobalState::LoadRd { paths } => self.render_load_rd(paths, area, buf),
            GlobalState::LoadOnset { name, index, count } => {
                self.render_load_onset(name, *index, *count, area, buf)
            }
        }
    }
}

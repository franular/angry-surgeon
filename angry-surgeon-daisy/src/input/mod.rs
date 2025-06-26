use angry_surgeon_core::{Event, FileHandler as _};
use embedded_io::ErrorType;

use crate::{
    audio::{self, SystemHandler},
    fs::FileHandler,
};

pub mod analog;
pub mod clock;
pub mod touch;

#[derive(PartialEq)]
enum BankState {
    Mangle,
    LoadKit,
    BakeRecord,
    BuildPool { cleared: bool },
}

struct BankHandler {
    bank: audio::Bank,
    downs: heapless::Vec<u8, { audio::PAD_COUNT }>,
    shift: bool,
    reverse: bool,
    hold: bool,
    state: BankState,
}

impl BankHandler {
    fn new(bank: audio::Bank) -> Self {
        Self {
            bank,
            downs: heapless::Vec::new(),
            shift: false,
            reverse: false,
            hold: false,
            state: BankState::Mangle,
        }
    }

    fn reverse_up(&mut self, system: &mut SystemHandler) {
        match self.state {
            BankState::Mangle => {
                self.reverse = false;
                system.banks[usize::from(self.bank)].assign_reverse(false);
            }
            BankState::BakeRecord => {
                // exit record
                self.state = BankState::Mangle;
                system.banks[usize::from(self.bank)].take_record(self.downs.first().copied());
            }
            _ => (),
        }
    }

    fn reverse_down(
        &mut self,
        system: &mut SystemHandler,
    ) -> Result<(), <FileHandler as ErrorType>::Error> {
        if self.state == BankState::Mangle {
            if self.shift {
                // init record
                self.state = BankState::BakeRecord;
                self.hold = false;
                if self.downs.is_empty() {
                    system.banks[usize::from(self.bank)].push_event(
                        &mut system.fs,
                        &mut system.rand,
                        Event::Sync,
                    )?;
                }
                system.banks[usize::from(self.bank)].bake_record(
                    &mut system.fs,
                    &mut system.rand,
                    audio::MAX_PHRASE_LEN as u16,
                )?;
            } else {
                self.reverse = true;
                system.banks[usize::from(self.bank)].assign_reverse(true);
            }
        }
        Ok(())
    }

    fn hold_up(&mut self, system: &mut SystemHandler) {
        if let BankState::BuildPool { cleared } = self.state {
            // exit build pool
            if !cleared {
                system.banks[usize::from(self.bank)].clear_pool();
            }
            self.state = BankState::Mangle;
        }
    }

    fn hold_down(
        &mut self,
        system: &mut SystemHandler,
    ) -> Result<(), <FileHandler as ErrorType>::Error> {
        if self.state == BankState::Mangle {
            if self.shift {
                // init build pool
                self.state = BankState::BuildPool { cleared: false };
            } else {
                self.hold = !self.hold;
                if !self.hold && self.downs.is_empty() {
                    system.banks[usize::from(self.bank)].push_event(
                        &mut system.fs,
                        &mut system.rand,
                        Event::Sync,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn kit_up(&mut self) {
        if self.state == BankState::LoadKit {
            // exit load kit
            self.state = BankState::Mangle;
        }
    }

    fn kit_down(&mut self) {
        if self.state == BankState::Mangle && !self.shift {
            // init load kit
            self.state = BankState::LoadKit;
        }
        // bank save hanled in InputHandler
    }

    fn pad_up(
        &mut self,
        system: &mut SystemHandler,
    ) -> Result<(), <FileHandler as ErrorType>::Error> {
        match self.state {
            BankState::Mangle => {
                if !self.hold {
                    self.pad_input(system)?;
                }
            }
            BankState::BakeRecord => {
                let len = if self.downs.len() > 1 {
                    self.binary_offset(self.downs[0])
                } else {
                    audio::MAX_PHRASE_LEN as u16
                };
                system.banks[usize::from(self.bank)].bake_record(
                    &mut system.fs,
                    &mut system.rand,
                    len,
                )?;
            }
            _ => (),
        }
        Ok(())
    }

    fn pad_down(
        &mut self,
        system: &mut SystemHandler,
    ) -> Result<(), <FileHandler as ErrorType>::Error> {
        match &mut self.state {
            BankState::Mangle => self.pad_input(system)?,
            BankState::LoadKit => {
                system.banks[usize::from(self.bank)].kit_index = self.downs[0] as usize;
            }
            BankState::BakeRecord => {
                let len = if self.downs.len() > 1 {
                    self.binary_offset(self.downs[0])
                } else {
                    audio::MAX_PHRASE_LEN as u16
                };
                system.banks[usize::from(self.bank)].bake_record(
                    &mut system.fs,
                    &mut system.rand,
                    len,
                )?;
            }
            BankState::BuildPool { cleared } => {
                if !*cleared {
                    *cleared = true;
                    system.banks[usize::from(self.bank)].clear_pool();
                }
                system.banks[usize::from(self.bank)].push_pool(*self.downs.last().unwrap());
            }
        }
        Ok(())
    }

    fn pad_input(
        &mut self,
        system: &mut SystemHandler,
    ) -> Result<(), <FileHandler as ErrorType>::Error> {
        if let Some(&index) = self.downs.first() {
            if self.downs.len() > 1 {
                // init loop start
                let len = self.binary_offset(index);
                system.banks[usize::from(self.bank)].push_event(
                    &mut system.fs,
                    &mut system.rand,
                    Event::Loop { index, len },
                )?;
            } else {
                // init loop stop | jump
                system.banks[usize::from(self.bank)].push_event(
                    &mut system.fs,
                    &mut system.rand,
                    Event::Hold { index },
                )?;
            }
        } else {
            // init sync
            system.banks[usize::from(self.bank)].push_event(
                &mut system.fs,
                &mut system.rand,
                Event::Sync,
            )?;
        }
        Ok(())
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

pub struct InputHandler {
    bank_a: BankHandler,
    bank_b: BankHandler,
}

impl InputHandler {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            bank_a: BankHandler::new(audio::Bank::A),
            bank_b: BankHandler::new(audio::Bank::B),
        }
    }

    /// save bank to new file
    fn save_bank(
        &self,
        bank: audio::Bank,
        system: &mut SystemHandler,
    ) -> Result<(), <FileHandler as ErrorType>::Error> {
        let bd = system.banks[usize::from(bank)].bank.clone();
        if let Ok(bytes) = serde_json::to_vec(&bd) {
            let mut index = 0;
            let file = loop {
                match system.fs.open(&alloc::format!("banks/banks{}.bd", index)) {
                    Err(embedded_sdmmc::Error::FileAlreadyExists) => index += 1,
                    Err(_) => panic!(),
                    Ok(file) => break file,
                }
            };
            let mut slice = &bytes[..];
            while !slice.is_empty() {
                let n = system.fs.write(&file, slice)?;
                slice = &slice[n..];
            }
            system.fs.close(&file)?;
        }
        Ok(())
    }

    pub fn touch_up(
        &mut self,
        bank: audio::Bank,
        index: u8,
        system: &mut SystemHandler,
    ) -> Result<(), <FileHandler as ErrorType>::Error> {
        let my_bank = match bank {
            audio::Bank::A => &mut self.bank_a,
            audio::Bank::B => &mut self.bank_b,
        };
        if index == touch::pads::SHIFT {
            my_bank.shift = false;
        } else if touch::pads::BANK.contains(&index) {
            my_bank.downs.retain(|&i| i != index);
            my_bank.pad_up(system)?;
        } else if index == touch::pads::REVERSE {
            my_bank.reverse_up(system);
        } else if index == touch::pads::HOLD {
            my_bank.hold_up(system);
        } else if index == touch::pads::KIT {
            my_bank.kit_up();
        }
        Ok(())
    }

    pub fn touch_down(
        &mut self,
        bank: audio::Bank,
        index: u8,
        system: &mut SystemHandler,
    ) -> Result<(), <FileHandler as ErrorType>::Error> {
        let my_bank = match bank {
            audio::Bank::A => &mut self.bank_a,
            audio::Bank::B => &mut self.bank_b,
        };
        if index == touch::pads::SHIFT {
            my_bank.shift = true;
        } else if index == touch::pads::KIT {
            if my_bank.shift {
                self.save_bank(bank, system)?;
            } else {
                my_bank.kit_down();
            }
        } else if touch::pads::BANK.contains(&index) {
            let _ = my_bank.downs.push(index);
            my_bank.pad_down(system)?;
        } else if index == touch::pads::REVERSE {
            my_bank.reverse_down(system)?;
        } else if index == touch::pads::HOLD {
            my_bank.hold_down(system)?;
        }
        Ok(())
    }
}

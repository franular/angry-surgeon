/// in hz
pub const SAMPLE_RATE: u32 = 48000;
pub const STEP_DIV: u16 = 4;
pub const BANK_COUNT: usize = 2;
pub const PAD_COUNT: usize = 8;
pub const MAX_PHRASE_LEN: usize = 2usize.pow(PAD_COUNT as u32 - 1);
pub const MAX_PHRASE_COUNT: usize = 64;

/// pulses per quarter
pub const PPQ: u16 = 2;

#[repr(u8)]
#[derive(Copy, Clone)]
pub enum Bank {
    A = 0,
    B = 1,
}

impl From<Bank> for usize {
    fn from(value: Bank) -> Self {
        value as u8 as usize
    }
}

pub type SystemHandler = angry_surgeon_core::SystemHandler<
    BANK_COUNT,
    PAD_COUNT,
    MAX_PHRASE_LEN,
    MAX_PHRASE_COUNT,
    crate::fs::FileHandler,
    tinyrand::Wyrand,
>;

use crate::hal::prelude::*;
use embedded_hal::delay::DelayNs;

macro_rules! write_byte {
    ($i2c:expr,$i2c_addr:expr,$mem_addr:ident,$offset:expr,$byte:expr) => {{
        $i2c.write($i2c_addr, &[Regs::$mem_addr as u8 + $offset, $byte])?;
        // idk girl
        crate::Mono.delay_ns(WRITE_DELAY);
    }};
    ($i2c:expr,$i2c_addr:expr,$mem_addr:ident,$byte:expr) => {
        write_byte!($i2c, $i2c_addr, $mem_addr, 0, $byte);
    };
}

/// in nanos
const WRITE_DELAY: u32 = 50;
const TOUCH_THRESH: u8 = 12;
const RELEASE_THRESH: u8 = 6;

pub mod pads {
    pub const BANK: core::ops::Range<u8> = 0..8;
    pub const SHIFT: u8 = 8;
    pub const REVERSE: u8 = 9;
    pub const HOLD: u8 = 10;
    pub const KIT: u8 = 11;
}

#[allow(non_camel_case_types, clippy::upper_case_acronyms)]
enum Regs {
    TOUCHSTATUS_L = 0x00,
    MHDR = 0x2b,
    NHDR = 0x2c,
    NCLR = 0x2d,
    FDLR = 0x2e,
    MHDF = 0x2f,
    NHDF = 0x30,
    NCLF = 0x31,
    FDLF = 0x32,
    NHDT = 0x33,
    NCLT = 0x34,
    FDLT = 0x35,

    TOUCHTH_0 = 0x41,
    RELEASETH_0 = 0x42,
    DEBOUNCE = 0x5b,
    CONFIG1 = 0x5c,
    CONFIG2 = 0x5d,
    ECR = 0x5e,
    AUTOCONFIG0 = 0x7b,
    UPLIMIT = 0x7d,
    LOWLIMIT = 0x7e,
    TARGETLIMIT = 0x7f,

    SOFTRESET = 0x80,
}

#[derive(Debug)]
pub enum Error {
    Boot,
    I2c(crate::hal::i2c::Error),
}

impl From<crate::hal::i2c::Error> for Error {
    fn from(value: crate::hal::i2c::Error) -> Self {
        Self::I2c(value)
    }
}

/// mpr121 data
pub struct Mpr121Data<P: crate::hal::gpio::ExtiPin> {
    pub addr: u8,
    pub irq: P,
    pub last: u16,
}

impl<P: crate::hal::gpio::ExtiPin> Mpr121Data<P> {
    pub fn new(
        addr: u8,
        mut irq: P,
        syscfg: &mut crate::hal::pac::SYSCFG,
        exti: &mut crate::hal::pac::EXTI,
    ) -> Self {
        irq.make_interrupt_source(syscfg);
        irq.trigger_on_edge(exti, crate::hal::gpio::Edge::Falling);
        irq.enable_interrupt(exti);
        Self {
            addr,
            irq,
            last: 0u16,
        }
    }
}

/// mpr121 interface/interpreter
pub struct Mpr121Interface {
    i2c: crate::hal::i2c::I2c<crate::hal::pac::I2C1>,
}

impl Mpr121Interface {
    pub fn new(i2c: crate::hal::i2c::I2c<crate::hal::pac::I2C1>) -> Self {
        Self { i2c }
    }

    pub fn init(&mut self, addr: u8) -> Result<(), Error> {
        // reset & stop
        write_byte!(self.i2c, addr, SOFTRESET, 0x63);
        write_byte!(self.i2c, addr, ECR, 0x00);

        // check boot state
        let mut buf = [0u8];
        self.i2c
            .write_read(addr, &[Regs::CONFIG2 as u8], &mut buf)?;
        // .await?;
        if buf[0] != 0x24 {
            return Err(Error::Boot);
        }

        // set thresholds
        for i in 0..12u8 {
            write_byte!(self.i2c, addr, TOUCHTH_0, 2 * i, TOUCH_THRESH);
            write_byte!(self.i2c, addr, RELEASETH_0, 2 * i, RELEASE_THRESH);
        }

        // set filters
        write_byte!(self.i2c, addr, MHDR, 0x01);
        write_byte!(self.i2c, addr, NHDR, 0x01);
        write_byte!(self.i2c, addr, NCLR, 0x0e);
        write_byte!(self.i2c, addr, FDLR, 0x00);

        write_byte!(self.i2c, addr, MHDF, 0x01);
        write_byte!(self.i2c, addr, NHDF, 0x05);
        write_byte!(self.i2c, addr, NCLF, 0x01);
        write_byte!(self.i2c, addr, FDLF, 0x00);

        write_byte!(self.i2c, addr, NHDT, 0x00);
        write_byte!(self.i2c, addr, NCLT, 0x00);
        write_byte!(self.i2c, addr, FDLT, 0x00);

        write_byte!(self.i2c, addr, DEBOUNCE, 0x00);
        write_byte!(self.i2c, addr, CONFIG1, 0x10); // default 16uA charge current
        write_byte!(self.i2c, addr, CONFIG2, 0x20); // 0.5us encoding, 1ms period

        // autoconfig for Vdd = 3.3V
        write_byte!(self.i2c, addr, AUTOCONFIG0, 0x0b);
        write_byte!(self.i2c, addr, UPLIMIT, 200); // (Vdd - 0.7) / Vdd * 256
        write_byte!(self.i2c, addr, TARGETLIMIT, 180); // UPLIMIT * 0.9
        write_byte!(self.i2c, addr, LOWLIMIT, 130); // UPLIMIT * 0.65

        // enable 12 electrodes & start
        write_byte!(self.i2c, addr, ECR, 0b10000000 + 12);

        Ok(())
    }

    pub fn touched(&mut self, addr: u8) -> Result<u16, crate::hal::i2c::Error> {
        let mut buf = [0u8; 2];
        self.i2c
            .write_read(addr, &[Regs::TOUCHSTATUS_L as u8], &mut buf)?;
        crate::Mono.delay_ns(WRITE_DELAY);
        Ok(u16::from_le_bytes(buf) & 0x0fff)
    }
}

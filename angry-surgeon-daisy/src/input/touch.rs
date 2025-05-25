use embassy_stm32::exti::ExtiInput;
use embedded_hal_async::i2c::I2c;

macro_rules! write_byte {
    ($i2c:expr,$addr:ident,$offset:expr,$byte:expr) => {{
        $i2c.write(ADDR, &[Regs::$addr as u8 + $offset, $byte])
            .await?;
        // rise/fall time
        embassy_time::Timer::after_nanos(DELAY).await;
    }};
    ($i2c:expr,$addr:ident,$byte:expr) => {
        write_byte!($i2c, $addr, 0, $byte);
    };
}

const DELAY: u64 = 300;
const ADDR: u8 = 0x5a;
const TOUCH_THRESH: u8 = 12;
const RELEASE_THRESH: u8 = 6;

#[derive(Debug)]
pub enum Error<T: embedded_hal_async::i2c::Error> {
    Boot,
    I2c(T),
}

impl<T: embedded_hal_async::i2c::Error> From<T> for Error<T> {
    fn from(value: T) -> Self {
        Self::I2c(value)
    }
}

#[allow(non_camel_case_types, clippy::upper_case_acronyms)]
enum Regs {
    TOUCHSTATUS_L = 0x00,
    // TOUCHSTATUS_H = 0x01,
    // FILTDATA_0L = 0x04,
    // FILTDATA_0H = 0x05,
    // BASELINE_0 = 0x1e,
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
    // CHARGECURR_0 = 0x5f,
    // CHARGETIME_1 = 0x6c,
    ECR = 0x5e,
    AUTOCONFIG0 = 0x7b,
    // AUTOCONFIG1 = 0x7c,
    UPLIMIT = 0x7d,
    LOWLIMIT = 0x7e,
    TARGETLIMIT = 0x7f,

    // GPIODIR = 0x76,
    // GPIOEN = 0x77,
    // GPIOSET = 0x78,
    // GPIOCLR = 0x79,
    // GPIOTOGGLE = 0x7a,
    SOFTRESET = 0x80,
}

pub struct Mpr121<'d, I: I2c> {
    i2c: I,
    exti: ExtiInput<'d>,
}

impl<'d, I: I2c> Mpr121<'d, I> {
    pub async fn new(mut i2c: I, exti: ExtiInput<'d>) -> Result<Self, Error<I::Error>> {
        // reset & stop
        write_byte!(i2c, SOFTRESET, 0x63);
        write_byte!(i2c, ECR, 0x00);

        // check boot state
        let mut buf = [0u8];
        i2c.write_read(ADDR, &[Regs::CONFIG2 as u8], &mut buf)
            .await?;
        if buf[0] != 0x24 {
            crate::print!("e", buf[0] as u32);
            return Err(Error::Boot);
        }

        // set thresholds
        for i in 0..12u8 {
            write_byte!(i2c, TOUCHTH_0, 2 * i, TOUCH_THRESH);
            write_byte!(i2c, RELEASETH_0, 2 * i, RELEASE_THRESH);
        }

        // set filters
        write_byte!(i2c, MHDR, 0x01);
        write_byte!(i2c, NHDR, 0x01);
        write_byte!(i2c, NCLR, 0x0e);
        write_byte!(i2c, FDLR, 0x00);

        write_byte!(i2c, MHDF, 0x01);
        write_byte!(i2c, NHDF, 0x05);
        write_byte!(i2c, NCLF, 0x01);
        write_byte!(i2c, FDLF, 0x00);

        write_byte!(i2c, NHDT, 0x00);
        write_byte!(i2c, NCLT, 0x00);
        write_byte!(i2c, FDLT, 0x00);

        write_byte!(i2c, DEBOUNCE, 0x00);
        write_byte!(i2c, CONFIG1, 0x10); // default 16uA charge current
        write_byte!(i2c, CONFIG2, 0x20); // 0.5us encoding, 1ms period

        // autoconfig for Vdd = 3.3V
        write_byte!(i2c, AUTOCONFIG0, 0x0b);
        write_byte!(i2c, UPLIMIT, 200); // (Vdd - 0.7) / Vdd * 256
        write_byte!(i2c, TARGETLIMIT, 180); // UPLIMIT * 0.9
        write_byte!(i2c, LOWLIMIT, 130); // UPLIMIT * 0.65

        // enable 12 electrodes & start
        write_byte!(i2c, ECR, 0b10000000 + 12);

        Ok(Self { i2c, exti })
    }

    pub async fn wait_for_touched(&mut self) -> Result<u16, Error<I::Error>> {
        self.exti.wait_for_falling_edge().await;
        let mut buf = [0u8; 2];
        self.i2c
            .write_read(ADDR, &[Regs::TOUCHSTATUS_L as u8], &mut buf)
            .await?;
        // embassy_time::Timer::after_nanos(DELAY).await;
        Ok(u16::from_le_bytes(buf) & 0x0fff)
    }
}

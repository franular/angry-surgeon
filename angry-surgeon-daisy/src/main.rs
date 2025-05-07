#![no_std]
#![no_main]

mod audio;
mod fs;
mod serial;

use embassy_executor::Spawner;
use embassy_stm32::time::Hertz;
use {defmt_rtt as _, panic_probe as _};

embassy_stm32::bind_interrupts!(struct Irqs {
    OTG_FS => embassy_stm32::usb::InterruptHandler<embassy_stm32::peripherals::USB_OTG_FS>;
    SDMMC1 => embassy_stm32::sdmmc::InterruptHandler<embassy_stm32::peripherals::SDMMC1>;
});

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let config = {
        use embassy_stm32::rcc::*;

        let mut config = embassy_stm32::Config::default();
        config.rcc.hse = Some(Hse {
            freq: Hertz::mhz(16),
            mode: HseMode::Oscillator,
        });
        config.rcc.pll1 = Some(Pll {
            source: PllSource::HSE,
            prediv: PllPreDiv::DIV4,
            mul: PllMul::MUL240,
            divp: Some(PllDiv::DIV2),
            divq: Some(PllDiv::DIV20),
            divr: Some(PllDiv::DIV2),
        });
        config.rcc.pll2 = Some(Pll {
            source: PllSource::HSE,
            prediv: PllPreDiv::DIV4,
            mul: PllMul::MUL50,
            divp: None,
            divq: None,
            divr: Some(PllDiv::DIV1),
        });
        config.rcc.pll3 = Some(Pll {
            source: PllSource::HSE,
            prediv: PllPreDiv::DIV6,
            mul: PllMul::MUL295,
            divp: Some(PllDiv::DIV16),
            divq: Some(PllDiv::DIV4),
            divr: Some(PllDiv::DIV32),
        });
        config.rcc.sys = Sysclk::PLL1_P; // 480 MHz
        config.rcc.mux.sai1sel = mux::Saisel::PLL3_P; // 49.2 MHz
        config.rcc.mux.sdmmcsel = mux::Sdmmcsel::PLL2_R; // 200 MHz
        config.rcc.mux.usbsel = mux::Usbsel::PLL1_Q; // 48 MHz
        config.rcc.ahb_pre = AHBPrescaler::DIV2; // 240 MHz
        config.rcc.apb1_pre = APBPrescaler::DIV2; // 120 MHz
        config.rcc.apb2_pre = APBPrescaler::DIV2; // 120 MHz
        config.rcc.apb3_pre = APBPrescaler::DIV2; // 120 MHz
        config.rcc.apb4_pre = APBPrescaler::DIV2; // 120 MHz
        config.rcc.voltage_scale = VoltageScale::Scale0;
        config
    };
    let p = embassy_stm32::init(config);

    // init user led
    let mut led = embassy_stm32::gpio::Output::new(
        p.PC7,
        embassy_stm32::gpio::Level::Low,
        embassy_stm32::gpio::Speed::High,
    );

    // init usb serial
    let serial_data =
        cortex_m::singleton!(: serial::SerialData = serial::SerialData::new()).unwrap();
    let (usb, class) = serial::init_usb_class(p.USB_OTG_FS, Irqs, p.PA12, p.PA11, serial_data);
    spawner.must_spawn(serial::serial(usb, class, serial::CHANNEL.dyn_receiver()));

    // init sai audio
    let (sai_tx, sai_rx) = audio::hw::init_sai_tx_rx(
        p.SAI1, p.PE5, p.PE4, p.PE2, p.PE6, p.PE3, p.DMA1_CH0, p.DMA1_CH1,
    );
    spawner.must_spawn(audio::pass(sai_tx, sai_rx, serial::CHANNEL.dyn_sender()));

    // init sd filesystem
    let sdmmc = fs::init_sdmmc(p.SDMMC1, Irqs, p.PC12, p.PD2, p.PC8, p.PC9, p.PC10, p.PC11).await;
    print!("sd card recognized!");
}

#![no_std]
#![no_main]

mod audio;
mod fs;
mod input;
mod serial;
mod tui;

use angry_surgeon_core::GRAIN_LEN;
use embassy_executor::Spawner;
use embassy_stm32::time::Hertz;
use embassy_sync::zerocopy_channel::Channel as ZeroCopyChannel;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel};
use ssd1306::mode::DisplayConfigAsync;
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

extern crate alloc;

embassy_stm32::bind_interrupts!(struct Irqs {
    I2C1_EV => embassy_stm32::i2c::EventInterruptHandler<embassy_stm32::peripherals::I2C1>;
    I2C1_ER => embassy_stm32::i2c::ErrorInterruptHandler<embassy_stm32::peripherals::I2C1>;
    OTG_FS => embassy_stm32::usb::InterruptHandler<embassy_stm32::peripherals::USB_OTG_FS>;
    SDMMC1 => embassy_stm32::sdmmc::InterruptHandler<embassy_stm32::peripherals::SDMMC1>;
});

#[global_allocator]
static HEAP: embedded_alloc::LlffHeap = embedded_alloc::LlffHeap::empty();

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    {
        use core::mem::MaybeUninit;
        const HEAP_SIZE: usize = 65535;
        static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
        unsafe { HEAP.init(HEAP_MEM.as_ptr() as usize, HEAP_SIZE) }
    }

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

    // init sd filesystem
    let sdmmc = fs::hw::init_sdmmc(p.SDMMC1, Irqs, p.PC12, p.PD2, p.PC8, p.PC9, p.PC10, p.PC11)
        .await
        .unwrap();
    let fs_options = embedded_fatfs::FsOptions::new();
    static FS: StaticCell<fs::hw::FileSystem> = StaticCell::new();
    let fs = FS.init_with(|| {
        embassy_futures::block_on(embedded_fatfs::FileSystem::new(sdmmc, fs_options)).unwrap()
    });
    let root_dir = fs.root_dir();

    static GRAIN_BUF: StaticCell<[[u16; GRAIN_LEN]; 1]> = StaticCell::new();
    let grain_buf = GRAIN_BUF.init_with(|| [[0u16; GRAIN_LEN]]);
    static GRAIN_CH: StaticCell<ZeroCopyChannel<'_, NoopRawMutex, [u16; GRAIN_LEN]>> =
        StaticCell::new();
    let (grain_tx, grain_rx) = GRAIN_CH
        .init_with(|| ZeroCopyChannel::new(grain_buf))
        .split();

    static AUDIO_CH: StaticCell<Channel<NoopRawMutex, audio::Cmd, 1>> = StaticCell::new();
    let audio_ch = AUDIO_CH.init_with(Channel::new);
    static TUI_CH: StaticCell<Channel<NoopRawMutex, tui::Cmd, 1>> = StaticCell::new();
    let tui_ch = TUI_CH.init_with(Channel::new);

    let scene = angry_surgeon_core::SceneHandler::new(audio::STEP_DIV, audio::LOOP_DIV);
    spawner.must_spawn(audio::scene_handler(
        root_dir.clone(),
        scene,
        grain_tx,
        audio_ch.dyn_receiver(),
    ));

    // init sai audio
    let sai_tx = audio::hw::init_sai_tx(p.SAI1, p.PE5, p.PE4, p.PE2, p.PE6, p.DMA1_CH0);
    spawner.must_spawn(audio::output(sai_tx, grain_rx));

    // init i2c1
    let i2c = embassy_stm32::i2c::I2c::new(
        p.I2C1,
        p.PB8,
        p.PB9,
        Irqs,
        p.DMA1_CH2,
        p.DMA1_CH3,
        Hertz(400_000),
        Default::default(),
    );
    static I2C_BUS: StaticCell<
        input::i2c::Shared<
            NoopRawMutex,
            embassy_stm32::i2c::I2c<'static, embassy_stm32::mode::Async>,
        >,
    > = StaticCell::new();
    let i2c_bus = I2C_BUS.init_with(|| input::i2c::Shared::new(i2c));

    // init mpr121
    let mpr121_irq =
        embassy_stm32::exti::ExtiInput::new(p.PB5, p.EXTI5, embassy_stm32::gpio::Pull::Up);
    let mpr121 = input::touch::Mpr121::new(i2c_bus.get_ref(), mpr121_irq)
        .await
        .unwrap();

    // init ssd1306
    let interface = ssd1306::I2CDisplayInterface::new(i2c_bus.get_ref());
    let ssd1306 = ssd1306::Ssd1306Async::new(
        interface,
        ssd1306::size::DisplaySize128x64,
        ssd1306::rotation::DisplayRotation::Rotate0,
    )
    .into_terminal_mode();

    // spawner.must_spawn(log(mpr121, ssd1306));

    // init encoder
    let ch1 = embassy_stm32::exti::ExtiInput::new(p.PA6, p.EXTI6, embassy_stm32::gpio::Pull::Up);
    let ch2 = embassy_stm32::exti::ExtiInput::new(p.PA7, p.EXTI7, embassy_stm32::gpio::Pull::Up);
    let encoder = input::digital::Encoder::new(ch1, ch2);

    let scenes_sw = input::digital::Debounce::new(
        embassy_stm32::exti::ExtiInput::new(p.PC0, p.EXTI0, embassy_stm32::gpio::Pull::Up),
        embassy_time::Duration::from_millis(20),
    );
    let onsets_sw = input::digital::Debounce::new(
        embassy_stm32::exti::ExtiInput::new(p.PA3, p.EXTI3, embassy_stm32::gpio::Pull::Up),
        embassy_time::Duration::from_millis(20),
    );

    spawner.must_spawn(input::input(
        root_dir,
        input::InputHandler::new(),
        scenes_sw,
        onsets_sw,
        encoder,
        mpr121,
        audio_ch.dyn_sender(),
        tui_ch.dyn_sender(),
    ));

    print!("here!");
}

#[embassy_executor::task]
async fn test_encoder(
    mut encoder: input::digital::Encoder<'static>,
    mut ssd1306: ssd1306::Ssd1306Async<
        ssd1306::prelude::I2CInterface<
            input::i2c::Ref<
                'static,
                NoopRawMutex,
                embassy_stm32::i2c::I2c<'static, embassy_stm32::mode::Async>,
            >,
        >,
        ssd1306::size::DisplaySize128x64,
        ssd1306::mode::TerminalModeAsync,
    >,
) {
    ssd1306.init().await.unwrap();
    let _ = ssd1306.clear().await;
    let mut count = 0u8;
    loop {
        match encoder.wait_for_direction().await {
            input::digital::Direction::Counterclockwise => count = count.wrapping_sub(1),
            input::digital::Direction::Clockwise => count = count.wrapping_add(1),
        }
        let _ = ssd1306.clear().await;
        let mut buf = [b' '; 12];
        if count == 0 {
            buf[0] = char::from_digit(0, 10).unwrap() as u8;
        } else {
            let mut i = 0;
            let mut digit = 10u32.pow(count.ilog10());
            while digit != 0 {
                buf[i] = char::from_digit((count as u32 / digit) % 10, 10).unwrap() as u8;
                digit /= 10;
                i += 1;
            }
        }
        let _ = ssd1306.write_str(core::str::from_utf8(&buf).unwrap()).await;
    }
}

#[embassy_executor::task]
async fn log(
    mut mpr121: input::touch::Mpr121<
        'static,
        input::i2c::Ref<
            'static,
            NoopRawMutex,
            embassy_stm32::i2c::I2c<'static, embassy_stm32::mode::Async>,
        >,
    >,
    mut ssd1306: ssd1306::Ssd1306Async<
        ssd1306::prelude::I2CInterface<
            input::i2c::Ref<
                'static,
                NoopRawMutex,
                embassy_stm32::i2c::I2c<'static, embassy_stm32::mode::Async>,
            >,
        >,
        ssd1306::size::DisplaySize128x64,
        ssd1306::mode::TerminalModeAsync,
    >,
) {
    ssd1306.init().await.unwrap();
    let _ = ssd1306.clear().await;
    loop {
        if let Ok(touched) = mpr121.wait_for_touched().await {
            let _ = ssd1306.clear().await;
            let mut buf = [0u8; 12];
            for (i, b) in buf.iter_mut().enumerate() {
                let down = (touched >> i) & 1;
                *b = char::from_digit(down as u32, 2).unwrap() as u8;
            }
            let _ = ssd1306.write_str(core::str::from_utf8(&buf).unwrap()).await;
        }
    }
}

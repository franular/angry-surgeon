#![no_std]
#![no_main]

mod audio;
mod fs;
mod input;
mod serial;
mod tui;

use angry_surgeon_core::GRAIN_LEN;
use embassy_executor::Spawner;
use embassy_stm32::adc::AdcChannel;
use embassy_stm32::{exti::ExtiInput, time::Hertz};
use embassy_sync::zerocopy_channel::Channel as ZeroCopyChannel;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel};
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
    // init allocator
    {
        use core::mem::MaybeUninit;
        const HEAP_SIZE: usize = 65535;
        static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
        #[allow(static_mut_refs)]
        unsafe {
            HEAP.init(HEAP_MEM.as_ptr() as usize, HEAP_SIZE)
        }
    }

    // init clocks
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

    // -----------------------------------------------------------------------------
    // --- USB SERIAL TASK
    static SERIAL_DATA: StaticCell<serial::SerialData> = StaticCell::new();
    let serial_data = SERIAL_DATA.init_with(|| serial::SerialData::new());
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

    // init channels
    static GRAIN_BUF: StaticCell<[[u16; GRAIN_LEN]; 1]> = StaticCell::new();
    let grain_buf = GRAIN_BUF.init_with(|| [[0u16; GRAIN_LEN]]);
    static GRAIN_CH: StaticCell<ZeroCopyChannel<'_, NoopRawMutex, [u16; GRAIN_LEN]>> =
        StaticCell::new();
    let (grain_tx, grain_rx) = GRAIN_CH
        .init_with(|| ZeroCopyChannel::new(grain_buf))
        .split();
    static AUDIO_CH: StaticCell<Channel<NoopRawMutex, audio::Cmd, 1>> = StaticCell::new();
    let audio_ch = AUDIO_CH.init_with(Channel::new);
    static CLOCK_CH: StaticCell<Channel<NoopRawMutex, f32, 1>> = StaticCell::new();
    let clock_ch = CLOCK_CH.init_with(Channel::new);
    static TUI_CH: StaticCell<Channel<NoopRawMutex, tui::Cmd, 1>> = StaticCell::new();
    let tui_ch = TUI_CH.init_with(Channel::new);

    // -----------------------------------------------------------------------------
    // --- TUI HANDLER TASK
    let interface = ssd1306::I2CDisplayInterface::new(i2c_bus.get_ref());
    let display = ssd1306::Ssd1306Async::new(
        interface,
        ssd1306::size::DisplaySize128x64,
        ssd1306::rotation::DisplayRotation::Rotate0,
    )
    .into_buffered_graphics_mode();
    spawner.must_spawn(tui::tui_handler(
        tui::TuiHandler::new(),
        display,
        tui_ch.dyn_receiver(),
    ));

    // -----------------------------------------------------------------------------
    // --- INPUT HANDLER TASK
    // touch sensors
    // TODO: add sensors for bank b and support in input::input()
    let mpr121_irq_a = ExtiInput::new(p.PB6, p.EXTI6, embassy_stm32::gpio::Pull::Up); // D13
    let mpr121_a = input::touch::Mpr121::new(i2c_bus.get_ref(), mpr121_irq_a)
        .await
        .unwrap();
    // encoder
    let ch1 = ExtiInput::new(p.PB4, p.EXTI4, embassy_stm32::gpio::Pull::Up); // D9
    let ch2 = ExtiInput::new(p.PB5, p.EXTI5, embassy_stm32::gpio::Pull::Up); // D10
    let encoder = input::digital::Encoder::new(ch1, ch2);
    // switches
    let scenes_sw = input::digital::Debounce::new(
        ExtiInput::new(p.PG10, p.EXTI10, embassy_stm32::gpio::Pull::Up), // D7
        embassy_time::Duration::from_millis(20),
    );
    let onsets_sw = input::digital::Debounce::new(
        ExtiInput::new(p.PG11, p.EXTI11, embassy_stm32::gpio::Pull::Up), // D8
        embassy_time::Duration::from_millis(20),
    );
    spawner.must_spawn(input::input(
        fs.root_dir(),
        input::InputHandler::new(),
        scenes_sw,
        onsets_sw,
        encoder,
        mpr121_a,
        audio_ch.dyn_sender(),
        tui_ch.dyn_sender(),
    ));

    // -----------------------------------------------------------------------------
    // --- POTENTIOMETERS TASK
    let pots_a = input::analog::Pots::new(
        p.PC0, // A0
        p.PA3, // A1
        p.PB1, // A2
        p.PA7, // A3
        p.PA6, // A4
    );
    let pots_b = input::analog::Pots::new(
        p.PC1, // A5
        p.PC4, // A6
        p.PA5, // A7
        p.PA4, // A8
        p.PA1, // A9
    );
    let tempo_pot = p.PA0.degrade_adc();
    spawner.must_spawn(input::analog::adc(
        embassy_stm32::adc::Adc::new(p.ADC1),
        p.DMA1_CH1,
        pots_a,
        pots_b,
        tempo_pot,
        clock_ch.dyn_sender(),
        audio_ch.dyn_sender(),
        tui_ch.dyn_sender(),
    ));

    // -----------------------------------------------------------------------------
    // --- CLOCK I/O TASK
    let ground_in = input::digital::Debounce::new(
        ExtiInput::new(p.PG9, p.EXTI9, embassy_stm32::gpio::Pull::Up), // D27
        embassy_time::Duration::from_millis(20),
    );
    let clock_in = input::digital::Debounce::new(
        ExtiInput::new(p.PA2, p.EXTI2, embassy_stm32::gpio::Pull::Down), // D28
        embassy_time::Duration::from_millis(20),
    );
    let clock_out = embassy_stm32::gpio::Output::new(
        p.PD11, // D26
        embassy_stm32::gpio::Level::Low,
        embassy_stm32::gpio::Speed::VeryHigh,
    );
    let tempo_led = embassy_stm32::gpio::Output::new(
        p.PC7, // user led
        embassy_stm32::gpio::Level::Low,
        embassy_stm32::gpio::Speed::High,
    );
    spawner.must_spawn(input::digital::clock(
        ground_in,
        clock_in,
        clock_out,
        tempo_led,
        audio_ch.dyn_sender(),
        clock_ch.dyn_receiver(),
    ));

    // -----------------------------------------------------------------------------
    // --- SCENE HANDLER TASK
    let scene_handler = angry_surgeon_core::SceneHandler::new(audio::STEP_DIV, audio::LOOP_DIV);
    spawner.must_spawn(audio::scene_handler(
        fs.root_dir(),
        scene_handler,
        grain_tx,
        audio_ch.dyn_receiver(),
    ));

    // -----------------------------------------------------------------------------
    // --- SAI OUTPUT TASK
    let sai_tx = audio::hw::init_sai_tx(p.SAI1, p.PE5, p.PE4, p.PE2, p.PE6, p.DMA1_CH0);
    spawner.must_spawn(audio::output(sai_tx, grain_rx));

    print!("finished init!");
}

#![no_std]
#![no_main]

extern crate alloc;
use stm32h7xx_hal as hal;

mod audio;
mod fs;
mod input;

rtic_monotonics::systick_monotonic!(Mono, 1_000_000); // us resolution

#[global_allocator]
static HEAP: embedded_alloc::LlffHeap = embedded_alloc::LlffHeap::empty();

#[rtic::app(device = hal::stm32, peripherals = true, dispatchers = [SPI1, SPI2])]
mod app {
    use crate::*;
    use angry_surgeon_core::{FileHandler, GRAIN_LEN};
    use embedded_hal::delay::DelayNs;
    use hal::prelude::*;
    use hal::traits::i2s::FullDuplex;
    use micromath::F32Ext;
    use rtic_monotonics::Monotonic;
    use rtic_monotonics::fugit::MicrosDurationU32;
    use rtic_sync::signal::Signal;
    use stm32h7xx_hal::gpio::ExtiPin;
    use tinyrand::Seeded;

    const DMA_BUFFER_LEN: usize = GRAIN_LEN * 2;

    #[unsafe(link_section = ".sram1_bss")]
    static TX_BUFFER0: grounded::uninit::GroundedArrayCell<u32, DMA_BUFFER_LEN> =
        grounded::uninit::GroundedArrayCell::uninit();
    #[unsafe(link_section = ".sram1_bss")]
    static TX_BUFFER1: grounded::uninit::GroundedArrayCell<u32, DMA_BUFFER_LEN> =
        grounded::uninit::GroundedArrayCell::uninit();
    #[unsafe(link_section = ".sram1_bss")]
    static ADC_BUFFER: grounded::uninit::GroundedArrayCell<u16, { input::analog::CHANNEL_COUNT }> =
        grounded::uninit::GroundedArrayCell::uninit();

    #[shared]
    struct Shared {
        tempo_tx: (
            input::clock::Source,
            rtic_sync::signal::SignalWriter<'static, f32>,
        ),
        system: angry_surgeon_core::SystemHandler<
            { audio::BANK_COUNT },
            { audio::PAD_COUNT },
            { audio::MAX_PHRASE_LEN },
            { audio::MAX_PHRASE_COUNT },
            fs::FileHandler,
            tinyrand::Wyrand,
        >,
        led: hal::gpio::PC7<hal::gpio::Output<hal::gpio::PushPull>>,
    }

    #[local]
    struct Local {
        shift_tx: [rtic_sync::signal::SignalWriter<'static, bool>; 2],
        shift_rx: [rtic_sync::signal::SignalReader<'static, bool>; 2],

        clock_in_signal: hal::gpio::PG10<hal::gpio::Input>,
        last_clock_in: Option<rtic_monotonics::fugit::Instant<u32, 1, 1_000_000>>,

        input_handler: input::InputHandler,
        mpr121: input::touch::Mpr121Interface,
        mpr121_a: input::touch::Mpr121Data<hal::gpio::gpiob::PB6<hal::gpio::Input>>,
        mpr121_b: input::touch::Mpr121Data<hal::gpio::gpiob::PB7<hal::gpio::Input>>,

        adc1_transfer: hal::dma::Transfer<
            hal::dma::dma::Stream1<hal::stm32::DMA1>,
            hal::adc::Adc<hal::pac::ADC1, hal::adc::Enabled>,
            hal::dma::PeripheralToMemory,
            &'static mut [u16],
            hal::dma::DBTransfer,
        >,
        adc_data: input::analog::AdcData,

        sai1_transfer: hal::dma::Transfer<
            hal::dma::dma::Stream0<hal::stm32::DMA1>,
            hal::sai::dma::ChannelA<hal::stm32::SAI1>,
            hal::dma::MemoryToPeripheral,
            &'static mut [u32],
            hal::dma::DBTransfer,
        >,
    }

    #[init]
    fn init(mut cx: init::Context) -> (Shared, Local) {
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

        let pwrcfg = cx.device.PWR.constrain().vos0(&cx.device.SYSCFG).freeze();
        let rcc = cx.device.RCC.constrain();

        let mut ccdr = rcc
            .use_hse(16.MHz())
            .sys_ck(480.MHz())
            .hclk(240.MHz())
            .pclk1(120.MHz())
            .pll1_strategy(hal::rcc::PllConfigStrategy::Iterative)
            .pll1_q_ck(96.MHz()) // sdmmc
            .pll2_p_ck(80.MHz()) // adc
            .pll3_strategy(hal::rcc::PllConfigStrategy::Fractional)
            .pll3_p_ck((audio::SAMPLE_RATE * 257).Hz()) // sai
            .freeze(pwrcfg, &cx.device.SYSCFG);
        ccdr.peripheral
            .kernel_sdmmc_clk_mux(hal::rcc::rec::SdmmcClkSel::Pll1Q);
        let sai1_rec = ccdr
            .peripheral
            .SAI1
            .kernel_clk_mux(hal::rcc::rec::Sai1ClkSel::Pll3P);

        Mono::start(cx.core.SYST, 480_000_000);

        let gpiob = cx.device.GPIOB.split(ccdr.peripheral.GPIOB);
        let gpioc = cx.device.GPIOC.split(ccdr.peripheral.GPIOC);
        let gpiod = cx.device.GPIOD.split(ccdr.peripheral.GPIOD);
        let gpioe = cx.device.GPIOE.split(ccdr.peripheral.GPIOE);
        let gpiog = cx.device.GPIOG.split(ccdr.peripheral.GPIOG);

        let led = gpioc.pc7.into_push_pull_output();
        let dma1_streams = hal::dma::dma::StreamsTuple::new(cx.device.DMA1, ccdr.peripheral.DMA1);

        // -------------------------------------------------------------------------
        // --- SDMMC INIT
        let sdmmc_pins = (
            gpioc
                .pc12
                .into_alternate()
                .internal_pull_up(false)
                .speed(hal::gpio::Speed::VeryHigh),
            gpiod
                .pd2
                .into_alternate()
                .internal_pull_up(true)
                .speed(hal::gpio::Speed::VeryHigh),
            gpioc
                .pc8
                .into_alternate()
                .internal_pull_up(true)
                .speed(hal::gpio::Speed::VeryHigh),
            gpioc
                .pc9
                .into_alternate()
                .internal_pull_up(true)
                .speed(hal::gpio::Speed::VeryHigh),
            gpioc
                .pc10
                .into_alternate()
                .internal_pull_up(true)
                .speed(hal::gpio::Speed::VeryHigh),
            gpioc
                .pc11
                .into_alternate()
                .internal_pull_up(true)
                .speed(hal::gpio::Speed::VeryHigh),
        );
        let mut sdmmc: hal::sdmmc::Sdmmc<_, hal::sdmmc::SdCard> =
            cx.device
                .SDMMC1
                .sdmmc(sdmmc_pins, ccdr.peripheral.SDMMC1, &ccdr.clocks);
        while sdmmc.init(24.MHz()).is_err() {
            Mono.delay_ms(1000);
        }
        let vol_mgr = embedded_sdmmc::VolumeManager::new_with_limits(
            sdmmc.sdmmc_block_device(),
            fs::TimeSource,
            0,
        );
        let fs = fs::FileHandler::new(vol_mgr).unwrap();

        // -------------------------------------------------------------------------
        // --- I2C INIT (MPR121)
        let i2c1_pins = (
            gpiob.pb8.into_alternate_open_drain(),
            gpiob.pb9.into_alternate_open_drain(),
        );
        let i2c1 = cx
            .device
            .I2C1
            .i2c(i2c1_pins, 400.kHz(), ccdr.peripheral.I2C1, &ccdr.clocks);

        let mut mpr121 = input::touch::Mpr121Interface::new(i2c1);
        let mut mpr121_a_irq = gpiob.pb6.into_floating_input(); // D13
        mpr121_a_irq.make_interrupt_source(&mut cx.device.SYSCFG);
        mpr121_a_irq.trigger_on_edge(&mut cx.device.EXTI, hal::gpio::Edge::Falling);
        mpr121_a_irq.enable_interrupt(&mut cx.device.EXTI);
        let mpr121_a = input::touch::Mpr121Data::new(
            0x5a, // ADDR to GND (use jumper)
            mpr121_a_irq,
            &mut cx.device.SYSCFG,
            &mut cx.device.EXTI,
        );
        let mut mpr121_b_irq = gpiob.pb7.into_floating_input(); // D14
        mpr121_b_irq.make_interrupt_source(&mut cx.device.SYSCFG);
        mpr121_b_irq.trigger_on_edge(&mut cx.device.EXTI, hal::gpio::Edge::Falling);
        mpr121_b_irq.enable_interrupt(&mut cx.device.EXTI);
        let mpr121_b = input::touch::Mpr121Data::new(
            0x5b, // ADDR to VSS (jumper cut)
            mpr121_b_irq,
            &mut cx.device.SYSCFG,
            &mut cx.device.EXTI,
        );
        mpr121.init(mpr121_a.addr).unwrap();
        mpr121.init(mpr121_b.addr).unwrap();

        unsafe {
            hal::pac::NVIC::unmask(hal::pac::interrupt::EXTI9_5);
        }

        // -------------------------------------------------------------------------
        // --- ADC INIT
        let adc3 = hal::adc::Adc::adc3(
            cx.device.ADC3,
            4.MHz(),
            &mut Mono,
            ccdr.peripheral.ADC3,
            &ccdr.clocks,
        );
        let adc_data = input::analog::init_data(adc3, &mut cx.device.ADC3_COMMON);
        let adc1 = hal::adc::Adc::adc1(
            cx.device.ADC1,
            4.MHz(),
            &mut Mono,
            ccdr.peripheral.ADC12,
            &ccdr.clocks,
        )
        .enable();
        let adc_buffer: &mut [u16] = unsafe {
            ADC_BUFFER.initialize_all_copied(0);
            let (ptr, len) = ADC_BUFFER.get_ptr_len();
            core::slice::from_raw_parts_mut(ptr, len)
        };
        let config = hal::dma::dma::DmaConfig::default()
            .priority(hal::dma::config::Priority::VeryHigh)
            .memory_increment(true)
            .transfer_complete_interrupt(true);
        let mut adc1_transfer: hal::dma::Transfer<_, _, hal::dma::PeripheralToMemory, _, _> =
            hal::dma::Transfer::init(dma1_streams.1, adc1, adc_buffer, None, config);

        unsafe {
            hal::pac::NVIC::unmask(hal::pac::Interrupt::DMA1_STR1);
        }

        adc1_transfer.start(input::analog::start_seq);

        // -------------------------------------------------------------------------
        // --- CLOCK INIT
        let mut clock_in_signal = gpiog.pg10.into_pull_down_input(); // D7
        clock_in_signal.make_interrupt_source(&mut cx.device.SYSCFG);
        clock_in_signal.trigger_on_edge(&mut cx.device.EXTI, hal::gpio::Edge::Rising);
        clock_in_signal.enable_interrupt(&mut cx.device.EXTI);
        // // FIXME: i think i broke this pin on my own hardware...
        // let mut clock_in_insert = gpiog.pg11.into_pull_up_input();   // D8
        // clock_in_insert.make_interrupt_source(&mut cx.device.SYSCFG);
        // clock_in_insert.trigger_on_edge(&mut cx.device.EXTI, hal::gpio::Edge::Falling);
        // clock_in_insert.enable_interrupt(&mut cx.device.EXTI);
        let clock_out = gpiob.pb4.into_push_pull_output(); // D9
        let tempo_led = gpiob.pb5.into_push_pull_output(); // D10

        unsafe {
            hal::pac::NVIC::unmask(hal::pac::Interrupt::EXTI15_10);
        }

        // -------------------------------------------------------------------------
        // --- SYSTEM HANDLER INIT
        let mut system =
            audio::SystemHandler::new(fs, tinyrand::Wyrand::seed(0xf2aa), audio::STEP_DIV, 8.);
        // init for testing
        {
            system.assign_tempo(192.);
            let bd_file = system.fs.open("banks/bank0.bd").unwrap();
            let mut reader = crate::fs::BufReader::new(&mut system.fs, bd_file).unwrap();
            let mut bytes = alloc::vec::Vec::new();
            while let Ok(Some(c)) = reader.next() {
                bytes.push(c);
            }
            if let Ok(bd) = serde_json::from_slice::<
                angry_surgeon_core::Bank<{ audio::PAD_COUNT }, { audio::MAX_PHRASE_LEN }>,
            >(&bytes)
            {
                system.banks[1].bank = bd;
            }
        }
        let input_handler = input::InputHandler::new();

        // -------------------------------------------------------------------------
        // --- SAI INIT
        let sai1_pins = (
            gpioe.pe2.into_alternate(),
            gpioe.pe5.into_alternate(),
            gpioe.pe4.into_alternate(),
            gpioe.pe6.into_alternate(),
            Option::<hal::gpio::Pin<'E', 3, hal::gpio::Alternate<6>>>::None,
        );
        let sai1_tx_config = hal::sai::I2SChanConfig::new(stm32h7xx_hal::sai::I2SDir::Tx)
            .set_clock_strobe(stm32h7xx_hal::sai::I2SClockStrobe::Falling)
            .set_frame_sync_active_high(true)
            .set_protocol(stm32h7xx_hal::sai::I2SProtocol::MSB)
            .set_frame_size(Some(64));
        let mut sai1 = cx.device.SAI1.i2s_ch_a(
            sai1_pins,
            48.kHz(),
            hal::sai::I2SDataSize::BITS_16,
            sai1_rec,
            &ccdr.clocks,
            hal::sai::I2sUsers::new(sai1_tx_config),
        );

        let tx_buffer0: &mut [u32] = unsafe {
            TX_BUFFER0.initialize_all_copied(0);
            let (ptr, len) = TX_BUFFER0.get_ptr_len();
            core::slice::from_raw_parts_mut(ptr, len)
        };
        let tx_buffer1: &mut [u32] = unsafe {
            TX_BUFFER1.initialize_all_copied(0);
            let (ptr, len) = TX_BUFFER1.get_ptr_len();
            core::slice::from_raw_parts_mut(ptr, len)
        };
        let dma_config = hal::dma::dma::DmaConfig::default()
            .priority(hal::dma::config::Priority::VeryHigh)
            .memory_increment(true)
            .transfer_complete_interrupt(true)
            .circular_buffer(true)
            .double_buffer(true);
        let mut sai1_transfer: hal::dma::Transfer<_, _, hal::dma::MemoryToPeripheral, _, _> =
            hal::dma::Transfer::init(
                dma1_streams.0,
                unsafe { hal::pac::Peripherals::steal().SAI1.dma_ch_a() },
                tx_buffer0,
                Some(tx_buffer1),
                dma_config,
            );

        unsafe {
            hal::pac::NVIC::unmask(hal::pac::Interrupt::DMA1_STR0);
        };

        sai1_transfer.start(|_| {
            sai1.enable_dma(hal::sai::SaiChannel::ChannelA);
            sai1.enable();
            sai1.try_send(0, 0).unwrap();
        });
        cx.core.SCB.enable_icache();

        let (tempo_tx, tempo_rx) = rtic_sync::make_signal!(f32);
        let (shift_a_tx, shift_a_rx) = rtic_sync::make_signal!(bool);
        let (shift_b_tx, shift_b_rx) = rtic_sync::make_signal!(bool);

        clock_out::spawn(tempo_rx, clock_out, tempo_led).unwrap();

        (
            Shared {
                tempo_tx: (input::clock::Source::Internal, tempo_tx),
                system,
                led,
            },
            Local {
                shift_tx: [shift_a_tx, shift_b_tx],
                shift_rx: [shift_a_rx, shift_b_rx],

                clock_in_signal,
                last_clock_in: None,

                input_handler,
                mpr121,
                mpr121_a,
                mpr121_b,

                adc1_transfer,
                adc_data,

                sai1_transfer,
            },
        )
    }

    #[task(shared = [system, led], priority = 2)]
    async fn clock_out(
        mut cx: clock_out::Context,
        mut tempo_rx: rtic_sync::signal::SignalReader<'static, f32>,
        clock_out: hal::gpio::PB4<hal::gpio::Output>,
        tempo_led: hal::gpio::PB5<hal::gpio::Output>,
    ) {
        use embassy_futures::select::*;

        let mut beat_dur = MicrosDurationU32::micros((60_000_000. / tempo_rx.wait().await) as u32);
        let mut last_step = Mono::now();

        let mut clock_out = input::clock::Blink::new(clock_out, last_step);
        let mut tempo_led = input::clock::Blink::new(tempo_led, last_step);

        loop {
            match select4(
                tempo_led.tick(
                    beat_dur,
                    MicrosDurationU32::micros(beat_dur.to_micros() / 2),
                ),
                clock_out.tick(
                    MicrosDurationU32::micros(beat_dur.to_micros() / audio::PPQ as u32),
                    MicrosDurationU32::millis(15),
                ),
                Mono::delay_until(
                    last_step
                        + MicrosDurationU32::micros(beat_dur.to_micros() / audio::STEP_DIV as u32),
                ),
                tempo_rx.wait(),
            )
            .await
            {
                Either4::First(()) => (),
                Either4::Second(()) => (),
                Either4::Third(()) => {
                    last_step +=
                        MicrosDurationU32::micros(beat_dur.to_micros() / audio::STEP_DIV as u32);
                    cx.shared.system.lock(|system| system.tick().unwrap());
                }
                Either4::Fourth(tempo) => {
                    beat_dur = MicrosDurationU32::micros((60_000_000. / tempo) as u32);
                    cx.shared.system.lock(|system| system.assign_tempo(tempo));
                }
            }
        }
    }

    #[task(binds = EXTI15_10, shared = [tempo_tx], local = [clock_in_signal, last_clock_in], priority = 3)]
    fn clock_in(mut cx: clock_in::Context) {
        cx.local.clock_in_signal.clear_interrupt_pending_bit();
        let now = Mono::now();
        // if cx.local.clock_in_insert.is_low() {
        //     // return to internal source when external clock cable disconnected (switch click)
        //     cx.shared.tempo_tx.lock(|tempo_tx| {
        //         tempo_tx.0 = input::clock::Source::Internal;
        //     });
        //     *cx.local.last_clock_in = None;
        // } else
        if let Some(last) = cx.local.last_clock_in {
            // tempo from external ppq when external clock cable already connected
            if now.checked_duration_since(*last).unwrap() > MicrosDurationU32::millis(15) {
                let beat_dur = MicrosDurationU32::micros(
                    now.checked_duration_since(*last).unwrap().to_micros() * audio::PPQ as u32,
                );
                let tempo = 60_000_000. / beat_dur.to_micros() as f32;
                cx.shared.tempo_tx.lock(|tempo_tx| {
                    tempo_tx.0 = input::clock::Source::External;
                    tempo_tx.1.write(tempo);
                });
                *cx.local.last_clock_in = Some(now);
            }
        } else {
            *cx.local.last_clock_in = Some(now);
        }
    }

    #[task(binds = EXTI9_5, shared = [system], local = [shift_tx, input_handler, mpr121, mpr121_a, mpr121_b], priority = 3)]
    fn mpr121(mut cx: mpr121::Context) {
        loop {
            match (
                cx.local.mpr121_a.irq.is_low(),
                cx.local.mpr121_b.irq.is_low(),
            ) {
                (true, _) => {
                    cx.local.mpr121_a.irq.clear_interrupt_pending_bit();
                    let curr = cx.local.mpr121.touched(cx.local.mpr121_a.addr).unwrap();
                    for index in 0..12 {
                        let curr = (curr >> index) & 1;
                        let last = (cx.local.mpr121_a.last >> index) & 1;
                        if curr != last {
                            if curr == 0 {
                                // release
                                cx.shared.system.lock(|system| {
                                    cx.local
                                        .input_handler
                                        .touch_up(audio::Bank::A, index, system)
                                        .unwrap();
                                });
                                if index == input::touch::pads::SHIFT {
                                    cx.local.shift_tx[usize::from(audio::Bank::A)].write(false);
                                }
                            } else {
                                // touch
                                cx.shared.system.lock(|system| {
                                    cx.local
                                        .input_handler
                                        .touch_down(audio::Bank::A, index, system)
                                        .unwrap();
                                });
                                if index == input::touch::pads::SHIFT {
                                    cx.local.shift_tx[usize::from(audio::Bank::A)].write(true);
                                }
                            }
                        }
                    }
                    cx.local.mpr121_a.last = curr;
                }
                (_, true) => {
                    cx.local.mpr121_b.irq.clear_interrupt_pending_bit();
                    let curr = cx.local.mpr121.touched(cx.local.mpr121_b.addr).unwrap();
                    for index in 0..12 {
                        let curr = (curr >> index) & 1;
                        let last = (cx.local.mpr121_b.last >> index) & 1;
                        if curr != last {
                            if curr == 0 {
                                // release
                                cx.shared.system.lock(|system| {
                                    cx.local
                                        .input_handler
                                        .touch_up(audio::Bank::B, index, system)
                                        .unwrap();
                                });
                                if index == input::touch::pads::SHIFT {
                                    cx.local.shift_tx[usize::from(audio::Bank::B)].write(false);
                                }
                            } else {
                                // touch
                                cx.shared.system.lock(|system| {
                                    cx.local
                                        .input_handler
                                        .touch_down(audio::Bank::B, index, system)
                                        .unwrap();
                                });
                                if index == input::touch::pads::SHIFT {
                                    cx.local.shift_tx[usize::from(audio::Bank::B)].write(true);
                                }
                            }
                        }
                    }
                    cx.local.mpr121_b.last = curr;
                }
                _ => break,
            }
        }
        cx.local.mpr121_a.irq.clear_interrupt_pending_bit();
        cx.local.mpr121_b.irq.clear_interrupt_pending_bit();
    }

    #[task(binds = DMA1_STR1, shared = [tempo_tx, system], local = [shift_rx, adc1_transfer, adc_data], priority = 3)]
    fn adc_in(mut cx: adc_in::Context) {
        let transfer = cx.local.adc1_transfer;
        let adc_data = cx.local.adc_data;

        for i in 0..audio::BANK_COUNT {
            if let Some(shift) = cx.local.shift_rx[i].try_read() {
                adc_data.pots[i].shift(shift);
            }
        }

        let _ = transfer.next_transfer_with(|buffer, _current, _incomplete| {
            for (index, sample) in buffer.iter().enumerate() {
                use input::analog::channels::*;

                let abs = *sample as f32 * adc_data.mult;

                macro_rules! pots {
                    ($bank:ident,$base:expr) => {
                        let index = index - $base as usize;
                        if adc_data.pots[usize::from(audio::Bank::$bank)].maybe_set(index, *sample)
                        {
                            cx.shared.system.lock(|system| {
                                let bank = &mut system.banks[usize::from(audio::Bank::$bank)];
                                match (index, adc_data.pots[usize::from(audio::Bank::$bank)].shift)
                                {
                                    (0, false) => bank.gain = abs * 2.,
                                    (0, true) => bank.width = abs,
                                    (1, false) => bank.speed.base = abs * 2.,
                                    (1, true) => bank.loop_div.base = (abs * 8.).round(),
                                    (2, false) => bank.kit_drift = abs,
                                    (2, true) => bank.phrase_drift = abs,
                                    _ => unreachable!(),
                                }
                            });
                        }
                    };
                }

                macro_rules! thumb {
                    ($bank:ident,$base:expr,$x_abs:expr) => {
                        let index = index - $base as usize;
                        let last = &mut adc_data.thumbs[usize::from(audio::Bank::$bank)][index];
                        if *sample != *last {
                            *last = *sample;
                            cx.shared.system.lock(|system| {
                                let bank = &mut system.banks[usize::from(audio::Bank::$bank)];
                                match index {
                                    0 => bank.speed.offset = $x_abs * 2.,
                                    1 => bank.loop_div.offset = abs * 2.,
                                    _ => unreachable!(),
                                }
                            });
                        }
                    };
                }

                match index as u8 {
                    TEMPO => {
                        if cx
                            .shared
                            .tempo_tx
                            .lock(|tempo_tx| matches!(tempo_tx.0, input::clock::Source::Internal))
                            && *sample != adc_data.tempo
                        {
                            adc_data.tempo = *sample;
                            let tempo = abs * 270. + 30.;
                            cx.shared.tempo_tx.lock(|tempo_tx| {
                                if tempo_tx.0 == input::clock::Source::Internal {
                                    tempo_tx.1.write(tempo);
                                }
                            });
                        }
                    }
                    i if POTS_A.contains(&i) => {
                        pots!(A, *POTS_A.start());
                    }
                    i if THUMB_A.contains(&i) => {
                        thumb!(A, *THUMB_A.start(), 1. - abs);
                    }
                    i if POTS_B.contains(&i) => {
                        pots!(B, *POTS_B.start());
                    }
                    i if THUMB_B.contains(&i) => {
                        thumb!(B, *THUMB_B.start(), abs);
                    }
                    _ => unreachable!(),
                }
            }
            (buffer, ())
        });
        transfer.start(|adc| {
            adc.inner_mut()
                .cr
                .modify(|_, w| w.adstart().start_conversion())
        });
    }

    #[task(binds = DMA1_STR0, shared = [led, system], local = [sai1_transfer], priority = 3)]
    fn audio_out(mut cx: audio_out::Context) {
        let transfer = cx.local.sai1_transfer;

        let mut f32_buffer = [0f32; DMA_BUFFER_LEN];
        cx.shared.system.lock(|system| {
            let _ = system.read_all::<{ audio::SAMPLE_RATE as u16 }, _>(&mut f32_buffer, 2);
        });
        unsafe {
            if transfer
                .next_dbm_transfer_with(|buffer, _current| {
                    for i in 0..DMA_BUFFER_LEN {
                        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
                        buffer[i] = (f32_buffer[i] * i16::MAX as f32) as i16 as u16 as u32;
                        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
                    }
                })
                .is_err()
            {
                cx.shared.led.lock(|led| led.set_high());
            };
        }
    }
}

#[inline(never)]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}

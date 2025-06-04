use embassy_stm32::{
    peripherals::SAI1,
    sai::{Dma, FsPin, Instance, MasterClockDivider, MclkPin, Sai, SckPin, SdPin, A},
    Peri,
};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use grounded::uninit::GroundedArrayCell;

pub(super) const HALF_DMA_BUFFER_LEN: usize = super::GRAIN_LEN * 2; // 2 channels
const DMA_BUFFER_LEN: usize = HALF_DMA_BUFFER_LEN * 2;

#[link_section = ".sram1_bss"]
static TX_BUFFER: GroundedArrayCell<u32, DMA_BUFFER_LEN> = GroundedArrayCell::uninit();

pub fn init_sai_tx<'d, T: Instance>(
    instance: Peri<'d, T>,
    sck: Peri<'d, impl SckPin<T, A>>,
    fs: Peri<'d, impl FsPin<T, A>>,
    mclk: Peri<'d, impl MclkPin<T, A>>,
    sd: Peri<'d, impl SdPin<T, A>>,
    dma: Peri<'d, impl Dma<T, A>>,
) -> Sai<'d, T, u32> {
    let (sub_block_tx, _) = embassy_stm32::sai::split_subblocks(instance);
    let tx_config = {
        use embassy_stm32::sai::*;

        let sai1_clk = embassy_stm32::rcc::frequency::<SAI1>().0;
        let mclk_div = (sai1_clk / (super::SAMPLE_RATE as u32 * 256)) as u8;

        let mut config = Config::default();
        config.mode = Mode::Master;
        config.tx_rx = TxRx::Transmitter;
        config.sync_output = true;
        config.clock_strobe = ClockStrobe::Falling;
        config.master_clock_divider = mclk_div_from_u8(mclk_div);
        config.stereo_mono = StereoMono::Stereo;
        config.data_size = DataSize::Data16;
        config.bit_order = BitOrder::MsbFirst;
        config.frame_sync_polarity = FrameSyncPolarity::ActiveHigh;
        config.frame_sync_offset = FrameSyncOffset::OnFirstBit;
        config.frame_length = 64;
        config.frame_sync_active_level_length = word::U7(32);
        config.fifo_threshold = FifoThreshold::Quarter;
        config
    };
    let tx_buffer: &mut [u32] = unsafe {
        TX_BUFFER.initialize_all_copied(0);
        let (ptr, len) = TX_BUFFER.get_ptr_len();
        core::slice::from_raw_parts_mut(ptr, len)
    };

    Sai::new_asynchronous_with_mclk(sub_block_tx, sck, sd, fs, mclk, dma, tx_buffer, tx_config)
}

const fn mclk_div_from_u8(v: u8) -> MasterClockDivider {
    match v {
        1 => MasterClockDivider::Div1,
        2 => MasterClockDivider::Div2,
        3 => MasterClockDivider::Div3,
        4 => MasterClockDivider::Div4,
        5 => MasterClockDivider::Div5,
        6 => MasterClockDivider::Div6,
        7 => MasterClockDivider::Div7,
        8 => MasterClockDivider::Div8,
        9 => MasterClockDivider::Div9,
        10 => MasterClockDivider::Div10,
        11 => MasterClockDivider::Div11,
        12 => MasterClockDivider::Div12,
        13 => MasterClockDivider::Div13,
        14 => MasterClockDivider::Div14,
        15 => MasterClockDivider::Div15,
        16 => MasterClockDivider::Div16,
        17 => MasterClockDivider::Div17,
        18 => MasterClockDivider::Div18,
        19 => MasterClockDivider::Div19,
        20 => MasterClockDivider::Div20,
        21 => MasterClockDivider::Div21,
        22 => MasterClockDivider::Div22,
        23 => MasterClockDivider::Div23,
        24 => MasterClockDivider::Div24,
        25 => MasterClockDivider::Div25,
        26 => MasterClockDivider::Div26,
        27 => MasterClockDivider::Div27,
        28 => MasterClockDivider::Div28,
        29 => MasterClockDivider::Div29,
        30 => MasterClockDivider::Div30,
        31 => MasterClockDivider::Div31,
        32 => MasterClockDivider::Div32,
        33 => MasterClockDivider::Div33,
        34 => MasterClockDivider::Div34,
        35 => MasterClockDivider::Div35,
        36 => MasterClockDivider::Div36,
        37 => MasterClockDivider::Div37,
        38 => MasterClockDivider::Div38,
        39 => MasterClockDivider::Div39,
        40 => MasterClockDivider::Div40,
        41 => MasterClockDivider::Div41,
        42 => MasterClockDivider::Div42,
        43 => MasterClockDivider::Div43,
        44 => MasterClockDivider::Div44,
        45 => MasterClockDivider::Div45,
        46 => MasterClockDivider::Div46,
        47 => MasterClockDivider::Div47,
        48 => MasterClockDivider::Div48,
        49 => MasterClockDivider::Div49,
        50 => MasterClockDivider::Div50,
        51 => MasterClockDivider::Div51,
        52 => MasterClockDivider::Div52,
        53 => MasterClockDivider::Div53,
        54 => MasterClockDivider::Div54,
        55 => MasterClockDivider::Div55,
        56 => MasterClockDivider::Div56,
        57 => MasterClockDivider::Div57,
        58 => MasterClockDivider::Div58,
        59 => MasterClockDivider::Div59,
        60 => MasterClockDivider::Div60,
        61 => MasterClockDivider::Div61,
        62 => MasterClockDivider::Div62,
        63 => MasterClockDivider::Div63,
        _ => panic!(),
    }
}

#[embassy_executor::task]
pub async fn output(
    // pull low
    mut sai_tx: embassy_stm32::sai::Sai<'static, embassy_stm32::peripherals::SAI1, u32>,
    mut grain_rx: embassy_sync::zerocopy_channel::Receiver<'static, NoopRawMutex, [u16; super::GRAIN_LEN]>,
) {
    let mut buf = [0u32; HALF_DMA_BUFFER_LEN];
    loop {
        let grain_fut = grain_rx.receive();
        sai_tx.write(&buf).await.unwrap();

        let grain = grain_fut.await;
        for i in 0..buf.len() {
            buf[i] = grain[i] as u32;
        }
        grain_rx.receive_done();
    }
}

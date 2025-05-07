use embassy_stm32::{
    peripherals::SAI1,
    sai::{Dma, FsPin, Instance, MasterClockDivider, MclkPin, Sai, SckPin, SdPin, A, B},
    Peri,
};
use grounded::uninit::GroundedArrayCell;

const BLOCK_LEN: usize = 32; // samples per channel
pub(super) const HALF_DMA_BUFFER_LEN: usize = BLOCK_LEN * 2; // 2 channels
const DMA_BUFFER_LEN: usize = HALF_DMA_BUFFER_LEN * 2;

#[link_section = ".sram1_bss"]
static TX_BUFFER: GroundedArrayCell<u32, DMA_BUFFER_LEN> = GroundedArrayCell::uninit();
#[link_section = ".sram1_bss"]
static RX_BUFFER: GroundedArrayCell<u32, DMA_BUFFER_LEN> = GroundedArrayCell::uninit();

#[allow(clippy::too_many_arguments)]
pub fn init_sai_tx_rx<'d, T: Instance>(
    instance: Peri<'d, T>,
    sck: Peri<'d, impl SckPin<T, A>>,
    fs: Peri<'d, impl FsPin<T, A>>,
    mclk: Peri<'d, impl MclkPin<T, A>>,
    sd_tx: Peri<'d, impl SdPin<T, A>>,
    sd_rx: Peri<'d, impl SdPin<T, B>>,
    dma_tx: Peri<'d, impl Dma<T, A>>,
    dma_rx: Peri<'d, impl Dma<T, B>>,
) -> (Sai<'d, T, u32>, Sai<'d, T, u32>) {
    let (sub_block_tx, sub_block_rx) = embassy_stm32::sai::split_subblocks(instance);
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
        config.data_size = DataSize::Data24;
        config.bit_order = BitOrder::MsbFirst;
        config.frame_sync_polarity = FrameSyncPolarity::ActiveHigh;
        config.frame_sync_offset = FrameSyncOffset::OnFirstBit;
        config.frame_length = 64;
        config.frame_sync_active_level_length = word::U7(32);
        config.fifo_threshold = FifoThreshold::Quarter;
        config
    };
    let rx_config = {
        use embassy_stm32::sai::*;

        let mut config = tx_config;
        config.mode = Mode::Slave;
        config.tx_rx = TxRx::Receiver;
        config.sync_output = false;
        config.sync_input = SyncInput::Internal;
        config.clock_strobe = ClockStrobe::Rising;
        config
    };
    let tx_buffer: &mut [u32] = unsafe {
        TX_BUFFER.initialize_all_copied(0);
        let (ptr, len) = TX_BUFFER.get_ptr_len();
        core::slice::from_raw_parts_mut(ptr, len)
    };
    let rx_buffer: &mut [u32] = unsafe {
        RX_BUFFER.initialize_all_copied(0);
        let (ptr, len) = RX_BUFFER.get_ptr_len();
        core::slice::from_raw_parts_mut(ptr, len)
    };
    let tx = Sai::new_asynchronous_with_mclk(
        sub_block_tx,
        sck,
        sd_tx,
        fs,
        mclk,
        dma_tx,
        tx_buffer,
        tx_config,
    );
    let rx = Sai::new_synchronous(sub_block_rx, sd_rx, dma_rx, rx_buffer, rx_config);
    (tx, rx)
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

pub mod hw;

pub const SAMPLE_RATE: u16 = 48000;

#[embassy_executor::task]
pub async fn pass(
    mut audio_tx: embassy_stm32::sai::Sai<'static, embassy_stm32::peripherals::SAI1, u32>,
    mut audio_rx: embassy_stm32::sai::Sai<'static, embassy_stm32::peripherals::SAI1, u32>,
    serial_tx: embassy_sync::channel::DynamicSender<'static, [u8; 64]>,
) {
    audio_rx.start().unwrap();
    let mut buf = [0u32; hw::HALF_DMA_BUFFER_LEN];
    loop {
        audio_tx.write(&buf).await.unwrap();
        audio_rx.read(&mut buf).await.unwrap();
    }
}

use crate::{audio, tui};
use embassy_stm32::{
    adc::{Adc, AdcChannel, AnyAdcChannel, Instance, SampleTime},
    peripherals::{ADC1, DMA1_CH1},
    Peri,
};
use embassy_sync::channel::DynamicSender;
use grounded::uninit::GroundedArrayCell;

#[link_section = ".sram1_bss"]
static ADC_BUFFER: GroundedArrayCell<u16, 11> = GroundedArrayCell::uninit();

pub struct Pots<T: Instance> {
    pub gain: AnyAdcChannel<T>,
    pub width: AnyAdcChannel<T>,
    pub speed: AnyAdcChannel<T>,
    pub phrase_drift: AnyAdcChannel<T>,
    pub kit_drift: AnyAdcChannel<T>,
}

impl<T: Instance> Pots<T> {
    pub fn new(
        gain: impl AdcChannel<T>,
        width: impl AdcChannel<T>,
        speed: impl AdcChannel<T>,
        phrase_drift: impl AdcChannel<T>,
        kit_drift: impl AdcChannel<T>,
    ) -> Self {
        Self {
            gain: gain.degrade_adc(),
            width: width.degrade_adc(),
            speed: speed.degrade_adc(),
            phrase_drift: phrase_drift.degrade_adc(),
            kit_drift: kit_drift.degrade_adc(),
        }
    }
}

#[embassy_executor::task]
pub async fn adc(
    mut adc: Adc<'static, ADC1>,
    mut dma: Peri<'static, DMA1_CH1>,
    mut pots_a: Pots<ADC1>,
    mut pots_b: Pots<ADC1>,
    mut tempo_pot: AnyAdcChannel<ADC1>,
    clock_tx: DynamicSender<'static, f32>,
    audio_tx: DynamicSender<'static, audio::Cmd<'static>>,
    tui_tx: DynamicSender<'static, tui::Cmd>,
) {
    let adc_buffer: &mut [u16] = unsafe {
        ADC_BUFFER.initialize_all_copied(0);
        let (ptr, len) = ADC_BUFFER.get_ptr_len();
        core::slice::from_raw_parts_mut(ptr, len)
    };
    let mut last = [0u8; 11];
    loop {
        adc.read(
            dma.reborrow(),
            [
                &mut pots_a.gain,
                &mut pots_a.width,
                &mut pots_a.speed,
                &mut pots_a.phrase_drift,
                &mut pots_a.kit_drift,
                &mut pots_b.gain,
                &mut pots_b.width,
                &mut pots_b.speed,
                &mut pots_b.phrase_drift,
                &mut pots_b.kit_drift,
                &mut tempo_pot,
            ]
            .into_iter()
            .map(|v| (v, SampleTime::CYCLES64_5)),
            adc_buffer,
        )
        .await;
        for i in 0..last.len() {
            let current = (adc_buffer[i] as f32 / u16::MAX as f32 * u8::MAX as f32) as u8;
            if current != last[i] {
                let v = current as f32 / u8::MAX as f32;
                if i == 11 {
                    clock_tx.send(v * 300.).await;
                } else {
                    let bank = if i / 5 == 0 {
                        audio::Bank::A
                    } else {
                        audio::Bank::B
                    };
                    let (audio_cmd, tui_cmd) = match i % 5 {
                        0 => (
                            audio::BankCmd::AssignGain(v * 2.),
                            tui::BankCmd::AssignGain(current),
                        ),
                        1 => (
                            audio::BankCmd::AssignWidth(v),
                            tui::BankCmd::AssignWidth(current),
                        ),
                        2 => (
                            audio::BankCmd::AssignSpeed(v * 2.),
                            tui::BankCmd::AssignSpeed(current),
                        ),
                        3 => (
                            audio::BankCmd::AssignPhraseDrift(v),
                            tui::BankCmd::AssignPhraseDrift(current),
                        ),
                        4 => (
                            audio::BankCmd::AssignKitDrift(v),
                            tui::BankCmd::AssignKitDrift(current),
                        ),
                        _ => unreachable!(),
                    };
                    audio_tx.send(audio::Cmd::Bank(bank, audio_cmd)).await;
                    tui_tx.send(tui::Cmd::Bank(bank, tui_cmd)).await;
                }
                last[i] = current;
            }
        }
    }
}

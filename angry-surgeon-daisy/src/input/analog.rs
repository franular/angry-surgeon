use crate::{audio, tui};
use embassy_stm32::{
    adc::{Adc, AdcChannel, AnyAdcChannel, Instance, SampleTime},
    peripherals::{ADC1, DMA1_CH1},
    Peri,
};
use embassy_sync::channel::{DynamicReceiver, DynamicSender};
use grounded::uninit::GroundedArrayCell;

#[link_section = ".sram1_bss"]
static ADC_BUFFER: GroundedArrayCell<u16, 7> = GroundedArrayCell::uninit();

pub struct Pots<T: Instance> {
    pub gain: AnyAdcChannel<T>,
    pub speed: AnyAdcChannel<T>,
    pub drift: AnyAdcChannel<T>,
}

impl<T: Instance> Pots<T> {
    pub fn new(
        gain: impl AdcChannel<T>,
        speed: impl AdcChannel<T>,
        drift: impl AdcChannel<T>,
    ) -> Self {
        Self {
            gain: gain.degrade_adc(),
            speed: speed.degrade_adc(),
            drift: drift.degrade_adc(),
        }
    }
}

pub struct Thumbstick<T: Instance> {
    pub x: AnyAdcChannel<T>,
    pub y: AnyAdcChannel<T>,
    pub flip_x: bool,
}

impl<T: Instance> Thumbstick<T> {
    pub fn new(x: impl AdcChannel<T>, y: impl AdcChannel<T>, flip_x: bool) -> Self {
        Self {
            x: x.degrade_adc(),
            y: y.degrade_adc(),
            flip_x,
        }
    }
}

#[derive(Default)]
struct LastBank {
    gain: u16,
    width: u16,
    speed: u16,
    roll: u16,
    kit_drift: u16,
    phrase_drift: u16,
    x: u16,
    y: u16,
}

#[embassy_executor::task]
pub async fn adc(
    mut adc: Adc<'static, ADC1>,
    mut dma: Peri<'static, DMA1_CH1>,
    mut tempo_pot: AnyAdcChannel<ADC1>,
    mut pots_a: Pots<ADC1>,
    mut thumbstick_a: Thumbstick<ADC1>,
    mut pots_b: Pots<ADC1>,
    mut thumbstick_b: Thumbstick<ADC1>,
    clock_tx: DynamicSender<'static, f32>,
    audio_tx: DynamicSender<'static, audio::Cmd<'static>>,
    tui_tx: DynamicSender<'static, tui::Cmd>,
    shift_rx: DynamicReceiver<'static, (audio::Bank, bool)>,
) {
    let adc_buffer: &mut [u16] = unsafe {
        ADC_BUFFER.initialize_all_copied(0);
        let (ptr, len) = ADC_BUFFER.get_ptr_len();
        core::slice::from_raw_parts_mut(ptr, len)
    };
    let mut last_tempo = 0u16;
    let mut last_a = LastBank::default();
    let mut shift_a = false;
    let mut last_b = LastBank::default();
    let mut shift_b = false;
    loop {
        if let Ok((bank, shift)) = shift_rx.try_receive() {
            match bank {
                audio::Bank::A => shift_a = shift,
                audio::Bank::B => shift_b = shift,
            }
        }
        adc.read(
            dma.reborrow(),
            [
                &mut tempo_pot,
                &mut pots_a.gain,
                &mut pots_a.speed,
                &mut pots_a.drift,
                &mut thumbstick_a.x,
                &mut thumbstick_a.y,
                &mut pots_b.gain,
                &mut pots_b.speed,
                &mut pots_b.drift,
                &mut thumbstick_b.x,
                &mut thumbstick_b.y,
            ]
            .into_iter()
            .map(|v| (v, SampleTime::CYCLES64_5)),
            adc_buffer,
        )
        .await;

        for i in 0..adc_buffer.len() {
            // quantize to 9 bits
            let current = (adc_buffer[i] as f32 / u16::MAX as f32 * (1 << 9) as f32) as u16;

            macro_rules! bank_pot {
                ($bank:expr,$cmd:ident,$last:expr,$value:expr) => {
                    if current != $last {
                        audio_tx
                            .send(super::audio_bank_cmd!($bank, $cmd, $value))
                            .await;
                        tui_tx.send(super::tui_bank_cmd!($bank, $cmd, $value)).await;
                        $last = current;
                    }
                };
            }

            let float = current as f32 / (1 << 9) as f32;
            if i == 0 {
                if current != last_tempo {
                    let v = 30. + float * 270.;
                    clock_tx.send(v).await;
                    audio_tx.send(audio::Cmd::AssignTempo(v)).await;
                    last_tempo = current;
                }
            } else {
                let (bank, thumbstick, last, shift) = if (i - 1) / 5 == 0 {
                    (audio::Bank::A, &thumbstick_a, &mut last_a, shift_a)
                } else {
                    (audio::Bank::B, &thumbstick_b, &mut last_b, shift_b)
                };
                match i {
                    1 | 6 => {
                        if shift {
                            bank_pot!(bank, AssignWidth, last.width, float);
                        } else {
                            bank_pot!(bank, AssignGain, last.gain, float * 2.);
                        }
                    }
                    2 | 7 => {
                        if shift {
                            bank_pot!(bank, AssignRoll, last.roll, float * 8.);
                        } else {
                            bank_pot!(bank, AssignSpeed, last.speed, float * 2.);
                        }
                    }
                    3 | 8 => {
                        if shift {
                            bank_pot!(bank, AssignPhraseDrift, last.phrase_drift, float);
                        } else {
                            bank_pot!(bank, AssignKitDrift, last.kit_drift, float);
                        }
                    }
                    4 | 9 => {
                        if current != last.x {
                            let v = if thumbstick.flip_x {
                                float * -2.
                            } else {
                                float * 2.
                            };
                            audio_tx
                                .send(super::audio_bank_cmd!(bank, OffsetSpeed, v))
                                .await;
                            last.x = current;
                        }
                    }
                    5 | 10 => {
                        if current != last.y {
                            let v = float * 2.;
                            audio_tx
                                .send(super::audio_bank_cmd!(bank, OffsetRoll, v))
                                .await;
                            last.y = current;
                        }
                    }
                    _ => unreachable!(),
                }
            }
        }
    }
}

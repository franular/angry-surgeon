use stm32h7xx_hal::adc::{Adc, Disabled, Enabled};

pub const CHANNEL_COUNT: usize = 11;

pub mod channels {
    pub const TEMPO: u8 = 0;
    pub const POTS_A: core::ops::RangeInclusive<u8> = 1..=3;
    pub const THUMB_A: core::ops::RangeInclusive<u8> = 4..=5;
    pub const POTS_B: core::ops::RangeInclusive<u8> = 6..=8;
    pub const THUMB_B: core::ops::RangeInclusive<u8> = 9..=10;
}

#[derive(Default)]
pub enum Preshift {
    #[default]
    None,
    Primed,
    FromLess,
    FromMore,
}

#[derive(Default)]
pub struct Last {
    pub preshift: Preshift,
    pub samples: [u16; 2],
}

#[derive(Default)]
pub struct Pots {
    pub shift: bool,
    pub last: [Last; 3],
}

impl Pots {
    pub fn shift(&mut self, shift: bool) {
        self.shift = shift;
        for l in self.last.iter_mut() {
            l.preshift = Preshift::Primed;
        }
    }

    pub fn last(&self, index: u8) -> u16 {
        self.last[index as usize].samples[self.shift as usize]
    }

    /// sets value if returned from shift discontinuity; returns true if set
    pub fn maybe_set(&mut self, index: usize, sample: u16) -> bool {
        let preshift = &mut self.last[index].preshift;
        let last = &mut self.last[index].samples[self.shift as usize];
        match preshift {
            Preshift::None => {
                if sample == *last {
                    false
                } else {
                    *last = sample;
                    true
                }
            }
            Preshift::Primed => {
                if sample < *last {
                    *preshift = Preshift::FromLess;
                    false
                } else if sample > *last {
                    *preshift = Preshift::FromMore;
                    false
                } else {
                    *preshift = Preshift::None;
                    false
                }
            }
            Preshift::FromLess => {
                if sample >= *last {
                    *preshift = Preshift::None;
                    *last = sample;
                    true
                } else {
                    false
                }
            }
            Preshift::FromMore => {
                if sample <= *last {
                    *preshift = Preshift::None;
                    *last = sample;
                    true
                } else {
                    false
                }
            }
        }
    }
}

#[derive(Default)]
pub struct AdcData {
    pub mult: f32,
    pub tempo: u16,
    pub pots: [Pots; 2],
    pub thumbs: [[u16; 2]; 2],
}

/// read initial vref for conversion factor (only available via adc3)
pub fn init_data(
    adc: Adc<crate::hal::pac::ADC3, Disabled>,
    common: &mut crate::hal::pac::ADC3_COMMON,
) -> AdcData {
    // enable vrefint
    common.ccr.modify(|_, w| w.vrefen().enabled());
    let mut adc = adc.enable();
    let regs = adc.inner_mut();

    // 32x oversampling with rightshift for averaging
    regs.cfgr2
        .modify(|_, w| w.rovse().enabled().osvr().variant(31).ovss().variant(5));
    // 12 bit, write to dr
    regs.cfgr.modify(|_, w| {
        w.res()
            .twelve_bit_v()
            .dmngt()
            .dr()
            .cont()
            .single()
            .discen()
            .enabled()
    });
    // zero lshift
    regs.cfgr2.modify(|_, w| w.lshift().variant(0));
    // preselect vref channel (19)
    regs.pcsel
        .modify(|r, w| unsafe { w.pcsel().bits(r.pcsel().bits() | 1 << 19) });
    // sample time
    regs.smpr2.modify(|_, w| w.smp19().cycles387_5());
    // build sequence
    regs.sqr1.modify(|_, w| w.l().variant(0).sq1().variant(19));
    // start conversion, wait
    regs.cr.modify(|_, w| w.adstart().start_conversion());
    while regs.isr.read().eoc().is_not_complete() {}

    let vrefint_cal = regs.calfact.read().calfact_s().bits();
    let vrefint_data = regs.dr.read().rdata().bits();
    // normally this would be multiplied by 3.3V, but this mult is proportion
    let mult = vrefint_cal as f32 / (vrefint_data as f32 * ((1 << 12) - 1) as f32);
    // disable adc and vref channel
    adc.disable();
    common.ccr.modify(|_, w| w.vrefen().disabled());

    AdcData {
        mult,
        ..Default::default()
    }
}

/// start hardcoded adc seqeunce (friendship ENDED with genericism)
pub fn start_seq(adc: &mut Adc<crate::hal::pac::ADC1, Enabled>) {
    let regs = adc.inner_mut();

    // 32x oversampling with rightshift for averaging
    regs.cfgr2
        .modify(|_, w| w.rovse().enabled().osvr().variant(31).ovss().variant(5));
    // 12 bit, dma circular
    regs.cfgr.modify(|_, w| {
        w.res()
            .twelve_bit_v()
            // ideally, this would be dma_circular() and the associated transfer
            // would be circular_buffer; unfortunately, this seems to cause dma
            // conflicts with sdmmc or something with higher tempo and speed,
            // causing adc and dma to desync such that the actually indicies of
            // the adc data array are shifted. to avoid this, the transfer is
            // one shot, and the adc restarted after every transfer in interrupt
            .dmngt()
            .dma_one_shot()
            .cont()
            .continuous()
            .discen()
            .disabled()
    });
    // zero lshift
    regs.cfgr2.modify(|_, w| w.lshift().variant(0));
    // preselect channels
    regs.pcsel.modify(|r, w| unsafe {
        w.pcsel().bits(
            r.pcsel().bits()
                | 1 << 10
                | 1 << 15
                | 1 << 5
                | 1 << 7
                | 1 << 3
                | 1 << 11
                | 1 << 4
                | 1 << 19
                | 1 << 18
                | 1 << 17
                | 1 << 16,
        )
    });
    // sample times
    regs.smpr1.modify(|_, w| {
        w.smp0()
            .cycles387_5()
            .smp1()
            .cycles387_5()
            .smp2()
            .cycles387_5()
            .smp3()
            .cycles387_5()
            .smp4()
            .cycles387_5()
            .smp5()
            .cycles387_5()
            .smp6()
            .cycles387_5()
            .smp7()
            .cycles387_5()
            .smp8()
            .cycles387_5()
            .smp9()
            .cycles387_5()
    });
    regs.smpr2.modify(|_, w| w.smp10().cycles387_5());
    // build sequence
    regs.sqr1.modify(
        |_, w| {
            w.l()
                .variant(CHANNEL_COUNT as u8 - 1)
                .sq1()
                .variant(10) // A0  tempo
                .sq2()
                .variant(15) // A1  pots a:
                .sq3()
                .variant(5) // A2
                .sq4()
                .variant(7)
        }, // A3
    );
    regs.sqr2.modify(
        |_, w| {
            w.sq5()
                .variant(3) // A4  thumb a:
                .sq6()
                .variant(11) // A5
                .sq7()
                .variant(4) // A6  pots b:
                .sq8()
                .variant(19) // A7
                .sq9()
                .variant(18)
        }, // A8
    );
    regs.sqr3.modify(
        |_, w| {
            w.sq10()
                .variant(17) // A9  thumb b:
                .sq11()
                .variant(16)
        }, // A10
    );
    // start conversion
    regs.cr.modify(|_, w| w.adstart().start_conversion());
}

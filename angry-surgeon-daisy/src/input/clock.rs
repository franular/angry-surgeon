use crate::hal::gpio::{Output, Pin};
use rtic_monotonics::{
    Monotonic,
    fugit::{Duration, Instant},
};

#[derive(PartialEq)]
pub enum Source {
    External,
    Internal,
}

pub struct Blink<const P: char, const N: u8> {
    output: crate::hal::gpio::Pin<P, N, crate::hal::gpio::Output>,
    last: Instant<u32, 1, 1_000_000>,
}

impl<const P: char, const N: u8> Blink<P, N> {
    pub fn new(
        output: Pin<P, N, Output>,
        now: Instant<u32, 1, 1_000_000>,
    ) -> Self {
        Self {
            output,
            last: now,
        }
    }

    pub async fn tick(&mut self, period: Duration<u32, 1, 1_000_000>, sustain: Duration<u32, 1, 1_000_000>) {
        if self.output.is_set_low() {
            self.output.set_high();
            crate::Mono::delay_until(self.last + sustain).await;
        } else {
            self.output.set_low();
            crate::Mono::delay_until(self.last + period).await;
            self.last += period;
        }
    }
}

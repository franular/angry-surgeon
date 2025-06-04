use crate::audio::{self, PPQ, STEP_DIV};
use embassy_stm32::{exti::ExtiInput, gpio::Level};
use embassy_sync::channel::{DynamicReceiver, DynamicSender};
use embassy_time::{Duration, Instant, Timer};

const ENCODER_DELAY: u64 = 10;

pub struct Debounce<'d> {
    exti: ExtiInput<'d>,
    timeout: Duration,
}

impl<'d> Debounce<'d> {
    pub fn new(exti: ExtiInput<'d>, timeout: Duration) -> Self {
        Self { exti, timeout }
    }

    pub async fn wait_for_any_edge(&mut self) -> Level {
        loop {
            let l1 = self.exti.get_level();
            self.exti.wait_for_any_edge().await;
            Timer::after(self.timeout).await;
            let l2 = self.exti.get_level();
            if l1 != l2 {
                return l2;
            }
        }
    }

    pub async fn wait_for_level(&mut self, level: Level) {
        if self.exti.get_level() == level {
            return;
        }
        loop {
            if self.wait_for_any_edge().await == level {
                return;
            }
        }
    }
}

pub enum Direction {
    Counterclockwise,
    Clockwise,
}

pub struct Encoder<'d> {
    ch1: Debounce<'d>,
    ch2: Debounce<'d>,
}

impl<'d> Encoder<'d> {
    pub fn new(ch1: ExtiInput<'d>, ch2: ExtiInput<'d>) -> Self {
        Self {
            ch1: Debounce::new(ch1, Duration::from_millis(ENCODER_DELAY)),
            ch2: Debounce::new(ch2, Duration::from_millis(ENCODER_DELAY)),
        }
    }

    pub async fn wait_for_direction(&mut self) -> Direction {
        use embassy_futures::select::{select, Either};

        self.ch1.wait_for_level(Level::High).await;
        self.ch2.wait_for_level(Level::High).await;

        match select(
            self.ch1.wait_for_level(Level::Low),
            self.ch2.wait_for_level(Level::Low),
        )
        .await
        {
            Either::First(()) => Direction::Clockwise,
            Either::Second(()) => Direction::Counterclockwise,
        }
    }
}

struct Blink<'d> {
    output: embassy_stm32::gpio::Output<'d>,
    last: Instant,
    sustain: Duration,
}

impl<'d> Blink<'d> {
    async fn tick(&mut self, period: Duration) {
        if self.output.is_set_low() {
            self.output.set_high();
            Timer::at(self.last + self.sustain).await;
        } else {
            self.output.set_low();
            Timer::at(self.last + period).await;
            self.last += period;
        }
    }
}

#[embassy_executor::task]
pub async fn encoder_sw(mut sw: Debounce<'static>, tx: DynamicSender<'static, Level>) {
    loop {
        let level = sw.wait_for_any_edge().await;
        tx.send(level).await;
    }
}

#[embassy_executor::task]
pub async fn encoder(mut encoder: Encoder<'static>, tx: DynamicSender<'static, Direction>) {
    loop {
        let direction = encoder.wait_for_direction().await;
        tx.send(direction).await;
    }
}

#[embassy_executor::task]
pub async fn clock(
    mut ground_in: Debounce<'static>,
    mut clock_in: Debounce<'static>,
    clock_out: embassy_stm32::gpio::Output<'static>,
    tempo_led: embassy_stm32::gpio::Output<'static>,
    audio_tx: DynamicSender<'static, audio::Cmd<'static>>,
    clock_rx: DynamicReceiver<'static, f32>,
) {
    use embassy_futures::select::*;

    // default to 192 bpm
    let mut beat_dur = Duration::from_micros((60_000_000. / 192.) as u64);
    let mut last_ext = None;
    let mut last_step = embassy_time::Instant::now();

    let mut clock_out = Blink {
        output: clock_out,
        last: last_step,
        sustain: Duration::from_millis(15),
    };
    let mut tempo_led = Blink {
        output: tempo_led,
        last: last_step,
        sustain: Duration::from_millis(50),
    };

    loop {
        match select6(
            ground_in.wait_for_any_edge(),
            clock_in.wait_for_any_edge(),
            clock_rx.receive(),
            embassy_time::Timer::at(last_step + beat_dur / STEP_DIV as u32),
            clock_out.tick(beat_dur / PPQ as u32),
            tempo_led.tick(beat_dur),
        )
        .await
        {
            Either6::First(level) => {
                if level == Level::High {
                    // disable external sync
                    last_ext = None;
                }
            }
            Either6::Second(level) => {
                if level == Level::High {
                    // set tempo from ext pulse; take care not to double tick on transition
                    let now = embassy_time::Instant::now();
                    if let Some(last) = last_ext {
                        let pulse_dur: Duration = now - last;
                        beat_dur = pulse_dur / PPQ as u32;
                        let tempo = 60_000_000. / beat_dur.as_micros() as f32;
                        audio_tx.send(audio::Cmd::AssignTempo(tempo)).await;
                    }
                    last_ext = Some(now);
                }
            }
            Either6::Third(tempo) => {
                // set tempo from pot; take care not to double tick on transition
                beat_dur = Duration::from_micros((60_000_000. / tempo) as u64);
                audio_tx.send(audio::Cmd::AssignTempo(tempo)).await;
            }
            Either6::Fourth(()) => {
                // tick system
                last_step = embassy_time::Instant::now();
                audio_tx.send(audio::Cmd::Clock).await;
            }
            Either6::Fifth(()) => (), // tick clock out
            Either6::Sixth(()) => (), // tick tempo led
        }
    }
}

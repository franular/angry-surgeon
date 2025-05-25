use embassy_stm32::{exti::ExtiInput, gpio::Level};
use embassy_time::{Duration, Timer};

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

use embassy_sync::{blocking_mutex::raw::RawMutex, mutex::Mutex};
use embedded_hal::i2c::{ErrorType, Operation};
use embedded_hal_async::i2c::I2c;

pub struct Shared<M: RawMutex, I: I2c> {
    bus: Mutex<M, I>,
}

impl<M: RawMutex, I: I2c> Shared<M, I> {
    pub fn new(bus: I) -> Self {
        Self {
            bus: Mutex::new(bus),
        }
    }

    pub fn get_ref(&self) -> Ref<M, I> {
        Ref { bus: &self.bus }
    }
}

pub struct Ref<'d, M: RawMutex, I: I2c> {
    bus: &'d Mutex<M, I>,
}

impl<M: RawMutex, I: I2c> ErrorType for Ref<'_, M, I> {
    type Error = I::Error;
}

impl<M: RawMutex, T: I2c> I2c for Ref<'_, M, T> {
    async fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        self.bus.lock().await.transaction(address, operations).await
    }
}

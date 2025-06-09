use embassy_stm32::{
    peripherals::USB_OTG_FS,
    usb::{DmPin, DpPin, Driver, Instance, InterruptHandler},
    Peri,
};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex as RawMutex;
use embassy_usb::{
    class::cdc_acm::{CdcAcmClass, State},
    UsbDevice,
};

#[macro_export]
macro_rules! print {
    ($str:expr,$num:expr) => {
        let mut buf = [0u8; 64];
        buf[..$str.len()].copy_from_slice($str.as_bytes());
        let rem = $num;
        if rem == 0 {
            buf[$str.len()] = char::from_digit(0, 10).unwrap() as u8;
        } else {
            let mut i = 0;
            let mut digit = 10u32.pow(rem.ilog10());
            while digit != 0 {
                buf[$str.len() + i] = char::from_digit((rem / digit) % 10, 10).unwrap() as u8;
                digit /= 10;
                i += 1;
            }
        }
        $crate::serial::CHANNEL.send(buf).await;
        // let _ = $crate::serial::CHANNEL.try_send(buf);
    };
    ($str:expr) => {{
        let mut buf = [0u8; 64];
        buf[..$str.len()].copy_from_slice($str.as_bytes());
        $crate::serial::CHANNEL.send(buf).await;
        // let _ = $crate::serial::CHANNEL.try_send(buf);
    }};
}

pub static CHANNEL: embassy_sync::channel::Channel<RawMutex, [u8; MAX_PACKET_LEN], 8> =
    embassy_sync::channel::Channel::new();

const MAX_PACKET_LEN: usize = 64;

pub struct SerialData<'d> {
    ep_out_buf: [u8; 256],
    config_buf: [u8; 256],
    bos_buf: [u8; 256],
    control_buf: [u8; 64],
    state: State<'d>,
}

impl SerialData<'_> {
    pub fn new() -> Self {
        Self {
            ep_out_buf: [0; 256],
            config_buf: [0; 256],
            bos_buf: [0; 256],
            control_buf: [0; 64],
            state: State::new(),
        }
    }
}

struct Disconnected {}

impl From<embassy_usb::driver::EndpointError> for Disconnected {
    fn from(val: embassy_usb::driver::EndpointError) -> Self {
        match val {
            embassy_usb::driver::EndpointError::BufferOverflow => panic!("buffer overflow"),
            embassy_usb::driver::EndpointError::Disabled => Disconnected {},
        }
    }
}

pub fn init_usb_class<'d, T: Instance>(
    _instance: Peri<'d, T>,
    _irq: impl embassy_stm32::interrupt::typelevel::Binding<T::Interrupt, InterruptHandler<T>> + 'd,
    dp: Peri<'d, impl DpPin<T>>,
    dm: Peri<'d, impl DmPin<T>>,
    serial: &'d mut SerialData<'d>,
) -> (UsbDevice<'d, Driver<'d, T>>, CdcAcmClass<'d, Driver<'d, T>>) {
    let mut config = embassy_stm32::usb::Config::default();
    config.vbus_detection = false;
    let driver =
        embassy_stm32::usb::Driver::new_fs(_instance, _irq, dp, dm, &mut serial.ep_out_buf, config);
    let mut config = embassy_usb::Config::new(0x4652, 0x414e);
    config.manufacturer = Some("franular");
    config.product = Some("angry surgeon");

    let mut builder = embassy_usb::Builder::<Driver<'d, T>>::new(
        driver,
        config,
        &mut serial.config_buf,
        &mut serial.bos_buf,
        &mut [], // no msos descriptor
        &mut serial.control_buf,
    );
    let class = embassy_usb::class::cdc_acm::CdcAcmClass::<Driver<'d, T>>::new(
        &mut builder,
        &mut serial.state,
        MAX_PACKET_LEN as u16,
    );
    let usb = builder.build();
    (usb, class)
}

#[embassy_executor::task]
pub async fn serial(
    mut usb: UsbDevice<'static, Driver<'static, USB_OTG_FS>>,
    mut class: CdcAcmClass<'static, Driver<'static, USB_OTG_FS>>,
    rx: embassy_sync::channel::DynamicReceiver<'static, [u8; MAX_PACKET_LEN]>,
) {
    let usb_fut = usb.run();
    let serial_fut = async {
        class.wait_connection().await;
        let mut buf = [0; 64];
        // wait for input before sending first message
        let _ = class.read_packet(&mut buf).await.unwrap();
        class
            .write_packet("serial connected!\r\n".as_bytes())
            .await
            .unwrap();
        loop {
            let msg = rx.receive().await;
            class.write_packet(&msg).await.unwrap();
            class.write_packet("\r\n".as_bytes()).await.unwrap();
        }
    };
    embassy_futures::join::join(usb_fut, serial_fut).await;
}

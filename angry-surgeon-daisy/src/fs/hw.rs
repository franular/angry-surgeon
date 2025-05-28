use angry_surgeon_core::FileHandler;
use embassy_stm32::sdmmc::Error as SdmmcError;
use embassy_stm32::sdmmc::{
    CkPin, CmdPin, D0Pin, D1Pin, D2Pin, D3Pin, Instance, InterruptHandler, Sdmmc,
};
use embassy_stm32::Peri;

use block_device_adapters::{BufStream, BufStreamError, StreamSlice, StreamSliceError};
use embedded_io_async::{Read, ReadExactError, Seek, SeekFrom};

type BlockDevice<'d> = StreamSlice<BufStream<Sdmmc<'d, embassy_stm32::peripherals::SDMMC1>, 512>>;
pub type FileSystem<'d> = embedded_fatfs::FileSystem<
    BlockDevice<'d>,
    embedded_fatfs::DefaultTimeProvider,
    embedded_fatfs::LossyOemCpConverter,
>;
pub type File<'d> = embedded_fatfs::File<
    'd,
    BlockDevice<'d>,
    embedded_fatfs::DefaultTimeProvider,
    embedded_fatfs::LossyOemCpConverter,
>;
pub type Dir<'d> = embedded_fatfs::Dir<
    'd,
    BlockDevice<'d>,
    embedded_fatfs::DefaultTimeProvider,
    embedded_fatfs::LossyOemCpConverter,
>;
pub type Error<'d> = <File<'d> as embedded_io_async::ErrorType>::Error;

pub struct SdmmcFileHandler<'d> {
    root: Dir<'d>,
}

impl<'d> SdmmcFileHandler<'d> {
    pub fn new(root: Dir<'d>) -> Self {
        Self { root }
    }
}

impl<'d> FileHandler for SdmmcFileHandler<'d> {
    type File = File<'d>;

    async fn open(
        &mut self,
        path: &str,
    ) -> Result<Self::File, <Self::File as embedded_io_async::ErrorType>::Error> {
        self.root.open_file(path).await
    }

    async fn try_clone(
        &mut self,
        file: &Self::File,
    ) -> Result<Self::File, <Self::File as embedded_io_async::ErrorType>::Error> {
        Ok(file.clone())
    }
}

#[derive(Debug)]
pub enum InitError {
    ReadExact,
    StreamSlice,
}

impl From<BufStreamError<SdmmcError>> for InitError {
    fn from(_value: BufStreamError<SdmmcError>) -> Self {
        Self::StreamSlice
    }
}

impl From<ReadExactError<BufStreamError<SdmmcError>>> for InitError {
    fn from(_value: ReadExactError<BufStreamError<SdmmcError>>) -> Self {
        Self::ReadExact
    }
}

impl From<StreamSliceError<BufStreamError<SdmmcError>>> for InitError {
    fn from(_value: StreamSliceError<BufStreamError<SdmmcError>>) -> Self {
        Self::StreamSlice
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn init_sdmmc<'d, T: Instance>(
    instance: Peri<'d, T>,
    _irq: impl embassy_stm32::interrupt::typelevel::Binding<T::Interrupt, InterruptHandler<T>> + 'd,
    clk: Peri<'d, impl CkPin<T>>,
    cmd: Peri<'d, impl CmdPin<T>>,
    d0: Peri<'d, impl D0Pin<T>>,
    d1: Peri<'d, impl D1Pin<T>>,
    d2: Peri<'d, impl D2Pin<T>>,
    d3: Peri<'d, impl D3Pin<T>>,
) -> Result<StreamSlice<BufStream<Sdmmc<'d, T>, 512>>, InitError> {
    let mut sdmmc = Sdmmc::new_4bit(instance, _irq, clk, cmd, d0, d1, d2, d3, Default::default());
    sdmmc
        .init_sd_card(embassy_stm32::time::Hertz::mhz(25))
        .await
        .unwrap();

    let mut stream = BufStream::new(sdmmc);
    let mut buf = [0u8; 8];
    // assumes GUID partition table
    // assume LBA block/sector size = 512 bytes
    stream.seek(SeekFrom::Start(512 + 0x48)).await?;
    stream.read_exact(&mut buf).await?;
    let partition_array_lba = u64::from_le_bytes(buf);

    stream
        .seek(SeekFrom::Start(partition_array_lba * 512 + 0x20))
        .await?;
    stream.read_exact(&mut buf).await?;
    let partition_lba_first = u64::from_le_bytes(buf);
    stream.read_exact(&mut buf).await?;
    let partition_lba_last = u64::from_le_bytes(buf); // inclusive

    stream.rewind().await?;
    Ok(StreamSlice::new(
        stream,
        partition_lba_first * 512,
        (partition_lba_last + 1) * 512, // exclusive
    )
    .await?)
}

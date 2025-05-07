use embassy_stm32::sdmmc::Error as SdmmcError;
use embassy_stm32::sdmmc::{
    CkPin, CmdPin, D0Pin, D1Pin, D2Pin, D3Pin, Instance, InterruptHandler, Sdmmc,
};
use embassy_stm32::Peri;

use block_device_adapters::{BufStream, BufStreamError, StreamSlice, StreamSliceError};
use embedded_fatfs::Error as FatfsError;
use embedded_fatfs::{Dir, OemCpConverter, ReadWriteSeek, TimeProvider};
use embedded_io_async::{Read, ReadExactError, Seek, SeekFrom};
use heapless::String;

pub const MAX_PATH_LEN: usize = 256;
pub const FILE_COUNT: usize = 5;

// pub async fn paths_recursive<'d, IO, TP, OCC>(
//     dir: &Dir<'d, IO, TP, OCC>,
//     offset: usize,
// ) -> Result<[String<MAX_PATH_LEN>; 5], FatfsError<IO::Error>>
// where
//     IO: ReadWriteSeek,
//     TP: TimeProvider,
//     OCC: OemCpConverter,
// {
//     let mut iter = dir.iter();
//     while let Some(entry) = iter.next().await {
//         let entry = entry?;
//         let ucs2 = entry.long_file_name_as_ucs2_units();
//         if let Some(Ok(name)) = ucs2.map(String::from_utf8) {

//         }
//     }
// }

// pub async fn paths_recursive<'d, IO, TP, OCC>(
//     dir: &Dir<'d, IO, TP, OCC>,
// ) -> Result<Vec<String<MAX_PATH_LEN>, MAX_DIR_LEN>, FatFsError<IO::Error>>
// where
//     IO: ReadWriteSeek,
//     TP: TimeProvider,
//     OCC: OemCpConverter,
// {
//     let mut children = Vec::new();
//     let mut iter = dir.iter();
//     while let Some(entry) = iter.next().await {
//         let entry = entry?;
//         let ucs2 = entry.long_file_name_as_ucs2_units();
//     }
//     Ok(children)
// }

#[derive(Debug)]
pub enum Error {
    ReadExact(ReadExactError<BufStreamError<SdmmcError>>),
    StreamSlice(StreamSliceError<BufStreamError<SdmmcError>>),
}

impl From<BufStreamError<SdmmcError>> for Error {
    fn from(value: BufStreamError<SdmmcError>) -> Self {
        Self::StreamSlice(value.into())
    }
}

impl From<ReadExactError<BufStreamError<SdmmcError>>> for Error {
    fn from(value: ReadExactError<BufStreamError<SdmmcError>>) -> Self {
        Self::ReadExact(value)
    }
}

impl From<StreamSliceError<BufStreamError<SdmmcError>>> for Error {
    fn from(value: StreamSliceError<BufStreamError<SdmmcError>>) -> Self {
        Self::StreamSlice(value)
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
) -> Result<StreamSlice<BufStream<Sdmmc<'d, T>, 512>>, Error> {
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

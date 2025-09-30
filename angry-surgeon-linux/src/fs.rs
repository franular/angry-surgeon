use color_eyre::eyre::Result;
use embedded_io::{Read, Seek};
use embedded_io_adapters::std::FromStd;

pub struct LinuxFileHandler {}

impl embedded_io::ErrorType for LinuxFileHandler {
    type Error = <FromStd<std::fs::File> as embedded_io::ErrorType>::Error;
}

impl angry_surgeon_core::FileHandler for LinuxFileHandler {
    type File = FromStd<std::fs::File>;

    fn open(&mut self, path: &str) -> Result<Self::File, Self::Error> {
        Ok(FromStd::new(std::fs::File::open(path)?))
    }

    fn try_clone(&mut self, file: &Self::File) -> Result<Self::File, Self::Error> {
        Ok(FromStd::new(file.inner().try_clone()?))
    }

    /// std::fs::File automatically closed when dropped, so impl not needed
    fn close(&mut self, _file: &Self::File) -> Result<(), Self::Error> {
        Ok(())
    }

    fn read(&mut self, file: &mut Self::File, buf: &mut [u8]) -> Result<usize, Self::Error> {
        file.read(buf)
    }

    fn seek(
        &mut self,
        file: &mut Self::File,
        pos: embedded_io::SeekFrom,
    ) -> Result<u64, Self::Error> {
        file.seek(pos)
    }
}

use angry_surgeon_core::FileHandler as _;
use embedded_io::{ErrorType, Seek, Write};
use embedded_sdmmc::{BlockDevice, File, LfnBuffer, RawDirectory, RawFile, VolumeManager};

pub const MAX_DIRS: usize = 3; // root always open, 2 more for file search
pub const MAX_FILES: usize = 5; // one for bd, 2 * 2 for active wavs
pub const MAX_VOLUMES: usize = 1;
const READER_LEN: usize = 512;

pub type FileHandler = SdmmcFileHandler<
    crate::hal::sdmmc::SdmmcBlockDevice<
        crate::hal::sdmmc::Sdmmc<crate::hal::pac::SDMMC1, crate::hal::sdmmc::SdCard>,
    >,
>;

pub struct BufReader<'a> {
    fs: &'a mut FileHandler,
    file: RawFile,
    buffer: [u8; READER_LEN],
    index: usize,
    rem: usize,
}

impl<'a> BufReader<'a> {
    pub fn new(
        fs: &'a mut FileHandler,
        file: RawFile,
    ) -> Result<Self, <FileHandler as ErrorType>::Error> {
        let rem = fs.vol_mgr.file_length(file)? as usize;
        Ok(Self {
            fs,
            file,
            buffer: [0; READER_LEN],
            index: READER_LEN, // init to end to force refill
            rem,
        })
    }

    /// read the next byte from the given file, returns None if EOF
    pub fn next(&mut self) -> Result<Option<u8>, <FileHandler as ErrorType>::Error> {
        if self.rem == 0 {
            return Ok(None);
        }
        if self.index >= self.buffer.len() {
            self.index -= self.buffer.len();
            // refill buffer
            let mut slice = &mut self.buffer[..];
            while !slice.is_empty() {
                match self.fs.read(&self.file, slice) {
                    Ok(n) => {
                        if n == 0 {
                            break; // reached EOF, read partially filled buffer
                        } else {
                            slice = &mut slice[n..];
                        }
                    }
                    Err(embedded_sdmmc::Error::EndOfFile) => break,
                    Err(e) => return Err(e),
                }
            }
        }
        let byte = self.buffer[self.index];
        self.index += 1;
        self.rem -= 1;
        Ok(Some(byte))
    }
}

pub struct TimeSource;

impl embedded_sdmmc::TimeSource for TimeSource {
    fn get_timestamp(&self) -> embedded_sdmmc::Timestamp {
        embedded_sdmmc::Timestamp {
            year_since_1970: 0,
            zero_indexed_month: 0,
            zero_indexed_day: 0,
            hours: 0,
            minutes: 0,
            seconds: 0,
        }
    }
}

pub struct SdmmcFileHandler<D: BlockDevice> {
    vol_mgr: VolumeManager<D, TimeSource, MAX_DIRS, MAX_FILES, MAX_VOLUMES>,
    root: RawDirectory,
}

impl<D: BlockDevice> SdmmcFileHandler<D> {
    pub fn new(
        vol_mgr: VolumeManager<D, TimeSource, MAX_DIRS, MAX_FILES, MAX_VOLUMES>,
    ) -> Result<Self, embedded_sdmmc::Error<D::Error>> {
        let vol = vol_mgr.open_raw_volume(embedded_sdmmc::VolumeIdx(0))?;
        let root = vol_mgr.open_root_dir(vol)?;
        Ok(Self { vol_mgr, root })
    }
}

impl<D: BlockDevice> embedded_io::ErrorType for SdmmcFileHandler<D> {
    type Error = embedded_sdmmc::Error<D::Error>;
}

impl<D: BlockDevice> angry_surgeon_core::FileHandler for SdmmcFileHandler<D> {
    type File = embedded_sdmmc::RawFile;

    fn open(&mut self, path: &str) -> Result<Self::File, Self::Error> {
        let mut rem = path.split_terminator('/').count();
        let chain = path.split_terminator('/');

        let mut dir = self.root;
        let mut bytes = [0u8; 255];
        let mut lfn_buffer = LfnBuffer::new(&mut bytes);

        for node in chain {
            rem -= 1;
            let mut sfn = None;
            self.vol_mgr
                .iterate_dir_lfn(dir, &mut lfn_buffer, |entry, lfn| {
                    if lfn == Some(node) {
                        sfn = Some(entry.name.clone());
                    }
                })?;
            if let Some(sfn) = sfn {
                if rem == 0 {
                    let file =
                        self.vol_mgr
                            .open_file_in_dir(dir, sfn, embedded_sdmmc::Mode::ReadOnly)?;
                    if dir != self.root {
                        self.vol_mgr.close_dir(dir)?;
                    }
                    return Ok(file);
                } else {
                    let new = self.vol_mgr.open_dir(dir, sfn)?;
                    if dir != self.root {
                        self.vol_mgr.close_dir(dir)?;
                    }
                    dir = new;
                }
            } else {
                return Err(embedded_sdmmc::Error::NotFound);
            }
        }
        Err(embedded_sdmmc::Error::NotFound)
    }

    fn try_clone(&mut self, file: &Self::File) -> Result<Self::File, Self::Error> {
        Ok(*file)
    }

    fn close(&mut self, file: &Self::File) -> Result<(), Self::Error> {
        self.vol_mgr.close_file(*file)
    }

    fn read(&mut self, file: &Self::File, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.vol_mgr.read(*file, buf)
    }

    fn write(&mut self, file: &Self::File, buf: &[u8]) -> Result<usize, Self::Error> {
        let mut file = file.to_file(&self.vol_mgr);
        let res =
            <File<D, TimeSource, MAX_DIRS, MAX_FILES, MAX_VOLUMES> as Write>::write(&mut file, buf);
        file.to_raw_file(); // don't close on drop
        res
    }

    fn seek(&mut self, file: &Self::File, pos: embedded_io::SeekFrom) -> Result<u64, Self::Error> {
        let mut file = file.to_file(&self.vol_mgr);
        let res =
            <File<D, TimeSource, MAX_DIRS, MAX_FILES, MAX_VOLUMES> as Seek>::seek(&mut file, pos);
        file.to_raw_file(); // don't close on drop
        res
    }

    fn stream_position(&mut self, file: &Self::File) -> Result<u64, Self::Error> {
        self.seek(file, embedded_io::SeekFrom::Current(0))
    }
}

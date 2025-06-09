use embedded_io_async::Read;

pub mod hw;

pub struct BufReader<'d, 'b> {
    file: &'b mut hw::File<'d>,
    buffer: [u8; 512],
    index: usize,
}

impl<'d, 'b> BufReader<'d, 'b> {
    pub fn new(file: &'b mut hw::File<'d>) -> Self {
        Self {
            file,
            buffer: [0; 512],
            index: 0,
        }
    }

    /// read the next byte from the given file, returns None if EOF
    pub async fn next(&mut self) -> Result<Option<u8>, hw::Error> {
        while self.index >= self.buffer.len() {
            // refill buffer
            let mut slice = &mut self.buffer[..];
            while !slice.is_empty() {
                let n = self.file.read(slice).await?;
                if n == 0 {
                    return Ok(None);
                }
                slice = &mut slice[n..];
            }
        }
        let byte = self.buffer[self.index];
        self.index += 1;
        Ok(Some(byte))
    }
}

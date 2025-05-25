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

// enum Word {
//     Key,
//     Elem(Elem),
// }

// #[derive(Copy, Clone)]
// enum Elem {
//     Tempo,
//     Steps,
//     Onset(usize),
// }

// impl Elem {
//     fn to_str(&self) -> &str {
//         match self {
//             Elem::Tempo => "tempo",
//             Elem::Steps => "steps",
//             Elem::Onset(_) => "onsets",
//         }
//     }
// }

// enum State {
//     Overflow,
//     Search(Word),
//     Parse(Word, usize),
// }

// pub struct Rd {
//     pub tempo: Option<f32>,
//     pub steps: Option<u16>,
//     pub onset: Option<u64>,
// }

// /// minimal iterative rd parser
// pub async fn rd_to_onset(
//     mut file: hw::File<'_>,
//     pcm_len: u64,
//     onset_index: usize,
// ) -> Result<Option<Onset<hw::File<'_>>>, crate::fs::hw::Error> {
//     let mut tempo = None;
//     let mut steps = None;
//     let mut onset = None;
//     let mut reader = BufReader::new(&mut file);
//     let mut buffer = [0u8; 10];
//     let mut state = State::Search(Word::Key);

//     while let Some(c) = reader.next().await? {
//         match &mut state {
//             State::Overflow => {
//                 if c.is_ascii_whitespace() || c.is_ascii_punctuation() && !(c == b'.' || c == b'_')
//                 {
//                     state = State::Search(Word::Key);
//                 }
//             }
//             State::Search(word) => match word {
//                 Word::Key => {
//                     if c.is_ascii_alphabetic() {
//                         buffer[0] = c;
//                         state = State::Parse(Word::Key, 1);
//                     }
//                 }
//                 Word::Elem(elem) => {
//                     if c.is_ascii_alphabetic() {
//                         state = State::Search(Word::Key);
//                     } else if c.is_ascii_digit() {
//                         buffer[0] = c;
//                         state = State::Parse(Word::Elem(*elem), 1);
//                     }
//                 }
//             },
//             State::Parse(word, index) => match word {
//                 Word::Key => {
//                     if !(c == b'_' || c.is_ascii_alphanumeric()) {
//                         // end of key, check buffer
//                         if let Ok(key) = core::str::from_utf8(&buffer[..*index]) {
//                             match key {
//                                 k if k == Elem::Tempo.to_str() => {
//                                     state = State::Search(Word::Elem(Elem::Tempo))
//                                 }
//                                 k if k == Elem::Steps.to_str() => {
//                                     state = State::Search(Word::Elem(Elem::Steps))
//                                 }
//                                 k if k == Elem::Onset(0).to_str() => {
//                                     state = State::Search(Word::Elem(Elem::Onset(0)))
//                                 }
//                                 _ => (),
//                             }
//                         } else if *index < buffer.len() {
//                             // nibble word
//                             buffer[*index] = c;
//                             *index += 1;
//                         } else {
//                             state = State::Overflow;
//                         }
//                     }
//                 }
//                 Word::Elem(elem) => {
//                     if !(c == b'.' || c.is_ascii_alphanumeric()) {
//                         // end of elem, check buffer
//                         match elem {
//                             Elem::Tempo => {
//                                 if let Ok(Ok(val)) =
//                                     core::str::from_utf8(&buffer[..*index]).map(|v| v.parse())
//                                 {
//                                     tempo = Some(val);
//                                 }
//                             }
//                             Elem::Steps => {
//                                 if let Ok(Ok(val)) =
//                                     core::str::from_utf8(&buffer[..*index]).map(|v| v.parse())
//                                 {
//                                     steps = Some(val);
//                                 }
//                             }
//                             Elem::Onset(i) => {
//                                 if let Ok(Ok(val)) =
//                                     core::str::from_utf8(&buffer[..*index]).map(|v| v.parse())
//                                 {
//                                     *i += 1;
//                                     if *i == onset_index {
//                                         onset = Some(val);
//                                         continue;
//                                     }
//                                 }
//                             }
//                         }
//                         state = State::Search(Word::Key);
//                         if tempo.is_some() && steps.is_some() && onset.is_some() {
//                             break;
//                         }
//                     } else if *index < buffer.len() {
//                         // nibble word
//                         buffer[*index] = c;
//                         *index += 1;
//                     } else {
//                         state = State::Overflow;
//                     }
//                 }
//             },
//         }
//     }

//     if let Some(start) = onset {
//         Ok(Some(Onset {
//             wav: Wav {
//                 tempo,
//                 steps,
//                 file,
//                 len: pcm_len,
//             },
//             start,
//         }))
//     } else {
//         Ok(None)
//     }
// }

use serde::Serialize;
use std::io::{self, Write};

pub(crate) enum JsonSizeError {
    TooLarge,
    Serialization(serde_json::Error),
}

struct SizeCounter {
    remaining: u64,
    exceeded: bool,
}
impl Write for SizeCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some(remaining) = self.remaining.checked_sub(bytes.len() as u64)
            && !self.exceeded
        {
            self.remaining = remaining;
            Ok(bytes.len())
        } else {
            self.exceeded = true;
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "JSON size limit exceeded",
            ))
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn check_json_size(
    value: &(impl Serialize + ?Sized),
    maximum: u64,
) -> Result<(), JsonSizeError> {
    json_size(
        value,
        maximum.checked_sub(1).ok_or(JsonSizeError::TooLarge)?,
    )
    .map(|_| ())
}

pub(crate) fn json_size(
    value: &(impl Serialize + ?Sized),
    maximum: u64,
) -> Result<u64, JsonSizeError> {
    let mut counter = SizeCounter {
        remaining: maximum,
        exceeded: false,
    };
    match serde_json::to_writer(&mut counter, value) {
        Ok(()) => Ok(maximum - counter.remaining),
        Err(_) if counter.exceeded => Err(JsonSizeError::TooLarge),
        Err(error) => Err(JsonSizeError::Serialization(error)),
    }
}

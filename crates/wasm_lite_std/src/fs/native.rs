// SPDX-License-Identifier: MIT OR Apache-2.0

use super::Priority;
use blocking::unblock;
use std::fmt;
use std::io::{Read, Seek};
use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug)]
pub struct File(Arc<std::fs::File>);

#[derive(Debug)]
pub struct Data(Box<[u8]>);

#[derive(Debug, Clone)]
pub struct Metadata(std::fs::Metadata);

#[derive(Debug)]
pub struct Error(std::io::Error);

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "I/O error: {}", self.0)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self(value)
    }
}

fn allocate_zeroed(len: usize) -> std::io::Result<Vec<u8>> {
    let mut data = Vec::new();
    data.try_reserve_exact(len).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::OutOfMemory,
            format!("could not allocate a {len}-byte file buffer: {error}"),
        )
    })?;
    data.resize(len, 0);
    Ok(data)
}

fn read_up_to(reader: &mut impl Read, len: usize) -> std::io::Result<Box<[u8]>> {
    let mut data = allocate_zeroed(len)?;
    let mut filled = 0;
    while filled < len {
        match reader.read(&mut data[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    data.truncate(filled);
    Ok(data.into_boxed_slice())
}

impl File {
    pub async fn open(path: impl AsRef<Path>, _priority: Priority) -> Result<Self, Error> {
        let path = path.as_ref().to_owned();
        unblock(move || std::fs::File::open(path))
            .await
            .map(|file| Self(Arc::new(file)))
            .map_err(Into::into)
    }

    pub async fn read(&self, len: usize, _priority: Priority) -> Result<Data, Error> {
        let file = Arc::clone(&self.0);
        unblock(move || {
            let mut file = &*file;
            read_up_to(&mut file, len).map(Data)
        })
        .await
        .map_err(Into::into)
    }

    pub async fn read_all(&self, _priority: Priority) -> Result<Data, Error> {
        let file = Arc::clone(&self.0);
        unblock(move || {
            let len = usize::try_from(file.metadata()?.len()).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "file is too large to fit in memory on this platform",
                )
            })?;
            let mut file = &*file;
            read_up_to(&mut file, len).map(Data)
        })
        .await
        .map_err(Into::into)
    }

    pub async fn seek(
        &mut self,
        position: std::io::SeekFrom,
        _priority: Priority,
    ) -> Result<u64, Error> {
        let mut file = Arc::clone(&self.0);
        unblock(move || file.seek(position))
            .await
            .map_err(Into::into)
    }

    pub async fn metadata(&self, _priority: Priority) -> Result<Metadata, Error> {
        let file = Arc::clone(&self.0);
        unblock(move || file.metadata().map(Metadata))
            .await
            .map_err(Into::into)
    }
}

impl Data {
    pub fn into_boxed_slice(self) -> Box<[u8]> {
        self.0
    }
}

impl AsRef<[u8]> for Data {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl Deref for Data {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl PartialEq for Data {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for Data {}

impl std::hash::Hash for Data {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl Metadata {
    pub fn len(&self) -> u64 {
        self.0.len()
    }
}

pub async fn exists(path: impl AsRef<Path>, _priority: Priority) -> bool {
    let path = path.as_ref().to_owned();
    unblock(move || path.exists()).await
}

pub fn set_default_origin(_origin: &'static str) {}

#[cfg(test)]
mod tests {
    use super::*;

    struct ShortReader<'a> {
        data: &'a [u8],
        position: usize,
        chunk: usize,
    }

    impl Read for ShortReader<'_> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            let remaining = &self.data[self.position..];
            let len = remaining.len().min(output.len()).min(self.chunk);
            output[..len].copy_from_slice(&remaining[..len]);
            self.position += len;
            Ok(len)
        }
    }

    #[test]
    fn read_up_to_collects_short_reads_and_stops_at_eof() {
        let mut reader = ShortReader {
            data: b"short reads still make a complete file",
            position: 0,
            chunk: 3,
        };
        let data = read_up_to(&mut reader, 1024).unwrap();
        assert_eq!(&*data, reader.data);
    }
}

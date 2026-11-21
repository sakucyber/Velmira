use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{validate_frontier, Cursor, DeviceId, Operation, SyncError, MAX_BATCH_OPS};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("synchronization I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("invalid synchronization frame size")]
    Size,
    #[error("unsupported synchronization protocol version {0}")]
    Version(u16),
    #[error("malformed synchronization frame")]
    Encoding,
    #[error(transparent)]
    Invalid(#[from] SyncError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Frame {
    Hello {
        device: DeviceId,
        name: String,
    },
    Frontier {
        received: Vec<Cursor>,
    },
    Operations {
        operations: Vec<Operation>,
        more: bool,
    },
    TransferComplete {
        more: bool,
    },
    Ack {
        stored: Vec<Cursor>,
    },
    Bye,
}

impl Frame {
    fn validate(&self) -> Result<(), SyncError> {
        match self {
            Self::Hello { name, .. } => {
                if name.trim().is_empty() || name.len() > 128 || name.chars().any(char::is_control)
                {
                    return Err(SyncError::Invalid("invalid device name"));
                }
            }
            Self::Frontier { received } => validate_frontier(received)?,
            Self::Operations { operations, more } => {
                if operations.len() > MAX_BATCH_OPS || (*more && operations.is_empty()) {
                    return Err(SyncError::Invalid("invalid operation batch size"));
                }
                for operation in operations {
                    operation.validate()?;
                }
            }
            Self::Ack { stored } => validate_frontier(stored)?,
            Self::TransferComplete { .. } | Self::Bye => {}
        }
        Ok(())
    }
}

/// Pack a batch by both operation count and encoded size. Pass the resulting
/// frame to `write_frame` before reading the next batch from the same iterator.
pub fn next_batch<'a>(
    operations: &mut std::iter::Peekable<impl Iterator<Item = &'a Operation>>,
) -> Result<Frame, ProtocolError> {
    bounded_batch(operations, MAX_BATCH_OPS)
}

pub(crate) fn bounded_batch<'a>(
    operations: &mut std::iter::Peekable<impl Iterator<Item = &'a Operation>>,
    limit: usize,
) -> Result<Frame, ProtocolError> {
    let mut batch = Vec::new();
    // Reserve space for protocol version, field names and the CBOR array header.
    let mut available_bytes = MAX_FRAME_BYTES - 128;
    while let Some(&operation) = operations.peek().filter(|_| batch.len() < limit) {
        operation.validate()?;
        let mut bytes = FrameBuffer::default();
        ciborium::into_writer(operation, &mut bytes).map_err(encode_error)?;
        if bytes.0.len() > available_bytes {
            if batch.is_empty() {
                return Err(ProtocolError::Size);
            }
            break;
        }
        available_bytes -= bytes.0.len();
        batch.push(operation.clone());
        operations.next();
    }
    Ok(Frame::Operations {
        operations: batch,
        more: operations.peek().is_some(),
    })
}

/// Wire format: big-endian length, big-endian version, then one CBOR value.
/// Read and write over an authenticated transport. The length includes the
/// two version bytes but excludes the four-byte length prefix.
pub fn write_frame(mut writer: impl Write, frame: &Frame) -> Result<(), ProtocolError> {
    frame.validate()?;
    let mut bytes = FrameBuffer(PROTOCOL_VERSION.to_be_bytes().to_vec());
    ciborium::into_writer(frame, &mut bytes).map_err(encode_error)?;
    writer.write_all(&(bytes.0.len() as u32).to_be_bytes())?;
    writer.write_all(&bytes.0)?;
    Ok(())
}

pub fn read_frame(mut reader: impl Read) -> Result<Frame, ProtocolError> {
    let mut length = [0; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if !(3..=MAX_FRAME_BYTES).contains(&length) {
        return Err(ProtocolError::Size);
    }
    let mut version = [0; 2];
    reader.read_exact(&mut version)?;
    let version = u16::from_be_bytes(version);
    if version != PROTOCOL_VERSION {
        return Err(ProtocolError::Version(version));
    }
    let mut payload = vec![0; length - 2];
    reader.read_exact(&mut payload)?;
    let mut remaining = payload.as_slice();
    let frame: Frame = ciborium::de::from_reader_with_recursion_limit(&mut remaining, 32)
        .map_err(|_| ProtocolError::Encoding)?;
    if !remaining.is_empty() {
        return Err(ProtocolError::Encoding);
    }
    frame.validate()?;
    Ok(frame)
}

#[derive(Default)]
struct FrameBuffer(Vec<u8>);

impl Write for FrameBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_FRAME_BYTES - self.0.len() {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "synchronization frame",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode_error(error: ciborium::ser::Error<io::Error>) -> ProtocolError {
    match error {
        ciborium::ser::Error::Io(error) if error.kind() == io::ErrorKind::FileTooLarge => {
            ProtocolError::Size
        }
        _ => ProtocolError::Encoding,
    }
}

use std::io::{self, Read, Write};

use snow::{Builder, HandshakeState, Keypair, TransportState};
use thiserror::Error;

use crate::{read_frame, write_frame, DeviceId, Frame, ProtocolError};

const PARAMS: &str = "Noise_XX_25519_ChaChaPoly_SHA256";
const PROLOGUE: &[u8] = b"Velmira pinned-device replication v1";
const MAX_HANDSHAKE_BYTES: usize = 256;
const TAG_BYTES: usize = 16;
const MAX_RECORD_BYTES: usize = u16::MAX as usize;
const MAX_CHUNK_BYTES: usize = MAX_RECORD_BYTES - TAG_BYTES;

#[derive(Debug, Error)]
pub enum ChannelError {
    #[error("synchronization channel I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("synchronization channel handshake failed: {0}")]
    Noise(#[from] snow::Error),
    #[error("unexpected synchronization channel public key")]
    Identity,
    #[error("invalid synchronization channel record size")]
    RecordSize,
    #[error("unexpected synchronization channel handshake payload")]
    HandshakePayload,
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
}

#[derive(Debug, Clone, Copy)]
pub enum Role {
    Initiator,
    Responder,
}

/// Generate a key pair for a device. The caller must persist its private key
/// securely and exchange its public key through an authenticated pairing flow.
pub fn generate_keypair() -> Result<Keypair, ChannelError> {
    Ok(Builder::new(PARAMS.parse()?).generate_keypair()?)
}

/// A mutually authenticated, ordered Noise stream. Drop the session after
/// any frame or I/O error. The caller configures timeouts on the underlying
/// connection and supplies an already trusted public key. This API does not
/// enroll new peers or authorize relayed operations.
pub struct Channel<T> {
    stream: T,
    state: TransportState,
    pub(crate) role: Role,
    peer: DeviceId,
    plaintext: Vec<u8>,
    cursor: usize,
    ciphertext: Vec<u8>,
}

impl<T: Read + Write> Channel<T> {
    pub fn connect(
        mut stream: T,
        role: Role,
        private_key: &[u8; 32],
        peer_key: &[u8; 32],
    ) -> Result<Self, ChannelError> {
        let builder = Builder::new(PARAMS.parse()?)
            .prologue(PROLOGUE)?
            .local_private_key(private_key)?;
        let handshake = match role {
            Role::Initiator => {
                let mut state = builder.build_initiator()?;
                send_handshake(&mut stream, &mut state)?;
                receive_handshake(&mut stream, &mut state)?;
                check_key(&state, peer_key)?;
                send_handshake(&mut stream, &mut state)?;
                state
            }
            Role::Responder => {
                let mut state = builder.build_responder()?;
                receive_handshake(&mut stream, &mut state)?;
                send_handshake(&mut stream, &mut state)?;
                receive_handshake(&mut stream, &mut state)?;
                check_key(&state, peer_key)?;
                state
            }
        };
        Ok(Self {
            stream,
            state: handshake.into_transport_mode()?,
            role,
            peer: DeviceId::from_public_key(peer_key),
            plaintext: Vec::new(),
            cursor: 0,
            ciphertext: Vec::new(),
        })
    }

    pub fn peer(&self) -> DeviceId {
        self.peer
    }

    pub fn send(&mut self, frame: &Frame) -> Result<(), ChannelError> {
        write_frame(&mut *self, frame)?;
        self.flush()?;
        Ok(())
    }

    pub fn receive(&mut self) -> Result<Frame, ChannelError> {
        Ok(read_frame(self)?)
    }
}

impl<T: Read> Read for Channel<T> {
    fn read(&mut self, destination: &mut [u8]) -> io::Result<usize> {
        if destination.is_empty() {
            return Ok(0);
        }
        if self.cursor == self.plaintext.len() {
            let size = read_size(&mut self.stream, TAG_BYTES + 1, MAX_RECORD_BYTES)?;
            self.ciphertext.resize(size, 0);
            self.stream.read_exact(&mut self.ciphertext)?;
            self.plaintext.resize(size, 0);
            let read = self
                .state
                .read_message(&self.ciphertext, &mut self.plaintext)
                .map_err(|source| io::Error::new(io::ErrorKind::InvalidData, source))?;
            self.plaintext.truncate(read);
            self.cursor = 0;
        }
        let length = destination.len().min(self.plaintext.len() - self.cursor);
        destination[..length].copy_from_slice(&self.plaintext[self.cursor..self.cursor + length]);
        self.cursor += length;
        Ok(length)
    }
}

impl<T: Write> Write for Channel<T> {
    fn write(&mut self, plaintext: &[u8]) -> io::Result<usize> {
        let size = plaintext.len().min(MAX_CHUNK_BYTES);
        if size == 0 {
            return Ok(0);
        }
        self.ciphertext.resize(size + TAG_BYTES, 0);
        let written = self
            .state
            .write_message(&plaintext[..size], &mut self.ciphertext)
            .map_err(|source| io::Error::new(io::ErrorKind::InvalidData, source))?;
        write_record(&mut self.stream, &self.ciphertext[..written])?;
        Ok(size)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

fn send_handshake(writer: &mut impl Write, state: &mut HandshakeState) -> Result<(), ChannelError> {
    let mut record = [0; MAX_HANDSHAKE_BYTES];
    let size = state.write_message(&[], &mut record)?;
    write_record(writer, &record[..size])?;
    writer.flush()?;
    Ok(())
}

fn receive_handshake(
    reader: &mut impl Read,
    state: &mut HandshakeState,
) -> Result<(), ChannelError> {
    let mut record = [0; MAX_HANDSHAKE_BYTES];
    let size = read_size(reader, 1, MAX_HANDSHAKE_BYTES)?;
    reader.read_exact(&mut record[..size])?;
    let mut payload = [0; MAX_HANDSHAKE_BYTES];
    if state.read_message(&record[..size], &mut payload)? != 0 {
        return Err(ChannelError::HandshakePayload);
    }
    Ok(())
}

fn check_key(state: &HandshakeState, expected: &[u8; 32]) -> Result<(), ChannelError> {
    if state.get_remote_static() == Some(expected.as_slice()) {
        Ok(())
    } else {
        Err(ChannelError::Identity)
    }
}

fn write_record(writer: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    writer.write_all(&(bytes.len() as u16).to_be_bytes())?;
    writer.write_all(bytes)
}

fn read_size(reader: &mut impl Read, min: usize, max: usize) -> io::Result<usize> {
    let mut size = [0; 2];
    reader.read_exact(&mut size)?;
    let size = u16::from_be_bytes(size) as usize;
    if !(min..=max).contains(&size) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            ChannelError::RecordSize,
        ));
    }
    Ok(size)
}

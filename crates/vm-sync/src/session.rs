use std::io::{self, Read, Write};

use thiserror::Error;

use crate::protocol::bounded_batch;
use crate::{
    Channel, ChannelError, Cursor, DeviceId, Frame, Operation, OperationLog, ProtocolError, Role,
    SyncError, MAX_BATCH_OPS,
};

const MAX_TRANSFER_OPS: usize = 1024;
const MAX_TRANSFER_FRAMES: usize = 64;

#[derive(Debug, Error)]
pub enum ExchangeError {
    #[error(transparent)]
    Channel(#[from] ChannelError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Data(#[from] SyncError),
    #[error("unexpected frame during journal exchange")]
    Unexpected,
    #[error("peer advertised a different identity than the authenticated key")]
    PeerIdentity,
    #[error("peer cannot relay operations for another device")]
    RelayedOperation,
    #[error("journal transfer exceeded the session limit")]
    TransferLimit,
    #[error("journal sequence does not match the negotiated frontier")]
    Sequence,
    #[error("journal persistence failed: {0}")]
    Persist(#[source] io::Error),
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ExchangeReport {
    pub sent: usize,
    pub inserted: usize,
    /// Either participant has another page to send in a later exchange.
    pub more: bool,
}

/// Exchange both peers' own operations. Forwarding third-party operations
/// requires independent origin authentication and is deliberately rejected.
///
/// The callback writes each batch of new operations in one durable transaction.
/// It must report success only after committing. Successful earlier batches
/// remain durable if a later step fails, so retries converge from the frontier.
/// Drop the connection after a protocol, persistence, or I/O error.
pub fn exchange<T: Read + Write>(
    channel: &mut Channel<T>,
    local: DeviceId,
    name: &str,
    log: &mut OperationLog,
    mut persist: impl FnMut(&[Operation]) -> io::Result<()>,
) -> Result<ExchangeReport, ExchangeError> {
    let peer = channel.peer();
    let hello = Frame::Hello {
        device: local,
        name: name.to_owned(),
    };
    let frontier = Frame::Frontier {
        received: vec![Cursor {
            device: peer,
            through: log.received_through(peer),
        }],
    };
    let through = match channel.role {
        Role::Initiator => {
            channel.send(&hello)?;
            receive_hello(channel, peer)?;
            channel.send(&frontier)?;
            receive_cursor(channel, local)?
        }
        Role::Responder => {
            receive_hello(channel, peer)?;
            channel.send(&hello)?;
            let through = receive_cursor(channel, local)?;
            channel.send(&frontier)?;
            through
        }
    };
    let (sent, sent_more, inserted, received_more) = match channel.role {
        Role::Initiator => {
            let (sent, sent_more) = send_operations(channel, log, local, through)?;
            let (inserted, received_more) = receive_operations(channel, log, peer, &mut persist)?;
            channel.send(&Frame::Bye)?;
            receive_bye(channel)?;
            (sent, sent_more, inserted, received_more)
        }
        Role::Responder => {
            let (inserted, received_more) = receive_operations(channel, log, peer, &mut persist)?;
            let (sent, sent_more) = send_operations(channel, log, local, through)?;
            receive_bye(channel)?;
            channel.send(&Frame::Bye)?;
            (sent, sent_more, inserted, received_more)
        }
    };
    Ok(ExchangeReport {
        sent,
        inserted,
        more: sent_more || received_more,
    })
}

fn receive_hello<T: Read + Write>(
    channel: &mut Channel<T>,
    peer: DeviceId,
) -> Result<(), ExchangeError> {
    match channel.receive()? {
        Frame::Hello { device, .. } if device == peer => Ok(()),
        Frame::Hello { .. } => Err(ExchangeError::PeerIdentity),
        _ => Err(ExchangeError::Unexpected),
    }
}

fn receive_cursor<T: Read + Write>(
    channel: &mut Channel<T>,
    local: DeviceId,
) -> Result<u64, ExchangeError> {
    if let Frame::Frontier { received } = channel.receive()? {
        if let [cursor] = received.as_slice() {
            if cursor.device == local {
                return Ok(cursor.through);
            }
        }
    }
    Err(ExchangeError::Unexpected)
}

fn send_operations<T: Read + Write>(
    channel: &mut Channel<T>,
    log: &OperationLog,
    local: DeviceId,
    through: u64,
) -> Result<(usize, bool), ExchangeError> {
    let contiguous = log.received_through(local);
    if through > contiguous {
        return Err(ExchangeError::Sequence);
    }
    let mut available = log
        .missing_from(local, through)
        .take_while(|op| op.id.sequence <= contiguous)
        .peekable();
    let mut sent = 0;
    for index in 0..MAX_TRANSFER_FRAMES {
        let frame = bounded_batch(&mut available, (MAX_TRANSFER_OPS - sent).min(MAX_BATCH_OPS))?;
        let Frame::Operations { operations, more } = &frame else {
            unreachable!("bounded_batch produces an operations frame");
        };
        sent += operations.len();
        channel.send(&frame)?;
        if !more || sent == MAX_TRANSFER_OPS || index + 1 == MAX_TRANSFER_FRAMES {
            channel.send(&Frame::TransferComplete { more: *more })?;
            let stored = match channel.receive()? {
                Frame::Ack { stored } => stored,
                _ => return Err(ExchangeError::Unexpected),
            };
            let [cursor] = stored.as_slice() else {
                return Err(ExchangeError::Unexpected);
            };
            if cursor.device != local || cursor.through < through + sent as u64 {
                return Err(ExchangeError::Sequence);
            }
            return Ok((sent, *more));
        }
    }
    Err(ExchangeError::TransferLimit)
}

fn receive_operations<T: Read + Write>(
    channel: &mut Channel<T>,
    log: &mut OperationLog,
    peer: DeviceId,
    persist: &mut impl FnMut(&[Operation]) -> io::Result<()>,
) -> Result<(usize, bool), ExchangeError> {
    let mut received = 0;
    let mut inserted = 0;
    let mut sequence = log.received_through(peer);
    for index in 0..=MAX_TRANSFER_FRAMES {
        let operations = match channel.receive()? {
            Frame::TransferComplete { more } => {
                channel.send(&Frame::Ack {
                    stored: vec![Cursor {
                        device: peer,
                        through: log.received_through(peer),
                    }],
                })?;
                return Ok((inserted, more));
            }
            Frame::Operations { operations, .. } if index < MAX_TRANSFER_FRAMES => operations,
            _ => return Err(ExchangeError::Unexpected),
        };
        received += operations.len();
        if received > MAX_TRANSFER_OPS {
            return Err(ExchangeError::TransferLimit);
        }
        for operation in &operations {
            if operation.id.device != peer {
                return Err(ExchangeError::RelayedOperation);
            }
            if sequence.checked_add(1) != Some(operation.id.sequence) {
                return Err(ExchangeError::Sequence);
            }
            sequence = operation.id.sequence;
        }
        inserted += log.extend_with(operations, |batch| {
            if !batch.is_empty() {
                persist(batch).map_err(ExchangeError::Persist)?;
            }
            Ok::<(), ExchangeError>(())
        })?;
    }
    Err(ExchangeError::TransferLimit)
}

fn receive_bye<T: Read + Write>(channel: &mut Channel<T>) -> Result<(), ExchangeError> {
    if matches!(channel.receive()?, Frame::Bye) {
        Ok(())
    } else {
        Err(ExchangeError::Unexpected)
    }
}

//! Deterministic reading-state replication. Persist operations before advertising a
//! frontier. Apply operations received from an authenticated and trusted session.

mod channel;
mod protocol;
mod replica;
mod session;

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use vm_core::{BookId, Loc};

pub use channel::{generate_keypair, Channel, ChannelError, Role};
pub use protocol::{
    next_batch, read_frame, write_frame, Frame, ProtocolError, MAX_FRAME_BYTES, PROTOCOL_VERSION,
};
pub use replica::{AnnotationHeads, OperationLog, ReadingState, VersionedAnnotation};
pub use session::{exchange, ExchangeError, ExchangeReport};

pub const MAX_BATCH_OPS: usize = 128;
pub const MAX_PEERS: usize = 64;
pub const MAX_ANNOTATION_PARENTS: usize = 16;
pub const NEARBY_PROGRESS_MS: u64 = 2_000;
const MAX_LABEL_BYTES: usize = 1024;
const MAX_NOTE_BYTES: usize = 16 * 1024;
const MAX_SELECTION_BYTES: usize = 16 * 1024;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SyncError {
    #[error("invalid synchronization data: {0}")]
    Invalid(&'static str),
    #[error("different operations claim the same device and sequence")]
    OperationCollision,
    #[error("hybrid logical clock overflow")]
    ClockOverflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DeviceId(pub [u8; 16]);

impl DeviceId {
    /// Identity is derived from a device's public key. A trusted binding to the
    /// public key is checked by the session layer.
    pub fn from_public_key(key: &[u8; 32]) -> Self {
        let mut id = [0; 16];
        id.copy_from_slice(&blake3::hash(key).as_bytes()[..16]);
        Self(id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct OpId {
    pub device: DeviceId,
    pub sequence: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Clock {
    pub wall_ms: u64,
    pub counter: u32,
}

impl Clock {
    /// Advances beyond the local clock and an optional received clock. The caller
    /// supplies UTC milliseconds, which allows deterministic tests and simulation.
    pub fn tick(&mut self, wall_ms: u64, received: Option<Self>) -> Result<Self, SyncError> {
        let newest = received.map_or(*self, |remote| remote.max(*self));
        *self = if wall_ms > newest.wall_ms {
            Self {
                wall_ms,
                counter: 0,
            }
        } else {
            Self {
                wall_ms: newest.wall_ms,
                counter: newest
                    .counter
                    .checked_add(1)
                    .ok_or(SyncError::ClockOverflow)?,
            }
        };
        Ok(*self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BookmarkKey {
    pub book: BookId,
    pub loc: Loc,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AnnotationKey {
    pub book: BookId,
    pub annotation: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationColor {
    Yellow,
    Green,
    Blue,
    Pink,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Annotation {
    pub start: Loc,
    pub end_off: u32,
    pub text: String,
    pub note: Option<String>,
    pub color: AnnotationColor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    Progress {
        book: BookId,
        loc: Loc,
    },
    AddBookmark {
        key: BookmarkKey,
        label: Option<String>,
    },
    RemoveBookmark {
        key: BookmarkKey,
    },
    /// Concurrent edits become separate heads. Resolve them by creating an
    /// edit with all the heads as parents. None represents a deletion.
    Annotation {
        key: AnnotationKey,
        parents: Vec<OpId>,
        value: Option<Annotation>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operation {
    pub id: OpId,
    pub clock: Clock,
    pub change: Change,
}

impl Operation {
    pub fn validate(&self) -> Result<(), SyncError> {
        if self.id.sequence == 0 {
            return Err(SyncError::Invalid("sequence must be positive"));
        }
        match &self.change {
            Change::Progress { book, .. } => validate_id(book),
            Change::AddBookmark { key, label } => {
                validate_id(&key.book)?;
                validate_text(label.as_deref(), MAX_LABEL_BYTES)
            }
            Change::RemoveBookmark { key } => validate_id(&key.book),
            Change::Annotation {
                key,
                parents,
                value,
            } => {
                validate_id(&key.book)?;
                validate_id(&key.annotation)?;
                if parents.len() > MAX_ANNOTATION_PARENTS
                    || parents.iter().any(|parent| {
                        parent.sequence == 0
                            || (parent.device == self.id.device
                                && parent.sequence >= self.id.sequence)
                    })
                    || parents.iter().collect::<BTreeSet<_>>().len() != parents.len()
                {
                    return Err(SyncError::Invalid("invalid annotation parents"));
                }
                if let Some(annotation) = value {
                    if annotation.end_off <= annotation.start.off
                        || annotation.text.chars().count()
                            != (annotation.end_off - annotation.start.off) as usize
                    {
                        return Err(SyncError::Invalid("invalid annotation selection"));
                    }
                    validate_text(Some(&annotation.text), MAX_SELECTION_BYTES)?;
                    validate_text(annotation.note.as_deref(), MAX_NOTE_BYTES)?;
                } else if parents.is_empty() {
                    return Err(SyncError::Invalid("annotation deletion requires a parent"));
                }
                Ok(())
            }
        }
    }

    pub(crate) fn order(&self) -> (Clock, OpId) {
        (self.clock, self.id)
    }
}

fn validate_id(id: &str) -> Result<(), SyncError> {
    if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
        return Err(SyncError::Invalid("invalid entity identifier"));
    }
    Ok(())
}

fn validate_text(value: Option<&str>, max_bytes: usize) -> Result<(), SyncError> {
    if value.is_some_and(|value| value.len() > max_bytes || value.contains('\0')) {
        return Err(SyncError::Invalid("invalid text length or control byte"));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub device: DeviceId,
    /// The greatest contiguous sequence durably stored for this device.
    pub through: u64,
}

pub(crate) fn validate_frontier(frontier: &[Cursor]) -> Result<(), SyncError> {
    if frontier.len() > MAX_PEERS
        || frontier
            .windows(2)
            .any(|pair| pair[0].device >= pair[1].device)
    {
        return Err(SyncError::Invalid("invalid synchronization frontier"));
    }
    Ok(())
}

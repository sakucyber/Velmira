use std::collections::{btree_map::Entry, BTreeMap, BTreeSet};
use std::ops::Bound::{Excluded, Included};

use vm_core::{BookId, Loc};

use crate::{
    validate_frontier, Annotation, AnnotationKey, BookmarkKey, Change, Clock, Cursor, DeviceId,
    OpId, Operation, SyncError, MAX_PEERS, NEARBY_PROGRESS_MS,
};

/// The caller owns the durable journal and must restore it before advertising
/// the frontier. Tombstones remain in the journal until a later retention
/// policy can establish that every trusted peer has observed them.
#[derive(Debug, Default)]
pub struct OperationLog {
    entries: BTreeMap<OpId, Operation>,
    through: BTreeMap<DeviceId, u64>,
}

impl OperationLog {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn operations(&self) -> impl Iterator<Item = &Operation> {
        self.entries.values()
    }

    pub fn insert(&mut self, operation: Operation) -> Result<bool, SyncError> {
        self.extend([operation]).map(|count| count == 1)
    }

    /// Validate the complete batch before modifying state. Exact duplicates
    /// are harmless. Sequence reuse with different content aborts the batch.
    pub fn extend(
        &mut self,
        operations: impl IntoIterator<Item = Operation>,
    ) -> Result<usize, SyncError> {
        self.extend_with(operations, |_| Ok(()))
    }

    pub(crate) fn extend_with<E: From<SyncError>>(
        &mut self,
        operations: impl IntoIterator<Item = Operation>,
        persist: impl FnOnce(&[Operation]) -> Result<(), E>,
    ) -> Result<usize, E> {
        let mut staged = BTreeMap::new();
        let mut touched = BTreeSet::new();
        for operation in operations {
            operation.validate()?;
            if let Some(existing) = self.entries.get(&operation.id) {
                if existing != &operation {
                    return Err(SyncError::OperationCollision.into());
                }
                continue;
            }
            match staged.entry(operation.id) {
                Entry::Occupied(entry) if entry.get() != &operation => {
                    return Err(SyncError::OperationCollision.into());
                }
                Entry::Occupied(_) => {}
                Entry::Vacant(entry) => {
                    touched.insert(operation.id.device);
                    entry.insert(operation);
                }
            }
        }
        let new_peers = touched
            .iter()
            .filter(|device| !self.through.contains_key(device))
            .count();
        if self.through.len() + new_peers > MAX_PEERS {
            return Err(SyncError::Invalid("too many devices in the operation log").into());
        }

        let staged = staged.into_values().collect::<Vec<_>>();
        persist(&staged)?;
        let inserted = staged.len();
        self.entries.extend(
            staged
                .into_iter()
                .map(|operation| (operation.id, operation)),
        );
        for device in touched {
            let through = self.through.entry(device).or_default();
            while let Some(sequence) = through.checked_add(1) {
                if !self.entries.contains_key(&OpId { device, sequence }) {
                    break;
                }
                *through = sequence;
            }
        }
        Ok(inserted)
    }

    pub fn frontier(&self) -> Vec<Cursor> {
        self.through
            .iter()
            .map(|(&device, &through)| Cursor { device, through })
            .collect()
    }

    pub(crate) fn received_through(&self, device: DeviceId) -> u64 {
        self.through.get(&device).copied().unwrap_or(0)
    }

    pub(crate) fn missing_from(
        &self,
        device: DeviceId,
        through: u64,
    ) -> impl Iterator<Item = &Operation> {
        self.entries
            .range((
                Excluded(OpId {
                    device,
                    sequence: through,
                }),
                Included(OpId {
                    device,
                    sequence: u64::MAX,
                }),
            ))
            .map(|(_, operation)| operation)
    }

    /// Deliver entries after the peer's durable contiguous frontier. Entries
    /// beyond holes may be delivered repeatedly until the missing entries arrive.
    pub fn missing_after<'a>(
        &'a self,
        frontier: &'a [Cursor],
    ) -> Result<impl Iterator<Item = &'a Operation> + 'a, SyncError> {
        validate_frontier(frontier)?;
        Ok(self.through.keys().flat_map(move |&device| {
            let through = frontier
                .binary_search_by_key(&device, |cursor| cursor.device)
                .map_or(0, |index| frontier[index].through);
            self.missing_from(device, through)
        }))
    }

    /// A hash of the journal in a fixed order, including deletions and conflicts.
    pub fn digest(&self) -> [u8; 32] {
        let mut hash = blake3::Hasher::new_derive_key("Velmira sync journal version 1");
        for operation in self.entries.values() {
            ciborium::into_writer(operation, &mut hash)
                .expect("operation serialization to an in-memory hash is infallible");
        }
        *hash.finalize().as_bytes()
    }

    pub fn reading_state(&self) -> ReadingState {
        ReadingState::from_operations(self.entries.values())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionedAnnotation {
    pub version: OpId,
    pub clock: Clock,
    pub value: Option<Annotation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotationHeads {
    /// Deterministic default. A deleted winner still retains concurrent edits.
    pub current: VersionedAnnotation,
    /// Other heads, ordered newest first. A user can choose which one to keep.
    pub conflicts: Vec<VersionedAnnotation>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReadingState {
    pub progress: BTreeMap<BookId, Loc>,
    pub bookmarks: BTreeMap<BookmarkKey, Option<String>>,
    pub annotations: BTreeMap<AnnotationKey, AnnotationHeads>,
}

impl ReadingState {
    fn from_operations<'a>(operations: impl Iterator<Item = &'a Operation>) -> Self {
        let mut progress = BTreeMap::<&str, BTreeMap<DeviceId, (Clock, OpId, Loc)>>::new();
        let mut bookmarks = BTreeMap::<&BookmarkKey, Vec<&Operation>>::new();
        let mut annotations = BTreeMap::<&AnnotationKey, Vec<&Operation>>::new();
        for operation in operations {
            match &operation.change {
                Change::Progress { book, loc } => {
                    let (clock, id) = operation.order();
                    let records = progress.entry(book).or_default();
                    let latest = records.entry(id.device).or_insert((clock, id, *loc));
                    *latest = (*latest).max((clock, id, *loc));
                }
                Change::AddBookmark { key, .. } | Change::RemoveBookmark { key } => {
                    bookmarks.entry(key).or_default().push(operation);
                }
                Change::Annotation { key, .. } => {
                    annotations.entry(key).or_default().push(operation);
                }
            }
        }
        Self {
            progress: progress
                .into_iter()
                .filter_map(|(book, latest)| merge_progress(latest).map(|loc| (book.into(), loc)))
                .collect(),
            bookmarks: bookmarks
                .into_iter()
                .filter_map(|(key, history)| merge_bookmarks(key, history))
                .collect(),
            annotations: annotations
                .into_iter()
                .filter_map(|(key, history)| merge_annotations(key, history))
                .collect(),
        }
    }
}

fn merge_progress(latest: BTreeMap<DeviceId, (Clock, OpId, Loc)>) -> Option<Loc> {
    let (clock, _, loc) = latest.values().max().copied()?;
    latest
        .into_values()
        .filter(|(candidate_clock, _, candidate)| {
            candidate.chapter == loc.chapter
                && clock.wall_ms.saturating_sub(candidate_clock.wall_ms) <= NEARBY_PROGRESS_MS
        })
        .map(|(_, _, candidate)| candidate)
        .max()
}

fn merge_bookmarks(
    key: &BookmarkKey,
    history: Vec<&Operation>,
) -> Option<(BookmarkKey, Option<String>)> {
    let removed = history
        .iter()
        .filter(|op| matches!(&op.change, Change::RemoveBookmark { .. }))
        .map(|op| op.order())
        .max();
    history
        .into_iter()
        .filter_map(|op| match &op.change {
            Change::AddBookmark { label, .. }
                if removed.is_none_or(|removed| op.order() > removed) =>
            {
                Some((op.order(), label))
            }
            _ => None,
        })
        .min_by_key(|(order, _)| *order)
        .map(|(_, label)| (key.clone(), label.clone()))
}

fn merge_annotations(
    key: &AnnotationKey,
    history: Vec<&Operation>,
) -> Option<(AnnotationKey, AnnotationHeads)> {
    let clocks: BTreeMap<_, _> = history.iter().map(|op| (op.id, op.clock)).collect();
    let mut superseded = BTreeSet::new();
    for operation in &history {
        let Change::Annotation { parents, .. } = &operation.change else {
            continue;
        };
        // Edges must reference the same entity, and the parent's clock must
        // strictly precede the child's. Missing parents may arrive later.
        superseded.extend(
            parents
                .iter()
                .filter(|id| clocks.get(id).is_some_and(|clock| *clock < operation.clock))
                .copied(),
        );
    }

    let mut heads: Vec<_> = history
        .into_iter()
        .filter_map(|op| match &op.change {
            Change::Annotation { value, .. } if !superseded.contains(&op.id) => {
                Some(VersionedAnnotation {
                    version: op.id,
                    clock: op.clock,
                    value: value.clone(),
                })
            }
            _ => None,
        })
        .collect();
    heads.sort_unstable_by_key(|head| std::cmp::Reverse((head.clock, head.version)));
    let mut heads = heads.into_iter();
    let current = heads.next()?;
    Some((
        key.clone(),
        AnnotationHeads {
            current,
            conflicts: heads.collect(),
        },
    ))
}

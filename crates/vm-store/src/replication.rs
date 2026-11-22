use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use vm_core::{BookId, Loc};
use vm_sync::{
    Annotation, AnnotationColor, AnnotationKey, BookmarkKey, Change, Clock, DeviceId, OpId,
    Operation, OperationLog, ReadingState,
};

use crate::{database, Error, Result};

const MAX_JOURNAL: usize = 50_000;
const MAX_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;
type ShadowKey = (String, String);
type Shadow = BTreeMap<ShadowKey, Change>;

#[derive(Default)]
struct Records {
    syncable: Shadow,
    local_only: BTreeSet<ShadowKey>,
}

pub struct Replication {
    db: Connection,
    local: DeviceId,
}

impl Replication {
    pub fn open(data_dir: &Path, local: DeviceId) -> Result<Self> {
        let mut db = database::open(&data_dir.join("velmira.db"))?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let expected = serde_json::to_string(&local)?;
        tx.execute(
            "INSERT OR IGNORE INTO meta(key,value) VALUES ('sync_device',?1)",
            [&expected],
        )?;
        let actual: String = tx.query_row(
            "SELECT value FROM meta WHERE key='sync_device'",
            [],
            |row| row.get(0),
        )?;
        if actual != expected {
            return Err(Error::Invalid(
                "library belongs to another synchronization identity".into(),
            ));
        }
        tx.commit()?;
        Ok(Self { db, local })
    }

    pub fn snapshot(&mut self) -> Result<OperationLog> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut journal = read_journal(&tx)?;
        capture(&tx, self.local, &mut journal)?;
        check_journal_size(&tx)?;
        materialize(&tx, &journal)?;
        tx.commit()?;
        Ok(journal)
    }

    pub fn ingest(&mut self, batch: &[Operation]) -> Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut journal = read_journal(&tx)?;
        capture(&tx, self.local, &mut journal)?;
        for operation in batch {
            if operation.id.device == self.local {
                return Err(Error::Invalid(
                    "a remote batch contains a local operation".into(),
                ));
            }
            append(&tx, &mut journal, operation.clone())?;
        }
        check_journal_size(&tx)?;
        materialize(&tx, &journal)?;
        tx.commit()?;
        Ok(())
    }
}

fn read_journal(db: &Connection) -> Result<OperationLog> {
    check_journal_size(db)?;
    let mut statement = db.prepare("SELECT json FROM sync_op ORDER BY device, seq LIMIT ?1")?;
    let mut rows = statement.query([(MAX_JOURNAL + 1) as u64])?;
    let mut operations = Vec::new();
    while let Some(row) = rows.next()? {
        operations.push(serde_json::from_str::<Operation>(database::text_column(
            row, 0,
        )?)?);
        if operations.len() > MAX_JOURNAL {
            return Err(Error::Invalid(
                "synchronization journal limit reached".into(),
            ));
        }
    }
    let mut journal = OperationLog::default();
    journal.extend(operations)?;
    Ok(journal)
}

fn append(db: &Connection, journal: &mut OperationLog, operation: Operation) -> Result<()> {
    if journal.insert(operation.clone())? {
        if journal.len() > MAX_JOURNAL {
            return Err(Error::Invalid(
                "synchronization journal limit reached".into(),
            ));
        }
        db.execute(
            "INSERT INTO sync_op(device, seq, json) VALUES (?1, ?2, ?3)",
            params![
                operation.id.device.0.as_slice(),
                operation.id.sequence,
                serde_json::to_string(&operation)?
            ],
        )?;
    }
    Ok(())
}

fn check_journal_size(db: &Connection) -> Result<()> {
    let bytes: u64 = db.query_row(
        "SELECT coalesce(sum(length(CAST(json AS BLOB))),0) FROM sync_op",
        [],
        |row| row.get(0),
    )?;
    if bytes > MAX_JOURNAL_BYTES {
        return Err(Error::Invalid(
            "synchronization journal size limit reached".into(),
        ));
    }
    Ok(())
}

fn key(change: &Change) -> Result<ShadowKey> {
    let (kind, id) = match change {
        Change::Progress { book, .. } => ("progress", book.clone()),
        Change::AddBookmark { key, .. } | Change::RemoveBookmark { key } => {
            ("bookmark", serde_json::to_string(key)?)
        }
        Change::Annotation { key, .. } => ("annotation", serde_json::to_string(key)?),
    };
    Ok((kind.into(), id))
}

fn records(
    db: &Connection,
    sql: &str,
    mut read: impl FnMut(&rusqlite::Row<'_>) -> Result<Change>,
    entries: &mut Records,
) -> Result<()> {
    let mut statement = db.prepare(sql)?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let candidate = Operation {
            id: OpId {
                device: DeviceId([0; 16]),
                sequence: 1,
            },
            clock: Clock::default(),
            change: read(row)?,
        };
        let key = key(&candidate.change)?;
        if candidate.validate().is_ok() {
            entries.syncable.insert(key, candidate.change);
        } else {
            // Legacy imports can contain values larger than the wire limits.
            entries.local_only.insert(key);
        }
        if entries.syncable.len() + entries.local_only.len() > MAX_JOURNAL {
            return Err(Error::Invalid(
                "too many reading records to synchronize".into(),
            ));
        }
    }
    Ok(())
}

fn loc_at(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<Loc> {
    Ok(Loc {
        chapter: row.get(offset)?,
        para: row.get(offset + 1)?,
        off: row.get(offset + 2)?,
    })
}

fn current(db: &Connection) -> Result<Records> {
    let mut entries = Records::default();
    records(
        db,
        "SELECT book_id, chapter, para, off FROM progress",
        |row| {
            Ok(Change::Progress {
                book: row.get(0)?,
                loc: loc_at(row, 1)?,
            })
        },
        &mut entries,
    )?;
    records(
        db,
        "SELECT book_id, chapter, para, off, label FROM bookmark",
        |row| {
            Ok(Change::AddBookmark {
                key: BookmarkKey {
                    book: row.get(0)?,
                    loc: loc_at(row, 1)?,
                },
                label: row.get(4)?,
            })
        },
        &mut entries,
    )?;
    records(
        db,
        "SELECT book_id,id,chapter,para,start_off,end_off,text,note,color FROM annotation",
        |row| {
            Ok(Change::Annotation {
                key: AnnotationKey {
                    book: row.get(0)?,
                    annotation: row.get(1)?,
                },
                parents: Vec::new(),
                value: Some(Annotation {
                    start: loc_at(row, 2)?,
                    end_off: row.get(5)?,
                    text: row.get(6)?,
                    note: row.get(7)?,
                    color: match database::text_column(row, 8)? {
                        "yellow" => AnnotationColor::Yellow,
                        "green" => AnnotationColor::Green,
                        "blue" => AnnotationColor::Blue,
                        "pink" => AnnotationColor::Pink,
                        _ => return Err(Error::Invalid("invalid annotation color".into())),
                    },
                }),
            })
        },
        &mut entries,
    )?;
    Ok(entries)
}

fn previous(db: &Connection) -> Result<Shadow> {
    let mut statement = db.prepare("SELECT kind,id,value FROM sync_shadow")?;
    let mut rows = statement.query([])?;
    let mut entries = Shadow::new();
    while let Some(row) = rows.next()? {
        entries.insert(
            (row.get(0)?, row.get(1)?),
            serde_json::from_str(database::text_column(row, 2)?)?,
        );
    }
    Ok(entries)
}

fn capture(db: &Connection, local: DeviceId, journal: &mut OperationLog) -> Result<()> {
    let old = previous(db)?;
    let now = current(db)?;
    let mut clock = journal
        .operations()
        .map(|op| op.clock)
        .max()
        .unwrap_or_default();
    let mut sequence = 0;
    let mut progress_clocks = BTreeMap::<BookId, Clock>::new();
    for operation in journal.operations().filter(|op| op.id.device == local) {
        sequence = sequence.max(operation.id.sequence);
        if let Change::Progress { book, .. } = &operation.change {
            let latest = progress_clocks.entry(book.clone()).or_default();
            *latest = (*latest).max(operation.clock);
        }
    }
    let state = journal.reading_state();
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let changes = now
        .syncable
        .iter()
        .filter(|(key, change)| old.get(*key) != Some(*change))
        .map(|(key, change)| (old.get(key), change.clone()))
        .chain(
            old.iter()
                .filter(|(key, _)| {
                    !now.syncable.contains_key(*key) && !now.local_only.contains(*key)
                })
                .filter_map(|(_, old)| match old {
                    Change::AddBookmark { key, .. } => {
                        Some(Change::RemoveBookmark { key: key.clone() })
                    }
                    Change::Annotation { key, .. } => Some(Change::Annotation {
                        key: key.clone(),
                        parents: Vec::new(),
                        value: None,
                    }),
                    _ => None,
                })
                .map(|change| (None, change)),
        );
    let mut record = |change, clock| {
        sequence = sequence
            .checked_add(1)
            .ok_or_else(|| Error::Invalid("sync sequence exhausted".into()))?;
        append(
            db,
            journal,
            Operation {
                id: OpId {
                    device: local,
                    sequence,
                },
                clock,
                change,
            },
        )
    };
    for (previous, mut change) in changes {
        if let Change::Annotation {
            key,
            parents,
            value,
        } = &mut change
        {
            if let Some(heads) = state.annotations.get(key) {
                // Editing the displayed revision preserves alternative revisions.
                parents.push(heads.current.version);
            } else if value.is_none() {
                continue;
            }
        }
        if let (Some(Change::AddBookmark { key, .. }), Change::AddBookmark { .. }) =
            (previous, &change)
        {
            record(
                Change::RemoveBookmark { key: key.clone() },
                clock.tick(wall, None)?,
            )?;
        }
        let stamp = match &change {
            Change::Progress { book, .. } => {
                // Compare reading times even when the devices capture together.
                // Use this book's clock to order local edits if time moved back.
                let seconds: i64 = db
                    .prepare_cached("SELECT updated_at FROM progress WHERE book_id=?1")?
                    .query_row([book], |row| row.get(0))?;
                let read_at = u64::try_from(seconds)
                    .unwrap_or_default()
                    .saturating_mul(1000)
                    .min(wall);
                let stamp = progress_clocks
                    .entry(book.clone())
                    .or_default()
                    .tick(read_at, None)?;
                clock = clock.max(stamp);
                stamp
            }
            _ => clock.tick(wall, None)?,
        };
        record(change, stamp)?;
    }
    Ok(())
}

fn materialize(db: &Connection, journal: &OperationLog) -> Result<()> {
    let state = journal.reading_state();
    let now = current(db)?;
    let managed_bookmarks: BTreeSet<_> = journal
        .operations()
        .filter_map(|op| match &op.change {
            Change::AddBookmark { key, .. } | Change::RemoveBookmark { key } => Some(key),
            _ => None,
        })
        .collect();
    apply_progress(db, &state, &now)?;
    for key in managed_bookmarks {
        let bookmark = state.bookmarks.get(key);
        let change = Change::AddBookmark {
            key: key.clone(),
            label: bookmark.cloned().flatten(),
        };
        let id = self::key(&change)?;
        if now.local_only.contains(&id) {
            continue;
        }
        let before = now.syncable.get(&id);
        match bookmark {
            Some(label)
                if before != Some(&change)
                    && database::paragraph_len(db, &key.book, &key.loc)?
                        .is_some_and(|n| key.loc.off <= n) =>
            {
                db.execute(
                    "INSERT INTO bookmark(book_id,chapter,para,off,label) VALUES (?1,?2,?3,?4,?5)
                    ON CONFLICT(book_id,chapter,para,off) DO UPDATE SET label=excluded.label",
                    params![key.book, key.loc.chapter, key.loc.para, key.loc.off, label],
                )?;
            }
            None if before.is_some() => {
                crate::reading_marks::delete_bookmark(db, &key.book, key.loc)?
            }
            _ => {}
        }
    }
    for (key, heads) in &state.annotations {
        let change = Change::Annotation {
            key: key.clone(),
            parents: Vec::new(),
            value: heads.current.value.clone(),
        };
        let id = self::key(&change)?;
        if now.local_only.contains(&id) {
            continue;
        }
        let before = now.syncable.get(&id);
        match &heads.current.value {
            Some(annotation) if before != Some(&change) => apply_annotation(db, key, annotation)?,
            None if before.is_some() => {
                db.execute(
                    "DELETE FROM annotation WHERE id=?1 AND book_id=?2",
                    params![key.annotation, key.book],
                )?;
            }
            _ => {}
        }
    }
    let next = current(db)?.syncable;
    let old = previous(db)?;
    for ((kind, id), _) in old.iter().filter(|(id, _)| !next.contains_key(*id)) {
        db.execute(
            "DELETE FROM sync_shadow WHERE kind=?1 AND id=?2",
            params![kind, id],
        )?;
    }
    for ((kind, id), change) in next
        .iter()
        .filter(|(id, change)| old.get(*id) != Some(*change))
    {
        db.execute(
            "INSERT INTO sync_shadow(kind,id,value) VALUES (?1,?2,?3)
            ON CONFLICT(kind,id) DO UPDATE SET value=excluded.value",
            params![kind, id, serde_json::to_string(change)?],
        )?;
    }
    Ok(())
}

fn apply_progress(db: &Connection, state: &ReadingState, now: &Records) -> Result<()> {
    for (book, loc) in &state.progress {
        let change = Change::Progress {
            book: book.clone(),
            loc: *loc,
        };
        let id = key(&change)?;
        if now.local_only.contains(&id)
            || now.syncable.get(&id) == Some(&change)
            || !database::valid_progress(db, book, loc)?
        {
            continue;
        }
        database::save_progress(db, book, loc)?;
    }
    Ok(())
}

fn apply_annotation(db: &Connection, key: &AnnotationKey, mark: &Annotation) -> Result<()> {
    let owner: Option<String> = db
        .query_row(
            "SELECT book_id FROM annotation WHERE id=?1",
            [&key.annotation],
            |row| row.get(0),
        )
        .optional()?;
    if owner.as_ref().is_some_and(|book| book != &key.book)
        || !database::paragraph_len(db, &key.book, &mark.start)?.is_some_and(|n| mark.end_off <= n)
    {
        return Ok(());
    }
    let chapter = database::chapter(db, &key.book, mark.start.chapter)?;
    let same_text = chapter
        .paragraphs
        .get(mark.start.para as usize)
        .is_some_and(|paragraph| {
            paragraph
                .chars()
                .skip(mark.start.off as usize)
                .take((mark.end_off - mark.start.off) as usize)
                .eq(mark.text.chars())
        });
    if same_text {
        let color = match mark.color {
            AnnotationColor::Yellow => "yellow",
            AnnotationColor::Green => "green",
            AnnotationColor::Blue => "blue",
            AnnotationColor::Pink => "pink",
        };
        db.execute("INSERT INTO annotation(id,book_id,chapter,para,start_off,end_off,text,note,color,created_at,updated_at)
                VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,unixepoch(),unixepoch())
                ON CONFLICT(id) DO UPDATE SET chapter=excluded.chapter,para=excluded.para,
                start_off=excluded.start_off,end_off=excluded.end_off,text=excluded.text,note=excluded.note,
                color=excluded.color,updated_at=unixepoch(),revision=annotation.revision+1",
            params![key.annotation,key.book,mark.start.chapter,mark.start.para,mark.start.off,
                mark.end_off,mark.text,mark.note,color])?;
    }
    Ok(())
}

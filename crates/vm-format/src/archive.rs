use std::io::{Read, Seek, SeekFrom};

use zip::read::ZipFile;
use zip::ZipArchive;

use crate::FormatError;

const ZIP_END_LEN: usize = 22;
const ZIP64_LOCATOR_LEN: usize = 20;
const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
pub(crate) const MAX_ARCHIVE_EXPANDED_BYTES: u64 = 512 * 1024 * 1024;
pub(crate) const MAX_ARCHIVE_ENTRIES: usize = 5000;
pub(crate) const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

pub(crate) fn open_archive<R: Read + Seek>(mut reader: R) -> Result<ZipArchive<R>, FormatError> {
    let directory_entry_count = check_directory_size(&mut reader)?;
    let mut archive = ZipArchive::new(reader)?;
    // zip indexes entries by decoded path. A duplicate name reduces the indexed count.
    if archive.len() != directory_entry_count {
        return Err(FormatError::InvalidArchive("条目数不一致或包含重复条目"));
    }
    let mut expanded_bytes = 0;
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index)?;
        if !safe_member_name(entry.name()) || entry.is_symlink() {
            return Err(FormatError::InvalidArchive("包含不安全路径或符号链接"));
        }
        expanded_bytes = entry.size().saturating_add(expanded_bytes);
        if expanded_bytes > MAX_ARCHIVE_EXPANDED_BYTES {
            return Err(FormatError::ArchiveLimit("总解压大小超过 512 MiB"));
        }
    }
    Ok(archive)
}

// Validate the conventional end record before the ZIP reader allocates per-entry metadata.
// ZIP64 is unnecessary under our input limits and rejected to keep these limits unambiguous.
fn check_directory_size(reader: &mut (impl Read + Seek)) -> Result<usize, FormatError> {
    let size = reader.seek(SeekFrom::End(0))?;
    if size > MAX_ARCHIVE_BYTES {
        return Err(FormatError::ArchiveLimit("文件超过 512 MiB"));
    }
    let tail_len = size.min((ZIP_END_LEN + ZIP64_LOCATOR_LEN + u16::MAX as usize) as u64) as usize;
    let mut tail = vec![0u8; tail_len];
    reader.seek(SeekFrom::End(-(tail_len as i64)))?;
    reader.read_exact(&mut tail)?;
    let record = (0..=tail_len.saturating_sub(ZIP_END_LEN))
        .rev()
        .find(|&pos| {
            let Some(data) = tail.get(pos..pos + ZIP_END_LEN) else {
                return false;
            };
            data.starts_with(b"PK\x05\x06")
                && pos + ZIP_END_LEN + usize::from(u16::from_le_bytes([data[20], data[21]]))
                    == tail_len
        });
    let pos = record.ok_or(FormatError::InvalidArchive("没有 ZIP 中央目录"))?;
    if pos >= ZIP64_LOCATOR_LEN
        && &tail[pos - ZIP64_LOCATOR_LEN..pos - ZIP64_LOCATOR_LEN + 4] == b"PK\x06\x07"
    {
        return Err(FormatError::InvalidArchive("不支持 ZIP64 中央目录"));
    }
    let data = &tail[pos..pos + ZIP_END_LEN];
    let count = u16::from_le_bytes([data[10], data[11]]);
    if data[4..8] != [0; 4] || u16::from_le_bytes([data[8], data[9]]) != count {
        return Err(FormatError::InvalidArchive("不支持分卷 ZIP"));
    }
    if usize::from(count) > MAX_ARCHIVE_ENTRIES {
        return Err(FormatError::ArchiveLimit("条目数超过 5000"));
    }
    let directory_length = u32::from_le_bytes(data[12..16].try_into().unwrap());
    if directory_length > 16 * 1024 * 1024 {
        return Err(FormatError::ArchiveLimit("ZIP 中央目录超过 16 MiB"));
    }
    Ok(count.into())
}

fn safe_member_name(name: &str) -> bool {
    let components = name.strip_suffix('/').unwrap_or(name);
    !components.is_empty()
        && !components.contains(['\\', ':', '\0'])
        && components
            .split('/')
            .all(|part| !matches!(part, "" | "." | ".."))
}

pub(crate) fn read_member<R: Read>(
    entry: ZipFile<'_, R>,
    limit: u64,
) -> Result<Vec<u8>, FormatError> {
    let expected = entry.size();
    if expected > limit {
        return Err(FormatError::ArchiveLimit("单条目解压后大小超限"));
    }
    let mut data = Vec::new();
    entry.take(limit + 1).read_to_end(&mut data)?;
    if data.len() as u64 > limit {
        return Err(FormatError::ArchiveLimit("单条目解压后大小超限"));
    }
    if data.len() as u64 != expected {
        return Err(FormatError::InvalidArchive("实际解压大小与目录记录不一致"));
    }
    Ok(data)
}

pub(crate) fn image_media_type(name: &str) -> Option<&'static str> {
    let (_, ext) = name.rsplit_once('.')?;
    match ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

pub(crate) fn check_raster_signature(bytes: &[u8], media_type: &str) -> Result<(), FormatError> {
    let magic_matches = match media_type {
        "image/jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        _ => false,
    };
    if !magic_matches {
        return Err(FormatError::InvalidArchive("图片格式与文件后缀不符"));
    }
    Ok(())
}

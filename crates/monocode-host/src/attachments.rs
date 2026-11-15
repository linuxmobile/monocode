//! Chunked attachment transfer for a remote desktop.
//!
//! An image pasted into a remote session has to reach this machine before the
//! provider can see it. RPC bodies are bounded, so bytes travel in chunks.
//!
//! Two properties matter:
//!
//! - **An upload is resumable and safe to retry.** Each chunk carries the offset
//!   it belongs at. Re-sending a chunk whose response was lost rewrites the same
//!   bytes; sending different bytes at an already-written offset is an error
//!   rather than a silent corruption. Chunks must arrive in order, because
//!   writing a sparse file would produce an attachment with holes in it.
//! - **A read never trusts a path.** `attachments.read` looks the id up among
//!   the blocks of the session named in the request, so a client can only fetch
//!   an attachment it already attached. A path sent by the client is never used
//!   to open a file.
//!
//! Chunk sizes are multiples of three so that the base64 of each chunk
//! concatenates, which the desktop client relies on.

use serde_json::{json, Value};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The largest attachment a host will accept, matching the desktop limit.
pub const MAX_BYTES: u64 = 20 * 1024 * 1024;
/// Bytes moved per chunk. Divisible by 3 so base64 chunks join cleanly.
pub const CHUNK_BYTES: usize = 512 * 1024;
/// A chunk's base64 form is four thirds of its byte length.
const MAX_ENCODED: usize = CHUNK_BYTES.div_ceil(3) * 4;

/// Why an attachment request was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum AttachmentError {
    InvalidId,
    InvalidChunk,
    TooLarge,
    OutOfOrder,
    Mismatch,
    NotFound,
    Incomplete,
}

impl std::fmt::Display for AttachmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidId => "Invalid attachment ID",
            Self::InvalidChunk => "Invalid attachment chunk",
            Self::TooLarge => "Invalid attachment chunk size",
            Self::OutOfOrder => "Attachment chunks are out of order",
            Self::Mismatch => "Attachment retry does not match uploaded bytes",
            Self::NotFound => "Image attachment not found",
            Self::Incomplete => "Attachment is incomplete",
        })
    }
}

impl From<AttachmentError> for String {
    fn from(error: AttachmentError) -> Self {
        error.to_string()
    }
}

/// True for the UUID shape an attachment id must have.
///
/// Checking the shape rather than sanitising means an id can never contain a
/// path separator, so it cannot escape the attachment directory.
pub fn valid_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        match index {
            8 | 13 | 18 | 23 => {
                if *byte != b'-' {
                    return false;
                }
            }
            _ => {
                if !byte.is_ascii_hexdigit() {
                    return false;
                }
            }
        }
    }
    true
}

fn path_for(directory: &Path, id: &str) -> Result<PathBuf, AttachmentError> {
    if !valid_id(id) {
        return Err(AttachmentError::InvalidId);
    }
    Ok(directory.join(id))
}

/// Appends one chunk at `offset`.
///
/// Returns the offset the next chunk belongs at.
pub fn write_chunk(
    directory: &Path,
    id: &str,
    offset: u64,
    size: u64,
    encoded: &str,
) -> Result<u64, AttachmentError> {
    if size > MAX_BYTES {
        return Err(AttachmentError::InvalidChunk);
    }
    if encoded.len() > MAX_ENCODED
        || !encoded.len().is_multiple_of(4)
        || !encoded
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
    {
        return Err(AttachmentError::InvalidChunk);
    }
    let bytes = base64_decode(encoded).ok_or(AttachmentError::InvalidChunk)?;
    if bytes.len() > CHUNK_BYTES || offset.saturating_add(bytes.len() as u64) > size {
        return Err(AttachmentError::TooLarge);
    }
    let path = path_for(directory, id)?;
    std::fs::create_dir_all(directory).map_err(|_| AttachmentError::InvalidChunk)?;

    // Read the current length without creating the file if it is not there.
    let existing = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if existing == offset {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|_| AttachmentError::InvalidChunk)?;
        file.write_all(&bytes)
            .map_err(|_| AttachmentError::InvalidChunk)?;
    } else if existing >= offset + bytes.len() as u64 {
        // A retry: the bytes must match what is already there, or the client is
        // reassembling a different file.
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .open(&path)
            .map_err(|_| AttachmentError::InvalidChunk)?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| AttachmentError::InvalidChunk)?;
        let mut current = vec![0u8; bytes.len()];
        file.read_exact(&mut current)
            .map_err(|_| AttachmentError::InvalidChunk)?;
        if current != bytes {
            return Err(AttachmentError::Mismatch);
        }
    } else {
        return Err(AttachmentError::OutOfOrder);
    }
    Ok(offset + bytes.len() as u64)
}

/// Reads one chunk of an attachment already attached to `session`.
///
/// `found` is the size the session's metadata claims; the file on disk must
/// match it, so a partially uploaded attachment is refused instead of being
/// handed to a provider as a truncated image.
pub fn read_chunk(
    directory: &Path,
    id: &str,
    expected_size: u64,
    offset: u64,
) -> Result<Value, AttachmentError> {
    if offset > expected_size {
        return Err(AttachmentError::InvalidChunk);
    }
    let path = path_for(directory, id)?;
    let actual = std::fs::metadata(&path).map_err(|_| AttachmentError::Incomplete)?;
    if actual.len() != expected_size {
        return Err(AttachmentError::Incomplete);
    }
    // A whole number of chunks, so every non-final response is joinable.
    let take = (CHUNK_BYTES as u64).min(expected_size - offset);
    let mut file = std::fs::File::open(&path).map_err(|_| AttachmentError::Incomplete)?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|_| AttachmentError::Incomplete)?;
    let mut bytes = vec![0u8; take as usize];
    let read = file
        .read(&mut bytes)
        .map_err(|_| AttachmentError::Incomplete)?;
    bytes.truncate(read);
    Ok(json!({
        "data": base64_encode(&bytes),
        "offset": offset + read as u64,
        "size": expected_size,
    }))
}

/// Finds an image attachment by id among the blocks of one session.
pub fn image_in_session(sessions: &crate::session::HostSession, id: &str) -> Option<u64> {
    let blocks = sessions.session.get("blocks")?.as_array()?;
    blocks
        .iter()
        .flat_map(|block| block.get("attachments").and_then(Value::as_array))
        .flatten()
        .find(|file| file.get("id").and_then(Value::as_str) == Some(id))
        .filter(|file| file.get("kind").and_then(Value::as_str) == Some("image"))
        .and_then(|file| file.get("size").and_then(Value::as_u64))
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let value = |byte: u8| -> Option<u32> {
        Some(match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let bytes = text.as_bytes();
    // Padding is only ever at the end, and at most two of them.
    let padding = bytes.iter().rev().take_while(|b| **b == b'=').count();
    if padding > 2 {
        return None;
    }
    let body = &bytes[..bytes.len() - padding];
    let mut out = Vec::with_capacity(body.len() / 4 * 3);
    for group in body.chunks(4) {
        let mut n = 0u32;
        for (index, byte) in group.iter().enumerate() {
            n |= value(*byte)? << (18 - 6 * index);
        }
        match group.len() {
            4 => {
                out.push((n >> 16) as u8);
                out.push((n >> 8) as u8);
                out.push(n as u8);
            }
            3 => {
                out.push((n >> 16) as u8);
                out.push((n >> 8) as u8);
            }
            2 => out.push((n >> 16) as u8),
            _ => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "123e4567-e89b-42d3-a456-426614174000";

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn base64_round_trips() {
        for length in 0..40usize {
            let bytes: Vec<u8> = (0..length).map(|i| (i * 7) as u8).collect();
            let encoded = base64_encode(&bytes);
            assert_eq!(base64_decode(&encoded).unwrap(), bytes, "length {length}");
        }
    }

    #[test]
    fn an_id_must_look_like_a_uuid() {
        assert!(valid_id(ID));
        for bad in ["", "../escape", "not-a-uuid", "123e4567e89b42d3a456426614174000"] {
            assert!(!valid_id(bad), "{bad} should be refused");
        }
    }

    #[test]
    fn chunks_are_written_in_order_and_reassembled() {
        let dir = dir();
        let first = base64_encode(b"hello ");
        let second = base64_encode(b"world");
        let size = 11;
        assert_eq!(write_chunk(dir.path(), ID, 0, size, &first).unwrap(), 6);
        assert_eq!(write_chunk(dir.path(), ID, 6, size, &second).unwrap(), 11);
        assert_eq!(
            std::fs::read(dir.path().join(ID)).unwrap(),
            b"hello world"
        );
    }

    #[test]
    fn a_retry_at_the_same_offset_is_accepted() {
        let dir = dir();
        let chunk = base64_encode(b"abcdef");
        write_chunk(dir.path(), ID, 0, 6, &chunk).unwrap();
        // The response was lost, so the client sends it again.
        assert_eq!(write_chunk(dir.path(), ID, 0, 6, &chunk).unwrap(), 6);
    }

    #[test]
    fn a_retry_with_different_bytes_is_refused() {
        let dir = dir();
        write_chunk(dir.path(), ID, 0, 6, &base64_encode(b"abcdef")).unwrap();
        assert_eq!(
            write_chunk(dir.path(), ID, 0, 6, &base64_encode(b"zzzzzz")).unwrap_err(),
            AttachmentError::Mismatch
        );
    }

    #[test]
    fn a_chunk_past_the_end_is_refused() {
        let dir = dir();
        // The file is two bytes but the chunk starts at 100.
        write_chunk(dir.path(), ID, 0, 2, &base64_encode(b"ab")).unwrap();
        assert_eq!(
            write_chunk(dir.path(), ID, 100, 200, &base64_encode(b"c")).unwrap_err(),
            AttachmentError::OutOfOrder
        );
    }

    #[test]
    fn a_chunk_larger_than_the_declared_size_is_refused() {
        let dir = dir();
        assert_eq!(
            write_chunk(dir.path(), ID, 0, 2, &base64_encode(b"abcdef")).unwrap_err(),
            AttachmentError::TooLarge
        );
    }

    #[test]
    fn a_read_returns_the_chunk_and_the_next_offset() {
        let dir = dir();
        write_chunk(dir.path(), ID, 0, 11, &base64_encode(b"hello world")).unwrap();
        let first = read_chunk(dir.path(), ID, 11, 0).unwrap();
        assert_eq!(first["offset"], 11);
        assert_eq!(first["size"], 11);
        assert_eq!(base64_decode(first["data"].as_str().unwrap()).unwrap(), b"hello world");
    }

    #[test]
    fn a_read_before_the_upload_finishes_is_refused() {
        let dir = dir();
        write_chunk(dir.path(), ID, 0, 4, &base64_encode(b"ab")).unwrap();
        // The session claims 4 bytes but only 2 arrived.
        assert_eq!(
            read_chunk(dir.path(), ID, 4, 0).unwrap_err(),
            AttachmentError::Incomplete
        );
    }

    #[test]
    fn a_read_past_the_end_is_refused() {
        let dir = dir();
        write_chunk(dir.path(), ID, 0, 4, &base64_encode(b"abcd")).unwrap();
        assert_eq!(
            read_chunk(dir.path(), ID, 4, 99).unwrap_err(),
            AttachmentError::InvalidChunk
        );
    }

    #[test]
    fn an_oversized_attachment_is_refused() {
        let dir = dir();
        assert_eq!(
            write_chunk(dir.path(), ID, 0, MAX_BYTES + 1, "").unwrap_err(),
            AttachmentError::InvalidChunk
        );
    }

    #[test]
    fn an_id_that_is_not_a_uuid_cannot_reach_the_filesystem() {
        let dir = dir();
        assert_eq!(
            write_chunk(dir.path(), "../../escape", 0, 1, &base64_encode(b"x")).unwrap_err(),
            AttachmentError::InvalidId
        );
        assert!(!Path::new(&dir.path().join("..").join("escape")).exists());
    }

    #[test]
    fn only_images_in_the_session_are_findable() {
        let session = crate::HostSessionFixture::new("s1")
            .with_blocks(vec![serde_json::json!({
                "id": "b1",
                "attachments": [
                    { "id": ID, "kind": "image", "size": 42 },
                    { "id": "123e4567-e89b-42d3-a456-426614174001", "kind": "file", "size": 7 }
                ]
            })])
            .build();
        assert_eq!(image_in_session(&session, ID), Some(42));
        // Not an image.
        assert_eq!(image_in_session(&session, "123e4567-e89b-42d3-a456-426614174001"), None);
        // Not in this session.
        assert_eq!(image_in_session(&session, "123e4567-e89b-42d3-a456-426614174002"), None);
    }
}

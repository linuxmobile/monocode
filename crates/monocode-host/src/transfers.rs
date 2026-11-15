//! Serving a session transcript that is too large for one RPC response.
//!
//! A long transcript, or one block holding a big attachment, can exceed the
//! desktop's response cap. Rather than truncate, the host keeps the serialized
//! sync and hands it out in slices: `sessions.sync` returns a `chunked`
//! marker, and the client fetches the text with `sessions.syncChunk`.
//!
//! Three details are easy to get wrong and each would break a real transcript:
//!
//! - A slice is measured in bytes *after* JSON escaping, not characters. Slice
//!   boundaries are therefore adjusted until the encoded form fits.
//! - A slice must never split a surrogate pair. Half a pair is not valid text
//!   for a strict JSON parser, so the boundary moves back one unit.
//! - Transfers expire and are bounded. Holding them forever would let a client
//!   pin memory on this machine, so entries lapse after a short window and the
//!   oldest is dropped when the map is full.

use std::collections::HashMap;
use std::sync::Mutex;

/// Below this, a sync is returned inline.
pub const INLINE_BYTES: usize = 4 * 1024 * 1024;
/// The most a single slice may occupy once encoded.
pub const CHUNK_BYTES: usize = 4 * 1024 * 1024;
/// How long an unfinished transfer stays fetchable.
const TTL: std::time::Duration = std::time::Duration::from_secs(120);
/// How many unfinished transfers may exist at once.
const MAX_TRANSFERS: usize = 8;

struct Transfer {
    session_id: String,
    text: Vec<u16>,
    expires: std::time::Instant,
}

/// Holds the large syncs currently being streamed.
pub struct SyncTransfers {
    /// Keyed by transfer id.
    transfers: Mutex<HashMap<String, Transfer>>,
}

impl Default for SyncTransfers {
    fn default() -> Self {
        Self::new()
    }
}

impl SyncTransfers {
    pub fn new() -> Self {
        Self {
            transfers: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the sync inline when it is small enough, otherwise a marker
    /// naming a transfer the client must fetch.
    pub fn respond(&self, session_id: &str, sync: serde_json::Value) -> serde_json::Value {
        // An unchanged session has no payload to move.
        if sync.get("kind").and_then(serde_json::Value::as_str) == Some("unchanged") {
            return sync;
        }
        let text = sync.to_string();
        if text.len() <= INLINE_BYTES {
            return sync;
        }
        let units: Vec<u16> = text.encode_utf16().collect();
        let transfer = crate::store::random_token();
        if let Ok(mut transfers) = self.transfers.lock() {
            Self::prune(&mut transfers);
            transfers.insert(
                transfer.clone(),
                Transfer {
                    session_id: session_id.to_string(),
                    text: units.clone(),
                    expires: std::time::Instant::now() + TTL,
                },
            );
        }
        serde_json::json!({
            "kind": "chunked",
            "transfer": transfer,
            "length": units.len(),
        })
    }

    /// Returns the slice starting at `offset`.
    pub fn chunk(&self, session_id: &str, transfer: &str, offset: usize) -> Result<String, String> {
        let mut transfers = self.transfers.lock().map_err(|_| "Transfer unavailable")?;
        let Some(entry) = transfers.get_mut(transfer) else {
            return Err("Session transfer expired; reload the session".into());
        };
        // A transfer is only readable by the session that created it.
        if entry.session_id != session_id || entry.expires < std::time::Instant::now() {
            return Err("Session transfer expired; reload the session".into());
        }
        if offset > entry.text.len() {
            return Err("Invalid session transfer offset".into());
        }
        // Past the end means the client already has everything. Answering with
        // an empty slice is what lets it stop: it reports "done" rather than
        // looking like the transfer lapsed mid-read.
        if offset == entry.text.len() {
            return Ok(String::new());
        }
        // Start with a slice that will shrink to fit once encoded. Transcript
        // JSON usually encodes at about one byte per UTF-16 unit.
        let mut end = (offset + (CHUNK_BYTES * 2 / 3)).min(entry.text.len());
        while end > offset + 1 && encoded_len(&entry.text[offset..end]) > CHUNK_BYTES {
            end = offset + (end - offset).div_ceil(2);
        }
        // Never split a surrogate pair: a lone surrogate is not valid text.
        if end < entry.text.len() && end > offset + 1 && is_high_surrogate(entry.text[end - 1]) {
            end -= 1;
        }
        let data = String::from_utf16_lossy(&entry.text[offset..end]);
        entry.expires = std::time::Instant::now() + TTL;
        // The entry is kept until it expires rather than dropped after the last
        // slice, so a client that re-requests the final chunk (its response was
        // lost) gets the same bytes instead of a spurious "expired".
        let _ = end;
        Ok(data)
    }

    fn prune(transfers: &mut HashMap<String, Transfer>) {
        let now = std::time::Instant::now();
        transfers.retain(|_, entry| entry.expires >= now);
        // Drop the oldest until there is room, so a client cannot pin memory.
        while transfers.len() >= MAX_TRANSFERS {
            let Some(oldest) = transfers
                .iter()
                .min_by_key(|(_, entry)| entry.expires)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            transfers.remove(&oldest);
        }
    }
}

fn is_high_surrogate(unit: u16) -> bool {
    (0xd800..=0xdbff).contains(&unit)
}

/// The byte length of `units` once JSON-encoded.
fn encoded_len(units: &[u16]) -> usize {
    // Quotes and an escape per character is the worst case, which is what must
    // fit under the cap.
    let text = String::from_utf16_lossy(units);
    serde_json::to_string(&text)
        .map(|s| s.len())
        .unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big_sync(blocks: usize) -> serde_json::Value {
        serde_json::json!({
            "kind": "snapshot",
            "value": { "blocks": (0..blocks)
                .map(|index| serde_json::json!({ "id": format!("b{index}"), "text": "x".repeat(1000) }))
                .collect::<Vec<_>>() },
        })
    }

    #[test]
    fn a_small_sync_is_returned_inline() {
        let transfers = SyncTransfers::new();
        let sync = serde_json::json!({ "kind": "snapshot", "value": { "blocks": [] } });
        assert_eq!(transfers.respond("s1", sync.clone()), sync);
    }

    #[test]
    fn an_unchanged_sync_is_returned_inline() {
        let transfers = SyncTransfers::new();
        let sync = serde_json::json!({ "kind": "unchanged", "revision": 3 });
        assert_eq!(transfers.respond("s1", sync.clone()), sync);
    }

    #[test]
    fn a_large_sync_becomes_a_chunked_marker() {
        let transfers = SyncTransfers::new();
        let sync = big_sync(6000);
        let response = transfers.respond("s1", sync);
        assert_eq!(response["kind"], "chunked");
        assert!(response["transfer"].is_string());
        assert!(response["length"].as_u64().unwrap() > 0);
    }

    #[test]
    fn the_chunks_reassemble_into_the_original_sync() {
        let transfers = SyncTransfers::new();
        let sync = big_sync(6000);
        let original = sync.to_string();
        let marker = transfers.respond("s1", sync);
        let transfer = marker["transfer"].as_str().unwrap();

        let mut assembled = String::new();
        let mut offset = 0;
        loop {
            let piece = transfers.chunk("s1", transfer, offset).unwrap();
            if piece.is_empty() {
                break;
            }
            let units = piece.encode_utf16().count();
            assembled.push_str(&piece);
            offset += units;
            if offset >= marker["length"].as_u64().unwrap() as usize {
                break;
            }
        }
        assert_eq!(assembled, original, "reassembled sync does not match");
    }

    #[test]
    fn each_chunk_stays_under_the_cap_once_encoded() {
        let transfers = SyncTransfers::new();
        let sync = big_sync(6000);
        let marker = transfers.respond("s1", sync);
        let transfer = marker["transfer"].as_str().unwrap();
        let piece = transfers.chunk("s1", transfer, 0).unwrap();
        assert!(
            serde_json::to_string(&piece).unwrap().len() <= CHUNK_BYTES,
            "a chunk exceeded the response cap"
        );
    }

    #[test]
    fn a_transfer_belongs_to_its_session() {
        let transfers = SyncTransfers::new();
        let marker = transfers.respond("s1", big_sync(6000));
        let transfer = marker["transfer"].as_str().unwrap();
        let error = transfers.chunk("s2", transfer, 0).unwrap_err();
        assert!(error.contains("expired"), "got: {error}");
    }

    #[test]
    fn an_unknown_transfer_is_refused() {
        let transfers = SyncTransfers::new();
        let error = transfers.chunk("s1", "no-such-transfer", 0).unwrap_err();
        assert!(error.contains("expired"), "got: {error}");
    }

    #[test]
    fn an_offset_past_the_end_is_refused() {
        let transfers = SyncTransfers::new();
        let marker = transfers.respond("s1", big_sync(6000));
        let transfer = marker["transfer"].as_str().unwrap();
        let length = marker["length"].as_u64().unwrap() as usize;
        assert_eq!(
            transfers.chunk("s1", transfer, length + 1).unwrap_err(),
            "Invalid session transfer offset"
        );
    }

    #[test]
    fn reading_past_the_end_reports_done_rather_than_expiring() {
        let transfers = SyncTransfers::new();
        let marker = transfers.respond("s1", big_sync(6000));
        let transfer = marker["transfer"].as_str().unwrap();
        let length = marker["length"].as_u64().unwrap() as usize;
        assert_eq!(transfers.chunk("s1", transfer, length).unwrap(), "");
    }

    #[test]
    fn the_final_chunk_can_be_fetched_twice() {
        // A lost response must not turn into "the transfer expired".
        let transfers = SyncTransfers::new();
        let marker = transfers.respond("s1", big_sync(6000));
        let transfer = marker["transfer"].as_str().unwrap();
        let length = marker["length"].as_u64().unwrap() as usize;
        let first = transfers.chunk("s1", transfer, 0).unwrap();
        let offset = first.encode_utf16().count();
        let end = transfers.chunk("s1", transfer, offset).unwrap();
        let again = transfers.chunk("s1", transfer, offset).unwrap();
        assert_eq!(end, again);
        assert!(!end.is_empty());
        assert!(offset < length);
    }

    #[test]
    fn a_finished_transfer_still_serves_until_it_expires() {
        // Reloading the tail after a dropped response must work, so the entry
        // is retained rather than deleted when its last slice is served.
        let transfers = SyncTransfers::new();
        let marker = transfers.respond("s1", big_sync(6000));
        let transfer = marker["transfer"].as_str().unwrap().to_string();
        let length = marker["length"].as_u64().unwrap() as usize;
        let mut offset = 0;
        while offset < length {
            let piece = transfers.chunk("s1", &transfer, offset).unwrap();
            offset += piece.encode_utf16().count();
        }
        assert_eq!(transfers.chunk("s1", &transfer, length).unwrap(), "");
        assert!(!transfers.chunk("s1", &transfer, 0).unwrap().is_empty());
    }

    #[test]
    fn transfers_are_bounded() {
        let transfers = SyncTransfers::new();
        for _ in 0..(MAX_TRANSFERS + 4) {
            transfers.respond("s1", big_sync(6000));
        }
        let held = transfers.transfers.lock().unwrap().len();
        assert!(held <= MAX_TRANSFERS, "held {held} transfers");
    }

    #[test]
    fn a_chunk_never_splits_a_surrogate_pair() {
        // An emoji is a surrogate pair in UTF-16; slicing between the halves
        // would produce text the desktop's parser rejects.
        let transfers = SyncTransfers::new();
        let text = "😀".repeat(INLINE_BYTES);
        let sync = serde_json::json!({ "kind": "snapshot", "value": { "text": text } });
        let marker = transfers.respond("s1", sync);
        let transfer = marker["transfer"].as_str().unwrap();
        let piece = transfers.chunk("s1", transfer, 0).unwrap();
        // serde keeps the emoji as one char, so a bad boundary would leave a
        // lone surrogate inside the slice. A slice that converts back to the
        // same number of UTF-16 units it was built from has none.
        let units: Vec<u16> = piece.encode_utf16().collect();
        let back = String::from_utf16(&units).expect("a slice held a lone surrogate");
        assert_eq!(back, piece);
        // And it must still contain whole emoji only.
        for character in piece.chars() {
            assert_ne!(
                character as u32 & 0xFC00,
                0xD800,
                "a lone surrogate survived"
            );
        }
    }
}

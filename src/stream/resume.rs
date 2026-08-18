//! Persistent resume checkpoints for interrupted transfers.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::stream::StreamError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeCheckpoint {
    pub file_name: String,
    pub file_size: u64,
    pub chunk_size: usize,
    pub next_sequence: u32,
    pub bytes_written: u64,
}

impl ResumeCheckpoint {
    pub fn new(file_name: impl Into<String>, file_size: u64, chunk_size: usize) -> Self {
        Self {
            file_name: file_name.into(),
            file_size,
            chunk_size,
            next_sequence: 0,
            bytes_written: 0,
        }
    }

    pub fn matches(&self, file_name: &str, file_size: u64, chunk_size: usize) -> bool {
        self.file_name == file_name && self.file_size == file_size && self.chunk_size == chunk_size
    }

    pub fn update(&mut self, next_sequence: u32, bytes_written: u64) {
        self.next_sequence = next_sequence;
        self.bytes_written = bytes_written;
    }

    pub fn load(path: &Path) -> Result<Self, StreamError> {
        let json = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&json)?)
    }

    pub fn save(&self, path: &Path) -> Result<(), StreamError> {
        let json = serde_json::to_string_pretty(self)?;
        // Never truncate the live checkpoint in place. A process interruption,
        // antivirus scan, or concurrent reader must leave either the previous
        // complete checkpoint or the new complete checkpoint available.
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let tmp = path.with_extension(format!("checkpoint.{nonce}.tmp"));
        {
            let mut file = fs::File::create(&tmp)?;
            file.write_all(json.as_bytes())?;
            file.flush()?;
            file.sync_data()?;
        }

        // Windows cannot rename over an existing file. Removing the old file
        // only after the temporary file is durable still avoids exposing a
        // partially-written JSON checkpoint to readers.
        if path.exists() {
            fs::remove_file(path)?;
        }
        if let Err(error) = fs::rename(&tmp, path) {
            let _ = fs::remove_file(&tmp);
            return Err(error.into());
        }
        Ok(())
    }

    pub fn clear(path: &Path) -> Result<(), StreamError> {
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

//! Incremental base64 transport decoding and bounded remote-output projection.

use std::{collections::VecDeque, path::PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use parking_lot::Mutex;
use seekdeep_subprocess::{SubprocessOutputRead, SubprocessOutputReader};

/// Reserved non-base64 frame proving that one remote encoder reached clean EOF.
pub const E2B_OUTPUT_COMPLETE_FRAME: &str = "!seekdeep-e2b-output-complete!";

/// Incremental decoder for newline-delimited base64 frames from E2B callbacks.
#[derive(Debug, Default)]
pub struct E2bBase64Decoder {
    pending: String,
    complete: bool,
}

impl E2bBase64Decoder {
    /// Decodes every complete frame in one arbitrarily split callback.
    ///
    /// # Errors
    ///
    /// Rejects malformed, non-canonical, duplicate, or post-completion frames.
    pub fn push(&mut self, text: &str) -> anyhow::Result<Vec<u8>> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        self.pending.push_str(text);
        let mut decoded = Vec::new();
        while let Some(boundary) = self.pending.find('\n') {
            let frame = self.pending[..boundary].to_owned();
            self.pending.drain(..=boundary);
            if frame == E2B_OUTPUT_COMPLETE_FRAME {
                anyhow::ensure!(
                    !self.complete,
                    "subprocess-e2b: duplicate output transport completion"
                );
                self.complete = true;
                continue;
            }
            anyhow::ensure!(
                !self.complete,
                "subprocess-e2b: output transport continued after completion"
            );
            anyhow::ensure!(
                !frame.is_empty(),
                "subprocess-e2b: invalid base64 output transport"
            );
            let bytes = STANDARD
                .decode(&frame)
                .map_err(|_| anyhow::anyhow!("subprocess-e2b: invalid base64 output transport"))?;
            anyhow::ensure!(
                STANDARD.encode(&bytes) == frame,
                "subprocess-e2b: invalid base64 output transport"
            );
            decoded.extend(bytes);
        }
        Ok(decoded)
    }

    /// Validates clean completion or discards a termination-interrupted trailing frame.
    ///
    /// # Errors
    ///
    /// Natural completion rejects a truncated frame or missing completion marker.
    pub fn finish(&mut self, require_complete: bool) -> anyhow::Result<()> {
        if !require_complete {
            self.pending.clear();
            return Ok(());
        }
        anyhow::ensure!(
            self.pending.is_empty(),
            "subprocess-e2b: truncated base64 output transport"
        );
        anyhow::ensure!(self.complete, "subprocess-e2b: incomplete output transport");
        Ok(())
    }
}

#[derive(Debug, Default)]
struct OutputState {
    chunks: VecDeque<Vec<u8>>,
    retained_bytes: usize,
    total_bytes: u64,
    spill_valid: bool,
}

/// Offset reader retaining an exact in-memory tail and advertising a remote spill.
#[derive(Debug)]
pub struct E2bOutputReader {
    max_bytes: usize,
    max_spill_bytes: Option<u64>,
    spill_path: PathBuf,
    state: Mutex<OutputState>,
}

impl E2bOutputReader {
    /// Creates a bounded reader over one remote spill path.
    #[must_use]
    pub fn new(
        max_bytes: usize,
        max_spill_bytes: Option<u64>,
        spill_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            max_bytes,
            max_spill_bytes,
            spill_path: spill_path.into(),
            state: Mutex::new(OutputState {
                spill_valid: true,
                ..OutputState::default()
            }),
        }
    }

    /// Total bytes observed from the SDK stream.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.state.lock().total_bytes
    }

    /// Stops advertising a remote spill whose writer did not reach clean EOF.
    pub fn invalidate_spill(&self) {
        self.state.lock().spill_valid = false;
    }

    /// Appends one byte-faithful decoded transport event.
    ///
    /// # Errors
    ///
    /// Returns when the whole-stream byte counter overflows.
    pub fn push(&self, bytes: &[u8]) -> anyhow::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let mut state = self.state.lock();
        state.total_bytes = state
            .total_bytes
            .checked_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
            .ok_or_else(|| anyhow::anyhow!("subprocess-e2b: output byte count overflow"))?;
        state.chunks.push_back(bytes.to_vec());
        state.retained_bytes = state.retained_bytes.saturating_add(bytes.len());
        while state.retained_bytes > self.max_bytes {
            let excess = state.retained_bytes - self.max_bytes;
            let Some(head) = state.chunks.pop_front() else {
                break;
            };
            if head.len() <= excess {
                state.retained_bytes -= head.len();
            } else {
                state.chunks.push_front(head[excess..].to_vec());
                state.retained_bytes -= excess;
            }
        }
        Ok(())
    }

    fn read(&self, from_byte: u64) -> SubprocessOutputRead {
        let state = self.state.lock();
        let retained = u64::try_from(state.retained_bytes).unwrap_or(u64::MAX);
        let first_retained = state.total_bytes.saturating_sub(retained);
        let lossy = from_byte < first_retained;
        let bytes = state
            .chunks
            .iter()
            .flat_map(|chunk| chunk.iter().copied())
            .collect::<Vec<_>>();
        let start = if lossy {
            0
        } else {
            usize::try_from(from_byte.saturating_sub(first_retained))
                .unwrap_or(usize::MAX)
                .min(bytes.len())
        };
        let spill_path = (lossy
            && state.spill_valid
            && self
                .max_spill_bytes
                .is_some_and(|maximum| state.total_bytes <= maximum))
        .then(|| self.spill_path.clone());
        SubprocessOutputRead {
            text: String::from_utf8_lossy(&bytes[start..]).into_owned(),
            next_offset: state.total_bytes,
            lossy,
            spill_path,
        }
    }
}

impl SubprocessOutputReader for E2bOutputReader {
    fn read_from(&self, from_byte: u64) -> SubprocessOutputRead {
        self.read(from_byte)
    }
}

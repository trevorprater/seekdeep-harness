//! Bounded tail collection with optional complete-stream spill recovery.

use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use parking_lot::Mutex;
use seekdeep_subprocess::{CollectedOutput, SubprocessOutputRead, SubprocessOutputReader};
use uuid::Uuid;

static DEFAULT_SPILL_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
static SPILL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Returns the lazily created, owner-private default spill directory.
pub(crate) fn default_spill_dir() -> io::Result<PathBuf> {
    let mut current = DEFAULT_SPILL_DIR.lock();
    if let Some(path) = current.as_ref() {
        return Ok(path.clone());
    }
    let directory = tempfile::Builder::new()
        .prefix("seekdeep-subprocess-")
        .tempdir_in(std::env::temp_dir())?;
    let path = directory.keep();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    }
    *current = Some(path.clone());
    Ok(path)
}

trait SpillWriter: Write + std::fmt::Debug + Send {
    fn seal(&mut self) -> io::Result<()>;
}

#[derive(Debug)]
struct HostSpillWriter(File);

impl Write for HostSpillWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl SpillWriter for HostSpillWriter {
    fn seal(&mut self) -> io::Result<()> {
        self.flush()
    }
}

trait SpillIo: std::fmt::Debug + Send + Sync {
    fn create_exclusive(&self, path: &std::path::Path) -> io::Result<Box<dyn SpillWriter>>;
    fn remove(&self, path: &std::path::Path) -> io::Result<()>;
}

#[derive(Debug)]
struct HostSpillIo;

impl SpillIo for HostSpillIo {
    fn create_exclusive(&self, path: &std::path::Path) -> io::Result<Box<dyn SpillWriter>> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        options
            .open(path)
            .map(HostSpillWriter)
            .map(|file| Box::new(file) as Box<dyn SpillWriter>)
    }

    fn remove(&self, path: &std::path::Path) -> io::Result<()> {
        fs::remove_file(path)
    }
}

#[derive(Debug)]
struct CollectorState {
    chunks: VecDeque<Vec<u8>>,
    retained_bytes: usize,
    dropped: bool,
    spill_file: Option<Box<dyn SpillWriter>>,
    spill_path: Option<PathBuf>,
    spill_disabled: bool,
    total_bytes: u64,
}

/// Thread-safe byte collector retaining an exact tail and optional full spill.
#[derive(Debug)]
pub struct OutputCollector {
    max_bytes: usize,
    max_spill_bytes: Option<u64>,
    label: String,
    spill_dir: PathBuf,
    io: Arc<dyn SpillIo>,
    state: Mutex<CollectorState>,
}

impl OutputCollector {
    /// Creates one collector from validated byte limits.
    #[must_use]
    pub fn new(
        max_bytes: usize,
        max_spill_bytes: Option<u64>,
        label: impl Into<String>,
        spill_dir: impl Into<PathBuf>,
    ) -> Self {
        Self::new_with_io(
            max_bytes,
            max_spill_bytes,
            label,
            spill_dir,
            Arc::new(HostSpillIo),
        )
    }

    fn new_with_io(
        max_bytes: usize,
        max_spill_bytes: Option<u64>,
        label: impl Into<String>,
        spill_dir: impl Into<PathBuf>,
        io: Arc<dyn SpillIo>,
    ) -> Self {
        Self {
            max_bytes,
            max_spill_bytes,
            label: label.into(),
            spill_dir: spill_dir.into(),
            io,
            state: Mutex::new(CollectorState {
                chunks: VecDeque::new(),
                retained_bytes: 0,
                dropped: false,
                spill_file: None,
                spill_path: None,
                spill_disabled: max_spill_bytes.is_none(),
                total_bytes: 0,
            }),
        }
    }

    /// Ingests one byte chunk.
    ///
    /// # Errors
    ///
    /// Returns spill-file creation or write failures.
    pub fn push(&self, chunk: &[u8]) -> io::Result<()> {
        let mut state = self.state.lock();
        state.total_bytes = state
            .total_bytes
            .checked_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX))
            .ok_or_else(|| io::Error::other("subprocess output byte count overflow"))?;
        let overflows = state.retained_bytes.saturating_add(chunk.len()) > self.max_bytes;
        if !state.spill_disabled && (overflows || state.spill_file.is_some()) {
            self.spill_all(&mut state, chunk)?;
        }

        state.chunks.push_back(chunk.to_vec());
        state.retained_bytes = state.retained_bytes.saturating_add(chunk.len());
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
            state.dropped = true;
        }
        Ok(())
    }

    fn spill_all(&self, state: &mut CollectorState, chunk: &[u8]) -> io::Result<()> {
        if self
            .max_spill_bytes
            .is_some_and(|maximum| state.total_bytes > maximum)
        {
            self.discard_spill(state);
            return Ok(());
        }
        if state.spill_file.is_none() {
            let (path, mut file) = self.create_spill_file()?;
            for prior in &state.chunks {
                file.write_all(prior)?;
            }
            state.spill_path = Some(path);
            state.spill_file = Some(file);
        }
        if let Some(file) = state.spill_file.as_mut() {
            file.write_all(chunk)?;
        }
        Ok(())
    }

    fn create_spill_file(&self) -> io::Result<(PathBuf, Box<dyn SpillWriter>)> {
        fs::create_dir_all(&self.spill_dir)?;
        for _ in 0..32 {
            let counter = SPILL_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
            let random = Uuid::new_v4().simple().to_string();
            let name = format!(
                "seekdeep-subprocess-{}-{counter}-{}-{}.log",
                std::process::id(),
                &random[..12],
                self.label
            );
            let path = self.spill_dir.join(name);
            match self.io.create_exclusive(&path) {
                Ok(file) => return Ok((path, file)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate an exclusive subprocess spill file",
        ))
    }

    fn discard_spill(&self, state: &mut CollectorState) {
        if let Some(mut file) = state.spill_file.take()
            && file.seal().is_err()
        {
            state.spill_file = Some(file);
        }
        if let Some(path) = state.spill_path.take() {
            let _ = self.io.remove(&path);
        }
        state.spill_disabled = true;
    }

    /// Closes an active spill file while retaining the in-memory tail.
    pub fn seal(&self) {
        let mut state = self.state.lock();
        if let Some(mut file) = state.spill_file.take()
            && file.seal().is_err()
        {
            state.spill_path = None;
        }
    }

    /// Seals the collector and returns its final tail projection.
    #[must_use]
    pub fn finalize(&self) -> CollectedOutput {
        self.seal();
        let state = self.state.lock();
        CollectedOutput {
            text: decode_chunks(&state.chunks),
            truncated: state.dropped,
            spill_path: state.spill_path.clone(),
        }
    }

    fn read(&self, from_byte: u64) -> SubprocessOutputRead {
        let state = self.state.lock();
        let retained = u64::try_from(state.retained_bytes).unwrap_or(u64::MAX);
        let window_start = state.total_bytes.saturating_sub(retained);
        let lossy = from_byte < window_start;
        let bytes = state
            .chunks
            .iter()
            .flat_map(|chunk| chunk.iter().copied())
            .collect::<Vec<_>>();
        let slice = if lossy {
            bytes.as_slice()
        } else {
            let offset = usize::try_from(from_byte.saturating_sub(window_start))
                .unwrap_or(usize::MAX)
                .min(bytes.len());
            &bytes[offset..]
        };
        SubprocessOutputRead {
            text: String::from_utf8_lossy(slice).into_owned(),
            next_offset: state.total_bytes,
            lossy,
            spill_path: state.spill_path.clone(),
        }
    }
}

impl SubprocessOutputReader for OutputCollector {
    fn read_from(&self, from_byte: u64) -> SubprocessOutputRead {
        self.read(from_byte)
    }
}

fn decode_chunks(chunks: &VecDeque<Vec<u8>>) -> String {
    let bytes = chunks
        .iter()
        .flat_map(|chunk| chunk.iter().copied())
        .collect::<Vec<_>>();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Verifies a spill path has the expected owner-only mode on Unix.
#[cfg(test)]
pub(crate) fn mode(path: &std::path::Path) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        0
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;

    #[derive(Debug)]
    struct FaultSpillIo {
        fail_next_seal: Arc<AtomicBool>,
        fail_next_remove: Arc<AtomicBool>,
    }

    impl SpillIo for FaultSpillIo {
        fn create_exclusive(&self, path: &std::path::Path) -> io::Result<Box<dyn SpillWriter>> {
            let file = OpenOptions::new().write(true).create_new(true).open(path)?;
            Ok(Box::new(FaultSpillWriter {
                file,
                fail_next_seal: self.fail_next_seal.clone(),
            }))
        }

        fn remove(&self, path: &std::path::Path) -> io::Result<()> {
            if self.fail_next_remove.swap(false, Ordering::AcqRel) {
                return Err(io::Error::other("simulated EIO on unlink"));
            }
            fs::remove_file(path)
        }
    }

    #[derive(Debug)]
    struct FaultSpillWriter {
        file: File,
        fail_next_seal: Arc<AtomicBool>,
    }

    impl Write for FaultSpillWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.file.write(buffer)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.file.flush()
        }
    }

    impl SpillWriter for FaultSpillWriter {
        fn seal(&mut self) -> io::Result<()> {
            if self.fail_next_seal.swap(false, Ordering::AcqRel) {
                return Err(io::Error::other("simulated EIO on close"));
            }
            self.flush()
        }
    }

    fn fault_collector(
        temp: &tempfile::TempDir,
    ) -> (OutputCollector, Arc<AtomicBool>, Arc<AtomicBool>) {
        let fail_next_seal = Arc::new(AtomicBool::new(false));
        let fail_next_remove = Arc::new(AtomicBool::new(false));
        let collector = OutputCollector::new_with_io(
            4,
            Some(8),
            "fault",
            temp.path(),
            Arc::new(FaultSpillIo {
                fail_next_seal: fail_next_seal.clone(),
                fail_next_remove: fail_next_remove.clone(),
            }),
        );
        (collector, fail_next_seal, fail_next_remove)
    }

    #[test]
    fn exact_tail_is_independent_of_chunk_boundaries() {
        let temp = tempfile::tempdir().unwrap();
        let collector = OutputCollector::new(10, None, "tail", temp.path());
        collector.push(b"aaaa").unwrap();
        collector.push(b"bbbbbb").unwrap();
        collector.push(b"cc").unwrap();
        assert_eq!(collector.finalize().text, "aabbbbbbcc");
        assert!(collector.finalize().truncated);
    }

    #[test]
    fn incremental_offsets_flag_a_slid_window() {
        let temp = tempfile::tempdir().unwrap();
        let collector = OutputCollector::new(10, Some(100), "stdout", temp.path());
        collector.push(b"aaaaa").unwrap();
        let first = collector.read_from(0);
        assert_eq!(first.text, "aaaaa");
        assert!(!first.lossy);
        collector.push(b"bbbbb").unwrap();
        let second = collector.read_from(first.next_offset);
        assert_eq!(second.text, "bbbbb");
        collector.push(&[b'c'; 20]).unwrap();
        let third = collector.read_from(second.next_offset);
        assert_eq!(third.text, "cccccccccc");
        assert!(third.lossy);
        assert!(third.spill_path.is_some());
    }

    #[test]
    fn spill_contains_full_stream_and_is_owner_only() {
        let temp = tempfile::tempdir().unwrap();
        let collector = OutputCollector::new(4, Some(100), "stdout", temp.path());
        collector.push(b"aaaa").unwrap();
        collector.push(b"bbbb").unwrap();
        let output = collector.finalize();
        let path = output.spill_path.unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"aaaabbbb");
        #[cfg(unix)]
        assert_eq!(mode(&path), 0o600);
    }

    #[test]
    fn spill_cap_overflow_removes_incomplete_recovery_file() {
        let temp = tempfile::tempdir().unwrap();
        let collector = OutputCollector::new(4, Some(7), "stdout", temp.path());
        collector.push(b"aaaa").unwrap();
        collector.push(b"bb").unwrap();
        let path = collector.read_from(0).spill_path.unwrap();
        assert!(path.exists());
        collector.push(b"cc").unwrap();
        let output = collector.finalize();
        assert!(output.spill_path.is_none());
        assert!(!path.exists());
        assert_eq!(output.text, "bbcc");
    }

    #[test]
    fn final_close_failure_is_contained_and_invalidates_the_spill() {
        let temp = tempfile::tempdir().unwrap();
        let (collector, fail_next_seal, _) = fault_collector(&temp);
        collector.push(b"aaaa").unwrap();
        collector.push(b"bbbb").unwrap();
        assert!(collector.read_from(0).spill_path.is_some());
        fail_next_seal.store(true, Ordering::Release);
        let output = collector.finalize();
        assert!(!fail_next_seal.load(Ordering::Acquire));
        assert_eq!(output.text, "bbbb");
        assert!(output.truncated);
        assert!(output.spill_path.is_none());
    }

    #[test]
    fn oversize_cleanup_contains_close_and_unlink_failures() {
        let temp = tempfile::tempdir().unwrap();
        let (collector, fail_next_seal, fail_next_remove) = fault_collector(&temp);
        collector.push(b"aaaa").unwrap();
        collector.push(b"bbbb").unwrap();
        let path = collector.read_from(0).spill_path.unwrap();
        fail_next_seal.store(true, Ordering::Release);
        fail_next_remove.store(true, Ordering::Release);
        collector.push(b"c").unwrap();
        assert!(!fail_next_seal.load(Ordering::Acquire));
        assert!(!fail_next_remove.load(Ordering::Acquire));
        assert!(collector.finalize().spill_path.is_none());
        fs::remove_file(path).unwrap();
    }
}

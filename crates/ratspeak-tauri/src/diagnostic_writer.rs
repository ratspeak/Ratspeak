//! Bounded desktop file logging for opt-in process diagnostics.
//!
//! The writer deliberately knows only five exact filenames. It never scans,
//! globs, or recursively mutates the log directory, and refuses special files
//! before opening or rotating any target.

use std::borrow::Cow;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, TryLockError};
use std::thread::{self, JoinHandle};

pub const ACTIVE_LOG_NAME: &str = "ratspeak.log";
pub const ARCHIVE_COUNT: usize = 4;
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_RECORD_BYTES: usize = 16 * 1024;
pub const WRITER_QUEUE_RECORDS: usize = 2_048;

const TRUNCATION_MARKER: &[u8] = b" [truncated]\n";
#[cfg(target_os = "windows")]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
#[cfg(target_os = "windows")]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

#[derive(Clone, Copy, Debug)]
struct WriterLimits {
    archive_count: usize,
    max_file_bytes: u64,
    max_record_bytes: usize,
}

impl WriterLimits {
    const PRODUCTION: Self = Self {
        archive_count: ARCHIVE_COUNT,
        max_file_bytes: MAX_FILE_BYTES,
        max_record_bytes: MAX_RECORD_BYTES,
    };

    fn validate(self) -> io::Result<Self> {
        if self.archive_count == 0
            || self.max_record_bytes < TRUNCATION_MARKER.len()
            || self.max_record_bytes as u64 > self.max_file_bytes
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid bounded diagnostic writer limits",
            ));
        }
        Ok(self)
    }
}

#[derive(Clone, Default)]
pub struct DroppedLogLines(Arc<AtomicUsize>);

impl DroppedLogLines {
    pub fn get(&self) -> usize {
        self.0.load(Ordering::Acquire)
    }

    fn increment(&self) {
        let _ = self
            .0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                (value != usize::MAX).then(|| value + 1)
            });
    }
}

enum WorkerMessage {
    Record(Vec<u8>),
    Shutdown(mpsc::Sender<io::Result<()>>),
}

struct QueueState {
    sender: SyncSender<WorkerMessage>,
    accepting: bool,
}

/// Cloneable producer handed to the tracing formatter. Each call to
/// `record_writer` creates one bounded per-event buffer.
#[derive(Clone)]
pub struct DiagnosticMakeWriter {
    queue: Arc<Mutex<QueueState>>,
    dropped: DroppedLogLines,
    max_record_bytes: usize,
}

impl DiagnosticMakeWriter {
    pub fn record_writer(&self) -> DiagnosticRecordWriter {
        DiagnosticRecordWriter {
            producer: self.clone(),
            bytes: Vec::with_capacity(self.max_record_bytes.min(1_024)),
            truncated: false,
            finished: false,
        }
    }

    fn enqueue(&self, record: Vec<u8>) {
        let queue = match self.queue.try_lock() {
            Ok(queue) => queue,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                self.dropped.increment();
                return;
            }
        };
        if !queue.accepting {
            self.dropped.increment();
            return;
        }
        match queue.sender.try_send(WorkerMessage::Record(record)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.dropped.increment();
            }
        }
    }
}

pub struct DiagnosticRecordWriter {
    producer: DiagnosticMakeWriter,
    bytes: Vec<u8>,
    truncated: bool,
    finished: bool,
}

impl DiagnosticRecordWriter {
    fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        if !self.bytes.is_empty() {
            self.producer.enqueue(std::mem::take(&mut self.bytes));
        }
    }

    fn mark_truncated(&mut self) {
        let payload_limit = self
            .producer
            .max_record_bytes
            .saturating_sub(TRUNCATION_MARKER.len());
        let mut cut = self.bytes.len().min(payload_limit);
        while cut > 0 && std::str::from_utf8(&self.bytes[..cut]).is_err() {
            cut -= 1;
        }
        self.bytes.truncate(cut);
        self.bytes.extend_from_slice(TRUNCATION_MARKER);
        self.truncated = true;
    }
}

impl Write for DiagnosticRecordWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.finished || self.truncated {
            return Ok(bytes.len());
        }
        let remaining = self
            .producer
            .max_record_bytes
            .saturating_sub(self.bytes.len());
        if bytes.len() <= remaining {
            self.bytes.extend_from_slice(bytes);
        } else {
            self.bytes.extend_from_slice(&bytes[..remaining]);
            self.mark_truncated();
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.finish();
        Ok(())
    }
}

impl Drop for DiagnosticRecordWriter {
    fn drop(&mut self) {
        self.finish();
    }
}

/// Process-lifetime file logger. `shutdown` is the normal path; `Drop` is a
/// fallback that uses the same FIFO drain, flush, acknowledgement, and join.
pub struct DiagnosticFileRuntime {
    writer: DiagnosticMakeWriter,
    worker: Option<JoinHandle<()>>,
}

impl DiagnosticFileRuntime {
    pub fn start(log_dir: &Path) -> io::Result<Self> {
        let writer = BoundedLogWriter::open(log_dir, WriterLimits::PRODUCTION)?;
        start_worker(writer, WRITER_QUEUE_RECORDS, MAX_RECORD_BYTES)
    }

    pub fn make_writer(&self) -> DiagnosticMakeWriter {
        self.writer.clone()
    }

    pub fn dropped_counter(&self) -> DroppedLogLines {
        self.writer.dropped.clone()
    }

    pub fn shutdown(mut self) -> io::Result<()> {
        self.shutdown_inner()
    }

    fn shutdown_inner(&mut self) -> io::Result<()> {
        let ack = {
            let mut queue = self
                .writer
                .queue
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !queue.accepting {
                None
            } else {
                queue.accepting = false;
                let (ack_tx, ack_rx) = mpsc::channel();
                queue
                    .sender
                    .send(WorkerMessage::Shutdown(ack_tx))
                    .map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "diagnostic worker stopped before shutdown",
                        )
                    })?;
                Some(ack_rx)
            }
        };

        let flush_result = match ack {
            Some(receiver) => receiver.recv().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "diagnostic worker stopped without acknowledging shutdown",
                )
            })?,
            None => Ok(()),
        };
        let join_result = self.worker.take().map_or(Ok(()), |worker| {
            worker
                .join()
                .map_err(|_| io::Error::other("diagnostic writer worker panicked during shutdown"))
        });
        flush_result.and(join_result)
    }
}

impl Drop for DiagnosticFileRuntime {
    fn drop(&mut self) {
        let _ = self.shutdown_inner();
    }
}

fn start_worker<W>(
    writer: W,
    queue_records: usize,
    max_record_bytes: usize,
) -> io::Result<DiagnosticFileRuntime>
where
    W: Write + Send + 'static,
{
    if queue_records == 0 || max_record_bytes < TRUNCATION_MARKER.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid diagnostic worker limits",
        ));
    }
    let (sender, receiver) = mpsc::sync_channel(queue_records);
    let dropped = DroppedLogLines::default();
    let worker_dropped = dropped.clone();
    let worker = thread::Builder::new()
        .name("ratspeak-diagnostic-writer".to_string())
        .spawn(move || worker_loop(receiver, writer, worker_dropped))?;
    let queue = Arc::new(Mutex::new(QueueState {
        sender,
        accepting: true,
    }));
    Ok(DiagnosticFileRuntime {
        writer: DiagnosticMakeWriter {
            queue,
            dropped,
            max_record_bytes,
        },
        worker: Some(worker),
    })
}

fn worker_loop<W>(receiver: Receiver<WorkerMessage>, mut writer: W, dropped: DroppedLogLines)
where
    W: Write,
{
    while let Ok(message) = receiver.recv() {
        match message {
            WorkerMessage::Record(record) => {
                if writer.write_all(&record).is_err() {
                    dropped.increment();
                }
            }
            WorkerMessage::Shutdown(acknowledge) => {
                let result = writer.flush();
                if result.is_err() {
                    dropped.increment();
                }
                let _ = acknowledge.send(result);
                return;
            }
        }
    }
    let _ = writer.flush();
}

struct BoundedLogWriter {
    log_dir: PathBuf,
    active: Option<File>,
    active_len: u64,
    limits: WriterLimits,
}

impl BoundedLogWriter {
    fn open(log_dir: &Path, limits: WriterLimits) -> io::Result<Self> {
        let limits = limits.validate()?;
        prepare_log_directory(log_dir)?;

        // Validate every exact target before creating or mutating any one of
        // them. Unrelated files are intentionally never enumerated.
        for index in 0..=limits.archive_count {
            validate_regular_target(&log_path(log_dir, index))?;
        }
        for index in 0..=limits.archive_count {
            let path = log_path(log_dir, index);
            repair_oversized_regular_file(&path, limits.max_file_bytes)?;
        }

        let active_path = log_path(log_dir, 0);
        let active = open_verified_regular(&active_path, true, true)?;
        let active_len = active.metadata()?.len();
        Ok(Self {
            log_dir: log_dir.to_path_buf(),
            active: Some(active),
            active_len,
            limits,
        })
    }

    fn rotate(&mut self) -> io::Result<()> {
        // Revalidate all exact paths immediately before mutation. This also
        // catches a special file swapped into an archive slot after startup.
        for index in 0..=self.limits.archive_count {
            validate_regular_target(&log_path(&self.log_dir, index))?;
        }

        if let Some(mut active) = self.active.take() {
            active.flush()?;
            drop(active);
        }

        let rotation_result = (|| {
            let oldest = log_path(&self.log_dir, self.limits.archive_count);
            if validate_regular_target(&oldest)?.is_some() {
                fs::remove_file(&oldest)?;
            }

            for index in (1..self.limits.archive_count).rev() {
                let from = log_path(&self.log_dir, index);
                if validate_regular_target(&from)?.is_some() {
                    fs::rename(&from, log_path(&self.log_dir, index + 1))?;
                }
            }

            let active = log_path(&self.log_dir, 0);
            if validate_regular_target(&active)?.is_some() {
                fs::rename(active, log_path(&self.log_dir, 1))?;
            }
            Ok(())
        })();

        // Always attempt to leave the writer usable, even if a filesystem
        // failure interrupted an archive rename.
        let active_path = log_path(&self.log_dir, 0);
        match open_verified_regular(&active_path, true, true) {
            Ok(active) => {
                self.active_len = active.metadata()?.len();
                self.active = Some(active);
                rotation_result
            }
            Err(reopen_error) => {
                self.active_len = 0;
                self.active = None;
                Err(reopen_error)
            }
        }
    }

    fn truncate_active(&mut self, len: u64) -> io::Result<()> {
        if let Some(mut active) = self.active.take() {
            if let Err(error) = active.flush() {
                self.active = Some(active);
                return Err(error);
            }
            drop(active);
        }

        let active_path = log_path(&self.log_dir, 0);
        let truncate_result = open_verified_regular(&active_path, false, false)
            .and_then(|active| active.set_len(len));

        match open_verified_regular(&active_path, true, true) {
            Ok(active) => {
                let active_len = match active.metadata() {
                    Ok(metadata) => metadata.len(),
                    Err(error) => {
                        self.active = Some(active);
                        return Err(error);
                    }
                };
                self.active_len = active_len;
                self.active = Some(active);
                truncate_result
            }
            Err(reopen_error) => {
                self.active_len = 0;
                self.active = None;
                Err(reopen_error)
            }
        }
    }
}

impl Write for BoundedLogWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let original_len = bytes.len();
        let bounded = bounded_record(bytes, self.limits.max_record_bytes).into_owned();
        let bounded_len = bounded.len() as u64;

        self.active_len = self
            .active
            .as_ref()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "diagnostic log file is unavailable",
                )
            })?
            .metadata()?
            .len();
        if self.active_len > self.limits.max_file_bytes {
            self.truncate_active(self.limits.max_file_bytes)?;
        }

        if self.active_len > 0
            && self.active_len.saturating_add(bounded_len) > self.limits.max_file_bytes
        {
            self.rotate()?;
        }

        let Some(active) = self.active.as_mut() else {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "diagnostic log file is unavailable",
            ));
        };
        if let Err(error) = active.write_all(&bounded) {
            self.active_len = active
                .metadata()
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            return Err(error);
        }
        self.active_len = self.active_len.saturating_add(bounded_len);
        Ok(original_len)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.active.as_mut().map_or(Ok(()), Write::flush)
    }
}

fn bounded_record(bytes: &[u8], max_record_bytes: usize) -> Cow<'_, [u8]> {
    if bytes.len() <= max_record_bytes {
        return Cow::Borrowed(bytes);
    }

    let mut cut = max_record_bytes.saturating_sub(TRUNCATION_MARKER.len());
    if let Ok(text) = std::str::from_utf8(bytes) {
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
    }
    let mut bounded = Vec::with_capacity(max_record_bytes.max(TRUNCATION_MARKER.len()));
    bounded.extend_from_slice(&bytes[..cut]);
    bounded.extend_from_slice(TRUNCATION_MARKER);
    Cow::Owned(bounded)
}

fn log_path(log_dir: &Path, index: usize) -> PathBuf {
    if index == 0 {
        log_dir.join(ACTIVE_LOG_NAME)
    } else {
        log_dir.join(format!("{ACTIVE_LOG_NAME}.{index}"))
    }
}

fn prepare_log_directory(log_dir: &Path) -> io::Result<()> {
    match fs::symlink_metadata(log_dir) {
        Ok(metadata) => validate_directory_metadata(&metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(log_dir)?;
            let metadata = fs::symlink_metadata(log_dir)?;
            validate_directory_metadata(&metadata)
        }
        Err(error) => Err(error),
    }
}

fn validate_directory_metadata(metadata: &Metadata) -> io::Result<()> {
    if metadata.file_type().is_symlink() || metadata_is_reparse_point(metadata) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "diagnostic log directory may not be a link or reparse point",
        ));
    }
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "diagnostic log path is not a directory",
        ));
    }
    Ok(())
}

fn validate_regular_target(path: &Path) -> io::Result<Option<Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || metadata_is_reparse_point(&metadata) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "diagnostic log target may not be a link or reparse point",
                ));
            }
            if !metadata.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "diagnostic log target is not a regular file",
                ));
            }
            Ok(Some(metadata))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn repair_oversized_regular_file(path: &Path, max_file_bytes: u64) -> io::Result<()> {
    let Some(metadata) = validate_regular_target(path)? else {
        return Ok(());
    };
    if metadata.len() <= max_file_bytes {
        return Ok(());
    }
    open_verified_regular(path, false, false)?.set_len(max_file_bytes)
}

fn open_verified_regular(path: &Path, create: bool, append: bool) -> io::Result<File> {
    open_verified_regular_after_validation(path, create, append, || {})
}

fn open_verified_regular_after_validation<F>(
    path: &Path,
    create: bool,
    append: bool,
    after_validation: F,
) -> io::Result<File>
where
    F: FnOnce(),
{
    let observed = validate_regular_target(path)?;
    after_validation();
    let file = match observed {
        Some(_) => open_regular_no_follow(path, append, false),
        None if create => match open_regular_no_follow(path, append, true) {
            Ok(file) => Ok(file),
            // Another process may have won the create race. Reopen without
            // following links, then verify the exact handle below.
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                open_regular_no_follow(path, append, false)
            }
            Err(error) => Err(error),
        },
        None => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "diagnostic log target does not exist",
        )),
    }?;

    let _path_metadata = validate_regular_target(path)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "diagnostic log target disappeared while opening",
        )
    })?;
    let file_metadata = file.metadata()?;
    if !file_metadata.is_file() || metadata_is_reparse_point(&file_metadata) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "opened diagnostic log target is not a regular file",
        ));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if _path_metadata.dev() != file_metadata.dev()
            || _path_metadata.ino() != file_metadata.ino()
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "diagnostic log target changed while opening",
            ));
        }
    }

    Ok(file)
}

fn open_regular_no_follow(path: &Path, append: bool, create_new: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(create_new);
    if append {
        options.append(true);
    } else {
        options.write(true);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }

    options.open(path)
}

#[cfg(target_os = "windows")]
fn metadata_is_reparse_point(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(target_os = "windows"))]
fn metadata_is_reparse_point(_metadata: &Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Condvar};

    use tempfile::tempdir;

    use super::*;

    fn test_limits(max_file_bytes: u64, max_record_bytes: usize) -> WriterLimits {
        WriterLimits {
            archive_count: ARCHIVE_COUNT,
            max_file_bytes,
            max_record_bytes,
        }
    }

    #[test]
    fn rotates_active_file_and_keeps_exact_aggregate_budget() {
        let dir = tempdir().unwrap();
        let mut writer = BoundedLogWriter::open(dir.path(), test_limits(16, 16)).unwrap();

        for digit in b'0'..=b'5' {
            writer.write_all(&[digit; 16]).unwrap();
        }
        writer.flush().unwrap();

        assert_eq!(fs::read(log_path(dir.path(), 0)).unwrap(), vec![b'5'; 16]);
        for archive in 1..=ARCHIVE_COUNT {
            assert_eq!(
                fs::read(log_path(dir.path(), archive)).unwrap(),
                vec![b'5' - archive as u8; 16]
            );
        }
        let aggregate: u64 = (0..=ARCHIVE_COUNT)
            .map(|index| fs::metadata(log_path(dir.path(), index)).unwrap().len())
            .sum();
        assert_eq!(aggregate, 5 * 16);
        assert!(!log_path(dir.path(), ARCHIVE_COUNT + 1).exists());
    }

    #[test]
    fn caps_each_formatted_record_and_preserves_a_line_ending() {
        let dir = tempdir().unwrap();
        let mut writer = BoundedLogWriter::open(dir.path(), test_limits(64, 16)).unwrap();
        let oversized = "🐀".repeat(32);

        assert_eq!(writer.write(oversized.as_bytes()).unwrap(), oversized.len());
        writer.flush().unwrap();

        let bytes = fs::read(log_path(dir.path(), 0)).unwrap();
        assert!(bytes.len() <= 16);
        assert!(bytes.ends_with(TRUNCATION_MARKER));
        assert!(std::str::from_utf8(&bytes).is_ok());
    }

    #[test]
    fn repairs_oversized_exact_targets_at_startup() {
        let dir = tempdir().unwrap();
        fs::write(log_path(dir.path(), 0), vec![b'a'; 50]).unwrap();
        fs::write(log_path(dir.path(), 1), vec![b'b'; 60]).unwrap();

        let writer = BoundedLogWriter::open(dir.path(), test_limits(20, 16)).unwrap();
        assert_eq!(writer.active_len, 20);
        assert_eq!(fs::metadata(log_path(dir.path(), 0)).unwrap().len(), 20);
        assert_eq!(fs::metadata(log_path(dir.path(), 1)).unwrap().len(), 20);
    }

    #[test]
    fn rotation_preserves_unrelated_files() {
        let dir = tempdir().unwrap();
        let unrelated = dir.path().join("keep.me");
        let fifth_archive = log_path(dir.path(), ARCHIVE_COUNT + 1);
        let legacy_daily = dir.path().join("ratspeak.log.2026-07-22");
        fs::write(&unrelated, b"do not touch").unwrap();
        fs::write(&fifth_archive, b"unmanaged archive").unwrap();
        fs::write(&legacy_daily, b"legacy daily log").unwrap();
        let mut writer = BoundedLogWriter::open(dir.path(), test_limits(16, 16)).unwrap();
        writer.write_all(b"1234567890abcdef").unwrap();
        writer.write_all(b"abcdefghijklmnop").unwrap();
        writer.flush().unwrap();

        assert_eq!(fs::read(unrelated).unwrap(), b"do not touch");
        assert_eq!(fs::read(fifth_archive).unwrap(), b"unmanaged archive");
        assert_eq!(fs::read(legacy_daily).unwrap(), b"legacy daily log");
    }

    #[test]
    fn checks_open_handle_length_before_every_record() {
        let dir = tempdir().unwrap();
        let mut writer = BoundedLogWriter::open(dir.path(), test_limits(20, 16)).unwrap();
        writer.write_all(b"first").unwrap();

        let mut external = OpenOptions::new()
            .append(true)
            .open(log_path(dir.path(), 0))
            .unwrap();
        external.write_all(b"-external-growth").unwrap();
        external.flush().unwrap();
        drop(external);

        writer.write_all(b"next").unwrap();
        writer.flush().unwrap();
        assert_eq!(fs::read(log_path(dir.path(), 0)).unwrap(), b"next");
        assert_eq!(
            fs::read(log_path(dir.path(), 1)).unwrap(),
            b"first-external-growt"
        );
    }

    #[test]
    fn refuses_directories_in_exact_log_slots() {
        let dir = tempdir().unwrap();
        fs::create_dir(log_path(dir.path(), 2)).unwrap();

        let error = BoundedLogWriter::open(dir.path(), test_limits(64, 16))
            .err()
            .expect("directory target must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(log_path(dir.path(), 2).is_dir());
        assert!(!log_path(dir.path(), 0).exists());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_targets_without_mutating_their_destination() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let outside = dir.path().join("outside.log");
        fs::write(&outside, b"private").unwrap();
        symlink(&outside, log_path(dir.path(), 0)).unwrap();

        let error = BoundedLogWriter::open(dir.path(), test_limits(64, 16))
            .err()
            .expect("symlink target must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read(outside).unwrap(), b"private");
    }

    #[cfg(unix)]
    #[test]
    fn no_follow_open_blocks_symlink_inserted_after_validation() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let active = log_path(dir.path(), 0);
        let outside = dir.path().join("outside.log");
        fs::write(&active, b"original log").unwrap();
        fs::write(&outside, b"outside").unwrap();

        let error = open_verified_regular_after_validation(&active, false, true, || {
            fs::remove_file(&active).unwrap();
            symlink(&outside, &active).unwrap();
        })
        .expect_err("a symlink inserted between validation and open must be rejected");

        assert_ne!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(fs::read(outside).unwrap(), b"outside");
    }

    #[derive(Default)]
    struct GateState {
        started: bool,
        released: bool,
        bytes: Vec<u8>,
    }

    #[derive(Clone, Default)]
    struct GateWriter(Arc<(Mutex<GateState>, Condvar)>);

    impl GateWriter {
        fn wait_until_started(&self) {
            let (lock, wake) = &*self.0;
            let mut state = lock.lock().unwrap();
            while !state.started {
                state = wake.wait(state).unwrap();
            }
        }

        fn release(&self) {
            let (lock, wake) = &*self.0;
            let mut state = lock.lock().unwrap();
            state.released = true;
            wake.notify_all();
        }
    }

    impl Write for GateWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let (lock, wake) = &*self.0;
            let mut state = lock.lock().unwrap();
            state.started = true;
            wake.notify_all();
            while !state.released {
                state = wake.wait(state).unwrap();
            }
            state.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn saturated_queue_drops_records_without_blocking_producers() {
        let sink = GateWriter::default();
        let observer = sink.clone();
        let runtime = start_worker(sink, 2, 64).unwrap();
        let writer = runtime.make_writer();
        let dropped = runtime.dropped_counter();

        write_record(&writer, b"first\n");
        observer.wait_until_started();
        write_record(&writer, b"second\n");
        write_record(&writer, b"third\n");
        write_record(&writer, b"dropped\n");
        assert_eq!(dropped.get(), 1);

        observer.release();
        drop(writer);
        runtime.shutdown().unwrap();
        let (lock, _) = &*observer.0;
        let state = lock.lock().unwrap();
        assert!(state.bytes.starts_with(b"first\n"));
        assert!(!state.bytes.windows(8).any(|window| window == b"dropped\n"));
    }

    fn write_record(writer: &DiagnosticMakeWriter, bytes: &[u8]) {
        let mut record = writer.record_writer();
        record.write_all(bytes).unwrap();
    }

    #[derive(Clone, Default)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn explicit_shutdown_flushes_queued_records_and_joins() {
        let sink = SharedWriter::default();
        let captured = Arc::clone(&sink.0);
        let runtime = start_worker(sink, 8, 64).unwrap();
        let writer = runtime.make_writer();
        write_record(&writer, b"one\n");
        write_record(&writer, b"two\n");
        drop(writer);
        runtime.shutdown().unwrap();

        assert_eq!(&*captured.lock().unwrap(), b"one\ntwo\n");
    }

    #[test]
    fn runtime_drop_fallback_flushes_queued_records_and_joins() {
        let sink = SharedWriter::default();
        let captured = Arc::clone(&sink.0);
        let runtime = start_worker(sink, 8, 64).unwrap();
        let writer = runtime.make_writer();
        write_record(&writer, b"fallback\n");
        drop(writer);
        drop(runtime);

        assert_eq!(&*captured.lock().unwrap(), b"fallback\n");
    }

    #[test]
    fn record_cap_is_applied_before_the_worker_queue() {
        let sink = SharedWriter::default();
        let captured = Arc::clone(&sink.0);
        let runtime = start_worker(sink, 8, 16).unwrap();
        let writer = runtime.make_writer();
        let dropped = runtime.dropped_counter();
        write_record(&writer, &vec![b'x'; 1_024]);
        drop(writer);
        runtime.shutdown().unwrap();

        let bytes = captured.lock().unwrap();
        assert!(bytes.len() <= 16);
        assert!(bytes.ends_with(TRUNCATION_MARKER));
        assert_eq!(dropped.get(), 0, "record truncation is not a dropped line");
    }

    struct RejectWriter;

    impl Write for RejectWriter {
        fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("injected write failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn worker_io_rejection_increments_the_drop_counter() {
        let runtime = start_worker(RejectWriter, 8, 64).unwrap();
        let writer = runtime.make_writer();
        let dropped = runtime.dropped_counter();
        write_record(&writer, b"rejected\n");
        drop(writer);
        runtime.shutdown().unwrap();

        assert_eq!(dropped.get(), 1);
    }

    #[test]
    fn producer_never_waits_for_internal_queue_lock() {
        let runtime = start_worker(SharedWriter::default(), 8, 64).unwrap();
        let writer = runtime.make_writer();
        let dropped = runtime.dropped_counter();
        let queue_guard = writer.queue.lock().unwrap();

        write_record(&writer, b"contended\n");
        assert_eq!(dropped.get(), 1);

        drop(queue_guard);
        drop(writer);
        runtime.shutdown().unwrap();
    }
}

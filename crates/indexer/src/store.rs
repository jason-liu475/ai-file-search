use std::collections::BTreeMap;
#[cfg(test)]
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ai_file_search_core::PathId;

use crate::writer_lock::publication_prefix;
use crate::{IndexWriterGuard, IndexedFile, ScanOptions};

const INDEX_HEADER: &str = "aifs-index-v1";
const TEMPORARY_CREATE_ATTEMPTS: usize = 32;

#[derive(Clone, Debug, Default)]
pub struct MemoryIndexStore {
    files: BTreeMap<String, IndexedFile>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RefreshSummary {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub unchanged: usize,
}

impl RefreshSummary {
    #[must_use]
    pub fn compare(old_files: &[IndexedFile], new_files: &[IndexedFile]) -> Self {
        let old_by_path = files_by_path(old_files);
        let new_by_path = files_by_path(new_files);

        let mut summary = Self::default();

        for (path, new_file) in &new_by_path {
            match old_by_path.get(path) {
                Some(old_file) if same_file_metadata(old_file, new_file) => {
                    summary.unchanged += 1;
                }
                Some(_) => {
                    summary.updated += 1;
                }
                None => {
                    summary.added += 1;
                }
            }
        }

        for path in old_by_path.keys() {
            if !new_by_path.contains_key(path) {
                summary.removed += 1;
            }
        }

        summary
    }
}

impl MemoryIndexStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert_file(&mut self, file: IndexedFile) {
        self.files
            .insert(file.relative_path.as_normalized().to_owned(), file);
    }

    pub fn replace_all(&mut self, files: Vec<IndexedFile>) {
        self.files.clear();
        for file in files {
            self.upsert_file(file);
        }
    }

    pub fn remove_path(&mut self, path: &PathId) {
        self.files.remove(path.as_normalized());
    }

    #[must_use]
    pub fn all_files(&self) -> Vec<IndexedFile> {
        self.files.values().cloned().collect()
    }

    /// Borrows records in normalized path order without cloning the snapshot.
    #[must_use]
    pub fn iter_files(
        &self,
    ) -> impl DoubleEndedIterator<Item = &IndexedFile> + ExactSizeIterator + '_ {
        self.files.values()
    }

    #[must_use]
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    #[must_use]
    pub fn total_size_bytes(&self) -> u64 {
        self.files.values().map(|file| file.size_bytes).sum()
    }

    #[must_use]
    pub fn search_by_name(&self, query: &str) -> Vec<IndexedFile> {
        let query = query.to_lowercase();

        self.files
            .values()
            .filter(|file| file_name(file).to_lowercase().contains(&query))
            .cloned()
            .collect()
    }
}

fn files_by_path(files: &[IndexedFile]) -> BTreeMap<&str, &IndexedFile> {
    files
        .iter()
        .map(|file| (file.relative_path.as_normalized(), file))
        .collect()
}

fn same_file_metadata(left: &IndexedFile, right: &IndexedFile) -> bool {
    left.size_bytes == right.size_bytes && left.modified_unix_seconds == right.modified_unix_seconds
}

fn file_name(file: &IndexedFile) -> &str {
    file.relative_path
        .as_normalized()
        .rsplit('/')
        .next()
        .unwrap_or_default()
}

/// An immutable, independently cloneable index snapshot.
///
/// Publication requires a [`FileIndexWriter`] borrowing an [`IndexWriterGuard`].
///
/// ```compile_fail
/// use std::path::Path;
/// use ai_file_search_indexer::FileIndexStore;
/// let snapshot = FileIndexStore::new(Path::new("index.txt"));
/// snapshot.save().unwrap();
/// ```
///
/// ```compile_fail
/// use std::path::Path;
/// use ai_file_search_indexer::FileIndexStore;
/// let mut snapshot = FileIndexStore::new(Path::new("index.txt"));
/// snapshot.replace_all(Vec::new());
/// ```
#[derive(Clone, Debug)]
pub struct FileIndexStore {
    path: PathBuf,
    memory: MemoryIndexStore,
    root_path: Option<PathBuf>,
    scan_policy: Option<ScanOptions>,
}

impl FileIndexStore {
    /// Creates an empty destination without reading or modifying an existing index.
    #[must_use]
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_owned(),
            memory: MemoryIndexStore::new(),
            root_path: None,
            scan_policy: None,
        }
    }

    /// Opens an index file, creating an empty in-memory store when the file does
    /// not exist yet.
    ///
    /// # Errors
    ///
    /// Returns an error when the index cannot be read or its scan policy metadata
    /// is malformed or uses an unsupported version.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::new(path)),
            Err(error) => return Err(error),
        };
        Self::from_reader(path, BufReader::new(file))
    }

    fn from_reader(path: &Path, mut reader: impl BufRead) -> io::Result<Self> {
        let mut memory = MemoryIndexStore::new();
        let mut root_path = None;
        let mut policy_seen = false;
        let mut exclusions_seen = false;
        let mut policy_options = ScanOptions::default();
        let mut first_line = true;
        let mut has_header = false;
        let mut line = String::new();

        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            // Match str::lines: strip CR only when it belongs to a CRLF ending.
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') {
                    line.pop();
                }
            }
            if first_line {
                first_line = false;
                has_header = line == INDEX_HEADER;
                if has_header {
                    continue;
                }
            }
            if line.is_empty() {
                continue;
            }
            if has_header {
                let mut parts = line.splitn(3, '\t');
                match (parts.next(), parts.next()) {
                    (Some("meta"), Some("scan_policy")) => {
                        if policy_seen || parts.next() != Some("1") {
                            return Err(invalid_scan_policy());
                        }
                        policy_seen = true;
                        continue;
                    }
                    (Some("meta"), Some("exclude_name")) => {
                        let Some(value) = parts.next() else {
                            return Err(invalid_scan_policy());
                        };
                        exclusions_seen = true;
                        policy_options =
                            policy_options.exclude_name(unescape_metadata_value(value));
                        continue;
                    }
                    _ => {}
                }
                if let Some(root) = parse_root_metadata_record(&line) {
                    root_path = Some(root);
                    continue;
                }
                if is_metadata_record(&line) {
                    continue;
                }
            }
            memory.upsert_file(parse_index_record(&line, has_header));
        }

        if exclusions_seen && !policy_seen {
            return Err(invalid_scan_policy());
        }

        Ok(Self {
            path: path.to_owned(),
            memory,
            root_path,
            scan_policy: policy_seen.then_some(policy_options),
        })
    }

    #[must_use]
    pub fn root_path(&self) -> Option<&Path> {
        self.root_path.as_deref()
    }

    #[must_use]
    pub fn scan_policy(&self) -> Option<&ScanOptions> {
        self.scan_policy.as_ref()
    }

    /// Inherits known scope, preserving default behavior for unknown legacy scope.
    ///
    /// # Errors
    ///
    /// Returns an error when explicitly requested exclusions differ from the
    /// persisted policy. Changing scope requires an explicit index rebuild.
    pub fn resolve_scan_options(
        &self,
        requested: Option<ScanOptions>,
    ) -> Result<ScanOptions, &'static str> {
        match (&self.scan_policy, requested) {
            (Some(stored), Some(requested)) if *stored != requested => {
                Err("exclude_names does not match stored scan policy")
            }
            (Some(stored), _) => Ok(stored.clone()),
            (None, requested) => Ok(requested.unwrap_or_default()),
        }
    }

    #[must_use]
    pub fn all_files(&self) -> Vec<IndexedFile> {
        self.memory.all_files()
    }

    /// Borrows records in normalized path order without cloning the snapshot.
    #[must_use]
    pub fn iter_files(
        &self,
    ) -> impl DoubleEndedIterator<Item = &IndexedFile> + ExactSizeIterator + '_ {
        self.memory.iter_files()
    }

    #[must_use]
    pub fn file_count(&self) -> usize {
        self.memory.file_count()
    }

    #[must_use]
    pub fn total_size_bytes(&self) -> u64 {
        self.memory.total_size_bytes()
    }

    #[must_use]
    pub fn search_by_name(&self, query: &str) -> Vec<IndexedFile> {
        self.memory.search_by_name(query)
    }
}

/// Exclusive mutation and publication access to an index snapshot.
///
/// The mutable guard borrow prevents overlapping writers; read APIs are available
/// through immutable dereferencing. Cloning a reader never clones this capability.
///
/// ```compile_fail
/// use std::path::Path;
/// use ai_file_search_indexer::{FileIndexWriter, IndexWriterGuard};
/// let mut guard = IndexWriterGuard::acquire(Path::new("index.txt")).unwrap();
/// let first = FileIndexWriter::new(&mut guard);
/// let second = FileIndexWriter::new(&mut guard);
/// first.save().unwrap();
/// second.save().unwrap();
/// ```
pub struct FileIndexWriter<'a> {
    store: FileIndexStore,
    _guard: &'a mut IndexWriterGuard,
}

impl<'a> FileIndexWriter<'a> {
    /// Creates an empty rebuild without reading the previous snapshot.
    #[must_use]
    pub fn new(guard: &'a mut IndexWriterGuard) -> Self {
        Self {
            store: FileIndexStore::new(guard.index_path()),
            _guard: guard,
        }
    }

    /// Opens the snapshot at the guard's canonical destination.
    ///
    /// # Errors
    ///
    /// Returns the same read or metadata errors as [`FileIndexStore::open`].
    pub fn open(guard: &'a mut IndexWriterGuard) -> io::Result<Self> {
        Ok(Self {
            store: FileIndexStore::open(guard.index_path())?,
            _guard: guard,
        })
    }

    pub fn upsert_file(&mut self, file: IndexedFile) {
        self.store.memory.upsert_file(file);
    }

    pub fn replace_all(&mut self, files: Vec<IndexedFile>) {
        self.store.memory.replace_all(files);
    }

    pub fn remove_path(&mut self, path: &PathId) {
        self.store.memory.remove_path(path);
    }

    pub fn set_root_path(&mut self, root_path: impl AsRef<Path>) {
        self.store.root_path = Some(root_path.as_ref().to_path_buf());
    }

    pub fn set_scan_policy(&mut self, options: ScanOptions) {
        self.store.scan_policy = Some(options);
    }

    /// Streams, flushes, syncs, and closes a unique adjacent temporary snapshot
    /// before replacing the destination. No old-index unlink fallback is used.
    ///
    /// Requires a trusted local directory. Unix creation requests mode 0600;
    /// Windows inherits directory ACLs. This is not a network-filesystem or
    /// power-loss durability guarantee. Hard-link destination aliases and
    /// hostile out-of-band writers are unsupported.
    ///
    /// # Errors
    ///
    /// Returns the underlying create, write, flush, sync, or replacement error.
    /// A pre-publication failure preserves the previous snapshot and attempts to
    /// remove only this attempt's temporary file, after closing its handles.
    pub fn save(&self) -> io::Result<()> {
        self.save_with(&mut PublicationControl::default())
    }

    fn save_with(&self, control: &mut PublicationControl) -> io::Result<()> {
        let (mut temporary, file) = create_temporary_file(&self.store.path, control)?;
        {
            // This scope closes all handles before failure cleanup on Windows.
            let mut output = BufWriter::new(file);
            writeln!(output, "{INDEX_HEADER}")?;
            #[cfg(test)]
            control.check(PublicationStage::Write)?;
            if let Some(root_path) = &self.store.root_path {
                write_metadata_record(&mut output, "root", &root_path.to_string_lossy())?;
            }
            if let Some(policy) = &self.store.scan_policy {
                write_metadata_record(&mut output, "scan_policy", "1")?;
                for name in policy.excluded_names() {
                    write_metadata_record(&mut output, "exclude_name", name)?;
                }
            }
            for file in self.store.memory.files.values() {
                writeln!(
                    output,
                    "{}\t{}\t{}",
                    file.size_bytes,
                    file.modified_unix_seconds,
                    file.relative_path.as_normalized()
                )?;
            }
            #[cfg(test)]
            control.check(PublicationStage::Flush)?;
            output.flush()?;
            #[cfg(test)]
            control.check(PublicationStage::Sync)?;
            output.get_ref().sync_all()?;
        }
        #[cfg(test)]
        control.check(PublicationStage::Replace)?;
        fs::rename(&temporary.path, &self.store.path)?;
        temporary.remove_on_drop = false;
        Ok(())
    }
}

impl Deref for FileIndexWriter<'_> {
    type Target = FileIndexStore;

    fn deref(&self) -> &Self::Target {
        &self.store
    }
}

struct OwnedTemporary {
    path: PathBuf,
    remove_on_drop: bool,
}

impl Drop for OwnedTemporary {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Default)]
struct PublicationControl {
    #[cfg(test)]
    failure: Option<PublicationStage>,
    #[cfg(test)]
    next_id: Option<u64>,
}

impl PublicationControl {
    #[cfg(test)]
    fn check(&self, stage: PublicationStage) -> io::Result<()> {
        if self.failure == Some(stage) {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("injected {stage:?} failure"),
            ))
        } else {
            Ok(())
        }
    }
}

fn next_temporary_id(control: &mut PublicationControl) -> u64 {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    #[cfg(test)]
    if let Some(id) = control.next_id.as_mut() {
        let next = *id;
        *id = id.wrapping_add(1);
        return next;
    }
    #[cfg(not(test))]
    let _ = control;
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PublicationStage {
    Create,
    Write,
    Flush,
    Sync,
    Replace,
}

fn create_temporary_file(
    index_path: &Path,
    control: &mut PublicationControl,
) -> io::Result<(OwnedTemporary, File)> {
    for _ in 0..TEMPORARY_CREATE_ATTEMPTS {
        let path = temporary_index_path(index_path, next_temporary_id(control))?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        #[cfg(test)]
        control.check(PublicationStage::Create)?;
        match options.open(&path) {
            Ok(file) => {
                return Ok((
                    OwnedTemporary {
                        path,
                        remove_on_drop: true,
                    },
                    file,
                ));
            }
            // Windows reports access denied for an existing directory collision.
            Err(error)
                if error.kind() == io::ErrorKind::AlreadyExists
                    || fs::symlink_metadata(&path).is_ok() => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "index temporary name collision limit reached",
    ))
}

fn invalid_scan_policy() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid or unsupported scan policy metadata",
    )
}

fn is_metadata_record(line: &str) -> bool {
    line.starts_with("meta\t")
}

fn parse_root_metadata_record(line: &str) -> Option<PathBuf> {
    let mut parts = line.splitn(3, '\t');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("meta"), Some("root"), Some(value)) => {
            Some(PathBuf::from(unescape_metadata_value(value)))
        }
        _ => None,
    }
}

fn write_metadata_record(output: &mut impl Write, key: &str, value: &str) -> io::Result<()> {
    write!(output, "meta\t{key}\t")?;
    for character in value.chars() {
        match character {
            '\\' => output.write_all(b"\\\\")?,
            '\t' => output.write_all(b"\\t")?,
            '\n' => output.write_all(b"\\n")?,
            '\r' => output.write_all(b"\\r")?,
            character => write!(output, "{character}")?,
        }
    }
    output.write_all(b"\n")
}

fn unescape_metadata_value(value: &str) -> String {
    let mut unescaped = String::new();
    let mut characters = value.chars();

    while let Some(character) = characters.next() {
        if character != '\\' {
            unescaped.push(character);
            continue;
        }

        match characters.next() {
            Some('\\') | None => unescaped.push('\\'),
            Some('t') => unescaped.push('\t'),
            Some('n') => unescaped.push('\n'),
            Some('r') => unescaped.push('\r'),
            Some(character) => {
                unescaped.push('\\');
                unescaped.push(character);
            }
        }
    }

    unescaped
}

fn parse_index_record(line: &str, has_header: bool) -> IndexedFile {
    if has_header {
        let mut parts = line.splitn(3, '\t');
        let size_bytes = parts
            .next()
            .and_then(|size| size.parse::<u64>().ok())
            .unwrap_or_default();
        let modified_unix_seconds = parts
            .next()
            .and_then(|size| size.parse::<u64>().ok())
            .unwrap_or_default();
        let path = parts.next().unwrap_or_default();

        IndexedFile {
            relative_path: PathId::from_user_path(path),
            size_bytes,
            modified_unix_seconds,
        }
    } else {
        IndexedFile {
            relative_path: PathId::from_user_path(line),
            size_bytes: 0,
            modified_unix_seconds: 0,
        }
    }
}

fn temporary_index_path(path: &Path, id: u64) -> io::Result<PathBuf> {
    if path.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "index destination has no file name",
        ));
    }
    let mut temporary_name = publication_prefix(path);
    temporary_name.push(format!("{}-{id}", std::process::id()));
    Ok(path.with_file_name(temporary_name))
}

#[cfg(test)]
mod streaming_load_tests {
    use std::io::{BufReader, Read};

    use super::*;

    #[test]
    fn tiny_buffers_preserve_unicode_crlf_duplicates_and_final_bare_cr() {
        let text = "aifs-index-v1\r\n\r\nmeta\troot\troot\\tname\r\nmeta\texclude_name\t.git\r\n7\t11\t\u{6587}/a.txt\r\nmeta\tscan_policy\t1\r\n9\t13\t\u{6587}/a.txt\r\n5\t6\tz.txt\r";
        for capacity in 1..=17 {
            let reader = BufReader::with_capacity(capacity, text.as_bytes());
            let store = FileIndexStore::from_reader(Path::new("unused-index.txt"), reader).unwrap();
            assert_eq!(
                store.all_files(),
                vec![
                    indexed_file("z.txt\r", 5, 6),
                    indexed_file("\u{6587}/a.txt", 9, 13),
                ]
            );
            assert_eq!(store.root_path(), Some(Path::new("root\tname")));
            assert_eq!(
                store.scan_policy(),
                Some(&ScanOptions::default().exclude_name(".git"))
            );
        }
    }

    #[test]
    fn tiny_buffers_do_not_drop_the_headerless_first_line() {
        let text = "\u{6587}/first.txt\r\n\r\nlast\rname.txt\r";
        for capacity in 1..=17 {
            let reader = BufReader::with_capacity(capacity, text.as_bytes());
            let store = FileIndexStore::from_reader(Path::new("unused-index.txt"), reader).unwrap();
            assert_eq!(
                store.all_files(),
                vec![
                    indexed_file("last\rname.txt\r", 0, 0),
                    indexed_file("\u{6587}/first.txt", 0, 0),
                ]
            );
        }
    }

    #[test]
    fn read_errors_before_or_after_records_never_return_a_snapshot() {
        for prefix in [
            "",
            "first.txt\n",
            "first.txt\npartial",
            "aifs-index-v1\n7\t11\tfirst.txt\n",
            "aifs-index-v1\nmeta\tscan_policy\t1\n",
        ] {
            let reader = BufReader::with_capacity(
                3,
                FailingReader {
                    prefix: prefix.as_bytes(),
                },
            );
            let error =
                FileIndexStore::from_reader(Path::new("unused-index.txt"), reader).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(error.to_string(), "injected read failure");
        }
    }

    #[test]
    fn tiny_buffers_reject_split_invalid_utf8_after_a_valid_record() {
        for text in [
            &b"first.txt\ninvalid\xff.txt\n"[..],
            &b"aifs-index-v1\n7\t11\tfirst.txt\n9\t13\tinvalid\xc3"[..],
        ] {
            for capacity in 1..=5 {
                let reader = BufReader::with_capacity(capacity, text);
                let error =
                    FileIndexStore::from_reader(Path::new("unused-index.txt"), reader).unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            }
        }
    }

    struct FailingReader<'a> {
        prefix: &'a [u8],
    }

    impl Read for FailingReader<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            if self.prefix.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected read failure",
                ));
            }
            self.prefix.read(output)
        }
    }

    fn indexed_file(path: &str, size_bytes: u64, modified_unix_seconds: u64) -> IndexedFile {
        IndexedFile {
            relative_path: PathId::from_user_path(path),
            size_bytes,
            modified_unix_seconds,
        }
    }
}

#[cfg(test)]
mod publication_tests {
    use super::*;

    const ORIGINAL: &[u8] = b"aifs-index-v1\nmeta\troot\tworkspace\n7\t1\told.txt\n";

    fn check_failure(stage: PublicationStage) {
        let fixture = TestDir::new(&format!("failure-{stage:?}"));
        let index_path = fixture.path.join("index.txt");
        fs::write(&index_path, ORIGINAL).unwrap();
        let mut guard = IndexWriterGuard::acquire(&index_path).unwrap();
        let unrelated = temporary_index_path(guard.index_path(), 99).unwrap();
        fs::write(&unrelated, b"preexisting temporary contents").unwrap();
        fs::write(fixture.path.join("index.txt.tmp"), b"old temp").unwrap();
        let before = fixture.entries();
        let mut writer = FileIndexWriter::open(&mut guard).unwrap();
        writer.replace_all(vec![indexed_file("new.txt", 9, 2)]);
        let mut control = PublicationControl {
            failure: Some(stage),
            next_id: Some(100),
        };

        let error = writer.save_with(&mut control).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), format!("injected {stage:?} failure"));
        assert_eq!(fs::read(&index_path).unwrap(), ORIGINAL);
        assert_eq!(fixture.entries(), before, "owned temp must be cleaned");
        assert_eq!(
            fs::read(unrelated).unwrap(),
            b"preexisting temporary contents"
        );
        assert_eq!(
            fs::read(fixture.path.join("index.txt.tmp")).unwrap(),
            b"old temp"
        );
    }

    #[test]
    fn create_failure_preserves_snapshot_and_unrelated_files() {
        check_failure(PublicationStage::Create);
    }

    #[test]
    fn write_failure_preserves_snapshot_and_cleans_owned_temp() {
        check_failure(PublicationStage::Write);
    }

    #[test]
    fn flush_failure_preserves_snapshot_and_cleans_owned_temp() {
        check_failure(PublicationStage::Flush);
    }

    #[test]
    fn sync_failure_preserves_snapshot_and_cleans_owned_temp() {
        check_failure(PublicationStage::Sync);
    }

    #[test]
    fn replacement_failure_preserves_snapshot_and_cleans_owned_temp() {
        check_failure(PublicationStage::Replace);
    }

    #[test]
    fn exclusive_creation_retries_without_touching_collisions_or_hard_links() {
        let fixture = TestDir::new("collisions-and-hard-links");
        let index_path = fixture.path.join("index.txt");
        fs::write(&index_path, ORIGINAL).unwrap();
        let unrelated = fixture.path.join("unrelated.txt");
        fs::write(&unrelated, b"unrelated contents").unwrap();
        let mut guard = IndexWriterGuard::acquire(&index_path).unwrap();
        let paths = (100..104)
            .map(|id| temporary_index_path(guard.index_path(), id).unwrap())
            .collect::<Vec<_>>();
        fs::write(&paths[0], b"collision").unwrap();
        fs::hard_link(&index_path, &paths[1]).unwrap();
        fs::hard_link(&unrelated, &paths[2]).unwrap();
        fs::create_dir(&paths[3]).unwrap();
        let before = fixture.entries();
        let mut writer = FileIndexWriter::new(&mut guard);
        writer.upsert_file(indexed_file("new.txt", 9, 2));

        writer
            .save_with(&mut PublicationControl {
                next_id: Some(100),
                ..PublicationControl::default()
            })
            .unwrap();

        assert_eq!(fs::read(&paths[0]).unwrap(), b"collision");
        assert_eq!(fs::read(&paths[1]).unwrap(), ORIGINAL);
        assert_eq!(fs::read(&paths[2]).unwrap(), b"unrelated contents");
        assert!(paths[3].is_dir());
        assert_eq!(fs::read(&unrelated).unwrap(), b"unrelated contents");
        assert_eq!(fixture.entries(), before);
        assert_eq!(
            fs::read(&index_path).unwrap(),
            b"aifs-index-v1\n9\t2\tnew.txt\n"
        );
    }

    #[test]
    fn exhausted_collision_retries_preserve_all_preexisting_files() {
        let fixture = TestDir::new("collision-limit");
        let index_path = fixture.path.join("index.txt");
        fs::write(&index_path, ORIGINAL).unwrap();
        let mut guard = IndexWriterGuard::acquire(&index_path).unwrap();
        let paths = (0..TEMPORARY_CREATE_ATTEMPTS)
            .map(|id| temporary_index_path(guard.index_path(), id as u64).unwrap())
            .collect::<Vec<_>>();
        for path in &paths {
            fs::write(path, b"collision").unwrap();
        }
        let before = fixture.entries();
        let writer = FileIndexWriter::new(&mut guard);
        let mut control = PublicationControl {
            next_id: Some(0),
            ..PublicationControl::default()
        };

        let error = writer.save_with(&mut control).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(control.next_id, Some(TEMPORARY_CREATE_ATTEMPTS as u64));
        assert_eq!(fs::read(&index_path).unwrap(), ORIGINAL);
        assert_eq!(fixture.entries(), before);
        for path in paths {
            assert_eq!(fs::read(path).unwrap(), b"collision");
        }
    }

    #[test]
    fn writer_constructors_always_use_the_guards_stable_destination() {
        let fixture = TestDir::new("stable-destination");
        let requested = fixture.path.join("new-directory").join("index.txt");
        let mut guard = IndexWriterGuard::acquire(&requested).unwrap();
        let destination = guard.index_path().to_owned();
        assert!(destination.is_absolute());
        let writer = FileIndexWriter::new(&mut guard);
        assert_eq!(writer.store.path, destination);
        writer.save().unwrap();
        drop(writer);
        let reopened = FileIndexWriter::open(&mut guard).unwrap();
        assert_eq!(reopened.store.path, destination);
        assert_eq!(FileIndexStore::open(&requested).unwrap().file_count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn exclusive_creation_skips_live_and_dangling_symlinks() {
        use std::os::unix::fs::symlink;

        let fixture = TestDir::new("symlink-collisions");
        let index_path = fixture.path.join("index.txt");
        fs::write(&index_path, ORIGINAL).unwrap();
        let unrelated = fixture.path.join("unrelated.txt");
        let missing = fixture.path.join("missing.txt");
        fs::write(&unrelated, b"unrelated contents").unwrap();
        let mut guard = IndexWriterGuard::acquire(&index_path).unwrap();
        let live_link = temporary_index_path(guard.index_path(), 100).unwrap();
        let dangling_link = temporary_index_path(guard.index_path(), 101).unwrap();
        symlink(&unrelated, &live_link).unwrap();
        symlink(&missing, &dangling_link).unwrap();
        let before = fixture.entries();
        let writer = FileIndexWriter::new(&mut guard);

        writer
            .save_with(&mut PublicationControl {
                next_id: Some(100),
                ..PublicationControl::default()
            })
            .unwrap();

        assert_eq!(fs::read(&unrelated).unwrap(), b"unrelated contents");
        assert_eq!(fs::read_link(live_link).unwrap(), unrelated);
        assert_eq!(fs::read_link(dangling_link).unwrap(), missing);
        assert!(!missing.exists());
        assert_eq!(fixture.entries(), before);
    }

    #[cfg(unix)]
    #[test]
    fn newly_created_temp_and_published_snapshot_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = TestDir::new("private-permissions");
        let index_path = fixture.path.join("index.txt");
        let mut guard = IndexWriterGuard::acquire(&index_path).unwrap();
        let (temporary, file) =
            create_temporary_file(guard.index_path(), &mut PublicationControl::default()).unwrap();
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o177, 0);
        drop(file);
        drop(temporary);
        let writer = FileIndexWriter::new(&mut guard);
        writer.save().unwrap();
        assert_eq!(
            fs::metadata(index_path).unwrap().permissions().mode() & 0o177,
            0
        );
    }

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(name: &str) -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "ai-file-search-publication-{name}-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self { path }
        }

        fn entries(&self) -> Vec<OsString> {
            let mut entries = fs::read_dir(&self.path)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>();
            entries.sort();
            entries
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.path).unwrap();
        }
    }

    fn indexed_file(path: &str, size_bytes: u64, modified_unix_seconds: u64) -> IndexedFile {
        IndexedFile {
            relative_path: PathId::from_user_path(path),
            size_bytes,
            modified_unix_seconds,
        }
    }
}

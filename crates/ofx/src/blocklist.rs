//! A crash blocklist for plug-in binaries, like the ones other OFX hosts keep.
//!
//! A native crash (access violation, C++ exception) inside a plug-in's library initialisers,
//! `OfxGetPlugin`, `Load`, `Describe` or `DescribeInContext` cannot be caught with
//! `catch_unwind`, so one bad plug-in in a standard OFX folder would crash FilmCraft on every
//! launch. Before the loader opens a binary it writes a marker file (`ofx_loading.<pid>.txt`,
//! holding the binary's canonical path) in the state directory, keeps it exclusively locked, and
//! removes it once the binary has been loaded and described, successfully or with an ordinary
//! error. If the process dies in between, the marker survives and the OS releases the lock; on
//! the next first load of any process a marker that can be locked is collected. A process that
//! was killed (closed during start-up, a timeout) leaves exactly the same marker as one that
//! crashed, and big suites take seconds to load (Sapphire about two), so one dead load only makes
//! the binary a suspect (`ofx_suspects.txt`); it is moved to `ofx_blocklist.txt` (one path per
//! line) and skipped from then on when a second load dies in it before any load of it succeeds.
//! A successful load clears the suspicion. A marker that is still locked belongs to another
//! FilmCraft process that is loading right now and is left alone.
//! `effects.plugins.scanOfx {"retry": true}` or deleting the line brings a binary back.
//!
//! The state directory is set once by the host ([`set_state_dir`]); without one (tests, tools with
//! no config directory) the blocklist is disabled. All file IO errors are ignored: the blocklist
//! is a safety net and must never make a load fail.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

/// Marker files are `ofx_loading.<owner>.txt`, one per process.
const MARKER_PREFIX: &str = "ofx_loading.";
const MARKER_SUFFIX: &str = ".txt";
/// The binaries that crashed the process while loading, one path per line.
const LIST: &str = "ofx_blocklist.txt";
/// The binaries a process died while loading once (killed or crashed), one path per line: a
/// second death blocklists them, a successful load forgives them.
const SUSPECTS: &str = "ofx_suspects.txt";

fn state_dir_cell() -> &'static Mutex<Option<PathBuf>> {
    static C: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

/// Whether the previous runs' markers have been dealt with in this process.
static RECOVERED: AtomicBool = AtomicBool::new(false);

/// Set the directory that holds the markers and `ofx_blocklist.txt` (the host's config
/// directory). Until this is called the blocklist is disabled.
pub fn set_state_dir(dir: PathBuf) {
    *state_dir_cell().lock().unwrap_or_else(PoisonError::into_inner) = Some(dir);
}

fn state_dir() -> Option<PathBuf> {
    state_dir_cell().lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Empty the blocklist (and forget leftover markers of dead processes): every binary is tried
/// again.
pub fn clear() {
    let Some(dir) = state_dir() else { return };
    clear_in(&dir);
    // A marker left by a previous run must not be blocklisted by the next load either.
    RECOVERED.store(true, Ordering::SeqCst);
}

/// The locked marker of the binary being loaded.
struct Marker {
    path: PathBuf,
    file: File,
    /// The binary being loaded, as recorded in the marker.
    bin: String,
}

impl Marker {
    /// The load finished: unlock (closing the file releases the lock), remove the marker, and
    /// clear any suspicion against the binary (it loads fine).
    fn finish(self) {
        let Marker { path, file, bin } = self;
        drop(file);
        let _ = std::fs::remove_file(&path);
        if let Some(dir) = path.parent() {
            remove_line(dir, SUSPECTS, &bin);
        }
    }
}

/// Held while a binary is being loaded; removes the marker when dropped (not reached if the
/// process dies).
pub(crate) struct LoadGuard(Option<Marker>);

impl Drop for LoadGuard {
    fn drop(&mut self) {
        if let Some(m) = self.0.take() {
            m.finish();
        }
    }
}

/// Called by the loader before it opens the binary at `bin` (canonical path; `label` is how
/// errors name it). `Err` = the binary crashed the process last time and is skipped.
pub(crate) fn begin(bin: &Path, label: &str) -> Result<LoadGuard, String> {
    let Some(dir) = state_dir() else { return Ok(LoadGuard(None)) };
    if !RECOVERED.swap(true, Ordering::SeqCst) {
        recover(&dir);
    }
    check_and_mark(&dir, bin, label, std::process::id()).map(LoadGuard)
}

/// How paths are compared: case-insensitively on Windows.
fn norm(path: &str) -> String {
    if cfg!(windows) { path.to_lowercase() } else { path.to_string() }
}

/// The paths listed in `dir/name`, one per line.
fn read_lines(dir: &Path, name: &str) -> Vec<String> {
    let text = std::fs::read_to_string(dir.join(name)).unwrap_or_default();
    text.lines().map(|l| l.trim_end_matches('\r')).filter(|l| !l.is_empty()).map(str::to_string).collect()
}

#[cfg(test)]
fn read_list(dir: &Path) -> Vec<String> {
    read_lines(dir, LIST)
}

fn is_listed(dir: &Path, name: &str, bin: &str) -> bool {
    let want = norm(bin);
    read_lines(dir, name).iter().any(|l| norm(l) == want)
}

fn is_blocked(dir: &Path, bin: &str) -> bool {
    is_listed(dir, LIST, bin)
}

fn append_line(dir: &Path, name: &str, bin: &str) {
    if let Ok(mut f) = File::options().create(true).append(true).open(dir.join(name)) {
        let _ = writeln!(f, "{bin}");
    }
}

/// Drop `bin` from `dir/name` (the file goes when it is empty). Nothing is written when it isn't
/// listed.
fn remove_line(dir: &Path, name: &str, bin: &str) {
    let lines = read_lines(dir, name);
    let want = norm(bin);
    let keep: Vec<&String> = lines.iter().filter(|l| norm(l) != want).collect();
    if keep.len() == lines.len() {
        return;
    }
    let path = dir.join(name);
    if keep.is_empty() {
        let _ = std::fs::remove_file(path);
    } else {
        let text: String = keep.iter().map(|l| format!("{l}\n")).collect();
        let _ = std::fs::write(path, text);
    }
}

fn marker_path(dir: &Path, owner: u32) -> PathBuf {
    dir.join(format!("{MARKER_PREFIX}{owner}{MARKER_SUFFIX}"))
}

fn is_marker(name: &str) -> bool {
    name.len() > MARKER_PREFIX.len() + MARKER_SUFFIX.len() && name.starts_with(MARKER_PREFIX) && name.ends_with(MARKER_SUFFIX)
}

fn marker_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(is_marker)).collect()
}

/// Open a marker and lock it; `None` while a live process still holds it (or it can't be opened).
fn take_dead(marker: &Path) -> Option<File> {
    let file = File::options().read(true).write(true).open(marker).ok()?;
    file.try_lock().ok()?;
    Some(file)
}

/// Skip a blocklisted binary; otherwise write and lock the marker of process `owner`.
fn check_and_mark(dir: &Path, bin: &Path, label: &str, owner: u32) -> Result<Option<Marker>, String> {
    let key = bin.to_string_lossy();
    if is_blocked(dir, &key) {
        return Err(format!(
            "{label}: skipped: it crashed FilmCraft while loading last time (remove it from {} or run effects.plugins.scanOfx {{\"retry\": true}} to try again)",
            dir.join(LIST).display()
        ));
    }
    // A path with a line break can't be stored one per line; it just goes unprotected.
    if key.contains(['\n', '\r']) {
        return Ok(None);
    }
    let _ = std::fs::create_dir_all(dir);
    let path = marker_path(dir, owner);
    let Ok(mut file) = File::create(&path) else { return Ok(None) };
    // Locked for as long as the load runs; a lock failure only weakens the multi-process check.
    let _ = file.lock();
    let _ = file.write_all(key.as_bytes()).and_then(|()| file.flush());
    Ok(Some(Marker { path, file, bin: key.into_owned() }))
}

/// Markers left by processes that died while loading a binary: the first death makes the binary
/// a suspect, a second one (with no successful load in between) blocklists it. Markers still
/// locked by a live process are left alone.
fn recover(dir: &Path) {
    for marker in marker_files(dir) {
        let Some(mut file) = take_dead(&marker) else { continue };
        let mut text = String::new();
        let _ = file.read_to_string(&mut text);
        // Emptied first, so a marker another program keeps open (a virus scanner) and that
        // can't be removed yet is not counted again by the next process.
        let _ = file.set_len(0);
        drop(file);
        let _ = std::fs::remove_file(&marker);
        let bin = text.lines().next().unwrap_or("").trim_end_matches('\r');
        if bin.is_empty() || is_blocked(dir, bin) {
            continue;
        }
        if is_listed(dir, SUSPECTS, bin) {
            remove_line(dir, SUSPECTS, bin);
            append_line(dir, LIST, bin);
        } else {
            append_line(dir, SUSPECTS, bin);
        }
    }
}

/// Forget the blocklist, the suspects and the markers of dead processes.
fn clear_in(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(LIST));
    let _ = std::fs::remove_file(dir.join(SUSPECTS));
    for marker in marker_files(dir) {
        if let Some(file) = take_dead(&marker) {
            drop(file);
            let _ = std::fs::remove_file(&marker);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, empty directory under the system temp folder; removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("filmcraft-ofx-blocklist-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn bin(name: &str) -> PathBuf {
        PathBuf::from(r"C:\OFX\Plugins").join(name)
    }

    /// The process "dies" holding its marker: the handle closes (the OS drops the lock) but the
    /// file stays.
    fn crash(m: Option<Marker>) {
        drop(m.map(|m| m.file));
    }

    /// Two runs in a row die loading `bin` (the second run recovers the first one's marker
    /// before it loads again), and a third run recovers the second marker.
    fn crash_twice(dir: &Path, bin: &Path, label: &str, first_owner: u32) {
        crash(check_and_mark(dir, bin, label, first_owner).unwrap());
        recover(dir);
        crash(check_and_mark(dir, bin, label, first_owner + 1).unwrap());
        recover(dir);
    }

    fn markers(dir: &Path) -> usize {
        marker_files(dir).len()
    }

    #[test]
    fn a_clean_load_leaves_nothing_behind() {
        let t = TempDir::new("clean");
        let b = bin("Good.ofx");
        let m = check_and_mark(&t.0, &b, "Good", 1).unwrap().unwrap();
        // (Windows won't let another handle read the marker while it is locked.)
        assert_eq!(markers(&t.0), 1);
        m.finish();
        assert_eq!(markers(&t.0), 0);
        // The next run has nothing to recover and nothing is blocked.
        recover(&t.0);
        assert!(!t.0.join(LIST).exists());
        assert!(check_and_mark(&t.0, &b, "Good", 2).is_ok());
    }

    #[test]
    fn a_crash_blocklists_the_binary_on_the_next_run() {
        let t = TempDir::new("crash");
        let bad = bin("Bad.ofx");
        let good = bin("Good.ofx");
        // Runs 1 and 2 die inside the plug-in; run 3 notices the second marker.
        crash_twice(&t.0, &bad, "Bad", 1);
        assert_eq!(markers(&t.0), 0);
        assert_eq!(read_list(&t.0), vec![bad.to_string_lossy().into_owned()]);
        assert!(read_lines(&t.0, SUSPECTS).is_empty());
        let err = check_and_mark(&t.0, &bad, "Bad", 3).err().unwrap_or_default();
        assert!(err.starts_with("Bad: skipped: it crashed FilmCraft while loading last time"), "{err}");
        assert!(err.contains("ofx_blocklist.txt") && err.contains("retry"), "{err}");
        // A refused binary writes no marker, and other binaries still load.
        assert_eq!(markers(&t.0), 0);
        assert!(check_and_mark(&t.0, &good, "Good", 3).unwrap().is_some());
    }

    /// Regression: a run killed while a slow plug-in (Sapphire takes ~2 s) was loading, by a
    /// timeout or by closing the app during start-up, blocklisted that plug-in for good, as if it
    /// had crashed.
    #[test]
    fn one_interrupted_load_does_not_blocklist() {
        let t = TempDir::new("killed");
        let slow = bin("Sapphire.ofx");
        let key = slow.to_string_lossy().into_owned();
        // Run 1 is killed while loading it; run 2 notices the marker.
        crash(check_and_mark(&t.0, &slow, "Sapphire", 1).unwrap());
        recover(&t.0);
        assert_eq!(markers(&t.0), 0);
        assert!(!is_blocked(&t.0, &key));
        assert_eq!(read_lines(&t.0, SUSPECTS), vec![key.clone()]);
        // Run 2 loads it fine, which clears the suspicion.
        check_and_mark(&t.0, &slow, "Sapphire", 2).unwrap().unwrap().finish();
        assert!(!t.0.join(SUSPECTS).exists());
        // So another kill much later is again only a first strike.
        crash(check_and_mark(&t.0, &slow, "Sapphire", 3).unwrap());
        recover(&t.0);
        assert!(!is_blocked(&t.0, &key));
        assert!(check_and_mark(&t.0, &slow, "Sapphire", 4).unwrap().is_some());
    }

    #[test]
    fn a_successful_load_forgives_only_its_own_binary() {
        let t = TempDir::new("forgive");
        let (a, b) = (bin("A.ofx"), bin("B.ofx"));
        crash(check_and_mark(&t.0, &a, "A", 1).unwrap());
        recover(&t.0);
        crash(check_and_mark(&t.0, &b, "B", 2).unwrap());
        recover(&t.0);
        assert_eq!(read_lines(&t.0, SUSPECTS).len(), 2);
        check_and_mark(&t.0, &a, "A", 3).unwrap().unwrap().finish();
        assert_eq!(read_lines(&t.0, SUSPECTS), vec![b.to_string_lossy().into_owned()]);
        // B dies again: blocklisted; A stays loadable.
        crash(check_and_mark(&t.0, &b, "B", 4).unwrap());
        recover(&t.0);
        assert!(is_blocked(&t.0, &b.to_string_lossy()));
        assert!(!is_blocked(&t.0, &a.to_string_lossy()));
        assert!(!t.0.join(SUSPECTS).exists());
    }

    #[test]
    fn another_process_loading_right_now_is_left_alone() {
        let t = TempDir::new("live");
        let busy = bin("Busy.ofx");
        // Process 1 is still loading (its marker stays locked) when process 2 starts.
        let live = check_and_mark(&t.0, &busy, "Busy", 1).unwrap().unwrap();
        recover(&t.0);
        clear_in(&t.0);
        assert!(!is_blocked(&t.0, &busy.to_string_lossy()));
        assert_eq!(markers(&t.0), 1);
        live.finish();
        assert_eq!(markers(&t.0), 0);
    }

    #[test]
    fn recovering_twice_does_not_duplicate_entries() {
        let t = TempDir::new("twice");
        let bad = bin("Bad.ofx");
        crash_twice(&t.0, &bad, "Bad", 1);
        // Another marker for the same binary (for example after a manual retry that crashed again).
        let _ = std::fs::write(marker_path(&t.0, 7), bad.to_string_lossy().as_bytes());
        recover(&t.0);
        assert_eq!(read_list(&t.0).len(), 1);
        assert!(!t.0.join(SUSPECTS).exists());
        assert_eq!(markers(&t.0), 0);
    }

    #[test]
    fn several_crashes_accumulate() {
        let t = TempDir::new("several");
        for (i, n) in ["A.ofx", "B.ofx"].into_iter().enumerate() {
            crash_twice(&t.0, &bin(n), n, 10 * i as u32);
        }
        assert_eq!(read_list(&t.0).len(), 2);
        assert!(is_blocked(&t.0, &bin("A.ofx").to_string_lossy()));
        assert!(is_blocked(&t.0, &bin("B.ofx").to_string_lossy()));
        assert!(!is_blocked(&t.0, &bin("C.ofx").to_string_lossy()));
    }

    #[test]
    fn clear_forgets_everything() {
        let t = TempDir::new("clear");
        let bad = bin("Bad.ofx");
        crash_twice(&t.0, &bad, "Bad", 1);
        assert!(check_and_mark(&t.0, &bad, "Bad", 3).is_err());
        // A suspect and a leftover marker of a dead process go too.
        crash(check_and_mark(&t.0, &bin("Other.ofx"), "Other", 4).unwrap());
        recover(&t.0);
        let _ = std::fs::write(marker_path(&t.0, 5), b"C:\\x.ofx");
        clear_in(&t.0);
        assert!(!t.0.join(LIST).exists());
        assert!(!t.0.join(SUSPECTS).exists());
        assert_eq!(markers(&t.0), 0);
        assert!(check_and_mark(&t.0, &bad, "Bad", 2).is_ok());
    }

    #[test]
    fn io_problems_are_ignored() {
        // The "directory" is a file, so nothing can be written under it.
        let t = TempDir::new("io");
        let _ = std::fs::create_dir_all(&t.0);
        let file = t.0.join("not-a-dir");
        let _ = std::fs::write(&file, b"x");
        let b = bin("Any.ofx");
        assert!(check_and_mark(&file, &b, "Any", 1).unwrap().is_none());
        recover(&file);
        clear_in(&file);
        // And a state directory that doesn't exist yet is created on demand.
        let fresh = t.0.join("a").join("b");
        let m = check_and_mark(&fresh, &b, "Any", 1).unwrap();
        assert_eq!(markers(&fresh), 1);
        drop(m);
    }

    #[test]
    fn paths_with_line_breaks_are_not_recorded() {
        let t = TempDir::new("newline");
        assert!(check_and_mark(&t.0, Path::new("odd\nname.ofx"), "odd", 1).unwrap().is_none());
        assert_eq!(markers(&t.0), 0);
    }

    #[test]
    fn only_marker_names_count() {
        assert!(is_marker("ofx_loading.1234.txt"));
        assert!(!is_marker("ofx_loading.txt") && !is_marker("ofx_blocklist.txt") && !is_marker("other.txt"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_compare_case_insensitively() {
        let t = TempDir::new("case");
        crash(check_and_mark(&t.0, Path::new(r"C:\OFX\Plugins\Bad.ofx"), "Bad", 1).unwrap());
        recover(&t.0);
        crash(check_and_mark(&t.0, Path::new(r"c:\ofx\plugins\bad.ofx"), "Bad", 2).unwrap());
        recover(&t.0);
        assert!(check_and_mark(&t.0, Path::new(r"c:\ofx\plugins\BAD.OFX"), "Bad", 3).is_err());
    }

    #[test]
    fn without_a_state_dir_the_guard_is_inert() {
        // `set_state_dir` is process-global and no test sets it, so `begin` must be a no-op here.
        if state_dir().is_none() {
            let guard = begin(&bin("Any.ofx"), "Any");
            assert!(guard.is_ok());
        }
    }
}

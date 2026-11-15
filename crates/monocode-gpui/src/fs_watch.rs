//! Recursive file-system change signals for a checkout, so Git status reads
//! again when something on disk changed instead of on a fixed timer.
//!
//! Every relevant event collapses into one pending signal; build output,
//! dependency folders and Git's object store never wake a reader. Only writes,
//! creates, deletes and renames are watched, so another process merely reading
//! the checkout costs nothing, and ignored trees get no watch at all.
use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};
use mio::unix::SourceFd;
use mio::{Events, Interest, Poll, Token, Waker};
use std::{
    collections::HashMap,
    io,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// Folders whose churn never changes what Git status shows.
const IGNORED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".next",
    ".turbo",
    ".cache",
    "__pycache__",
    ".venv",
    "DerivedData",
];
/// Parts of a Git directory that change without changing status.
const IGNORED_GIT_DIRS: &[&str] = &["objects", "logs", "lfs", "hooks"];

const WATCH: Token = Token(0);
const WAKE: Token = Token(1);

/// What a checkout changes: content writes, file and folder adds and removes,
/// renames and permission changes. Reads (`IN_ACCESS`, `IN_OPEN`,
/// `IN_CLOSE_NOWRITE`) are deliberately absent: they never change Git status,
/// and watching them makes every `rg`, build or language server read in the
/// tree wake this thread.
fn mask() -> WatchMask {
    WatchMask::MODIFY
        | WatchMask::CLOSE_WRITE
        | WatchMask::ATTRIB
        | WatchMask::CREATE
        | WatchMask::DELETE
        | WatchMask::MOVED_FROM
        | WatchMask::MOVED_TO
        | WatchMask::DELETE_SELF
        | WatchMask::MOVE_SELF
}

pub(crate) struct Watch {
    waker: Arc<Waker>,
    stopped: Arc<AtomicBool>,
}
impl Drop for Watch {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        let _ = self.waker.wake();
    }
}

/// Watches each existing root recursively. The receiver holds at most one
/// pending signal however many files changed. None if the platform watcher
/// can't start, so callers fall back to polling.
pub(crate) fn watch(roots: &[PathBuf]) -> Option<(Watch, async_channel::Receiver<()>)> {
    let (tx, rx) = async_channel::bounded(1);
    let inotify = Inotify::init().ok()?;
    let poll = Poll::new().ok()?;
    let waker = Arc::new(Waker::new(poll.registry(), WAKE).ok()?);
    let mut watched = HashMap::new();
    let mut watches = inotify.watches();
    for root in roots {
        add_tree(&mut watches, root, &mut watched);
    }
    if watched.is_empty() {
        return None;
    }
    let fd = {
        use std::os::unix::io::AsRawFd;
        inotify.as_raw_fd()
    };
    poll.registry()
        .register(&mut SourceFd(&fd), WATCH, Interest::READABLE)
        .ok()?;
    let stopped = Arc::new(AtomicBool::new(false));
    {
        let stopped = stopped.clone();
        let waker = waker.clone();
        std::thread::Builder::new()
            .name("monocode-fs-watch".into())
            .spawn(move || run(inotify, poll, watched, tx, stopped, waker))
            .ok()?;
    }
    Some((Watch { waker, stopped }, rx))
}

fn run(
    mut inotify: Inotify,
    mut poll: Poll,
    mut watched: HashMap<WatchDescriptor, PathBuf>,
    tx: async_channel::Sender<()>,
    stopped: Arc<AtomicBool>,
    _waker: Arc<Waker>,
) {
    let mut events = Events::with_capacity(4);
    let mut buffer = vec![0; 64 * 1024];
    loop {
        match poll.poll(&mut events, None) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return,
            Ok(()) => {}
        }
        let mut signal = false;
        for event in &events {
            match event.token() {
                WAKE => {
                    if stopped.load(Ordering::Acquire) {
                        return;
                    }
                }
                WATCH => signal |= drain(&mut inotify, &mut buffer, &mut watched),
                _ => {}
            }
        }
        if signal {
            let _ = tx.try_send(());
        }
    }
}

/// Reads every pending inotify event, adds watches for new folders and reports
/// whether anything relevant to Git status changed.
fn drain(
    inotify: &mut Inotify,
    buffer: &mut [u8],
    watched: &mut HashMap<WatchDescriptor, PathBuf>,
) -> bool {
    let mut signal = false;
    let mut added = Vec::new();
    let mut removed = Vec::new();
    loop {
        let batches = match inotify.read_events(buffer) {
            Ok(events) => events,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            // A dropped stream: read again to be safe.
            Err(_) => return true,
        };
        for event in batches {
            if event.mask.contains(EventMask::Q_OVERFLOW) {
                signal = true;
            }
            let Some(dir) = watched.get(&event.wd) else {
                continue;
            };
            let path = match event.name {
                Some(name) => dir.join(name),
                None => dir.clone(),
            };
            if event.mask.contains(EventMask::IGNORED)
                || event.mask.contains(EventMask::DELETE_SELF)
            {
                removed.push(event.wd);
            }
            if event.mask.contains(EventMask::ISDIR)
                && event
                    .mask
                    .intersects(EventMask::CREATE | EventMask::MOVED_TO)
            {
                added.push(path.clone());
            }
            if relevant(&path) {
                signal = true;
            }
        }
    }
    for wd in removed {
        watched.remove(&wd);
    }
    if !added.is_empty() {
        let mut watches = inotify.watches();
        for path in added {
            add_tree(&mut watches, &path, watched);
        }
    }
    signal
}

/// Watches `root` and every folder under it that can change Git status.
fn add_tree(
    watches: &mut inotify::Watches,
    root: &Path,
    watched: &mut HashMap<WatchDescriptor, PathBuf>,
) {
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        if !dir.is_dir() {
            continue;
        }
        if let Ok(wd) = watches.add(&dir, mask()) {
            watched.insert(wd, dir.clone());
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if IGNORED_DIRS.iter().any(|ignored| name == *ignored) {
                continue;
            }
            if IGNORED_GIT_DIRS.iter().any(|ignored| name == *ignored) && in_git(&dir) {
                continue;
            }
            // `file_type` comes from the directory entry and never follows a
            // symlink, so a link back up the tree cannot loop the walk.
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                pending.push(entry.path());
            }
        }
    }
}

fn in_git(dir: &Path) -> bool {
    dir.components().any(|component| match component {
        Component::Normal(name) => name
            .to_str()
            .is_some_and(|name| name == ".git" || name.ends_with(".git")),
        _ => false,
    })
}

fn relevant(path: &Path) -> bool {
    if path
        .file_name()
        .is_some_and(|name| name == "index.lock" || name == ".DS_Store")
    {
        return false;
    }
    let mut in_git = false;
    for component in path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        let Some(name) = name.to_str() else {
            continue;
        };
        if in_git && IGNORED_GIT_DIRS.contains(&name) {
            return false;
        }
        if IGNORED_DIRS.contains(&name) {
            return false;
        }
        // A main checkout's `.git`, or a common dir such as `repo.git`.
        in_git = name == ".git" || name.ends_with(".git");
    }
    true
}

#[cfg(test)]
mod tests {
    use super::relevant;
    use std::path::Path;

    #[test]
    fn source_index_and_refs_wake_readers_but_build_output_does_not() {
        assert!(relevant(Path::new("/repo/src/main.rs")));
        assert!(relevant(Path::new("/repo/.git/index")));
        assert!(relevant(Path::new("/repo/.git/HEAD")));
        assert!(relevant(Path::new("/repo/.git/refs/heads/main")));
        assert!(!relevant(Path::new("/repo/.git/objects/ab/cdef")));
        assert!(!relevant(Path::new("/repo/.git/index.lock")));
        assert!(!relevant(Path::new("/repo/target/debug/app")));
        assert!(!relevant(Path::new("/repo/web/node_modules/x/index.js")));
    }

    #[test]
    fn an_edit_signals_once_and_build_output_stays_quiet() {
        use std::time::Duration;
        let root = std::env::temp_dir().join(format!("monocode-fs-watch-{}", std::process::id()));
        std::fs::create_dir_all(root.join("target")).unwrap();
        let root = root.canonicalize().unwrap();
        let (_watch, changes) = super::watch(std::slice::from_ref(&root)).unwrap();
        while changes.try_recv().is_ok() {}

        std::fs::write(root.join("target/out.o"), "build").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            changes.try_recv().is_err(),
            "build output must not wake readers"
        );

        for n in 0..5 {
            std::fs::write(root.join("main.rs"), format!("fn main() {{ {n} }}")).unwrap();
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while changes.is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            changes.len(),
            1,
            "a burst of edits leaves one pending signal"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_folder_added_later_is_watched() {
        use std::time::Duration;
        let root =
            std::env::temp_dir().join(format!("monocode-fs-watch-new-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let (_watch, changes) = super::watch(std::slice::from_ref(&root)).unwrap();
        while changes.try_recv().is_ok() {}

        std::fs::create_dir_all(root.join("src/deep")).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while changes.is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(changes.try_recv().is_ok(), "a new folder reports");
        while changes.try_recv().is_ok() {}

        std::fs::write(root.join("src/deep/main.rs"), "fn main() {}").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while changes.is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            changes.try_recv().is_ok(),
            "a file in the new folder reports"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}

//! The inotify backend of `fs.watch()`: src/runtime/node/path_watcher.rs on the thread's uSockets loop. One inotify
//! descriptor per thread is a poll of that loop; there is no reader thread. When it is readable, what the events say
//! becomes, for each `FSWatcher` that watches the path, an [`Event`] that `node_fs_watcher` delivers in a task of the
//! watcher's event loop.
//!
//! A `PathWatcher` is one per resolved path and `recursive`, shared by every handler (an `FSWatcher`) that watches
//! it; the last handler to detach removes the watch.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use bun_core::{ZBox, ZStr, strings};
use bun_paths as path;
use bun_sys::linux::IN;
use bun_sys::{self as sys, E, Fd, Tag};
use bun_watcher::inotify_watcher::EventIterator;

use super::node_fs_watcher::{self, Event, HandlerId, WatchEventKind};

bun_core::declare_scope!(fs_watch, hidden);

const WATCH_FILE_MASK: u32 = IN::MODIFY | IN::ATTRIB | IN::MOVE_SELF | IN::DELETE_SELF;
const WATCH_DIR_MASK: u32 = IN::MODIFY
    | IN::ATTRIB
    | IN::CREATE
    | IN::DELETE
    | IN::DELETE_SELF
    | IN::MOVED_FROM
    | IN::MOVED_TO
    | IN::MOVE_SELF
    | IN::ONLYDIR;

/// The slot of a `PathWatcher` in `Manager::watchers`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct WatcherId(usize);

/// What a handler holds until it detaches.
pub(crate) struct Handle(WatcherId);

/// Per-handler duplicate suppression, as path_watcher.rs's: the same path and event type within 1 ms is one event.
#[derive(Default)]
struct ChangeEvent {
    hash: Cell<u64>,
    event_type: Cell<WatchEventKind>,
    timestamp: Cell<i64>,
}

impl ChangeEvent {
    fn should_emit(&self, hash: u64, timestamp: i64, event_type: WatchEventKind) -> bool {
        let time_diff = timestamp - self.timestamp.get();
        if self.timestamp.get() == 0 || time_diff > 1 || self.event_type.get() != event_type || self.hash.get() != hash
        {
            self.timestamp.set(timestamp);
            self.event_type.set(event_type);
            self.hash.set(hash);
            return true;
        }
        false
    }
}

struct PathWatcher {
    /// The resolved path.
    path: ZBox,
    recursive: bool,
    is_file: bool,
    handlers: Vec<(HandlerId, ChangeEvent)>,
    /// The inotify watch descriptors: one, or one per directory of a recursive watch.
    descriptors: Vec<i32>,
}

/// What the handlers of the watchers are told, in order.
type Deliveries = Vec<(HandlerId, Event)>;

impl PathWatcher {
    fn emit(&self, out: &mut Deliveries, event_type: WatchEventKind, rel_path: &[u8], is_file: bool) {
        let timestamp = bun_core::time::milli_timestamp();
        let hash = bun_wyhash::hash(rel_path);
        for (handler, change) in &self.handlers {
            if change.should_emit(hash, timestamp, event_type) {
                node_fs_watcher::log_path_update(*handler, rel_path, is_file);
                out.push((*handler, event_type.to_event(rel_path.into())));
            }
        }
    }

    /// As `emit`, without duplicate suppression: the `IN_IGNORED` of a deleted root comes in the same millisecond as
    /// its `IN_DELETE_SELF`, and libuv reports both.
    fn emit_unsuppressed(&self, out: &mut Deliveries, event_type: WatchEventKind, rel_path: &[u8], is_file: bool) {
        for (handler, _) in &self.handlers {
            node_fs_watcher::log_path_update(*handler, rel_path, is_file);
            out.push((*handler, event_type.to_event(rel_path.into())));
        }
    }

    /// The queue overflowed and events were lost: `('change', null)`.
    fn emit_overflow(&self, out: &mut Deliveries) {
        for (handler, _) in &self.handlers {
            out.push((*handler, Event::NoFilename(WatchEventKind::Change)));
        }
    }

    fn emit_error(&self, out: &mut Deliveries, err: &sys::Error, close: bool) {
        for (handler, _) in &self.handlers {
            out.push((
                *handler,
                Event::Error {
                    err: err.clone(),
                    close,
                },
            ));
        }
    }
}

struct WdOwner {
    watcher: WatcherId,
    /// The watched directory or file relative to the watcher's path; empty for the root.
    subpath: Vec<u8>,
}

/// The thread's inotify descriptor, and what each watch descriptor is of.
struct Inotify {
    file: sys::CloseOnDrop,
    /// Which poll of the uSockets loop it is: it lives as long as the thread.
    _poll: *mut bun_usockets_rs::us_internal_async,
    /// `inotify_add_watch` answers the same wd for the same inode: watchers whose roots overlap share it.
    wd_map: HashMap<i32, Vec<WdOwner>>,
}

#[derive(Default)]
struct Manager {
    /// The resolved path and `recursive` → the watcher.
    by_path: HashMap<Vec<u8>, WatcherId>,
    watchers: Vec<Option<PathWatcher>>,
    free: Vec<WatcherId>,
    inotify: Option<Inotify>,
}

thread_local! {
    static MANAGER: RefCell<Manager> = RefCell::default();
}

impl Manager {
    fn watcher(&mut self, id: WatcherId) -> &mut PathWatcher {
        self.watchers[id.0].as_mut().expect("a watcher that is not removed")
    }

    fn insert(&mut self, watcher: PathWatcher) -> WatcherId {
        match self.free.pop() {
            Some(id) => {
                self.watchers[id.0] = Some(watcher);
                id
            }
            None => {
                self.watchers.push(Some(watcher));
                WatcherId(self.watchers.len() - 1)
            }
        }
    }

    fn remove(&mut self, id: WatcherId) -> Option<PathWatcher> {
        let watcher = self.watchers[id.0].take();
        if watcher.is_some() {
            self.free.push(id);
        }
        watcher
    }

    fn inotify(&mut self) -> sys::Result<&mut Inotify> {
        if self.inotify.is_none() {
            let rc = sys::linux::inotify_init1(IN::CLOEXEC | IN::NONBLOCK);
            if rc < 0 {
                return Err(sys::Error::from_code_int(sys::last_errno(), Tag::watch));
            }
            let file = sys::CloseOnDrop::new(Fd::from_native(rc));
            let loop_ = bun_uws_sys::Loop::get();
            let raw = std::ptr::from_ref::<bun_uws_sys::Loop>(loop_).cast_mut().cast();
            // SAFETY: this thread's loop, and a descriptor that stays open as long as the thread.
            let poll = unsafe { bun_usockets_rs::us_internal_create_callback_poll(raw, rc, on_readable) };
            if poll.is_null() {
                return Err(sys::Error::from_code_int(sys::last_errno(), Tag::watch));
            }
            self.inotify = Some(Inotify {
                file,
                _poll: poll,
                wd_map: HashMap::new(),
            });
        }
        Ok(self.inotify.as_mut().expect("made above"))
    }
}

impl Inotify {
    /// Adds one inotify watch and records whose it is.
    fn add_one(
        &mut self,
        id: WatcherId,
        wds: &mut Vec<i32>,
        abs_path: &ZStr,
        subpath: &[u8],
        is_file: bool,
    ) -> sys::Result<()> {
        let mask = if is_file { WATCH_FILE_MASK } else { WATCH_DIR_MASK };
        let rc = sys::linux::inotify_add_watch(self.file.fd().native(), abs_path, mask);
        if rc < 0 {
            let err = sys::Error::from_code_int(sys::last_errno(), Tag::watch);
            // A recursive walk that raced a removal.
            if !subpath.is_empty() && matches!(err.get_errno(), E::ENOENT | E::ENOTDIR) {
                return Ok(());
            }
            return Err(err.with_path(abs_path.as_bytes()));
        }
        let owners = self.wd_map.entry(rc).or_default();
        // The same watcher again: IN_CREATE raced the walk, or a directory moved inside the tree (the same inode,
        // under its new name).
        for owner in owners.iter_mut() {
            if owner.watcher == id {
                if !strings::eql(&owner.subpath, subpath) {
                    owner.subpath = subpath.to_vec();
                }
                return Ok(());
            }
        }
        owners.push(WdOwner {
            watcher: id,
            subpath: subpath.to_vec(),
        });
        wds.push(rc);
        bun_core::scoped_log!(
            fs_watch,
            "inotify_add_watch({}) = {} sub='{}'",
            String::from_utf8_lossy(abs_path.as_bytes()),
            rc,
            String::from_utf8_lossy(subpath)
        );
        Ok(())
    }

    /// Watches each directory below `root`; the first failure, which does not stop the walk.
    fn walk_and_add(&mut self, id: WatcherId, wds: &mut Vec<i32>, root: &ZStr) -> Option<sys::Error> {
        let mut first_err = None;
        walk_subtree::<true>(root, b"", &mut |abs, rel, _| {
            if let Err(err) = self.add_one(id, wds, abs, rel, false) {
                first_err.get_or_insert(err);
            }
        });
        first_err
    }

    /// `inotify_rm_watch` for each wd of the watcher that no other watcher has.
    fn remove_watch(&mut self, id: WatcherId, watcher: &PathWatcher) {
        for &wd in &watcher.descriptors {
            let Some(owners) = self.wd_map.get_mut(&wd) else { continue };
            owners.retain(|owner| owner.watcher != id);
            if owners.is_empty() {
                self.wd_map.remove(&wd);
                sys::linux::inotify_rm_watch(self.file.fd().native(), wd);
            }
        }
    }
}

/// Watches `path` for `handler`: the watcher of the path if there is one, else a new one. The error has no path.
pub(crate) fn watch(path: &ZStr, recursive: bool, handler: HandlerId) -> sys::Result<Handle> {
    // O_PATH|O_DIRECTORY, then without O_DIRECTORY on ENOTDIR: a file or a directory, and its real path.
    let mut is_file = false;
    let probe = match sys::open(path, sys::O::PATH | sys::O::DIRECTORY | sys::O::CLOEXEC, 0) {
        Ok(fd) => sys::CloseOnDrop::new(fd),
        Err(err) if err.get_errno() != E::ENOTDIR => return Err(err.without_path()),
        Err(_) => {
            is_file = true;
            match sys::open(path, sys::O::PATH | sys::O::CLOEXEC, 0) {
                Ok(fd) => sys::CloseOnDrop::new(fd),
                Err(err) => return Err(err.without_path()),
            }
        }
    };
    let mut resolve_buf = path::path_buffer_pool::get();
    let resolved = sys::get_fd_path_z(probe.fd(), &mut resolve_buf).unwrap_or(path);
    let mut key = resolved.as_bytes().to_vec();
    key.push(if recursive { b'R' } else { b'N' });

    let mut deliveries = Deliveries::new();
    let result = MANAGER.with_borrow_mut(|manager| {
        if let Some(&existing) = manager.by_path.get(&key) {
            manager.watcher(existing).handlers.push((handler, ChangeEvent::default()));
            return Ok(Handle(existing));
        }
        manager.inotify()?;
        let id = manager.insert(PathWatcher {
            path: ZBox::from_bytes(resolved.as_bytes()),
            recursive,
            is_file,
            handlers: vec![(handler, ChangeEvent::default())],
            descriptors: Vec::new(),
        });
        let Manager { watchers, inotify, .. } = &mut *manager;
        let inotify = inotify.as_mut().expect("made above");
        let watcher = watchers[id.0].as_mut().expect("inserted above");
        let mut wds = std::mem::take(&mut watcher.descriptors);
        let added = inotify.add_one(id, &mut wds, &watcher.path, b"", watcher.is_file);
        if added.is_ok() && watcher.recursive && !watcher.is_file {
            if let Some(err) = inotify.walk_and_add(id, &mut wds, &watcher.path) {
                // Partial coverage: an 'error', and the watcher stays, as Node's.
                watcher.emit_error(&mut deliveries, &err, false);
            }
        }
        watcher.descriptors = wds;
        if let Err(err) = added {
            manager.remove(id);
            return Err(err.without_path());
        }
        manager.by_path.insert(key, id);
        Ok(Handle(id))
    });
    node_fs_watcher::deliver(deliveries);
    result
}

/// Removes `handler` from the watcher; the last handler out removes the watch.
pub(crate) fn detach(Handle(id): Handle, handler: HandlerId) {
    let removed = MANAGER.with_borrow_mut(|manager| {
        let watcher = manager.watcher(id);
        watcher.handlers.retain(|(h, _)| *h != handler);
        if !watcher.handlers.is_empty() {
            return None;
        }
        let watcher = manager.remove(id)?;
        if let Some(inotify) = manager.inotify.as_mut() {
            inotify.remove_watch(id, &watcher);
        }
        manager.by_path.retain(|_, w| *w != id);
        Some(watcher)
    });
    drop(removed);
}

/// The uSockets loop's callback: the inotify descriptor is readable.
unsafe extern "C" fn on_readable(_: *mut bun_usockets_rs::us_loop_t) {
    // inotify gives whole events per read.
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let Some(fd) = MANAGER.with_borrow(|manager| manager.inotify.as_ref().map(|i| i.file.fd().native())) else {
            return;
        };
        let rc = sys::linux::read(fd, &mut buf);
        let deliveries = match sys::get_errno(rc) {
            E::SUCCESS if rc == 0 => return,
            E::SUCCESS => MANAGER.with_borrow_mut(|manager| manager.dispatch(&buf[..rc as usize])),
            E::EINTR => continue,
            E::EAGAIN => return,
            errno => {
                // Fatal: every watcher hears of it and closes.
                let err = sys::Error {
                    errno: errno as u16,
                    syscall: Tag::read,
                    ..Default::default()
                };
                let mut out = Deliveries::new();
                MANAGER.with_borrow(|manager| {
                    for watcher in manager.watchers.iter().flatten() {
                        watcher.emit_error(&mut out, &err, true);
                    }
                });
                node_fs_watcher::deliver(out);
                return;
            }
        };
        node_fs_watcher::deliver(deliveries);
    }
}

impl Manager {
    /// What the events in `bytes` tell the handlers: the loop of path_watcher.rs's `thread_main`.
    fn dispatch(&mut self, bytes: &[u8]) -> Deliveries {
        let mut out = Deliveries::new();
        let Manager {
            by_path,
            watchers,
            inotify,
            ..
        } = self;
        let Some(plat) = inotify.as_mut() else { return out };
        let mut rel_path = path::Path::new();
        for ev in EventIterator(bytes) {
            let wd = ev.watch_descriptor;
            // The queue reached fs.inotify.max_queued_events: every watcher lost events.
            if ev.mask & IN::Q_OVERFLOW != 0 {
                for &w in by_path.values() {
                    if let Some(watcher) = watchers[w.0].as_ref() {
                        watcher.emit_overflow(&mut out);
                    }
                }
                continue;
            }
            // The kernel retired this wd. For a deleted root that is a second "rename" after IN_DELETE_SELF, as
            // libuv's; a recursive sub-wd stays silent, its parent's IN_DELETE said it.
            if ev.mask & IN::IGNORED != 0 {
                if let Some(owners) = plat.wd_map.remove(&wd) {
                    for owner in owners {
                        let Some(w) = watchers[owner.watcher.0].as_mut() else { continue };
                        if owner.subpath.is_empty() && (w.is_file || !w.recursive) {
                            w.emit_unsuppressed(&mut out, WatchEventKind::Rename, path::basename(w.path.as_bytes()), w.is_file);
                        }
                        w.descriptors.retain(|&x| x != wd);
                    }
                }
                continue;
            }
            if !plat.wd_map.contains_key(&wd) {
                continue;
            }
            let name: &[u8] = ev.name().unwrap_or_default();
            let is_dir_child = ev.mask & IN::ISDIR != 0;
            let event_type = if ev.mask
                & (IN::CREATE | IN::DELETE | IN::DELETE_SELF | IN::MOVE_SELF | IN::MOVED_FROM | IN::MOVED_TO)
                != 0
            {
                WatchEventKind::Rename
            } else {
                WatchEventKind::Change
            };
            // `add_one` below may add to `wd_map`: the owners are found by key each time.
            let mut oi = 0;
            loop {
                let (id, owner_subpath) = {
                    let Some(owners) = plat.wd_map.get(&wd) else { break };
                    let Some(owner) = owners.get(oi) else { break };
                    (owner.watcher, owner.subpath.clone())
                };
                oi += 1;
                let Some(watcher) = watchers[id.0].as_mut() else { continue };
                let watcher_path = watcher.path.as_bytes().to_vec();
                // The path relative to this owner's root.
                let rel: Vec<u8> = if watcher.is_file {
                    path::basename(&watcher_path).to_vec()
                } else if owner_subpath.is_empty() {
                    if name.is_empty() && !watcher.recursive {
                        // About the watched directory itself: libuv reports its basename, as for a file. Node's
                        // recursive watcher keeps "".
                        path::basename(&watcher_path).to_vec()
                    } else {
                        name.to_vec()
                    }
                } else if name.is_empty() {
                    owner_subpath.clone()
                } else {
                    rel_path.set(&owner_subpath);
                    rel_path.push(name);
                    rel_path.as_bytes().to_vec()
                };
                let is_file = !is_dir_child
                    && !((ev.mask & (IN::DELETE_SELF | IN::MOVE_SELF) != 0) && !watcher.is_file);
                watcher.emit(&mut out, event_type, &rel, is_file);

                // Recursive: a new directory in the tree is watched, and what it already has is reported, as
                // Node's recursive watcher does for a folder it finds (lib/internal/fs/recursive_watch.js).
                if watcher.recursive && is_dir_child && (ev.mask & (IN::CREATE | IN::MOVED_TO) != 0) && !name.is_empty() {
                    let child_abs = path::Path::join(&[&watcher_path[..], &owner_subpath[..], name]);
                    let mut wds = std::mem::take(&mut watcher.descriptors);
                    let mut add_err = plat.add_one(id, &mut wds, &child_abs, &rel, false).err();
                    walk_subtree::<false>(&child_abs, &rel, &mut |abs, entry_rel, entry_is_file| {
                        if !entry_is_file {
                            if let Err(err) = plat.add_one(id, &mut wds, abs, entry_rel, false) {
                                add_err.get_or_insert(err);
                            }
                        }
                        watcher.emit(&mut out, WatchEventKind::Rename, entry_rel, entry_is_file);
                    });
                    watcher.descriptors = wds;
                    if let Some(err) = add_err {
                        watcher.emit_error(&mut out, &err, false);
                    }
                }
            }
        }
        out
    }
}

// ───────────────────────────── the walk ─────────────────────────────

/// Buffers that `walk_subtree` reuses across directories.
#[derive(Default)]
struct WalkScratch {
    /// The directory being read, or the entry passed to `cb`.
    abs: path::Path,
    /// The same, relative to the root of the watch.
    rel: path::Path,
    /// Entries not visited yet as `[is_file][name][0]`, the innermost directory's last.
    pending: Vec<u8>,
    /// The directories with entries in `pending`, the outermost first.
    levels: Vec<WalkLevel>,
}

struct WalkLevel {
    abs_len: usize,
    rel_len: usize,
    /// Where the directory's entries start in `pending`.
    start: usize,
    /// The first entry not visited yet.
    next: usize,
}

impl WalkScratch {
    fn set_entry<'a>(abs: &'a mut path::Path, rel: &mut path::Path, abs_len: usize, rel_len: usize, name: &[u8]) -> &'a ZStr {
        rel.truncate(rel_len);
        rel.push(name);
        abs.truncate(abs_len);
        abs.push(name);
        abs
    }

    fn open_dir(abs: &ZStr) -> sys::Result<sys::Dir> {
        sys::open(abs, sys::O::RDONLY | sys::O::DIRECTORY | sys::O::CLOEXEC, 0).map(sys::Dir::from_fd)
    }

    /// Reads every entry of `dir`, which is at `self.abs`, so that it is closed before the walk descends.
    fn read_dir<const DIRS_ONLY: bool>(&mut self, dir: &sys::Dir, cb: &mut impl FnMut(&ZStr, &[u8], bool)) {
        let abs_len = self.abs.len();
        let rel_len = self.rel.len();
        let start = self.pending.len();
        let mut it = sys::dir_iterator::iterate(dir.fd());
        let mut found_dir = false;
        while let Ok(Some(entry)) = it.next() {
            let is_file = entry.kind != sys::EntryKind::Directory;
            if DIRS_ONLY && is_file {
                continue;
            }
            let name = entry.name.slice_u8();
            if is_file && !found_dir {
                // No directory comes before it: it is next in the walk's order.
                let abs = Self::set_entry(&mut self.abs, &mut self.rel, abs_len, rel_len, name);
                cb(abs, &self.rel, true);
                continue;
            }
            found_dir = true;
            self.pending.push(u8::from(is_file));
            self.pending.extend_from_slice(name);
            self.pending.push(0);
        }
        if self.pending.len() > start {
            self.levels.push(WalkLevel {
                abs_len,
                rel_len,
                start,
                next: start,
            });
        }
    }
}

/// `cb(abs, rel, is_file)` for each entry below `abs_dir`, a directory before what it has; with `DIRS_ONLY` only the
/// directories. One directory is open at a time; a directory that cannot be read ends that branch.
fn walk_subtree<const DIRS_ONLY: bool>(abs_dir: &ZStr, rel_dir: &[u8], cb: &mut impl FnMut(&ZStr, &[u8], bool)) {
    let mut scratch = WalkScratch {
        abs: path::Path::from(abs_dir),
        rel: path::Path::from(rel_dir),
        ..Default::default()
    };
    if let Ok(dir) = WalkScratch::open_dir(abs_dir) {
        scratch.read_dir::<DIRS_ONLY>(&dir, cb);
    }
    while let Some(level) = scratch.levels.last_mut() {
        if level.next == scratch.pending.len() {
            scratch.pending.truncate(level.start);
            scratch.levels.pop();
            continue;
        }
        let record = &scratch.pending[level.next..];
        let is_file = record[0] != 0;
        let name = &record[1..];
        let name = &name[..strings::index_of_char_usize(name, 0).unwrap_or(name.len())];
        level.next += 1 + name.len() + 1;
        let name = name.to_vec();
        let abs = WalkScratch::set_entry(&mut scratch.abs, &mut scratch.rel, level.abs_len, level.rel_len, &name);
        cb(abs, &scratch.rel, is_file);
        if !is_file && let Ok(dir) = WalkScratch::open_dir(abs) {
            // `cb` watches it already: what is made meanwhile still has events.
            scratch.read_dir::<DIRS_ONLY>(&dir, cb);
        }
    }
}

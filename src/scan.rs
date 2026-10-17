//! Fast, read-only, parallel directory scanner for macOS.
//!
//! Uses `getattrlistbulk(2)` so a single syscall returns names, types and
//! allocated sizes for a whole batch of directory entries (no per-file
//! `lstat`). Directories are fanned out over a rayon work-stealing pool.
//! Sizes are *allocated* bytes (what actually occupies the disk), hard
//! links are counted once, symlinks are never followed and other volumes
//! are never crossed.
//!
//! Nothing in this module writes to the filesystem.

use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use parking_lot::Mutex;
use rayon::prelude::*;

use crate::rules::{self, Classifier, NO_RULE, Zone};

pub const F_DENIED: u8 = 1;
pub const F_OTHER_VOLUME: u8 = 2;
pub const F_TRASHED: u8 = 4;
pub const F_SKIPPED: u8 = 8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Dir,
    File,
    /// Aggregate of many small files in one directory.
    Others,
}

pub struct Node {
    pub name: Box<str>,
    /// Allocated bytes on disk (recursive for directories).
    pub size: u64,
    /// Number of files (recursive for directories, count for `Others`).
    pub items: u64,
    /// Sorted by size, largest first.
    pub children: Vec<Node>,
    pub kind: Kind,
    pub flags: u8,
    /// Dev-junk rule id, or `NO_RULE`.
    pub rule: u16,
}

impl Node {
    fn dir(name: Box<str>, flags: u8) -> Self {
        Node { name, size: 0, items: 0, children: Vec::new(), kind: Kind::Dir, flags, rule: NO_RULE }
    }
    pub fn trashed(&self) -> bool {
        self.flags & F_TRASHED != 0
    }
}

pub struct ScanConfig {
    pub root: PathBuf,
    pub title: String,
    pub allowed_devs: Vec<libc::dev_t>,
    /// Exact paths that are never entered (e.g. /System/Volumes).
    pub skip: Vec<PathBuf>,
    /// Bytes reported as used by the volume (for "not visible" accounting).
    pub volume_used: Option<u64>,
}

#[derive(Default)]
pub struct Progress {
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    pub bytes: AtomicU64,
    pub denied: AtomicU64,
    pub cancel: AtomicBool,
}

pub struct DevItem {
    /// Child-index path from the scan root to this node.
    pub idx: Vec<u32>,
    pub path: PathBuf,
    pub rule: u16,
    pub size: u64,
    pub items: u64,
    pub modified: Option<SystemTime>,
    pub trashed: bool,
}

pub struct ScanOutput {
    pub root: Node,
    pub root_path: PathBuf,
    pub title: String,
    pub elapsed: Duration,
    pub dev_items: Vec<DevItem>,
    pub denied: u64,
    pub dirs: u64,
    pub volume_used: Option<u64>,
}

/// Files at least this big are always listed individually.
const KEEP_FILE_MIN: u64 = 1 << 20;
/// The largest N files of every directory are always listed individually.
const KEEP_TOP_FILES: usize = 48;

struct Ctx<'a> {
    cfg: &'a ScanConfig,
    progress: &'a Progress,
    classifier: &'a Classifier,
    hardlinks: Mutex<HashSet<(libc::dev_t, u64)>>,
}

pub fn dev_of(path: &Path) -> Option<libc::dev_t> {
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::stat(c.as_ptr(), &mut st) } == 0 { Some(st.st_dev) } else { None }
}

pub fn run(cfg: ScanConfig, progress: Arc<Progress>, classifier: Arc<Classifier>) -> Option<ScanOutput> {
    let started = Instant::now();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads((threads * 2).clamp(8, 32))
        .stack_size(16 << 20)
        .thread_name(|i| format!("devdisk-scan-{i}"))
        .build()
        .ok()?;

    let ctx = Ctx { cfg: &cfg, progress: &progress, classifier: &classifier, hardlinks: Mutex::new(HashSet::new()) };
    let zone = classifier.zone_for(&cfg.root);
    let (mut root, _) = pool.install(|| scan_dir(&ctx, cfg.root.clone(), cfg.title.clone().into_boxed_str(), zone));
    if progress.cancel.load(Ordering::Relaxed) {
        return None;
    }
    root.flags &= !F_OTHER_VOLUME;

    let mut dev_items = Vec::new();
    let mut idx = Vec::new();
    let mut path = cfg.root.clone();
    collect_dev(&root, &mut idx, &mut path, &mut dev_items, true);
    dev_items.par_iter_mut().for_each(|d| {
        d.modified = std::fs::symlink_metadata(&d.path).and_then(|m| m.modified()).ok();
    });
    dev_items.sort_by(|a, b| b.size.cmp(&a.size));

    Some(ScanOutput {
        root,
        root_path: cfg.root.clone(),
        title: cfg.title.clone(),
        elapsed: started.elapsed(),
        dev_items,
        denied: progress.denied.load(Ordering::Relaxed),
        dirs: progress.dirs.load(Ordering::Relaxed),
        volume_used: cfg.volume_used,
    })
}

fn collect_dev(node: &Node, idx: &mut Vec<u32>, path: &mut PathBuf, out: &mut Vec<DevItem>, is_root: bool) {
    if !is_root && node.rule != NO_RULE {
        out.push(DevItem {
            idx: idx.clone(),
            path: path.clone(),
            rule: node.rule,
            size: node.size,
            items: node.items,
            modified: None,
            trashed: false,
        });
        // Top-most match only: nested node_modules etc. belong to it.
        return;
    }
    for (i, child) in node.children.iter().enumerate() {
        if child.kind != Kind::Dir || child.size == 0 && child.rule == NO_RULE {
            continue;
        }
        idx.push(i as u32);
        path.push(&*child.name);
        collect_dev(child, idx, path, out, false);
        path.pop();
        idx.pop();
    }
}

thread_local! {
    static BUF: RefCell<Vec<u8>> = RefCell::new(vec![0u8; 256 * 1024]);
}

const VREG: u32 = 1;
const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const VDIR: u32 = 2;

struct FileEnt {
    name_start: u32,
    name_len: u32,
    size: u64,
}

struct Fd(libc::c_int);
impl Drop for Fd {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

#[inline]
fn rd_u32(b: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes(b[at..at + 4].try_into().unwrap())
}
#[inline]
fn rd_i32(b: &[u8], at: usize) -> i32 {
    i32::from_ne_bytes(b[at..at + 4].try_into().unwrap())
}
#[inline]
fn rd_u64(b: &[u8], at: usize) -> u64 {
    u64::from_ne_bytes(b[at..at + 8].try_into().unwrap())
}

/// Returns the node and the marker bits of the entries inside it.
fn scan_dir(ctx: &Ctx, path: PathBuf, name: Box<str>, zone: Zone) -> (Node, u64) {
    let progress = ctx.progress;
    if progress.cancel.load(Ordering::Relaxed) {
        return (Node::dir(name, F_SKIPPED), 0);
    }
    progress.dirs.fetch_add(1, Ordering::Relaxed);

    let Ok(cpath) = CString::new(path.as_os_str().as_bytes()) else {
        return (Node::dir(name, F_DENIED), 0);
    };
    let fd = unsafe {
        libc::open(cpath.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
    };
    if fd < 0 {
        progress.denied.fetch_add(1, Ordering::Relaxed);
        return (Node::dir(name, F_DENIED), 0);
    }
    let fd = Fd(fd);

    // Never cross into another volume (mount points, other disks, devfs...).
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd.0, &mut st) } != 0 {
        progress.denied.fetch_add(1, Ordering::Relaxed);
        return (Node::dir(name, F_DENIED), 0);
    }
    let dev = st.st_dev;
    if !ctx.cfg.allowed_devs.contains(&dev) {
        return (Node::dir(name, F_OTHER_VOLUME), 0);
    }

    let mut markers = 0u64;
    let mut subdirs: Vec<Box<str>> = Vec::new();
    let mut files: Vec<FileEnt> = Vec::new();
    let mut names: Vec<u8> = Vec::new();
    let mut file_bytes = 0u64;
    let mut file_count = 0u64;
    let mut denied = false;

    BUF.with(|buf| {
        let mut buf = buf.borrow_mut();
        let mut al: libc::attrlist = unsafe { std::mem::zeroed() };
        al.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
        al.commonattr =
            libc::ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_ERROR | libc::ATTR_CMN_NAME | libc::ATTR_CMN_OBJTYPE | libc::ATTR_CMN_FILEID;
        al.fileattr = libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_ALLOCSIZE;

        loop {
            let n = unsafe {
                libc::getattrlistbulk(
                    fd.0,
                    &mut al as *mut libc::attrlist as *mut libc::c_void,
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                    0,
                )
            };
            if n < 0 {
                if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                denied = true;
                break;
            }
            if n == 0 {
                break;
            }
            let b: &[u8] = &buf;
            let mut p = 0usize;
            for _ in 0..n {
                if p + 24 > b.len() {
                    break;
                }
                let entry_len = rd_u32(b, p) as usize;
                if entry_len == 0 || p + entry_len > b.len() {
                    break;
                }
                let next = p + entry_len;
                let mut f = p + 4;
                let ret_common = rd_u32(b, f);
                let ret_file = rd_u32(b, f + 12);
                f += 20; // attribute_set_t (5 x u32)
                // Per man page, ATTR_CMN_ERROR comes right after the returned set.
                let mut entry_err = 0u32;
                if ret_common & ATTR_CMN_ERROR != 0 {
                    entry_err = rd_u32(b, f);
                    f += 4;
                }

                let mut ename: &[u8] = &[];
                if ret_common & libc::ATTR_CMN_NAME != 0 {
                    let off = rd_i32(b, f);
                    let len = rd_u32(b, f + 4) as usize;
                    let start = (f as isize + off as isize) as usize;
                    if len > 0 && start + len <= next {
                        ename = &b[start..start + len - 1]; // strip NUL
                    }
                    f += 8;
                }
                let mut objtype = 0u32;
                if ret_common & libc::ATTR_CMN_OBJTYPE != 0 {
                    objtype = rd_u32(b, f);
                    f += 4;
                }
                let mut fileid = 0u64;
                if ret_common & libc::ATTR_CMN_FILEID != 0 {
                    fileid = rd_u64(b, f);
                    f += 8;
                }
                let mut links = 1u32;
                if ret_file & libc::ATTR_FILE_LINKCOUNT != 0 {
                    links = rd_u32(b, f);
                    f += 4;
                }
                let mut alloc = 0u64;
                if ret_file & libc::ATTR_FILE_ALLOCSIZE != 0 {
                    alloc = rd_u64(b, f);
                }
                p = next;

                if entry_err != 0 {
                    progress.denied.fetch_add(1, Ordering::Relaxed);
                }
                if ename.is_empty() || ename == b"." || ename == b".." {
                    continue;
                }
                markers |= rules::marker_for(ename);

                if objtype == VDIR {
                    subdirs.push(String::from_utf8_lossy(ename).into());
                    continue;
                }
                // Regular files, symlinks (size of the link itself), sockets...
                let mut size = alloc;
                if objtype == VREG && links > 1 && !ctx.hardlinks.lock().insert((dev, fileid)) {
                    size = 0; // already counted via another hard link
                }
                file_bytes += size;
                file_count += 1;
                let start = names.len() as u32;
                names.extend_from_slice(ename);
                files.push(FileEnt { name_start: start, name_len: ename.len() as u32, size });
            }
        }
    });
    drop(fd);

    if denied {
        progress.denied.fetch_add(1, Ordering::Relaxed);
    }
    progress.files.fetch_add(file_count, Ordering::Relaxed);
    progress.bytes.fetch_add(file_bytes, Ordering::Relaxed);

    // Recurse into sub-directories in parallel.
    let classifier = ctx.classifier;
    let skip = &ctx.cfg.skip;
    let mut children: Vec<Node> = subdirs
        .into_par_iter()
        .map(|sub| {
            let child_path = path.join(&*sub);
            if skip.iter().any(|s| *s == child_path) {
                return Node::dir(sub, F_SKIPPED);
            }
            let czone = classifier.child_zone(zone, &child_path, &sub);
            let (mut node, child_markers) = scan_dir(ctx, child_path.clone(), sub, czone);
            if node.flags & (F_OTHER_VOLUME | F_SKIPPED | F_DENIED) == 0 {
                let rule_zone = if zone == Zone::In && czone == Zone::In { Zone::In } else { Zone::Out };
                node.rule = classifier.classify(&child_path, node.name.as_bytes(), markers, child_markers, rule_zone);
            }
            node
        })
        .collect();

    // Files: keep big ones and the top N, fold the rest into one "others" node.
    if files.len() > KEEP_TOP_FILES {
        files.sort_unstable_by(|a, b| b.size.cmp(&a.size));
    }
    let mut other_bytes = 0u64;
    let mut other_count = 0u64;
    for (i, fe) in files.iter().enumerate() {
        if i < KEEP_TOP_FILES || fe.size >= KEEP_FILE_MIN {
            let s = fe.name_start as usize;
            let nm = String::from_utf8_lossy(&names[s..s + fe.name_len as usize]).into_owned().into_boxed_str();
            children.push(Node { name: nm, size: fe.size, items: 1, children: Vec::new(), kind: Kind::File, flags: 0, rule: NO_RULE });
        } else {
            other_bytes += fe.size;
            other_count += 1;
        }
    }
    if other_count > 0 {
        children.push(Node {
            name: format!("{other_count} smaller files").into_boxed_str(),
            size: other_bytes,
            items: other_count,
            children: Vec::new(),
            kind: Kind::Others,
            flags: 0,
            rule: NO_RULE,
        });
    }

    let mut size = file_bytes;
    let mut items = file_count;
    for c in &children {
        if c.kind == Kind::Dir {
            size += c.size;
            items += c.items;
        }
    }
    children.sort_unstable_by(|a, b| b.size.cmp(&a.size));
    children.shrink_to_fit();

    let node = Node {
        name,
        size,
        items,
        children,
        kind: Kind::Dir,
        flags: if denied { F_DENIED } else { 0 },
        rule: NO_RULE,
    };
    (node, markers)
}

/// Mark the node at `idx` as moved to the Trash and subtract its size from
/// all ancestors. Index paths of other nodes stay valid (nothing is removed
/// from the vectors).
pub fn mark_trashed(root: &mut Node, idx: &[u32]) {
    let Some(target) = node_at(root, idx) else { return };
    let (size, items) = (target.size, target.items);
    let mut cur = &mut *root;
    cur.size = cur.size.saturating_sub(size);
    cur.items = cur.items.saturating_sub(items);
    for &i in idx {
        cur = &mut cur.children[i as usize];
        cur.size = cur.size.saturating_sub(size);
        cur.items = cur.items.saturating_sub(items);
    }
    cur.flags |= F_TRASHED;
    cur.children = Vec::new();
    cur.size = 0;
}

pub fn node_at<'a>(root: &'a Node, idx: &[u32]) -> Option<&'a Node> {
    let mut cur = root;
    for &i in idx {
        cur = cur.children.get(i as usize)?;
    }
    Some(cur)
}

#[cfg(test)]
mod tests {
    //! Read-only checks. The fixture lives in this crate's own `target/`
    //! folder; nothing is ever moved or deleted by these tests.
    use super::*;
    use crate::rules::{self, Risk};

    fn cfg_for(root: &Path) -> ScanConfig {
        ScanConfig {
            root: root.to_path_buf(),
            title: "t".into(),
            allowed_devs: vec![dev_of(root).unwrap()],
            skip: vec![],
            volume_used: None,
        }
    }

    fn home() -> PathBuf {
        std::fs::canonicalize(std::env::var("HOME").unwrap()).unwrap()
    }

    #[test]
    fn sizes_match_du() {
        // DEVDISK_TEST_DIR lets you point this at any folder (read-only).
        let dir = std::env::var("DEVDISK_TEST_DIR").unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/src").into());
        let root = std::fs::canonicalize(dir).unwrap();
        let t = Instant::now();
        let out = run(cfg_for(&root), Arc::new(Progress::default()), Arc::new(Classifier::new(home()))).unwrap();
        let du = std::process::Command::new("du").arg("-sk").arg(&root).output().unwrap();
        let kb: u64 = String::from_utf8_lossy(&du.stdout).split_whitespace().next().unwrap().parse().unwrap();
        let ours = out.root.size / 1024;
        eprintln!("du={kb}KB ours={ours}KB files={} dirs={} in {:?}", out.root.items, out.dirs, t.elapsed());
        // du also counts each directory's own blocks; allow for that.
        assert!(ours <= kb && ours + 64 + out.dirs * 4 >= kb, "du={kb} ours={ours}");
        assert!(out.root.items >= 7);
    }

    #[test]
    fn classifies_fixture_conservatively() {
        let base = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/target/devdisk-fixture"));
        let w = |rel: &str| {
            let p = base.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, vec![b'x'; 5000]).unwrap();
        };
        w("proj/package.json");
        w("proj/src/index.js");
        w("proj/node_modules/a/package.json");
        w("proj/node_modules/a/node_modules/b/x.js");
        w("proj/.next/cache/f");
        w("proj/__pycache__/m.pyc");
        w("proj/build/out.js"); // `build` next to package.json is NOT a rule
        w("notproj/node_modules/x.js"); // no package.json -> not flagged
        w("venvproj/.venv/pyvenv.cfg");
        w("venvproj/.venv/lib/x.py");
        w("rust/Cargo.toml");
        w("rust/target/.rustc_info.json");
        w("rust/target/debug/bin");
        w("fake/target/stuff"); // no Cargo.toml -> not flagged
        w("Thing.app/Contents/package.json");
        w("Thing.app/Contents/node_modules/x.js"); // inside a bundle -> never

        let root = std::fs::canonicalize(&base).unwrap();
        let classifier = Arc::new(Classifier::new(home()));
        let out = run(cfg_for(&root), Arc::new(Progress::default()), classifier.clone()).unwrap();
        let mut got: Vec<String> = out
            .dev_items
            .iter()
            .map(|d| d.path.strip_prefix(&root).unwrap().display().to_string())
            .collect();
        got.sort();
        assert_eq!(got, vec!["proj/.next", "proj/__pycache__", "proj/node_modules", "rust/target", "venvproj/.venv"]);

        for d in &out.dev_items {
            assert!(classifier.verify(d.rule, &d.path).is_ok(), "verify {}", d.path.display());
            assert!(crate::trash::safety_check(&d.path, &classifier.home).is_ok(), "safety {}", d.path.display());
        }
        let venv = out.dev_items.iter().find(|d| d.path.ends_with(".venv")).unwrap();
        assert_eq!(rules::rule(venv.rule).risk, Risk::Caution);

        // verify() must reject a path that does not match the rule's markers.
        let nm = out.dev_items.iter().find(|d| d.path.ends_with("node_modules")).unwrap();
        assert!(classifier.verify(nm.rule, &root.join("notproj/node_modules")).is_err());
        assert!(classifier.verify(nm.rule, &root.join("Thing.app/Contents/node_modules")).is_err());

        // Structural guard rails.
        let h = home();
        for p in ["/", "/Users", "/System/Library", "/Applications", "/usr/local/lib"] {
            assert!(crate::trash::safety_check(Path::new(p), &h).is_err(), "{p}");
        }
        for rel in ["", "Desktop", "Documents", "Library", "Library/Mobile Documents/x"] {
            assert!(crate::trash::safety_check(&h.join(rel), &h).is_err(), "~/{rel}");
        }
    }
}

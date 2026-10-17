//! The ONLY module that modifies the filesystem.
//!
//! Rules enforced here, for every single item, right before it is touched:
//!   * Items are moved to the macOS Trash (NSFileManager). Nothing is ever
//!     permanently deleted; there is no `remove_dir_all` anywhere in DevDisk.
//!   * The path must be absolute, symlink-free (canonical) and a real directory.
//!   * The path must be at least two levels inside your home folder or inside
//!     an external volume, and never a protected location.
//!   * The dev-junk rule that flagged it is re-verified against the live
//!     filesystem (name, project marker files, expected contents).
//!   * Info-only rules (Docker, simulators, toolchains...) are always refused.
//!   * Every move is appended to ~/Library/Logs/DevDisk/trash.log.

use std::io::Write;
use std::path::{Component, Path, PathBuf};

use objc2::rc::{Retained, autoreleasepool};
use objc2_foundation::{NSFileManager, NSString, NSURL};

use crate::rules::Classifier;

pub struct TrashRequest {
    pub path: PathBuf,
    pub rule: u16,
    pub size: u64,
    /// Index into the UI's dev item list, passed back in the outcome.
    pub item: usize,
}

pub enum TrashOutcome {
    Moved { item: usize, size: u64 },
    Refused { path: PathBuf, reason: String },
}

const PROTECTED_HOME_PREFIXES: &[&str] = &[
    "Library/Mobile Documents",
    "Library/CloudStorage",
    "Library/Keychains",
    "Library/Mail",
    "Library/Messages",
    "Library/Photos",
    "Library/Containers/com.apple.",
    "Library/Group Containers",
    ".ssh",
    ".gnupg",
    ".aws",
    ".kube",
    ".config",
    ".Trash",
];

const PROTECTED_ROOT_PREFIXES: &[&str] = &[
    "/System", "/bin", "/sbin", "/usr", "/etc", "/private", "/var", "/Library", "/Applications", "/opt", "/cores",
    "/dev",
];

const BUNDLE_SUFFIXES: &[&str] = &[".app", ".framework", ".bundle", ".plugin", ".appex", ".xpc", ".kext", ".photoslibrary"];

/// Structural safety checks independent of any rule.
pub fn safety_check(path: &Path, home: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("path is not absolute".into());
    }
    for c in path.components() {
        match c {
            Component::RootDir => {}
            Component::Normal(s) => {
                let s = s.to_str().ok_or("path is not valid UTF-8")?;
                if BUNDLE_SUFFIXES.iter().any(|b| s.ends_with(b)) {
                    return Err("path is inside an app/framework bundle".into());
                }
            }
            _ => return Err("path contains '.' or '..' components".into()),
        }
    }
    if home.as_os_str().is_empty() || home == Path::new("/") {
        return Err("home folder is unknown".into());
    }
    // Never the home folder or anything above it.
    if home.starts_with(path) {
        return Err("refusing to touch the home folder or one of its parents".into());
    }

    let canonical = std::fs::canonicalize(path).map_err(|e| format!("cannot resolve path: {e}"))?;
    if canonical != path {
        return Err(format!("path resolves elsewhere ({}); symlinks are never followed", canonical.display()));
    }
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("cannot read item: {e}"))?;
    if meta.file_type().is_symlink() {
        return Err("item is a symlink".into());
    }
    if !meta.is_dir() {
        return Err("item is not a folder".into());
    }

    if let Ok(rel) = path.strip_prefix(home) {
        if rel.components().count() < 2 {
            return Err("item is too close to the top of the home folder".into());
        }
        let rel_s = rel.to_str().ok_or("path is not valid UTF-8")?;
        for p in PROTECTED_HOME_PREFIXES {
            if rel_s == *p || rel_s.starts_with(&format!("{p}/")) || (p.ends_with('.') && rel_s.starts_with(p)) {
                return Err(format!("~/{p} is a protected location"));
            }
        }
        return Ok(());
    }

    if let Ok(rel) = path.strip_prefix("/Volumes") {
        // /Volumes/<disk>/<a>/<b> at minimum.
        if rel.components().count() < 3 {
            return Err("item is too close to the top of the volume".into());
        }
        return Ok(());
    }

    for p in PROTECTED_ROOT_PREFIXES {
        if path.starts_with(p) {
            return Err(format!("{p} is a protected system location"));
        }
    }
    Err("only items inside your home folder or on external volumes can be moved".into())
}

/// Validate and move one item to the Trash.
pub fn trash_one(req: &TrashRequest, classifier: &Classifier) -> TrashOutcome {
    let refuse = |reason: String| TrashOutcome::Refused { path: req.path.clone(), reason };
    if let Err(e) = safety_check(&req.path, &classifier.home) {
        return refuse(e);
    }
    if let Err(e) = classifier.verify(req.rule, &req.path) {
        return refuse(e);
    }
    match ns_trash(&req.path) {
        Ok(dest) => {
            log_line(&classifier.home, req, dest.as_deref());
            TrashOutcome::Moved { item: req.item, size: req.size }
        }
        Err(e) => refuse(format!("macOS refused to move it to the Trash: {e}")),
    }
}

fn ns_trash(path: &Path) -> Result<Option<PathBuf>, String> {
    let s = path.to_str().ok_or("path is not valid UTF-8")?;
    autoreleasepool(|_| {
        let ns_path = NSString::from_str(s);
        let url = NSURL::fileURLWithPath_isDirectory(&ns_path, true);
        let fm = NSFileManager::defaultManager();
        let mut resulting: Option<Retained<NSURL>> = None;
        match fm.trashItemAtURL_resultingItemURL_error(&url, Some(&mut resulting)) {
            Ok(()) => Ok(resulting.and_then(|u| u.path()).map(|p| PathBuf::from(p.to_string()))),
            Err(err) => Err(err.localizedDescription().to_string()),
        }
    })
}

pub fn log_path(home: &Path) -> PathBuf {
    home.join("Library/Logs/DevDisk/trash.log")
}

fn log_line(home: &Path, req: &TrashRequest, dest: Option<&Path>) {
    let path = log_path(home);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(
            f,
            "{}\t{}\t{}\t-> {}",
            timestamp(),
            crate::fmt::bytes(req.size),
            req.path.display(),
            dest.map(|d| d.display().to_string()).unwrap_or_else(|| "Trash".into())
        );
    }
}

fn timestamp() -> String {
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&now, &mut tm) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

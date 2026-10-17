//! Mounted volumes the user cares about (read-only).

use std::ffi::CStr;
use std::path::{Path, PathBuf};

use crate::scan::{ScanConfig, dev_of};

#[derive(Clone, Debug)]
pub struct Volume {
    pub name: String,
    pub mount: PathBuf,
    pub fs: String,
    pub total: u64,
    pub avail: u64,
    pub is_root: bool,
}

impl Volume {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.avail)
    }
}

const DATA_VOLUME: &str = "/System/Volumes/Data";

fn cstr(buf: &[libc::c_char]) -> String {
    unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
}

fn statfs(path: &Path) -> Option<libc::statfs> {
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statfs(c.as_ptr(), &mut s) } == 0).then_some(s)
}

/// Name of the startup disk, from the `/Volumes/<name> -> /` symlink.
fn root_name() -> String {
    if let Ok(rd) = std::fs::read_dir("/Volumes") {
        for e in rd.flatten() {
            if let Ok(t) = std::fs::read_link(e.path()) {
                if t == Path::new("/") {
                    return e.file_name().to_string_lossy().into_owned();
                }
            }
        }
    }
    "Macintosh HD".into()
}

pub fn list() -> Vec<Volume> {
    let mut out = Vec::new();
    let mut buf: *mut libc::statfs = std::ptr::null_mut();
    let n = unsafe { libc::getmntinfo(&mut buf, libc::MNT_NOWAIT) };
    if n <= 0 || buf.is_null() {
        return out;
    }
    let mounts = unsafe { std::slice::from_raw_parts(buf, n as usize) };
    for m in mounts {
        let mount = cstr(&m.f_mntonname);
        let fs = cstr(&m.f_fstypename);
        let flags = m.f_flags as i32;
        if matches!(fs.as_str(), "devfs" | "autofs" | "nullfs" | "lifs" | "fdesc") {
            continue;
        }
        let is_root = mount == "/";
        if !is_root
            && (flags & libc::MNT_DONTBROWSE != 0
                || mount.starts_with("/System/Volumes")
                || mount.starts_with("/private/")
                || mount.starts_with("/Library/Developer/CoreSimulator")
                || mount.starts_with("/dev"))
        {
            continue;
        }
        let bsize = m.f_bsize as u64;
        let mut total = m.f_blocks * bsize;
        let mut avail = m.f_bavail * bsize;
        if is_root {
            // APFS: the Data volume shares the container and reports the
            // numbers Finder shows.
            if let Some(d) = statfs(Path::new(DATA_VOLUME)) {
                total = d.f_blocks * d.f_bsize as u64;
                avail = d.f_bavail * d.f_bsize as u64;
            }
        }
        let name = if is_root {
            root_name()
        } else {
            Path::new(&mount).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or(mount.clone())
        };
        out.push(Volume {
            name,
            mount: PathBuf::from(&mount),
            fs,
            total,
            avail,
            is_root,
        });
    }
    out.sort_by_key(|v| (!v.is_root, v.name.to_lowercase()));
    out
}

/// Devices that count as "the startup disk" (sealed system + data volume,
/// joined via firmlinks such as /Users and /Applications).
fn startup_devs() -> Vec<libc::dev_t> {
    let mut devs: Vec<libc::dev_t> = Vec::new();
    for p in ["/", DATA_VOLUME] {
        if let Some(d) = dev_of(Path::new(p)) {
            if !devs.contains(&d) {
                devs.push(d);
            }
        }
    }
    devs
}

fn standard_skips() -> Vec<PathBuf> {
    // Mount trees: every real volume under these is a separate disk or a
    // view of the Data volume we already reach through firmlinks.
    ["/System/Volumes", "/Volumes", "/dev", "/Library/Developer/CoreSimulator/Volumes"]
        .iter()
        .map(PathBuf::from)
        .collect()
}

pub fn config_for_volume(v: &Volume) -> ScanConfig {
    if v.is_root {
        ScanConfig {
            root: PathBuf::from("/"),
            title: v.name.clone(),
            allowed_devs: startup_devs(),
            skip: standard_skips(),
            volume_used: Some(v.used()),
        }
    } else {
        ScanConfig {
            root: v.mount.clone(),
            title: v.name.clone(),
            allowed_devs: dev_of(&v.mount).into_iter().collect(),
            skip: Vec::new(),
            volume_used: Some(v.used()),
        }
    }
}

pub fn config_for_folder(path: &Path) -> Option<ScanConfig> {
    let root = std::fs::canonicalize(path).ok()?;
    let dev = dev_of(&root)?;
    let startup = startup_devs();
    let allowed = if startup.contains(&dev) { startup } else { vec![dev] };
    let title = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());
    Some(ScanConfig { root, title, allowed_devs: allowed, skip: standard_skips(), volume_used: None })
}

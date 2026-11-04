//! One successful read-only ABI probe, bound to its original connection and helper.
use super::*;
use std::{fs::File, path::Path};

#[derive(Default)]
pub(in crate::desktop_service) struct Cache(Mutex<Option<Entry>>);
struct Entry {
    configuration: String,
    epoch: String,
    generation: u64,
    stop: Weak<AtomicBool>,
    routing: Weak<zerus_client_core::desktop::relay::Connection>,
    image: Arc<Image>,
}
impl Cache {
    #[cfg(all(test, unix))]
    pub(super) fn is_empty(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_none()
    }
    pub(in crate::desktop_service) fn clear(&self) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take();
    }
    pub(super) fn matches(&self, read: &Read, image: &Image) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|entry| {
                entry.configuration == read.configuration
                    && entry.epoch == read.epoch
                    && entry.generation == read.generation
                    && entry.stop.ptr_eq(&Arc::downgrade(&read.stop))
                    && entry.routing.ptr_eq(&Arc::downgrade(&read.routing))
                    && entry.image.path == image.path
                    && entry.image.stamp == image.stamp
            })
    }
    pub(super) fn remember(&self, read: &Read, image: Arc<Image>) {
        let mut entry = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        // Serialize against stop's clear: a completed old probe cannot repopulate it.
        if read.current() {
            *entry = Some(Entry {
                configuration: read.configuration.clone(),
                epoch: read.epoch.clone(),
                generation: read.generation,
                stop: Arc::downgrade(&read.stop),
                routing: Arc::downgrade(&read.routing),
                image,
            });
        }
    }
}

/// The open original prevents file-ID reuse. Version stamps catch in-place writes.
pub(in crate::desktop_service) struct Image {
    path: PathBuf,
    original: File,
    stamp: Stamp,
}
#[derive(PartialEq, Eq)]
struct Stamp {
    volume: u64,
    index: u64,
    bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    attributes: u32,
}
impl Image {
    pub(in crate::desktop_service) fn capture(path: &Path) -> Option<Self> {
        // A bare PATH lookup has no fixed named identity here: keep probing it.
        if !path.is_absolute() {
            return None;
        }
        let original = open(path)?;
        let stamp = stamp(&original)?;
        let image = Self {
            path: path.into(),
            original,
            stamp,
        };
        image.current(path).then_some(image)
    }
    pub(in crate::desktop_service) fn current(&self, path: &Path) -> bool {
        path == self.path
            && stamp(&self.original).as_ref() == Some(&self.stamp)
            && open(path).and_then(|file| stamp(&file)).as_ref() == Some(&self.stamp)
    }
}

fn open(path: &Path) -> Option<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A raced replacement with a FIFO must not park a metadata worker.
        options.custom_flags(libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES;
        // Permit the maintained updater's atomic replacement while pinning the original.
        options.access_mode(FILE_READ_ATTRIBUTES).share_mode(7);
    }
    options.open(path).ok()
}
#[cfg(unix)]
fn stamp(file: &File) -> Option<Stamp> {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata().ok()?;
    if m.ino() == 0 || (m.ctime() == 0 && m.ctime_nsec() == 0) {
        return None;
    }
    m.is_file().then(|| Stamp {
        volume: m.dev(),
        index: m.ino(),
        bytes: m.len(),
        modified: (m.mtime(), m.mtime_nsec()),
        changed: (m.ctime(), m.ctime_nsec()),
        attributes: m.mode(),
    })
}
#[cfg(windows)]
fn stamp(file: &File) -> Option<Stamp> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileBasicInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
        BY_HANDLE_FILE_INFORMATION, FILE_BASIC_INFO,
    };
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    // SAFETY: both zeroed POD buffers are writable, sizes match their API classes,
    // and file retains the original valid handle through both calls.
    let (info, basic) = unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        let mut basic: FILE_BASIC_INFO = std::mem::zeroed();
        if GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) == 0
            || GetFileInformationByHandleEx(
                file.as_raw_handle() as _,
                FileBasicInfo,
                (&mut basic as *mut FILE_BASIC_INFO).cast(),
                std::mem::size_of::<FILE_BASIC_INFO>() as u32,
            ) == 0
        {
            return None;
        }
        (info, basic)
    };
    if info.dwVolumeSerialNumber == 0
        || (info.nFileIndexHigh == 0 && info.nFileIndexLow == 0)
        || basic.ChangeTime <= 0
    {
        return None;
    }
    Some(Stamp {
        volume: info.dwVolumeSerialNumber.into(),
        index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        bytes: (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow),
        modified: (basic.LastWriteTime, 0),
        changed: (basic.ChangeTime, 0),
        attributes: basic.FileAttributes,
    })
}
#[cfg(not(any(unix, windows)))]
fn stamp(_: &File) -> Option<Stamp> {
    None
}

pub(super) async fn capture(
    read: &Read,
    resources: Arc<dyn Send + Sync>,
) -> Result<Option<Arc<Image>>, &'static str> {
    let routing = read.routing.clone();
    let job = tauri::async_runtime::spawn_blocking(move || {
        let _resources = resources; // Retain admission even if metadata I/O outlives its caller.
        routing
            .executable()
            .map(|path| Image::capture(path).map(Arc::new))
    });
    tokio::time::timeout(RPC_WAIT, job)
        .await
        .map_err(|_| UNAVAILABLE)?
        .map_err(|_| UNAVAILABLE)?
        .map_err(|_| UNAVAILABLE)
}

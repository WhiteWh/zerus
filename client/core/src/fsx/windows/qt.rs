//! A foreign Qt writer has no Settings lease. Keep parent pins and a source
//! fingerprint, never a leaf handle during report publication/receipt waits.
//! Each bounded read opens with all sharing and closes before returning.
use super::{bytes, check_limit, filename, info, owned, refused, verify, wide, Parents};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};
use windows_sys::Win32::{
    Foundation::GENERIC_READ,
    Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    },
};
use zerus_desktop_migration::sha256;

#[derive(PartialEq, Eq)]
struct Stamp {
    volume: u32,
    index: (u32, u32),
    size: (u32, u32),
    written: (u32, u32),
}
impl Stamp {
    fn read(file: &File) -> io::Result<Self> {
        let value = info(file)?;
        Ok(Self {
            volume: value.dwVolumeSerialNumber,
            index: (value.nFileIndexHigh, value.nFileIndexLow),
            size: (value.nFileSizeHigh, value.nFileSizeLow),
            written: (
                value.ftLastWriteTime.dwHighDateTime,
                value.ftLastWriteTime.dwLowDateTime,
            ),
        })
    }
}
fn open(path: &Path) -> io::Result<File> {
    // GENERIC_READ includes attributes and READ_CONTROL for verification, with
    // no write/delete/create/timestamp permission and no ACL repair.
    let raw = unsafe {
        CreateFileW(
            wide(path)?.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    let file = File::from(owned(raw)?);
    verify(&file, false)?;
    Ok(file)
}

pub(crate) struct Source {
    _parents: Parents,
    pub(crate) modified_at: Option<SystemTime>,
    path: PathBuf,
    stamp: Stamp,
    hash: String,
    bytes: usize,
}
impl Source {
    pub(crate) fn unchanged_bytes(&mut self, path: &Path, limit: usize) -> io::Result<Vec<u8>> {
        if self.path != path {
            return Err(refused("Qt source path changed"));
        }
        // The pinned parent chain keeps this the original exact managed path.
        // A replaced leaf is opened no-reparse and checked, never adopted.
        let mut file = open(path)?;
        if Stamp::read(&file)? != self.stamp {
            return Err(refused("Qt source identity or metadata changed"));
        }
        let data = bytes(&mut file, limit)?;
        if Stamp::read(&file)? != self.stamp
            || data.len() != self.bytes
            || sha256(&data) != self.hash
        {
            return Err(refused("Qt source identity, metadata or bytes changed"));
        }
        // Classic MoveFileExW replacement can fail with a leaf handle open,
        // even when it shares DELETE. Never retain it across report/helper I/O.
        drop(file);
        Ok(data)
    }
}
fn current(path: &Path, limit: usize) -> io::Result<Option<Source>> {
    check_limit(limit)?;
    filename(path)?;
    let parents = match Parents::open(
        path.parent()
            .ok_or_else(|| refused("Qt source parent missing"))?,
        false,
    ) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut file = match open(path) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let stamp = Stamp::read(&file)?;
    let modified_at = file.metadata()?.modified().ok();
    let data = bytes(&mut file, limit)?;
    if Stamp::read(&file)? != stamp {
        return Err(refused("Qt source changed during capture"));
    }
    drop(file);
    let source = Source {
        _parents: parents,
        modified_at,
        path: path.into(),
        stamp,
        hash: sha256(&data),
        bytes: data.len(),
    };
    Ok(Some(source))
}
pub(crate) fn capture_with_deadline(
    path: &Path,
    limit: usize,
    deadline: Instant,
) -> io::Result<Option<Source>> {
    for attempt in 0..=20 {
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        match current(path, limit) {
            Err(error) if error.raw_os_error() == Some(32) && attempt < 20 => {
                std::thread::sleep(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(10)),
                );
            }
            result => return result,
        }
    }
    unreachable!()
}

use std::ffi::{c_char, CString};
use std::os::fd::{AsRawFd, FromRawFd};
use std::fs::File;
use std::path::{PathBuf, Path};
use std::io::{self, Read};
use std::collections::HashMap;

unsafe extern "C" {
    fn inotify_init1(flags: i32) -> i32;
    fn inotify_add_watch(fd: i32, path: *const c_char, mask: u32) -> i32;
}

const IN_CLOEXEC: i32 = 0o2000000;
const IN_MODIFY: u32 = 0x002;
const IN_CLOSE_WRITE: u32 = 0x008;
const IN_MOVED_TO: u32 = 0x080;
const IN_CREATE: u32 = 0x100;
const MASK: u32 = IN_MODIFY | IN_CLOSE_WRITE | IN_MOVED_TO | IN_CREATE;

pub struct Inotify {
    file: File,
    buf: Vec<u8>,
    pub watched_paths: HashMap<i32, PathBuf>
}

impl Inotify {
    pub fn new() -> io::Result<Self> {
        let fd = unsafe { inotify_init1(IN_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(Inotify {
            file: unsafe { File::from_raw_fd(fd) },
            buf: vec![0; 4096],
            watched_paths: HashMap::new(),
        })
    }

    pub fn watch(&mut self, path: impl AsRef<Path>) -> io::Result<()> {
        let c = CString::new(path.as_ref().as_os_str().as_encoded_bytes())?;
        let wd = unsafe { inotify_add_watch(self.file.as_raw_fd(), c.as_ptr(), MASK) };

        if wd < 0 {
            Err(io::Error::last_os_error())
        } else {
            self.watched_paths.insert(wd, path.as_ref().to_path_buf());
            Ok(())
        }
    }

    fn parse_events(&self, mut b: &[u8]) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let u32_at = |b: &[u8], o: usize| u32::from_ne_bytes(b[o..o + 4].try_into().unwrap());
        while b.len() >= 16 {
            let len = u32_at(b, 12) as usize;
            let raw = &b[16..16 + len];
            let name = raw.split(|&c| c == 0).next().filter(|n| !n.is_empty());
            let wd = u32_at(b, 0) as i32;
            if let Some(file) = self.watched_paths.get(&wd) {
                if let Some(name) = name {
                    out.push(file.join(&*String::from_utf8_lossy(name)));
                } else {
                    out.push(file.clone());
                }
            }
            b = &b[16 + len..];
        }
        out
    }

    pub fn read_events(&mut self) -> io::Result<Vec<PathBuf>> {
        let n = self.file.read(&mut self.buf)?;
        Ok(self.parse_events(&self.buf[..n]))
    }
}

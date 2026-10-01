//! 目录句柄：宿主在沙盒外写「应用可写目录」时，不再按路径字符串反复解析，而是先拿到目录句柄，
//! 之后所有文件操作（建临时文件、写、改名覆盖、删除）都相对这个句柄做。
//!
//! 威胁：应用对自己的 agenthome/sessions/apps 目录有写权限，可以在宿主「校验路径」与
//! 「按路径写入」之间把目录改名并放一个符号链接，让宿主在别处建文件。句柄一旦拿到，指向的
//! 就是打开那一刻的那个目录，之后路径怎么换都与它无关；打开本身用 `O_DIRECTORY | O_NOFOLLOW`，
//! 且要求句柄的 (dev, ino) 与校验时记下的一致。

use std::path::Path;

/// 目录身份（设备号 + inode），用来确认「打开的就是校验过的那个目录」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirIdentity {
    pub dev: u64,
    pub ino: u64,
}

/// 目录项的类别（不跟随链接：符号链接就是 `Symlink`，不会被当成它指向的东西）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// 目录项的元数据（对链接取的是链接自己的，不是目标的）。
#[derive(Debug, Clone)]
pub struct EntryInfo {
    /// 文件名按原始字节保存（不要求是 UTF-8）。
    pub name: std::ffi::OsString,
    pub kind: EntryKind,
    pub size: u64,
    pub mtime: std::time::SystemTime,
}

/// 不跟随链接地读出路径本身的身份；不是真目录（链接、文件）一律报错。
#[cfg(unix)]
pub fn identity_of_real_dir(path: &Path) -> Result<DirIdentity, String> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !m.file_type().is_dir() {
        return Err(format!("不是真实目录（链接或非目录）：{}", path.display()));
    }
    Ok(DirIdentity {
        dev: m.dev(),
        ino: m.ino(),
    })
}

#[cfg(unix)]
pub use unix_impl::DirHandle;

#[cfg(unix)]
mod unix_impl {
    use super::*;
    use rustix::fs::{AtFlags, Mode, OFlags, CWD};
    use std::io::Write;
    use std::os::fd::OwnedFd;

    /// 已打开的目录句柄。所有操作都相对它，不再解析任何路径。
    pub struct DirHandle {
        fd: OwnedFd,
    }

    fn io_err(e: rustix::io::Errno) -> std::io::Error {
        std::io::Error::from_raw_os_error(e.raw_os_error())
    }

    fn check_name(name: &std::ffi::OsStr) -> Result<(), String> {
        use std::os::unix::ffi::OsStrExt;
        let b = name.as_bytes();
        if b.is_empty() || b == b"." || b == b".." || b.contains(&b'/') || b.contains(&0) {
            return Err(format!("非法的文件名：{name:?}"));
        }
        Ok(())
    }

    impl DirHandle {
        /// 以 `O_DIRECTORY | O_NOFOLLOW` 打开 `path`，并要求它的身份等于 `expected`。
        /// 末级分量是符号链接 → 打开失败；被换成了别的目录 → 身份不符，拒绝。
        pub fn open_expecting(path: &Path, expected: DirIdentity) -> Result<Self, String> {
            let fd = rustix::fs::openat(
                CWD,
                path,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| {
                format!(
                    "打开目录失败（可能被替换成了符号链接）：{}：{e}",
                    path.display()
                )
            })?;
            let st = rustix::fs::fstat(&fd).map_err(|e| e.to_string())?;
            let got = DirIdentity {
                dev: st.st_dev as u64,
                ino: st.st_ino as u64,
            };
            if got != expected {
                return Err(format!(
                    "目录在校验后被替换（身份不符），拒绝使用：{}",
                    path.display()
                ));
            }
            Ok(Self { fd })
        }

        /// 在句柄目录里取子目录句柄：不存在就 `mkdirat` 建，再以 `O_DIRECTORY|O_NOFOLLOW`
        /// 打开。子目录若是符号链接 → 打开失败，绝不跟随。
        pub fn open_subdir_creating(&self, name: &str) -> Result<DirHandle, String> {
            check_name(name.as_ref())?;
            match rustix::fs::mkdirat(&self.fd, name, Mode::from_raw_mode(0o755)) {
                Ok(()) => {}
                Err(e) if e == rustix::io::Errno::EXIST => {}
                Err(e) => return Err(io_err(e).to_string()),
            }
            let fd = rustix::fs::openat(
                &self.fd,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| format!("子目录 {name} 不是真实目录（可能是符号链接）：{e}"))?;
            Ok(DirHandle { fd })
        }

        /// 句柄自己的身份。
        pub fn identity(&self) -> Result<DirIdentity, String> {
            let st = rustix::fs::fstat(&self.fd).map_err(|e| e.to_string())?;
            Ok(DirIdentity {
                dev: st.st_dev as u64,
                ino: st.st_ino as u64,
            })
        }

        /// 在句柄目录里以「O_EXCL 临时文件 → 写入 → fsync → renameat 覆盖」写 `name`。
        /// 临时文件创建带 `O_NOFOLLOW`，目录项里已有任何东西都会失败而不是被跟随；
        /// 覆盖替换的是目录项本身（目标若是应用放的链接，链接被替换掉）。
        pub fn write_file_replacing(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            check_name(name.as_ref())?;
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let tmp = format!(
                ".{name}.{nanos}.{}.{}.tmp",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            );
            let result = (|| -> std::io::Result<()> {
                let tfd = rustix::fs::openat(
                    &self.fd,
                    tmp.as_str(),
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::from_raw_mode(0o600),
                )
                .map_err(io_err)?;
                let mut f = std::fs::File::from(tfd);
                f.write_all(bytes)?;
                f.sync_all()?;
                drop(f);
                rustix::fs::renameat(&self.fd, tmp.as_str(), &self.fd, name).map_err(io_err)
            })();
            if result.is_err() {
                let _ = rustix::fs::unlinkat(&self.fd, tmp.as_str(), AtFlags::empty());
            }
            result.map_err(|e| e.to_string())
        }

        /// 在句柄目录里打开**已存在**的子目录（`O_DIRECTORY | O_NOFOLLOW`，不创建）。
        /// 子目录是符号链接或不是目录 → Err，绝不跟随。
        pub fn open_subdir(&self, name: impl AsRef<std::ffi::OsStr>) -> Result<DirHandle, String> {
            let name: &std::ffi::OsStr = name.as_ref();
            check_name(name)?;
            let fd = rustix::fs::openat(
                &self.fd,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| format!("子目录 {name:?} 不是真实目录（可能是符号链接）：{e}"))?;
            Ok(DirHandle { fd })
        }

        /// 列出句柄目录下的全部目录项（不含 `.`、`..`），元数据一律 `AT_SYMLINK_NOFOLLOW`。
        /// 单项读不到（被并发删走等）就跳过。
        pub fn entries(&self) -> Result<Vec<EntryInfo>, String> {
            let mut dir = rustix::fs::Dir::read_from(&self.fd).map_err(|e| e.to_string())?;
            dir.rewind();
            use std::os::unix::ffi::OsStrExt;
            let mut names: Vec<std::ffi::CString> = Vec::new();
            for ent in dir.by_ref() {
                let Ok(ent) = ent else { continue };
                let n = ent.file_name().to_bytes();
                if n == b"." || n == b".." {
                    continue;
                }
                names.push(ent.file_name().to_owned());
            }
            let mut out = Vec::new();
            for cname in names {
                let Ok(st) =
                    rustix::fs::statat(&self.fd, cname.as_c_str(), AtFlags::SYMLINK_NOFOLLOW)
                else {
                    continue;
                };
                let name = std::ffi::OsStr::from_bytes(cname.to_bytes()).to_os_string();
                let kind = match rustix::fs::FileType::from_raw_mode(st.st_mode) {
                    rustix::fs::FileType::RegularFile => EntryKind::File,
                    rustix::fs::FileType::Directory => EntryKind::Dir,
                    rustix::fs::FileType::Symlink => EntryKind::Symlink,
                    _ => EntryKind::Other,
                };
                let secs: i64 = st.st_mtime;
                let mtime = if secs >= 0 {
                    std::time::UNIX_EPOCH
                        + std::time::Duration::new(
                            secs as u64,
                            (st.st_mtime_nsec as u32) % 1_000_000_000,
                        )
                } else {
                    std::time::UNIX_EPOCH
                };
                out.push(EntryInfo {
                    name,
                    kind,
                    size: st.st_size as u64,
                    mtime,
                });
            }
            Ok(out)
        }

        /// 删除句柄目录里的一个空子目录（`unlinkat(AT_REMOVEDIR)`）；非空 / 不是目录 → Err。
        pub fn remove_empty_dir(&self, name: impl AsRef<std::ffi::OsStr>) -> Result<(), String> {
            let name: &std::ffi::OsStr = name.as_ref();
            check_name(name)?;
            rustix::fs::unlinkat(&self.fd, name, AtFlags::REMOVEDIR)
                .map_err(|e| io_err(e).to_string())
        }

        /// 删除句柄目录里的 `name`（对符号链接只删链接本身）；不存在视为成功。
        pub fn remove_file_if_exists(
            &self,
            name: impl AsRef<std::ffi::OsStr>,
        ) -> Result<(), String> {
            let name: &std::ffi::OsStr = name.as_ref();
            check_name(name)?;
            match rustix::fs::unlinkat(&self.fd, name, AtFlags::empty()) {
                Ok(()) => Ok(()),
                Err(e) if e == rustix::io::Errno::NOENT => Ok(()),
                Err(e) => Err(io_err(e).to_string()),
            }
        }
    }
}

// 非 unix 平台退化实现：**仅为通过编译，当前不支持这些平台**——没有沙盒（沙盒只有 macOS
// seatbelt），也没有任何抗换链保证：按路径操作、临时文件用固定名（不随机、不 O_EXCL）。
// 真要支持这些平台，必须用对应平台的句柄式 API 重写，不能沿用这里的实现。
#[cfg(not(unix))]
pub fn identity_of_real_dir(path: &Path) -> Result<DirIdentity, String> {
    let m = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !m.file_type().is_dir() {
        return Err(format!("不是真实目录（链接或非目录）：{}", path.display()));
    }
    Ok(DirIdentity { dev: 0, ino: 0 })
}

#[cfg(not(unix))]
pub struct DirHandle {
    path: std::path::PathBuf,
}

#[cfg(not(unix))]
impl DirHandle {
    pub fn open_expecting(path: &Path, _expected: DirIdentity) -> Result<Self, String> {
        identity_of_real_dir(path)?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }
    pub fn open_subdir_creating(&self, name: &str) -> Result<DirHandle, String> {
        let p = self.path.join(name);
        std::fs::create_dir_all(&p).map_err(|e| e.to_string())?;
        identity_of_real_dir(&p)?;
        Ok(DirHandle { path: p })
    }
    pub fn identity(&self) -> Result<DirIdentity, String> {
        Ok(DirIdentity { dev: 0, ino: 0 })
    }
    pub fn write_file_replacing(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let tmp = self.path.join(format!(".{name}.tmp"));
        std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, self.path.join(name)).map_err(|e| e.to_string())
    }
    pub fn open_subdir(&self, name: impl AsRef<std::ffi::OsStr>) -> Result<DirHandle, String> {
        let p = self.path.join(name);
        identity_of_real_dir(&p)?;
        Ok(DirHandle { path: p })
    }
    pub fn entries(&self) -> Result<Vec<EntryInfo>, String> {
        let mut out = Vec::new();
        for ent in std::fs::read_dir(&self.path)
            .map_err(|e| e.to_string())?
            .flatten()
        {
            let Ok(m) = std::fs::symlink_metadata(ent.path()) else {
                continue;
            };
            let ft = m.file_type();
            let kind = if ft.is_symlink() {
                EntryKind::Symlink
            } else if ft.is_dir() {
                EntryKind::Dir
            } else if ft.is_file() {
                EntryKind::File
            } else {
                EntryKind::Other
            };
            out.push(EntryInfo {
                name: ent.file_name(),
                kind,
                size: m.len(),
                mtime: m.modified().unwrap_or(std::time::UNIX_EPOCH),
            });
        }
        Ok(out)
    }
    pub fn remove_empty_dir(&self, name: impl AsRef<std::ffi::OsStr>) -> Result<(), String> {
        std::fs::remove_dir(self.path.join(name)).map_err(|e| e.to_string())
    }
    pub fn remove_file_if_exists(&self, name: impl AsRef<std::ffi::OsStr>) -> Result<(), String> {
        match std::fs::remove_file(self.path.join(name)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

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

    fn check_name(name: &str) -> Result<(), String> {
        if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
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
            .map_err(|e| format!("打开目录失败（可能被替换成了符号链接）：{}：{e}", path.display()))?;
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
            check_name(name)?;
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
            check_name(name)?;
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

        /// 删除句柄目录里的 `name`（对符号链接只删链接本身）；不存在视为成功。
        pub fn remove_file_if_exists(&self, name: &str) -> Result<(), String> {
            check_name(name)?;
            match rustix::fs::unlinkat(&self.fd, name, AtFlags::empty()) {
                Ok(()) => Ok(()),
                Err(e) if e == rustix::io::Errno::NOENT => Ok(()),
                Err(e) => Err(io_err(e).to_string()),
            }
        }
    }
}

// 非 unix 平台没有沙盒（沙盒只有 macOS seatbelt），这里退化为按路径操作，仅保证能编译。
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
    pub fn remove_file_if_exists(&self, name: &str) -> Result<(), String> {
        match std::fs::remove_file(self.path.join(name)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

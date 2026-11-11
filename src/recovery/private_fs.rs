//! Descriptor-relative private storage. No recovery operation follows links.
use anyhow::{Context, Result, bail, ensure};
use serde::{
    Deserialize,
    de::{MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
use std::{
    ffi::{CString, OsStr},
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path, PathBuf},
};

pub const MAX_DOCUMENT: u64 = 32 * 1024 * 1024;

#[derive(Debug)]
pub struct Directory {
    file: File,
    pub path: PathBuf,
}
fn cstr(s: &OsStr) -> Result<CString> {
    Ok(CString::new(s.as_bytes())?)
}
fn checked(fd: i32) -> Result<File> {
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: successful open/openat returned a new owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn directory(file: File, path: PathBuf, private: bool) -> Result<Directory> {
    let m = file.metadata()?;
    ensure!(m.is_dir(), "recovery directory required");
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    if private {
        ensure!(
            m.uid() == uid && m.mode() & 0o777 == 0o700,
            "recovery directory must be owned and mode 0700"
        );
    } else {
        ensure!(
            m.mode() & 0o022 == 0 || m.mode() & libc::S_ISVTX != 0,
            "unsafe recovery ancestor"
        );
    }
    Ok(Directory { file, path })
}
impl Directory {
    pub fn open(path: &Path, create: bool) -> Result<Self> {
        ensure!(path.is_absolute(), "recovery path must be absolute");
        let root = cstr(OsStr::new("/"))?;
        // SAFETY: root is a valid NUL-terminated string.
        let mut current = directory(
            checked(unsafe {
                libc::open(
                    root.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                )
            })?,
            PathBuf::from("/"),
            false,
        )?;
        let parts: Vec<_> = path.components().collect();
        for (i, part) in parts.iter().enumerate() {
            match part {
                Component::RootDir => continue,
                Component::Normal(name) => {
                    current = current.child_impl(name, create, i + 1 == parts.len())?;
                }
                _ => bail!("non-normal recovery path"),
            }
        }
        directory(current.file, current.path, true)
    }
    fn child_impl(&self, name: &OsStr, create: bool, private: bool) -> Result<Self> {
        let n = cstr(name)?;
        if create {
            // SAFETY: live directory fd and valid relative component.
            let result = unsafe { libc::mkdirat(self.file.as_raw_fd(), n.as_ptr(), 0o700) };
            if result == 0 {
                self.file.sync_all()?;
            }
            if result < 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        // SAFETY: valid fd/string; O_NOFOLLOW also protects the last component.
        let f = checked(unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                n.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        })?;
        directory(f, self.path.join(name), private)
    }
    pub fn child(&self, name: &str, create: bool) -> Result<Self> {
        component(name)?;
        self.child_impl(OsStr::new(name), create, true)
    }
    pub fn file(&self, name: &str, create: bool) -> Result<File> {
        component(name)?;
        let n = CString::new(name)?;
        let flags = libc::O_RDWR
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if create { libc::O_CREAT } else { 0 };
        // SAFETY: live fd, valid component and specified creation mode.
        let f = checked(unsafe { libc::openat(self.file.as_raw_fd(), n.as_ptr(), flags, 0o600) })?;
        validate_file(&f, MAX_DOCUMENT)?;
        Ok(f)
    }
    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        let mut f = self.file(name, false)?;
        let mut b = Vec::new();
        std::io::Read::by_ref(&mut f)
            .take(MAX_DOCUMENT + 1)
            .read_to_end(&mut b)?;
        ensure!(
            b.len() as u64 <= MAX_DOCUMENT,
            "oversized recovery document"
        );
        Ok(b)
    }
    pub fn publish(&self, name: &str, bytes: &[u8]) -> Result<()> {
        component(name)?;
        ensure!(
            bytes.len() as u64 <= MAX_DOCUMENT,
            "oversized recovery publication"
        );
        let temp = CString::new(format!(".{}.tmp", uuid::Uuid::new_v4()))?;
        // SAFETY: O_EXCL prevents substitution of the private temporary file.
        let mut f = checked(unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                temp.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        })?;
        let target = CString::new(name)?;
        let result = (|| -> Result<()> {
            f.write_all(bytes)?;
            f.sync_all()?;
            // SAFETY: both paths are components relative to the held directory.
            ensure!(
                unsafe {
                    libc::renameat(
                        self.file.as_raw_fd(),
                        temp.as_ptr(),
                        self.file.as_raw_fd(),
                        target.as_ptr(),
                    )
                } == 0,
                "recovery atomic rename failed: {}",
                std::io::Error::last_os_error()
            );
            self.file.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            // SAFETY: only the temporary name created above is removed.
            unsafe { libc::unlinkat(self.file.as_raw_fd(), temp.as_ptr(), 0) };
        }
        result.context("durable recovery publication failed")
    }
    pub fn sync(&self) -> Result<()> {
        Ok(self.file.sync_all()?)
    }
}
pub fn component(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 255
            && !name.contains(['/', '\\', '\0'])
            && name != "."
            && name != "..",
        "invalid recovery component"
    );
    Ok(())
}
pub fn validate_file(f: &File, max: u64) -> Result<()> {
    let m = f.metadata()?;
    // SAFETY: geteuid has no preconditions.
    ensure!(
        m.is_file()
            && m.uid() == unsafe { libc::geteuid() }
            && m.mode() & 0o777 == 0o600
            && m.nlink() == 1
            && m.len() <= max,
        "unsafe or oversized recovery file"
    );
    Ok(())
}
pub fn lock(file: &File, nonblocking: bool) -> Result<()> {
    // SAFETY: flock acts on a live owned descriptor.
    let rc = unsafe {
        libc::flock(
            file.as_raw_fd(),
            libc::LOCK_EX | if nonblocking { libc::LOCK_NB } else { 0 },
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

/// Reject duplicate properties at every depth, before deserializing a contract.
struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Unique;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("unambiguous bounded JSON")
            }
            fn visit_map<M: MapAccess<'de>>(
                self,
                mut map: M,
            ) -> std::result::Result<Unique, M::Error> {
                let mut out = serde_json::Map::new();
                while let Some((k, Unique(v))) = map.next_entry::<String, Unique>()? {
                    if out.insert(k, v).is_some() {
                        return Err(serde::de::Error::custom("duplicate recovery property"));
                    }
                }
                Ok(Unique(Value::Object(out)))
            }
            fn visit_seq<S: SeqAccess<'de>>(
                self,
                mut seq: S,
            ) -> std::result::Result<Unique, S::Error> {
                let mut out = vec![];
                while let Some(Unique(v)) = seq.next_element()? {
                    out.push(v);
                }
                Ok(Unique(Value::Array(out)))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::String(v.into())))
            }
            fn visit_string<E: serde::de::Error>(
                self,
                v: String,
            ) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::String(v)))
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::Bool(v)))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Unique, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| Unique(Value::Number(n)))
                    .ok_or_else(|| E::custom("invalid number"))
            }
            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_none<E: serde::de::Error>(self) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
        }
        d.deserialize_any(V)
    }
}
pub fn parse<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    ensure!(
        bytes.len() as u64 <= MAX_DOCUMENT,
        "oversized recovery document"
    );
    let Unique(value) = serde_json::from_slice(bytes)?;
    Ok(serde_json::from_value(value)?)
}

/// Serialize through a bounded writer, stopping before an oversized document
/// can allocate memory proportional to all accumulated completion records.
pub fn serialize<T: serde::Serialize>(value: &T) -> Result<Vec<u8>> {
    struct Bounded(Vec<u8>);
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0.len().saturating_add(bytes.len()) as u64 > MAX_DOCUMENT {
                return Err(std::io::Error::other("oversized recovery serialization"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Bounded(Vec::new());
    serde_json::to_writer(&mut writer, value)?;
    Ok(writer.0)
}

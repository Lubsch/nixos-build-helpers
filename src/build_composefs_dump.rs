//! Replacement for nixos/modules/system/etc/build-composefs-dump.py.
//!
//! Builds a composefs dump (see composefs-dump(5)) describing /etc and prints it to stdout.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};
use nanoserde::DeJson;

use crate::{file_name, glob};

/// Regular files at or below this size are inlined into the metadata image.
/// Must match INLINE_CONTENT_MAX in upstream's build-composefs-dump.py and etc.nix.
const INLINE_CONTENT_MAX: u64 = 4096;
/// A high approximation of the size of a symlink, as upstream uses.
const SYMLINK_SIZE: u64 = 100;
const DIR_SIZE: u64 = 4096;

#[derive(DeJson)]
pub struct Config {
    entries: Vec<Entry>,
}

#[derive(DeJson)]
struct Entry {
    target: String,
    source: String,
    mode: String,
    uid: u64,
    gid: u64,
}

/// The file type as the octal prefix of `st_mode`.
#[derive(Clone, Copy)]
enum FileType {
    Directory = 4,
    File = 10,
    Symlink = 12,
}

/// One line of the dump. rdev, nlink, mtime and digest are constant.
struct DumpEntry {
    size: u64,
    filetype: FileType,
    mode: String,
    uid: u64,
    gid: u64,
    payload: String,
    content: String,
}

impl DumpEntry {
    fn new(
        entry: &Entry,
        size: u64,
        filetype: FileType,
        mode: &str,
        payload: &str,
    ) -> Result<Self> {
        if !(matches!(mode.len(), 3 | 4) && u32::from_str_radix(mode, 8).is_ok()) {
            bail!("mode should be 3 or 4 octal digits, got: {mode:?}");
        }
        Ok(Self {
            size,
            filetype,
            mode: mode.to_string(),
            uid: entry.uid,
            gid: entry.gid,
            payload: payload.to_string(),
            content: "-".to_string(),
        })
    }

    fn write_line(&self, path: &str, out: &mut String) {
        let Self {
            size,
            filetype,
            mode,
            uid,
            gid,
            payload,
            content,
        } = self;
        let ft = *filetype as u8;
        // PATH SIZE MODE NLINK UID GID RDEV MTIME PAYLOAD CONTENT DIGEST
        writeln!(
            out,
            "{path} {size} {ft}{mode:0>4} 1 {uid} {gid} 0 1.0 {payload} {content} -"
        )
        .unwrap();
    }
}

pub fn run(config: Config) -> Result<()> {
    let dump = build_dump(config)?;
    std::io::stdout().lock().write_all(dump.as_bytes())?;
    Ok(())
}

fn build_dump(config: Config) -> Result<String> {
    let mut paths: BTreeMap<String, DumpEntry> = BTreeMap::new();

    for entry in &config.entries {
        let target = normalize_path(&entry.target);
        let source = &entry.source;

        if source.contains('*') {
            for glob_source in glob(source)? {
                let glob_target =
                    format!("{target}/{}", file_name(&glob_source)?.to_str().unwrap());
                let payload = glob_source.to_str().unwrap();
                let e = DumpEntry::new(entry, SYMLINK_SIZE, FileType::Symlink, "0777", payload)?;
                insert(&mut paths, glob_target, e, entry)?;
            }
            continue;
        }

        let e = if matches!(entry.mode.as_str(), "symlink" | "direct-symlink") {
            DumpEntry::new(entry, SYMLINK_SIZE, FileType::Symlink, "0777", source)?
        } else if Path::new(source).is_dir() {
            DumpEntry::new(entry, DIR_SIZE, FileType::Directory, &entry.mode, source)?
        } else {
            let size = fs::metadata(source)
                .with_context(|| format!("stat {source}"))?
                .len();
            if size == 0 {
                // mkcomposefs treats size 0 as an empty inline file.
                DumpEntry::new(entry, 0, FileType::File, &entry.mode, "-")?
            } else if size <= INLINE_CONTENT_MAX {
                let raw = fs::read(source)?;
                let mut e =
                    DumpEntry::new(entry, raw.len() as u64, FileType::File, &entry.mode, "-")?;
                e.content = escape_dump_field(&raw);
                e
            } else {
                // Served from the basedir lower layer, which mirrors the target path.
                let payload = target.strip_prefix('/').unwrap();
                DumpEntry::new(entry, size, FileType::File, &entry.mode, payload)?
            }
        };
        insert(&mut paths, target, e, entry)?;
    }

    let mut dump = String::from("/ 4096 40755 1 0 0 0 0.0 - - -\n"); // root directory
    for (path, e) in &paths {
        e.write_line(path, &mut dump);
    }
    Ok(dump)
}

/// Insert `e` at `path`, plus its leading directories, which mkcomposefs needs listed explicitly.
/// Later entries overwrite earlier ones, like upstream's dict.
fn insert(
    paths: &mut BTreeMap<String, DumpEntry>,
    path: String,
    e: DumpEntry,
    entry: &Entry,
) -> Result<()> {
    for dir in leading_directories(&path) {
        paths.insert(
            dir,
            DumpEntry::new(entry, DIR_SIZE, FileType::Directory, "0755", "-")?,
        );
    }
    paths.insert(path, e);
    Ok(())
}

/// `"/" + os.path.normpath(path).lstrip("/")`
fn normalize_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|p| *p != "..") => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    format!("/{}", parts.join("/"))
}

/// The leading directories of an absolute path: "/a/b/c" -> ["/a", "/a/b"]
fn leading_directories(path: &str) -> Vec<String> {
    let mut dirs: Vec<String> = Path::new(path)
        .ancestors()
        .skip(1)
        .filter(|p| !matches!(p.to_str(), Some("" | "/")))
        .map(|p| p.to_str().unwrap().to_string())
        .collect();
    dirs.reverse();
    dirs
}

/// Escape raw bytes for a composefs-dump field. Fields are separated by spaces, '\' escapes,
/// and a bare '-' means "unset", so all of these, '=' and non-printable bytes are escaped.
fn escape_dump_field(data: &[u8]) -> String {
    assert!(!data.is_empty(), "empty content must be emitted as '-'");
    if data == b"-" {
        return String::from(r"\x2d");
    }
    let mut out = String::with_capacity(data.len());
    for &b in data {
        match b {
            b'\\' => out.push_str(r"\\"),
            b'\n' => out.push_str(r"\n"),
            b'\r' => out.push_str(r"\r"),
            b'\t' => out.push_str(r"\t"),
            b' ' | b'=' => write!(out, "\\x{b:02x}").unwrap(),
            0x20..=0x7e => out.push(b as char),
            _ => write!(out, "\\x{b:02x}").unwrap(),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_dirs() {
        assert_eq!(
            leading_directories("/alsa/conf.d/50-pipewire.conf"),
            ["/alsa", "/alsa/conf.d"]
        );
        assert!(leading_directories("/hosts").is_empty());
    }

    #[test]
    fn normalize() {
        assert_eq!(normalize_path("hosts"), "/hosts");
        assert_eq!(normalize_path("//a/./b//c/"), "/a/b/c");
        assert_eq!(normalize_path("a/../b"), "/b");
        assert_eq!(normalize_path("../x"), "/../x"); // like python's normpath
    }

    #[test]
    fn escaping() {
        assert_eq!(escape_dump_field(b"-"), r"\x2d");
        assert_eq!(escape_dump_field(b"--"), "--");
        assert_eq!(escape_dump_field(b"a b=c\n\t\\\r"), r"a\x20b\x3dc\n\t\\\r");
        assert_eq!(escape_dump_field("ä\x7f".as_bytes()), r"\xc3\xa4\x7f");
    }
}

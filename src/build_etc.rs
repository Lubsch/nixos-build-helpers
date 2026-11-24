//! Replacement for the bash builder of `system.build.etc` (nixos/modules/system/etc/etc.nix).

use std::fs::{create_dir_all, read_link, write};
use std::os::unix::fs::symlink;
use std::path::Path;

use anyhow::{Context, Result};
use nanoserde::DeJson;

use crate::{file_name, glob};

#[derive(Debug, DeJson)]
pub struct Config {
    entries: Vec<Entry>,
}

#[derive(Debug, DeJson)]
struct Entry {
    source: String,
    target: String,
    mode: String,
    user: String,
    group: String,
}

pub fn run(config: Config, out: &Path) -> Result<()> {
    let etc = out.join("etc");
    create_dir_all(&etc)?;
    for entry in config.entries {
        make_etc_entry(&entry, &etc).with_context(|| format!("etc entry {:?}", entry.target))?;
    }
    Ok(())
}

fn make_etc_entry(entry: &Entry, etc: &Path) -> Result<()> {
    let target = etc.join(&entry.target);

    if entry.source.contains('*') {
        create_dir_all(&target)?;
        for source in glob(&entry.source)? {
            symlink(&source, target.join(file_name(&source)?))?;
        }
        return Ok(());
    }

    create_dir_all(target.parent().context("target has no parent")?)?;
    // Upstream tests `-e`, which follows symlinks.
    if !target.exists() {
        symlink(&entry.source, &target)?;
    } else {
        println!("duplicate entry {:?} -> {:?}", entry.target, entry.source);
        let existing = read_link(&target)?;
        if existing != Path::new(&entry.source) {
            // Upstream sets `ret=1` here but never uses it, so this isn't fatal either.
            println!(
                "mismatched duplicate entry {existing:?} <-> {:?}",
                entry.source
            );
        }
    }

    if entry.mode != "symlink" {
        write(
            target.with_added_extension("mode"),
            format!("{}\n", entry.mode),
        )?;
        write(
            target.with_added_extension("uid"),
            format!("{}\n", entry.user),
        )?;
        write(
            target.with_added_extension("gid"),
            format!("{}\n", entry.group),
        )?;
    }
    Ok(())
}

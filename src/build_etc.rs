use anyhow::Context;
use glob::glob;
use nanoserde::DeJson;

use std::fs::read_to_string;
use std::{
    env,
    fs::{create_dir_all, read_link, write},
    os::unix::fs::symlink,
    path::{Path, PathBuf},
};

#[derive(Debug, DeJson)]
struct Config {
    #[nserde(rename = "etc'")]
    etc: Vec<Entry>,
}

#[derive(Debug, DeJson)]
struct Entry {
    source: String,
    target: String,
    mode: String,
    user: String,
    group: String,
}

fn make_etc_entry(entry: Entry, etc: &Path) -> anyhow::Result<()> {
    let target = etc.join(entry.target);

    if entry.source.contains('*') {
        create_dir_all(&target)?;
        for entry in glob(&entry.source)? {
            let entry = entry?;
            let target = target.join(entry.file_name().context("")?);
            symlink(&entry, target)?;
        }
    } else {
        create_dir_all(target.parent().context("")?)?;
        if let Err(e) = symlink(&entry.source, &target) {
            if target.try_exists()? {
                println!("duplicate entry {target:?} -> {:?}", entry.source);
                let link_content = read_link(&target)?;
                if link_content != entry.source {
                    println!(
                        "mismatched duplicate entry {link_content:?} <-> {:?}",
                        entry.source
                    );
                }
            } else {
                anyhow::bail!("symlink error: {e:?}");
            }
        }

        // NOTE differs from original that it fails here when there are duplicates
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
    }
    Ok(())
}

pub fn run() -> anyhow::Result<()> {
    let config_path = env::var("NIX_ATTRS_JSON_FILE").context("No json config in env")?;
    let config_str = read_to_string(config_path).context("Config isn't accessible")?;
    let config = Config::deserialize_json(&config_str).context("Config is invalid")?;
    let entries = config.etc;

    let etc = PathBuf::from(env::var("out")?).join("etc");
    create_dir_all(&etc)?;

    for entry in entries {
        make_etc_entry(entry, &etc)?;
    }
    Ok(())
}

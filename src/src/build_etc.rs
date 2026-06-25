use std::{env::{self, Args}, fs::{create_dir_all, read, read_link, write}, os::unix::fs::symlink, path::{Path, PathBuf}};
use std::collections::BTreeMap;
use glob::glob;
use anyhow::Context;

#[derive(Debug, serde::Deserialize)]
struct Entry {
    source: PathBuf,
    target: PathBuf,
    mode: String,
    user: String,
    group: String,
}

fn make_etc_entry(entry: Entry, etc: &Path) -> anyhow::Result<()> {
    let target = etc.join(entry.target);

    if entry.source.to_str().context("")?.contains('*') {
        create_dir_all(&target)?;
        for entry in glob(entry.source.to_str().context("")?)? {
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
                    println!("mismatched duplicate entry {link_content:?} <-> {:?}", entry.source);
                }
            } else {
                println!("symlink error: {e:?}");
            }
        }

        // NOTE differs from original that it fails here when there are duplicates
        if entry.mode != "symlink" {
            write(target.with_extension("mode"), format!("{}\n", entry.mode))?;
            write(target.with_extension("uid"), format!("{}\n", entry.user))?;
            write(target.with_extension("gid"), format!("{}\n", entry.group))?;
        }
    }
    Ok(())
}

pub fn run(mut _args: Args) -> anyhow::Result<()> {
    let config_path = std::env::var("NIX_ATTRS_JSON_FILE").context("No json config in env")?;
    let config_bytes =
        read(config_path).context("Config isn't accessible")?;
    let mut config: BTreeMap<String, serde_json::Value> = serde_json::from_slice(&config_bytes).context("Config is invalid")?;
    let entries: Vec<Entry> = serde_json::from_value(config.remove("etc'").unwrap()).unwrap();

    let etc = PathBuf::from(env::var("out")?).join("etc");
    create_dir_all(&etc)?;

    for entry in entries {
        make_etc_entry(entry, &etc)?;
    }
    Ok(())
}

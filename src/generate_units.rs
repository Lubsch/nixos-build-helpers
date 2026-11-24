//! Replacement for `generateUnits` in nixos/lib/systemd-lib.nix (system-, user-, initrd-, nspawn-units).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, create_dir_all, read_link, remove_file};
use std::io::ErrorKind::AlreadyExists;
use std::os::unix::fs::symlink;
use std::path::Path;

use anyhow::{Context, Result, bail};
use nanoserde::DeJson;

use crate::{file_name, glob_dir};

#[derive(DeJson, Debug)]
struct Unit {
    unit: String,
    #[nserde(rename = "overrideStrategy")]
    override_strategy: String,
    aliases: Vec<String>,
    #[nserde(rename = "wantedBy")]
    wanted_by: Vec<String>,
    #[nserde(rename = "upheldBy")]
    upheld_by: Vec<String>,
    #[nserde(rename = "requiredBy")]
    required_by: Vec<String>,
}

#[derive(DeJson, Debug)]
pub struct Args {
    #[nserde(rename = "allowCollisions")]
    allow_collisions: bool,
    #[nserde(rename = "type")]
    units_type: String,
    units: BTreeMap<String, Unit>,
    #[nserde(rename = "upstreamUnits")]
    upstream_units: Vec<String>,
    #[nserde(rename = "upstreamWants")]
    upstream_wants: Vec<String>,
    /// A set, like upstream's associative array.
    packages: BTreeSet<String>,
    package: String,
    #[nserde(rename = "defaultUnit")]
    default_unit: String,
    #[nserde(rename = "ctrlAltDelUnit")]
    ctrl_alt_del_unit: String,
}

pub fn run(args: Args, out: &Path) -> Result<()> {
    let type_dir = match args.units_type.as_str() {
        "system" | "initrd" => "system",
        "user" => "user",
        "nspawn" => "nspawn",
        t => bail!("type must be one of system | initrd | user | nspawn, got {t:?}"),
    };
    create_dir_all(out)?;
    let upstream = Path::new(&args.package)
        .join("example/systemd")
        .join(type_dir);

    // Copy the upstream systemd units we're interested in.
    for unit in &args.upstream_units {
        let p = upstream.join(unit);
        let out_p = out.join(file_name(&p)?);
        let meta = p
            .symlink_metadata()
            .with_context(|| format!("missing {p:?}"))?;
        if !p.exists() {
            bail!("missing {p:?}");
        }
        if meta.is_symlink() && !read_link(&p)?.starts_with("..") {
            copy_no_deref(&p, &out_p)?;
        } else if meta.is_symlink() {
            symlink(p.canonicalize()?, out_p)?; // ln -s "$(readlink -f "$fn")"
        } else {
            symlink(&p, out_p)?;
        }
    }

    // Copy .wants links, but only those that point to units that we're interested in.
    for wants in &args.upstream_wants {
        let p = upstream.join(wants);
        if !p.exists() {
            bail!("missing {p:?}");
        }
        let out_dir = out.join(file_name(&p)?);
        fs::create_dir(&out_dir)?;
        for link in glob_dir(&p)? {
            let out_link = out_dir.join(file_name(&link)?);
            copy_no_deref(&link, &out_link)?;
            if !out_link.exists() {
                remove_file(&out_link)?; // dangling
            }
        }
    }

    // Symlink all units provided listed in systemd.packages.
    for pkg in &args.packages {
        let mut entries = glob_dir(&Path::new(pkg).join("etc/systemd").join(type_dir))?;
        entries.extend(glob_dir(
            &Path::new(pkg).join("lib/systemd").join(type_dir),
        )?);
        for p in entries {
            let name = file_name(&p)?;
            // upstream: [[ "$fn" =~ .wants$ ]]
            if p.as_os_str().as_encoded_bytes().ends_with(b"wants") {
                continue;
            }
            if p.is_dir() {
                lndir(&p, &out.join(name))?;
            } else {
                symlink(&p, out.join(name))?;
            }
        }
    }

    // Symlink units defined by systemd.units. With asDropinIfExists, units that are also provided
    // by systemd or systemd.packages become <unit>.d/overrides.conf, extending the upstream unit.
    for u in args.units.values() {
        // A unit derivation contains exactly one file, the unit.
        let dir = Path::new(&u.unit);
        let src = fs::read_dir(dir)?
            .next()
            .context("empty unit derivation")??
            .path();
        let name = file_name(&src)?;
        let out_p = out.join(name);
        let as_dropin = || -> Result<()> {
            let dropin_dir = out_p.with_added_extension("d");
            create_dir_all(&dropin_dir)?;
            symlink(&src, dropin_dir.join("overrides.conf"))?;
            Ok(())
        };
        match u.override_strategy.as_str() {
            "asDropinIfExists" if !out_p.exists() => symlink_force(&src, &out_p)?,
            "asDropinIfExists" if src.canonicalize()? == Path::new("/dev/null") => {
                symlink_force(Path::new("/dev/null"), &out_p)?
            }
            "asDropinIfExists" if args.allow_collisions => as_dropin()?,
            "asDropinIfExists" => bail!("Found multiple derivations configuring {name:?}!"),
            "asDropin" => as_dropin()?,
            s => bail!("unknown overrideStrategy {s:?}"),
        }
    }

    // Create service aliases from aliases option.
    for (name, u) in &args.units {
        for alias in &u.aliases {
            symlink_force(Path::new(name), &out.join(alias))?;
        }
    }

    // Create .wants, .upholds and .requires symlinks from wantedBy, upheldBy and requiredBy.
    for (name, u) in &args.units {
        let deps = [
            (&u.wanted_by, "wants"),
            (&u.upheld_by, "upholds"),
            (&u.required_by, "requires"),
        ];
        for (targets, suffix) in deps {
            for target in targets {
                let dir = out.join(target).with_added_extension(suffix);
                create_dir_all(&dir)?;
                symlink_force(Path::new(&format!("../{name}")), &dir.join(name))?;
            }
        }
    }

    if args.units_type == "system" {
        // Stupid misc. symlinks.
        symlink(&args.default_unit, out.join("default.target"))?;
        symlink(&args.ctrl_alt_del_unit, out.join("ctrl-alt-del.target"))?;
        symlink(
            "../remote-fs.target",
            out.join("multi-user.target.wants/remote-fs.target"),
        )?;
    }

    Ok(())
}

/// `cp -pd`
fn copy_no_deref(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.symlink_metadata()?.is_symlink() {
        symlink(read_link(src)?, dst)
    } else {
        fs::copy(src, dst).map(|_| ())
    }
}

/// `ln -sfn`
fn symlink_force(original: &Path, link: &Path) -> std::io::Result<()> {
    match symlink(original, link) {
        Err(e) if e.kind() == AlreadyExists => {
            remove_file(link)?;
            symlink(original, link)
        }
        x => x,
    }
}

/// `mkdir -p dst && lndir src dst`: mirror the tree, linking files to `src`, but copying symlinks
/// as they are (including symlinked directories). Existing entries are kept.
fn lndir(src: &Path, dst: &Path) -> std::io::Result<()> {
    create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let (from, to) = (entry.path(), dst.join(entry.file_name()));
        let ft = entry.file_type()?;
        let res = if ft.is_dir() {
            lndir(&from, &to)
        } else if ft.is_symlink() {
            symlink(read_link(&from)?, &to)
        } else {
            symlink(&from, &to)
        };
        match res {
            Err(e) if e.kind() == AlreadyExists => {}
            x => x?,
        }
    }
    Ok(())
}

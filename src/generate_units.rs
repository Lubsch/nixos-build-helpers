use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, create_dir_all, read_link, remove_file};
use std::io::ErrorKind::AlreadyExists;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use anyhow::Context;
use nanoserde::DeJson;

#[derive(DeJson, Debug)]
struct Config {
    #[nserde(rename = "generate-units-args")]
    generate_units_args: Args,
}

#[derive(DeJson, Debug)]
struct Unit {
    unit: String,
    #[nserde(rename = "overrideStrategy")]
    override_strategy: Option<String>,
    aliases: Vec<String>,
    #[nserde(rename = "wantedBy")]
    wanted_by: Vec<String>,
    #[nserde(rename = "upheldBy")]
    upheld_by: Vec<String>,
    #[nserde(rename = "requiredBy")]
    required_by: Vec<String>,
}

#[derive(DeJson, Debug)]
struct Args {
    #[nserde(rename = "allowCollisions")]
    allow_collisions: bool,
    #[nserde(rename = "type")]
    units_type: String,
    units: BTreeMap<String, Unit>,
    #[nserde(rename = "upstreamUnits")]
    upstream_units: Vec<String>,
    #[nserde(rename = "upstreamWants")]
    upstream_wants: Vec<String>,
    packages: BTreeSet<String>,
    package: String,
    #[nserde(rename = "defaultUnit")]
    default_unit: String,
    #[nserde(rename = "ctrlAltDelUnit")]
    ctrl_alt_del_unit: String,
}

fn copy_no_deref(src: &Path, dst: &Path) -> std::io::Result<()> {
    let meta = fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() {
        let target = fs::read_link(src)?;
        symlink(target, dst)?;
    } else {
        fs::copy(src, dst)?;
    }
    Ok(())
}

fn lnf(original: &Path, link: &Path) -> std::io::Result<()> {
    match symlink(original, link) {
        Err(ref e) if e.kind() == AlreadyExists => {
            remove_file(link)?;
            symlink(original, link)
        }
        x => x,
    }
}

fn lndir(src: &Path, dst: &Path) -> std::io::Result<()> {
    create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        // file_type() doesn't follow symlinks, matching lndir treating
        // symlinked dirs as links, not recursion targets
        let ft = entry.file_type()?;
        if ft.is_dir() {
            lndir(&from, &to)?;
        } else {
            // leaf (file or symlink) -> symlink pointing at source
            match symlink(&from, &to) {
                Err(ref e) if e.kind() == AlreadyExists => {} // lndir skips existing, warns
                x => x?,
            }
        }
    }
    Ok(())
}

pub fn run(mut _args: std::env::Args) -> anyhow::Result<()> {
    let config_path = std::env::var("NIX_ATTRS_JSON_FILE").context("No json config in env")?;
    let config_str = fs::read_to_string(config_path).context("Config isn't accessible")?;
    let config = Config::deserialize_json(&config_str).context("Config is invalid")?;
    let args = config.generate_units_args;

    let type_dir = match args.units_type.as_str() {
        "system" => Path::new("system"),
        "initrd" => Path::new("system"),
        "user" => Path::new("user"),
        "nspawn" => Path::new("nspawn"),
        _ => panic!("type must be one of system | initrd | user | nspawn"),
    };

    let out: PathBuf = std::env::var("out").unwrap().into();
    // let tmpdir = tempfile::tempdir()?;
    // let out: PathBuf = tmpdir.keep();
    // dbg!(&out);
    std::fs::create_dir_all(&out)?;

    for unit in args.upstream_units {
        let p = Path::new(&args.package)
            .join("example/systemd")
            .join(type_dir)
            .join(unit);
        let Ok(meta) = p.symlink_metadata() else {
            panic!("missing {p:?}");
        };
        if meta.is_symlink() {
            let target = read_link(&p)?;
            if target.iter().next() == Some(OsStr::new("..")) {
                symlink(&p, out.join(p.file_name().unwrap()))?;
            } else {
                copy_no_deref(&p, &out.join(p.file_name().unwrap()))?
            }
        } else {
            symlink(&p, out.join(p.file_name().unwrap()))?;
        }
    }

    for unit in args.upstream_wants {
        let p = Path::new(&args.package)
            .join("example/systemd")
            .join(type_dir)
            .join(unit);
        if !p.exists() {
            panic!("missing {p:?}");
        };
        let x = &out.join(p.file_name().unwrap());
        fs::create_dir(x)?;
        for i in glob::glob(p.join("*").to_str().unwrap()).unwrap() {
            let i = &i.unwrap();
            let y = x.join(i.file_name().unwrap());
            copy_no_deref(i, &y)?;
            // dangling symlink case
            if !y.exists() {
                remove_file(&y)?;
            }
        }
    }

    for pkg in args.packages {
        let (pkg, type_dir) = (pkg, type_dir.to_str().unwrap());

        for p in glob::glob(&format!("{pkg}/etc/systemd/{type_dir}/*"))
            .unwrap()
            .chain(glob::glob(&format!("{pkg}/lib/systemd/{type_dir}/*")).unwrap())
        {
            let p = p.unwrap();
            if p.file_name().unwrap().to_str().unwrap().ends_with(".wants") {
                continue;
            }
            if p.is_dir() {
                let target_dir = out.join(p.file_name().unwrap());
                create_dir_all(target_dir)?;

                lndir(&p, &out.join(p.file_name().unwrap()))?;
            } else {
                symlink(&p, out.join(p.file_name().unwrap()))?;
            }
        }
    }

    for u in args.units.values().filter(|u| {
        matches!(
            u.override_strategy.as_deref(),
            Some("asDropinIfExists") | None
        )
    }) {
        // There's guaranteed to be a unit file in there
        let unit = Path::new(&u.unit);
        let p = unit.read_dir()?.next().unwrap()?.path();
        let p = p.file_name().unwrap();
        let mut p_out = out.join(p);
        if p_out.exists() {
            if unit.join(p).canonicalize()? == Path::new("/dev/null") {
                remove_file(&p_out)?;
                symlink(Path::new("/dev/null"), p_out)?;
            } else {
                if args.allow_collisions {
                    p_out.as_mut_os_string().push(".d");
                    create_dir_all(&p_out)?;
                    symlink(unit.join(p), p_out.join("overrides.conf"))?;
                } else {
                    panic!("Found multiple derivations configuring {:?}", u.unit);
                }
            }
        } else {
            symlink(unit.join(p), p_out)?;
        }
    }

    for u in args
        .units
        .values()
        .filter(|u| matches!(u.override_strategy.as_deref(), Some("asDropin")))
    {
        let unit = Path::new(&u.unit);
        let p = unit.read_dir()?.next().unwrap()?.path();
        let p = p.file_name().unwrap();

        let mut p_out = out.join(p);
        p_out.as_mut_os_string().push(".d");
        create_dir_all(&p_out)?;
        symlink(unit.join(p), p_out.join("overrides.conf"))?;
    }

    for (name, u) in &args.units {
        for name2 in &u.aliases {
            lnf(Path::new(&name), &out.join(name2))?;
        }
    }

    for (name, u) in &args.units {
        for name2 in &u.wanted_by {
            let mut wants = out.join(name2);
            wants.as_mut_os_string().push(".wants");
            create_dir_all(&wants)?;
            lnf(Path::new(&format!("../{name}")), &wants.join(name))?;
        }
    }

    for (name, u) in &args.units {
        for name2 in &u.upheld_by {
            let mut upholds = out.join(name2);
            upholds.as_mut_os_string().push(".upholds");
            create_dir_all(&upholds)?;
            lnf(Path::new(&format!("../{name}")), &upholds.join(name))?;
        }
    }

    for (name, u) in &args.units {
        for name2 in &u.required_by {
            // TODO DRY this everywhere
            let mut requires = out.join(name2);
            requires.as_mut_os_string().push(".requires");
            create_dir_all(&requires)?;
            lnf(Path::new(&format!("../{name}")), &requires.join(name))?;
        }
    }

    if args.units_type == "system" {
        symlink(args.default_unit, out.join("default.target"))?;
        symlink(args.ctrl_alt_del_unit, out.join("ctrl-alt-del.target"))?;

        symlink(
            "../remote-fs.target",
            out.join("multi-user.target.wants/remote-fs.target"),
        )?;
    }

    Ok(())
}

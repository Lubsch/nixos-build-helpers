use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
};

use anyhow::Context as _;
use nanoserde::DeJson;

#[derive(Debug, DeJson)]
struct Outputs {
    out: String,
}

#[derive(Debug, DeJson)]
struct ChosenOutput {
    paths: Vec<String>,
    priority: i32,
}

#[derive(Debug, DeJson)]
struct Args {
    outputs: Outputs,
    #[nserde(rename = "extraPrefix")]
    extra_prefix: String,
    #[nserde(rename = "pathsToLink")]
    paths_to_link: Vec<String>,
    #[nserde(rename = "ignoreCollisions")]
    ignore_collisions: bool,
    #[nserde(rename = "checkCollisionContents")]
    check_collision_contents: bool,
    #[nserde(rename = "ignoreSingleFileOutputs")]
    ignore_single_file_outputs: bool,
    #[nserde(rename = "chosenOutputs")]
    chosen_outputs: Vec<ChosenOutput>,
    #[nserde(rename = "extraPathsFrom")]
    extra_paths_from: String,
    manifest: String,
}

#[derive(Default)]
struct Discovery {
    done: BTreeSet<String>,
    postponed: BTreeSet<String>,
    roots: Vec<Root>,
}

// Collision policy. The JSON only ever carries the user-facing bool
// (True/False); TrueDontWarn is set in code for the propagated phase,
// mirroring the Perl which passes ignoreCollisions = 2 there.
#[derive(Debug, Clone, Copy)]
enum IgnoreCollisions {
    True,
    False,
    TrueDontWarn,
}

// One package's contribution at a single rel_name.
struct Contribution {
    target: PathBuf,
    priority: i32,
    is_dir: bool,
    // Policy this contribution was walked under (explicit vs propagated).
    ignore_collisions: IgnoreCollisions,
}

// What a rel_name resolves to.
#[derive(Debug)]
enum Resolution {
    Symlink(PathBuf),
    Directory,
}

// A package root or sub-directory to walk, with its priority and policy.
type Root = (PathBuf, i32, IgnoreCollisions);

// One frontier item: a rel_name plus every directory contributing to it.
struct FrontierItem {
    rel_name: String,
    dirs: Vec<Root>,
}

// Is `path` at or below some pathsToLink entry? (keep its subtree)
fn is_in_paths_to_link(paths_to_link: &[String], path: &str) -> bool {
    paths_to_link
        .iter()
        .any(|p| path == p || (path.starts_with(p) && path.as_bytes().get(p.len()) == Some(&b'/')))
}

// Is `path` at or above some pathsToLink entry? (must descend through it)
// Empty path is an ancestor of everything, matching the Perl's `$path eq ""`.
fn has_paths_to_link(paths_to_link: &[String], path: &str) -> bool {
    path.is_empty()
        || paths_to_link.iter().any(|p| {
            p == path || (p.starts_with(path) && p.as_bytes().get(path.len()) == Some(&b'/'))
        })
}

// The "Urgh, hacky..." blocklist.
fn skip(paths_to_link: &[String], rel_name: &str) -> bool {
    let base = rel_name.rsplit('/').next();
    matches!(rel_name, "/propagated-build-inputs" | "/nix-support")
        || rel_name.ends_with("info/dir")
        || (rel_name.starts_with("/share/mime")
            && rel_name != "/share/mime"
            && !rel_name.starts_with("/share/mime/packages"))
        || matches!(base, Some("perllocal.pod" | "log"))
        || !(has_paths_to_link(paths_to_link, rel_name)
            || is_in_paths_to_link(paths_to_link, rel_name))
}

// Compare two files byte-for-byte; permission mismatch is treated as differing.
fn check_collision(a: &Path, b: &Path) -> anyhow::Result<bool> {
    let (ap, bp) = (
        fs::metadata(a)?.permissions(),
        fs::metadata(b)?.permissions(),
    );
    if ap != bp {
        eprintln!("different permissions in {a:?} and {b:?}: {ap:?} <-> {bp:?}");
        return Ok(false);
    }
    Ok(fs::read(a)? == fs::read(b)?)
}

// A genuine file/file collision: ignore / check-contents / die.
// `policy` is the *rival's* policy, matching the Perl which evaluates the
// collision under the flag of the contributor currently being merged in
fn handle_collision(
    a: &Contribution,
    b: &Contribution,
    policy: IgnoreCollisions,
    check_contents: bool,
) -> anyhow::Result<()> {
    match policy {
        IgnoreCollisions::True => {
            eprintln!(
                "colliding subpath (ignored): {:?} and {:?}",
                a.target, b.target
            );
            Ok(())
        }
        IgnoreCollisions::TrueDontWarn => Ok(()),
        IgnoreCollisions::False => {
            if check_contents && check_collision(&a.target, &b.target)? {
                Ok(())
            } else {
                anyhow::bail!(
                    "two given paths contain a conflicting subpath:\n  {:?} and\n  {:?}\n\
                     hint: this may be caused by two different versions of the same package",
                    a.target,
                    b.target,
                );
            }
        }
    }
}

// Shallow-read one directory: stat each child, no descent.
// Pure I/O on `target` only, no shared state, becomes par_iter later
fn read_level(
    rel_name: &str,
    target: &Path,
    priority: i32,
    ignore_collisions: IgnoreCollisions,
) -> anyhow::Result<Vec<(String, Contribution)>> {
    let Ok(entries) = fs::read_dir(target) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();

    for entry in entries.flatten() {
        let name = entry.file_name();
        let child_target = target.join(&name);
        let child_rel = [rel_name, &name.to_string_lossy()].join("/");

        let ftype = entry.file_type()?;
        let is_dir = if ftype.is_symlink() {
            fs::metadata(&child_target)
                .map(|m| m.is_dir())
                .unwrap_or(false)
        } else {
            ftype.is_dir()
        };

        out.push((
            child_rel,
            Contribution {
                target: child_target,
                priority,
                is_dir,
                ignore_collisions,
            },
        ));
    }
    Ok(out)
}

// Decide a single rel_name from all its contributors.
// Returns the resolution and, for a forced merge, the dirs to descend into.
fn resolve(
    mut contribs: Vec<Contribution>,
    check_contents: bool,
) -> anyhow::Result<(Resolution, Option<Vec<Root>>)> {
    // Lower priority number wins; stable tiebreak on target keeps output
    // deterministic regardless of walk order.
    contribs.sort_by_key(|c| c.priority);

    let winner = &contribs[0];

    // winner is a file: it shadows everything, single symlink.
    if !winner.is_dir {
        // Evaluate each same-priority rival against the winner, under the
        // rival's own policy (propagated rivals stay silent).
        for rival in contribs.iter().skip(1) {
            if rival.priority == winner.priority && rival.target != winner.target {
                handle_collision(winner, rival, rival.ignore_collisions, check_contents)?;
            }
        }
        return Ok((Resolution::Symlink(winner.target.clone()), None));
    }

    // After the winner is a directory, only directory contributors merge;
    // lower-priority files are shadowed and dropped.
    let dirs = contribs.iter().filter(|c| c.is_dir);

    // exactly one directory: link it whole, don't descend (laziness).
    if dirs.clone().count() == 1 {
        return Ok((Resolution::Symlink(winner.target.clone()), None));
    }

    // multiple directories must merge: real dir, descend into each.
    let to_descend = dirs
        .map(|c| (c.target.clone(), c.priority, c.ignore_collisions))
        .collect();
    Ok((Resolution::Directory, Some(to_descend)))
}

// The frontier loop: alternate shallow reads with a
// sequential, deterministic fold that owns the resolution map.
fn build_symlinks(
    roots: Vec<Root>,
    paths_to_link: &[String],
    check_contents: bool,
) -> anyhow::Result<BTreeMap<String, Resolution>> {
    let mut resolutions = BTreeMap::new();

    for p in paths_to_link {
        for ancestor in Path::new(p).ancestors() {
            let key = ancestor.to_str().unwrap().to_string();
            resolutions.entry(key).or_insert(Resolution::Directory);
        }
    }

    let mut frontier = vec![FrontierItem {
        rel_name: String::new(),
        dirs: roots,
    }];

    while !frontier.is_empty() {
        let levels: Vec<Vec<(String, Contribution)>> = frontier
            .iter()
            .flat_map(|item| {
                item.dirs
                    .iter()
                    .map(move |(target, prio, ic)| read_level(&item.rel_name, target, *prio, *ic))
            })
            .collect::<anyhow::Result<_>>()?;

        let mut by_rel: BTreeMap<String, Vec<Contribution>> = BTreeMap::new();
        for (rel, c) in levels.into_iter().flatten() {
            if skip(paths_to_link, &rel) {
                continue;
            }
            by_rel.entry(rel).or_default().push(c);
        }

        frontier.clear();
        for (rel, contribs) in by_rel {
            let (resolution, descend) = resolve(contribs, check_contents)?;
            if let Some(dirs) = descend {
                frontier.push(FrontierItem {
                    rel_name: rel.clone(),
                    dirs,
                });
            }
            resolutions.insert(rel, resolution);
        }
    }

    Ok(resolutions)
}

// Append a package root if not already seen; queue its propagated packages.
fn discover_root(
    d: &mut Discovery,
    pkg_dir: &str,
    priority: i32,
    policy: IgnoreCollisions,
    store_dir: &Path,
    ignore_single_file_outputs: bool,
) -> anyhow::Result<()> {

    if !d.done.insert(pkg_dir.to_string()) {
        return Ok(());
    }

    let pkg_dir = Path::new(pkg_dir);
    // A store path that is a plain file can't be merged.
    if pkg_dir.is_file() && pkg_dir.starts_with(store_dir) {
        if ignore_single_file_outputs {
            eprintln!("The store path {pkg_dir:?} is a file and can't be merged, ignoring it");
            return Ok(());
        }
        anyhow::bail!("The store path {pkg_dir:?} is a file and can't be merged!");
    }

    d.roots.push((pkg_dir.to_path_buf(), priority, policy));

    let propagated = pkg_dir.join("nix-support/propagated-user-env-packages");
    if let Ok(content) = fs::read_to_string(&propagated) {
        for p in content.split_whitespace() {
            if !d.done.contains(p) {
                d.postponed.insert(p.to_string());
            }
        }
    }
    Ok(())
}

pub fn run(mut _args_iter: std::env::Args) -> anyhow::Result<()> {
    let config_path = std::env::var("NIX_ATTRS_JSON_FILE").unwrap();
    // let config_path = args_iter.next().context("supply JSON config as arg")?;
    let config = fs::read_to_string(config_path).context("cannot open structured attrs JSON file")?;
    let args: Args = Args::deserialize_json(&config).context("config is invalid")?;

    // TODO get caller supplied
    let store_dir =
        PathBuf::from(std::env::var("NIX_STORE").unwrap_or_else(|_| "/nix/store".to_string()));

    let mut d = Discovery::default();
    let ignore_collisions = if args.ignore_collisions {
        IgnoreCollisions::True
    } else {
        IgnoreCollisions::False
    };

    // explicitly chosen packages, under the user's collision policy.
    for pkg in &args.chosen_outputs {
        for path in &pkg.paths {
            if Path::new(path).try_exists()? {
                discover_root(
                    &mut d,
                    path,
                    pkg.priority,
                    ignore_collisions,
                    &store_dir,
                    args.ignore_single_file_outputs,
                )?;
            }
        }
    }

    // propagated packages, lower priority, collisions always silent
    // priority is assigned by sorted order so it's deterministic regardless
    // of discovery order.
    let mut priority = 1000;
    while !d.postponed.is_empty() {
        let batch: Vec<String> = std::mem::take(&mut d.postponed).into_iter().collect();
        // BTreeSet iterates sorted, collect preserves that order.
        for pkg in batch {
            discover_root(
                &mut d,
                &pkg,
                priority,
                IgnoreCollisions::TrueDontWarn,
                &store_dir,
                args.ignore_single_file_outputs,
            )?;
            priority += 1;
        }
    }

    // extra paths from a file (directories only), priority 1000.
    if !args.extra_paths_from.is_empty() {
        let content = fs::read_to_string(&args.extra_paths_from)
            .with_context(|| format!("cannot open extra paths file {:?}", args.extra_paths_from))?;
        for pkg in content.lines() {
            if Path::new(pkg).is_dir() {
                discover_root(
                    &mut d,
                    pkg,
                    1000,
                    ignore_collisions,
                    &store_dir,
                    args.ignore_single_file_outputs,
                )?;
            }
        }
    }

    // walk all roots and resolve into the final symlink map.
    let resolutions = build_symlinks(d.roots, &args.paths_to_link, args.check_collision_contents)?;

    // realize, btreemap order guarantees parents precede children.

    let base = Path::new(&args.outputs.out).join(&args.extra_prefix);
    let abs = |rel: &str| base.join(rel.strip_prefix('/').unwrap_or(rel));

    let mut nr_links = 0;
    for (rel, res) in &resolutions {
        match res {
            Resolution::Directory => fs::create_dir(abs(rel))?,
            Resolution::Symlink(target) =>  {
                symlink(target, abs(rel))?;
                nr_links += 1;
            }
        }
    }

    eprintln!("created {nr_links} symlinks in user environment");

    if !args.manifest.is_empty() {
        symlink(&args.manifest, Path::new(&args.outputs.out).join("manifest"))
            .context("cannot create manifest")?;
    }

    Ok(())
}

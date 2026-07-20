use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fs::{self, read_to_string},
    os::unix::fs::symlink,
    path::{Path, PathBuf},
};

use anyhow::Context as _;
use nanoserde::DeJson;

pub fn run() -> anyhow::Result<()> {
    let config_path = env::var("NIX_ATTRS_JSON_FILE")
        .context("missing required environment variable NIX_ATTRS_JSON_FILE")?;
    let config = read_to_string(config_path).context("cannot read structured attrs JSON file")?;
    let args: Args = Args::deserialize_json(&config).context("config is invalid")?;

    let store_dir = PathBuf::from(env::var("NIX_STORE").unwrap_or("/nix/store".into()));

    let mut discovery = Discovery {
        store_dir,
        ignore_single_file_outputs: args.ignore_single_file_outputs,
        ..Default::default()
    };

    let ignore_collisions = match args.ignore_collisions {
        true => IgnoreCollisions::True,
        false => IgnoreCollisions::False,
    };

    for pkg in &args.chosen_outputs {
        for path in &pkg.paths {
            if Path::new(path).try_exists()? {
                discovery.add_root(path, pkg.priority, ignore_collisions)?;
            }
        }
    }

    let mut priority = 1000;
    while !discovery.postponed.is_empty() {
        for pkg in std::mem::take(&mut discovery.postponed) {
            discovery.add_root(&pkg, priority, IgnoreCollisions::TrueDontWarn)?;
            priority += 1;
        }
    }

    if !args.extra_paths_from.is_empty() {
        let content = read_to_string(&args.extra_paths_from)
            .with_context(|| format!("cannot open extra paths file {:?}", args.extra_paths_from))?;
        for pkg in content.lines() {
            if Path::new(pkg).is_dir() {
                discovery.add_root(pkg, 1000, ignore_collisions)?;
            }
        }
    }

    // Pre-create the pathsToLink entry chains (this also creates the output base
    // and every forced ancestor directory), then fuse-walk.
    let base = Path::new(&args.outputs.out).join(&args.extra_prefix);
    fs::create_dir_all(&base)?;
    for p in &args.paths_to_link {
        fs::create_dir_all(base.join(p.strip_prefix('/').unwrap_or(p)))?;
    }

    // pathsToLink entries plus all their ancestors. These rel_names
    // are always realized as real directories and always descended into, so that
    // ancestors are traversed to reach their entry and empty entries still exist.
    let forced: BTreeSet<String> = args
        .paths_to_link
        .iter()
        .flat_map(|p| Path::new(p).ancestors())
        .map(|p| p.to_str().unwrap().to_string())
        .collect();

    let nr_links = args.merge("", &base, &discovery.roots, &forced)?;

    eprintln!("created {nr_links} symlinks in user environment");

    if !args.manifest.is_empty() {
        let manifest = Path::new(&args.outputs.out).join("manifest");
        symlink(&args.manifest, manifest).context("cannot create manifest")?;
    }

    Ok(())
}

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

// Collision policy. The JSON only carries the user-facing bool; TrueDontWarn is
// set in code for the propagated phase, mirroring the Perl ignoreCollisions = 2.
#[derive(Debug, Clone, Copy)]
enum IgnoreCollisions {
    True,
    False,
    TrueDontWarn,
}

// A directory to walk: its real path, priority, and collision policy.
type Root = (PathBuf, i32, IgnoreCollisions);

// One package's contribution at a single child name.
struct Contribution {
    target: PathBuf,
    priority: i32,
    is_dir: bool,
    dangling: bool,
    ignore_collisions: IgnoreCollisions,
}

impl Args {
    // Depth-first merge of `dirs` into `out_dir` (which already exists). Creates
    // symlinks at leaves and directories on descent, in one pass.
    fn merge(
        &self,
        rel_name: &str,
        out_dir: &Path,
        dirs: &[Root],
        forced: &BTreeSet<String>,
    ) -> anyhow::Result<u32> {
        let mut nr_links = 0;
        // directories may collide; gather all contributions per child name, then resolve by priority.
        let mut by_name: BTreeMap<String, Vec<Contribution>> = BTreeMap::new();
        for &(ref target, priority, ignore_collisions) in dirs {
            for entry in fs::read_dir(target)? {
                let entry = entry?;
                let name = entry.file_name();
                let name = name
                    .to_str()
                    .with_context(|| format!("non-UTF-8 filename under {target:?}: {name:?}"))?;
                let child_rel = [rel_name, name].join("/");

                if skip(&child_rel, &self.paths_to_link) {
                    continue;
                }

                let child_target = target.join(name);

                // (is_dir, dangling) for a surviving entry. Only called after `skip`, so pruned
                // entries are never classified; only symlinks need the follow-up stat.
                let ftype = entry.file_type()?;
                let (is_dir, dangling) = if ftype.is_symlink() {
                    match fs::metadata(&child_target) {
                        Ok(m) => (m.is_dir(), false),
                        Err(_) => (false, true), // dangling symlink: treated as a leaf
                    }
                } else {
                    (ftype.is_dir(), false)
                };

                let contrib = Contribution {
                    target: child_target,
                    priority,
                    is_dir,
                    dangling,
                    ignore_collisions,
                };
                by_name.entry(name.to_string()).or_default().push(contrib);
            }
        }

        for (name, mut contribs) in by_name {
            let child_out = out_dir.join(&name);

            // Lowest priority number wins; stable so ties keep discovery order.
            contribs.sort_by_key(|c| c.priority);
            let winner = &contribs[0];

            // A file winner shadows everything: one symlink.
            if !winner.is_dir {
                // Handle genuine file/file collision, evaluated under the rival's own policy
                // (propagated rivals stay silent).
                for rival in contribs.iter().skip(1).filter(|rival| {
                    rival.priority == winner.priority && rival.target != winner.target
                }) {
                    match rival.ignore_collisions {
                        IgnoreCollisions::True => eprintln!(
                            "colliding subpath (ignored): {:?} and {:?}",
                            winner.target, rival.target
                        ),
                        IgnoreCollisions::TrueDontWarn => {}
                        IgnoreCollisions::False
                            if self.check_collision_contents
                                && check_collision(&winner.target, &rival.target)? => {}
                        IgnoreCollisions::False => anyhow::bail!(
                            "two given paths contain a conflicting subpath:\n  {:?} and\n  {:?}\n\
                            hint: this may be caused by two different versions of the same package",
                            winner.target,
                            rival.target,
                        ),
                    }
                }

                link_leaf(&child_out, &winner.target, winner.dangling)?;
                nr_links += 1;
                continue;
            }

            // Directory winner: only directory contributors merge.
            let dir_contribs: Vec<Root> = contribs
                .iter()
                .filter(|c| c.is_dir)
                .map(|c| (c.target.clone(), c.priority, c.ignore_collisions))
                .collect();

            let child_rel = [rel_name, &name].join("/");

            // A lone, unforced directory links whole; forced dirs and multi-dir
            // nodes become real directories we descend into.
            if dir_contribs.len() == 1 && !forced.contains(&child_rel) {
                link_leaf(&child_out, &winner.target, false)?;
                nr_links += 1;
                continue;
            }

            if !forced.contains(&child_rel) {
                fs::create_dir(&child_out)?;
            }
            nr_links += self.merge(&child_rel, &child_out, &dir_contribs, forced)?;
        }

        Ok(nr_links)
    }
}

fn skip(child_rel: &str, paths_to_link: &[String]) -> bool {
    let base = child_rel.rsplit('/').next();
    matches!(child_rel, "/propagated-build-inputs" | "/nix-support")
        || child_rel.ends_with("info/dir")
        || (child_rel.starts_with("/share/mime/") && !child_rel.starts_with("/share/mime/packages"))
        || matches!(base, Some("perllocal.pod" | "log"))
        || !(
            // Is `path` at or above some pathsToLink entry? (must descend through it)
            child_rel.is_empty()
                || paths_to_link.iter().any(|p| {
                    p == child_rel || (p.starts_with(&child_rel) && p.as_bytes().get(child_rel.len()) == Some(&b'/'))
                })
            // Is `path` at or below some pathsToLink entry? (its subtree is wanted)
            // `p == "/"` matches everything, mirroring the Perl special case.
            || paths_to_link.iter().any(|p| {
                    p == "/"
                        ||  p == child_rel
                        || (child_rel.starts_with(p.as_str()) && child_rel.as_bytes().get(p.len()) == Some(&b'/'))
                })
        )
}

// Compare two files byte-for-byte; a permission mismatch counts as differing.
fn check_collision(a: &Path, b: &Path) -> anyhow::Result<bool> {
    let ap = fs::metadata(a)?.permissions();
    let bp = fs::metadata(b)?.permissions();
    if ap != bp {
        eprintln!("different permissions in {a:?} and {b:?}: {ap:?} <-> {bp:?}");
        return Ok(false);
    }
    Ok(fs::read(a)? == fs::read(b)?)
}

// Emit one symlink (a file leaf, a dangling leaf, or a linked-whole directory).
fn link_leaf(out: &Path, target: &Path, dangling: bool) -> std::io::Result<()> {
    if dangling {
        let link = fs::read_link(target).unwrap_or_default();
        eprintln!("creating dangling symlink `{out:?}' -> `{target:?}' -> `{link:?}'");
    }
    symlink(target, out)
}

#[derive(Default)]
struct Discovery {
    roots: Vec<Root>,
    done: BTreeSet<String>,
    postponed: BTreeSet<String>,
    store_dir: PathBuf,
    ignore_single_file_outputs: bool,
}

impl Discovery {
    // Append a package root if unseen; queue its propagated packages.
    fn add_root(
        &mut self,
        pkg_dir: &str,
        priority: i32,
        policy: IgnoreCollisions,
    ) -> anyhow::Result<()> {
        if !self.done.insert(pkg_dir.to_string()) {
            return Ok(());
        }

        let pkg_path = Path::new(pkg_dir);
        if pkg_path.is_file() && pkg_path.starts_with(&self.store_dir) {
            if self.ignore_single_file_outputs {
                eprintln!("The store path {pkg_path:?} is a file and can't be merged, ignoring it");
                return Ok(());
            }
            anyhow::bail!("The store path {pkg_path:?} is a file and can't be merged!");
        }

        self.roots.push((pkg_path.to_path_buf(), priority, policy));

        let propagated = pkg_path.join("nix-support/propagated-user-env-packages");
        if let Ok(content) = read_to_string(&propagated) {
            for p in content.split_whitespace() {
                if !self.done.contains(p) {
                    self.postponed.insert(p.to_string());
                }
            }
        }
        Ok(())
    }
}

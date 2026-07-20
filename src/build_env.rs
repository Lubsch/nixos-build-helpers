use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fs::{self, read_to_string},
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

// Is `path` at or below some pathsToLink entry? (its subtree is wanted)
// `p == "/"` matches everything, mirroring the Perl special case.
fn is_in_paths_to_link(paths_to_link: &[String], path: &str) -> bool {
    paths_to_link.iter().any(|p| {
        p == "/"
            || path == p
            || (path.starts_with(p.as_str()) && path.as_bytes().get(p.len()) == Some(&b'/'))
    })
}

// Is `path` at or above some pathsToLink entry? (must descend through it)
fn has_paths_to_link(paths_to_link: &[String], path: &str) -> bool {
    path.is_empty()
        || paths_to_link.iter().any(|p| {
            p == path || (p.starts_with(path) && p.as_bytes().get(path.len()) == Some(&b'/'))
        })
}

// The "Urgh, hacky..." blocklist plus the pathsToLink prune.
fn skip(paths_to_link: &[String], rel_name: &str) -> bool {
    let base = rel_name.rsplit('/').next();
    matches!(rel_name, "/propagated-build-inputs" | "/nix-support")
        || rel_name.ends_with("info/dir")
        || (rel_name.starts_with("/share/mime/") && !rel_name.starts_with("/share/mime/packages"))
        || matches!(base, Some("perllocal.pod" | "log"))
        || !(has_paths_to_link(paths_to_link, rel_name)
            || is_in_paths_to_link(paths_to_link, rel_name))
}

// pathsToLink entries plus all their ancestors (and root ""). These rel_names
// are always realized as real directories and always descended into, so that
// ancestors are traversed to reach their entry and empty entries still exist.
fn forced_dirs(paths_to_link: &[String]) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    set.insert(String::new());
    for p in paths_to_link {
        let mut cur = String::new();
        for part in p.split('/').filter(|s| !s.is_empty()) {
            cur.push('/');
            cur.push_str(part);
            set.insert(cur.clone());
        }
    }
    set
}

// (is_dir, dangling) for a surviving entry. Only called after `skip`, so pruned
// entries are never classified; only symlinks need the follow-up stat.
fn classify(entry: &fs::DirEntry, path: &Path) -> anyhow::Result<(bool, bool)> {
    let ftype = entry.file_type()?;
    Ok(if ftype.is_symlink() {
        match fs::metadata(path) {
            Ok(m) => (m.is_dir(), false),
            Err(_) => (false, true), // dangling symlink: treated as a leaf
        }
    } else {
        (ftype.is_dir(), false)
    })
}

// Compare two files byte-for-byte; a permission mismatch counts as differing.
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

// A genuine file/file collision, evaluated under the rival's own policy
// (propagated rivals stay silent).
fn handle_collision(
    winner: &Contribution,
    rival: &Contribution,
    check_contents: bool,
) -> anyhow::Result<()> {
    match rival.ignore_collisions {
        IgnoreCollisions::True => {
            eprintln!(
                "colliding subpath (ignored): {:?} and {:?}",
                winner.target, rival.target
            );
            Ok(())
        }
        IgnoreCollisions::TrueDontWarn => Ok(()),
        IgnoreCollisions::False => {
            if check_contents && check_collision(&winner.target, &rival.target)? {
                Ok(())
            } else {
                anyhow::bail!(
                    "two given paths contain a conflicting subpath:\n  {:?} and\n  {:?}\n\
                     hint: this may be caused by two different versions of the same package",
                    winner.target,
                    rival.target,
                );
            }
        }
    }
}

struct MergeContext {
    forced: BTreeSet<String>,
    paths_to_link: Vec<String>,
    check_collision_contents: bool,
    nr_links: u64,
}

impl MergeContext {
    // Emit one symlink (a file leaf, a dangling leaf, or a linked-whole directory).
    fn link_leaf(&mut self, out: &Path, target: &Path, dangling: bool) -> anyhow::Result<()> {
        if dangling {
            let link = fs::read_link(target).unwrap_or_default();
            eprintln!("creating dangling symlink `{out:?}' -> `{target:?}' -> `{link:?}'");
        }
        symlink(target, out)?;
        self.nr_links += 1;
        Ok(())
    }

    // Depth-first merge of `dirs` into `out_dir` (which already exists). Creates
    // symlinks at leaves and directories on descent, in one pass.
    fn merge(&mut self, rel_name: &str, out_dir: &Path, dirs: &[Root]) -> anyhow::Result<()> {
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

                if skip(&self.paths_to_link, &child_rel) {
                    continue;
                }

                let child_target = target.join(name);
                let (is_dir, dangling) = classify(&entry, &child_target)?;
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
                for rival in contribs.iter().skip(1) {
                    if rival.priority == winner.priority && rival.target != winner.target {
                        handle_collision(winner, rival, self.check_collision_contents)?;
                    }
                }
                self.link_leaf(&child_out, &winner.target, winner.dangling)?;
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
            if dir_contribs.len() == 1 && !self.forced.contains(&child_rel) {
                self.link_leaf(&child_out, &winner.target, false)?;
                continue;
            }
            if !self.forced.contains(&child_rel) {
                fs::create_dir(&child_out)?;
            }
            self.merge(&child_rel, &child_out, &dir_contribs)?;
        }

        Ok(())
    }
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
    fn add_root(&mut self, pkg_dir: &str, priority: i32, policy: IgnoreCollisions) {
        if !self.done.insert(pkg_dir.to_string()) {
            return;
        }

        let pkg_path = Path::new(pkg_dir);
        if pkg_path.is_file() && pkg_path.starts_with(&self.store_dir) {
            if self.ignore_single_file_outputs {
                eprintln!("The store path {pkg_path:?} is a file and can't be merged, ignoring it");
                return;
            }
            panic!("The store path {pkg_path:?} is a file and can't be merged!");
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
    }
}

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
                discovery.add_root(path, pkg.priority, ignore_collisions);
            }
        }
    }

    let mut priority = 1000;
    while !discovery.postponed.is_empty() {
        for pkg in std::mem::take(&mut discovery.postponed) {
            discovery.add_root(&pkg, priority, IgnoreCollisions::TrueDontWarn);
            priority += 1;
        }
    }

    if !args.extra_paths_from.is_empty() {
        let content = read_to_string(&args.extra_paths_from)
            .with_context(|| format!("cannot open extra paths file {:?}", args.extra_paths_from))?;
        for pkg in content.lines() {
            if Path::new(pkg).is_dir() {
                discovery.add_root(pkg, 1000, ignore_collisions);
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

    let mut merge_context = MergeContext {
        nr_links: 0,
        forced: forced_dirs(&args.paths_to_link),
        paths_to_link: args.paths_to_link,
        check_collision_contents: args.check_collision_contents,
    };

    merge_context.merge("", &base, &discovery.roots)?;

    eprintln!(
        "created {} symlinks in user environment",
        merge_context.nr_links
    );

    if !args.manifest.is_empty() {
        let manifest = Path::new(&args.outputs.out).join("manifest");
        symlink(&args.manifest, manifest).context("cannot create manifest")?;
    }

    Ok(())
}

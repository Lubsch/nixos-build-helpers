//! Replacement for pkgs/build-support/buildenv/builder.pl.
//!
//! builder.pl walks all packages one after another and keeps a global map from relative path to
//! (target, priority), recursing into directories when two packages provide one. Instead, we walk
//! all packages at once: for every path we collect all contributions (in package order) and fold
//! them with builder.pl's rules, which yields the same result in a single pass that creates the
//! output while it goes.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, read_to_string};
use std::os::unix::fs::{MetadataExt as _, symlink};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
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
pub struct Args {
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

/// builder.pl's `$ignoreCollisions`: 0, 1, or 2 (ignore silently, for propagated packages).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Collisions {
    Fail,
    Warn,
    Ignore,
}

/// One package's file or directory at some relative path.
#[derive(Debug, Clone)]
struct Root {
    path: PathBuf,
    priority: i32,
    collisions: Collisions,
    /// File type, from readdir (and a stat for symlinks), so we stat each file at most once.
    symlink: bool,
    /// Perl's `-d`: a directory or a symlink to one
    dir: bool,
    dangling: bool,
    /// Whether no component below the package's store path is a symlink, so that the path
    /// resolves to itself (relative to the store).
    real: bool,
}

pub fn run(args: Args) -> Result<()> {
    let store_dir = std::env::var("NIX_STORE").unwrap_or("/nix/store".into());
    let collisions = if args.ignore_collisions {
        Collisions::Warn
    } else {
        Collisions::Fail
    };
    let mut d = Discovery {
        store_dir: PathBuf::from(store_dir),
        ignore_single_file_outputs: args.ignore_single_file_outputs,
        ..Default::default()
    };

    // The packages that have been installed explicitly.
    for pkg in &args.chosen_outputs {
        for path in &pkg.paths {
            if Path::new(path).exists() {
                d.add(path, pkg.priority, collisions)?;
            }
        }
    }
    // Packages propagated by them, with lower priority (we don't care about their collisions).
    let mut priority = 1000;
    while !d.postponed.is_empty() {
        for pkg in std::mem::take(&mut d.postponed) {
            d.add(&pkg, priority, Collisions::Ignore)?;
            priority += 1;
        }
    }
    if !args.extra_paths_from.is_empty() {
        let content = read_to_string(&args.extra_paths_from)
            .with_context(|| format!("cannot open extra paths file {:?}", args.extra_paths_from))?;
        for pkg in content.lines() {
            if Path::new(pkg).is_dir() {
                d.add(pkg, 1000, collisions)?;
            }
        }
    }

    // pathsToLink and their ancestors are always directories.
    let mut forced = BTreeSet::from([String::new()]);
    for p in &args.paths_to_link {
        let mut cur = String::new();
        for part in p.split('/').filter(|s| !s.is_empty()) {
            cur = format!("{cur}/{part}");
            forced.insert(cur.clone());
        }
    }

    let base = Path::new(&args.outputs.out).join(args.extra_prefix.trim_start_matches('/'));
    fs::create_dir_all(&base)?;
    let ctx = Ctx {
        args: &args,
        forced,
    };
    let nr_links = ctx.merge("", &base, &d.roots)?;
    // Create the pathsToLink that no package provides.
    for p in &args.paths_to_link {
        fs::create_dir_all(base.join(p.trim_start_matches('/')))?;
    }
    eprintln!("created {nr_links} symlinks in user environment");

    if !args.manifest.is_empty() {
        symlink(
            &args.manifest,
            Path::new(&args.outputs.out).join("manifest"),
        )
        .context("cannot create manifest")?;
    }
    Ok(())
}

struct Ctx<'a> {
    args: &'a Args,
    forced: BTreeSet<String>,
}

/// What a relative path ends up as, while folding its contributions.
enum Node {
    /// A symlink to `target` (a file or directory). `root` is the full contribution.
    Link(Root),
    /// A directory merging these package directories. (Its priority is never used.)
    Dir(Vec<Root>),
}

impl Ctx<'_> {
    /// Create `out_dir`'s contents (`out_dir` exists) from the directories `dirs` at `rel`.
    fn merge(&self, rel: &str, out_dir: &Path, dirs: &[Root]) -> Result<u32> {
        // Contributions per child name, in package order.
        let mut children: BTreeMap<String, Vec<Root>> = BTreeMap::new();
        for dir in dirs {
            for entry in
                fs::read_dir(&dir.path).with_context(|| format!("cannot open {:?}", dir.path))?
            {
                let entry = entry?;
                let name = entry.file_name();
                let name = name
                    .to_str()
                    .with_context(|| format!("non-UTF-8 file name in {:?}: {name:?}", dir.path))?;
                if self.skip(&format!("{rel}/{name}"), name) {
                    continue;
                }
                let path = dir.path.join(name);
                let symlink = entry.file_type()?.is_symlink();
                let (is_dir, dangling) = match symlink {
                    true => fs::metadata(&path).map_or((false, true), |m| (m.is_dir(), false)),
                    false => (entry.file_type()?.is_dir(), false),
                };
                children.entry(name.to_string()).or_default().push(Root {
                    path,
                    symlink,
                    dir: is_dir,
                    dangling,
                    real: dir.real && !symlink,
                    ..dir.clone()
                });
            }
        }

        let mut nr_links = 0;
        for (name, contributions) in children {
            let child_rel = format!("{rel}/{name}");
            let out = out_dir.join(&name);
            match self.fold(&child_rel, contributions)? {
                Node::Link(root) => {
                    if root.dangling {
                        let link = fs::read_link(&root.path)?;
                        eprintln!(
                            "creating dangling symlink {out:?} -> {:?} -> {link:?}",
                            root.path
                        );
                    }
                    // (Everything outside pathsToLink is skipped or a forced directory.)
                    symlink(&root.path, &out)?;
                    nr_links += 1;
                }
                Node::Dir(roots) => {
                    fs::create_dir_all(&out)?;
                    nr_links += self.merge(&child_rel, &out, &roots)?;
                }
            }
        }
        Ok(nr_links)
    }

    /// builder.pl's `findFiles`, for all contributions to one path.
    fn fold(&self, rel: &str, contributions: Vec<Root>) -> Result<Node> {
        let mut node = self.forced.contains(rel).then(|| Node::Dir(vec![]));

        for new in contributions {
            let Some(old) = node.take() else {
                node = Some(Node::Link(new));
                continue;
            };
            node = Some(match old {
                // A symlink to a file in a lower-priority package is overwritten.
                Node::Link(old) if new.priority < old.priority && !old.dir => Node::Link(new),
                // Both resolve to the same file: prefer the one that isn't a symlink.
                Node::Link(old) if same_file(&old, &new) => {
                    if old.symlink && !new.symlink {
                        Node::Link(new)
                    } else {
                        Node::Link(old)
                    }
                }
                // A symlink to a file in a higher-priority package stays.
                Node::Link(old) if new.priority > old.priority && !old.dir => Node::Link(old),
                Node::Dir(_) if !new.dir => bail!("not a directory: `{:?}'", new.path),
                Node::Dir(mut roots) => {
                    roots.push(new);
                    Node::Dir(roots)
                }
                Node::Link(old) if old.dir && new.dir => Node::Dir(vec![old, new]),
                Node::Link(old) => {
                    self.collision(&old.path, &new)?;
                    Node::Link(old)
                }
            });
        }
        Ok(node.expect("a path without contributions"))
    }

    /// Two different files at one path, of which `old` wins. Fails unless it may be ignored.
    fn collision(&self, old: &Path, new: &Root) -> Result<()> {
        match new.collisions {
            Collisions::Ignore => Ok(()),
            Collisions::Warn => {
                eprintln!("colliding subpath (ignored): {:?} and {old:?}", new.path);
                Ok(())
            }
            Collisions::Fail
                if self.args.check_collision_contents && same_contents(old, &new.path)? =>
            {
                Ok(())
            }
            Collisions::Fail => bail!(
                "two given paths contain a conflicting subpath:\n  {:?} and\n  {old:?}\n\
                 hint: this may be caused by two different versions of the same package in buildEnv's `paths` parameter\n\
                 hint: `pkgs.nix-diff` can be used to compare derivations",
                new.path
            ),
        }
    }

    /// Paths that are never linked: builder.pl's "Urgh, hacky..." list and paths outside pathsToLink.
    fn skip(&self, rel: &str, name: &str) -> bool {
        matches!(rel, "/propagated-build-inputs" | "/nix-support")
            || rel.ends_with("info/dir")
            || (rel.starts_with("/share/mime/") && !rel.starts_with("/share/mime/packages"))
            || matches!(name, "perllocal.pod" | "log")
            || !(self.has_paths_to_link(rel) || self.in_paths_to_link(rel))
    }

    /// Whether `rel` is at or below a pathsToLink entry.
    fn in_paths_to_link(&self, rel: &str) -> bool {
        let rel = if rel.is_empty() { "/" } else { rel };
        self.args
            .paths_to_link
            .iter()
            .any(|p| p == "/" || is_at_or_below(rel, p))
    }

    /// Whether `rel` may contain files in a pathsToLink entry.
    fn has_paths_to_link(&self, rel: &str) -> bool {
        self.args
            .paths_to_link
            .iter()
            .any(|p| rel.is_empty() || is_at_or_below(p, rel))
    }
}

fn is_at_or_below(path: &str, dir: &str) -> bool {
    path.strip_prefix(dir)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Perl's `abs_path($a) eq abs_path($b)`, false if either doesn't resolve.
fn same_file(a: &Root, b: &Root) -> bool {
    // Different paths without symlinks can't resolve to the same file. This skips resolving
    // (a readlink per path component) for almost all collisions.
    if a.real && b.real {
        return a.path == b.path;
    }
    matches!((fs::canonicalize(&a.path), fs::canonicalize(&b.path)), (Ok(a), Ok(b)) if a == b)
}

/// builder.pl's `checkCollision`: same mode (including the file type) and contents.
fn same_contents(a: &Path, b: &Path) -> Result<bool> {
    let (Ok(ma), Ok(mb)) = (fs::metadata(a), fs::metadata(b)) else {
        return Ok(false);
    };
    if ma.mode() != mb.mode() {
        eprintln!(
            "different permissions in {a:?} and {b:?}: {:04o} <-> {:04o}",
            ma.mode() & 0o7777,
            mb.mode() & 0o7777
        );
        return Ok(false);
    }
    Ok(ma.is_file() && fs::read(a)? == fs::read(b)?)
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
    /// builder.pl's `addPkg`: add a package if unseen, and queue the packages it propagates.
    fn add(&mut self, pkg: &str, priority: i32, collisions: Collisions) -> Result<()> {
        if !self.done.insert(pkg.to_string()) {
            return Ok(());
        }
        let path = Path::new(pkg);
        if path.is_file() && path.parent() == Some(&self.store_dir) {
            if self.ignore_single_file_outputs {
                eprintln!(
                    "The store path {pkg} is a file and can't be merged into an environment using pkgs.buildEnv, ignoring it"
                );
                return Ok(());
            }
            bail!(
                "The store path {pkg} is a file and can't be merged into an environment using pkgs.buildEnv!"
            );
        }
        self.roots.push(Root {
            path: path.to_path_buf(),
            priority,
            collisions,
            symlink: path.is_symlink(),
            dir: path.is_dir(),
            dangling: false,
            real: !path.is_symlink(),
        });

        // builder.pl only reads the first line.
        if let Ok(content) = read_to_string(path.join("nix-support/propagated-user-env-packages")) {
            for p in content.lines().next().unwrap_or("").split_whitespace() {
                if !self.done.contains(p) {
                    self.postponed.insert(p.to_string());
                }
            }
        }
        Ok(())
    }
}

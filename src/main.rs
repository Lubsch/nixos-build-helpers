use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use nanoserde::DeJson;

mod build_composefs_dump;
mod build_env;
mod build_etc;
mod generate_units;

const COMMANDS: &str = "Commands: build-composefs-dump, build-etc, generate-units, build-env";

fn main() -> Result<()> {
    let command = std::env::args()
        .nth(1)
        .with_context(|| anyhow!("Missing command argument\n{COMMANDS}"))?;

    match command.as_str() {
        "build-composefs-dump" => build_composefs_dump::run(read_attrs()?),
        "build-etc" => build_etc::run(read_attrs()?, &out()?),
        "generate-units" => generate_units::run(read_attrs()?, &out()?),
        "build-env" => build_env::run(read_attrs()?),
        _ => Err(anyhow!("unknown command: \"{command}\"\n{COMMANDS}")),
    }
}

/// Read the derivation's attributes (`__structuredAttrs = true`).
fn read_attrs<T: DeJson>() -> Result<T> {
    let path = std::env::var("NIX_ATTRS_JSON_FILE")
        .context("NIX_ATTRS_JSON_FILE is unset; the derivation needs __structuredAttrs = true")?;
    let json = std::fs::read_to_string(&path).with_context(|| format!("cannot read {path}"))?;
    T::deserialize_json(&json).with_context(|| format!("invalid attributes in {path}"))
}

fn out() -> Result<PathBuf> {
    Ok(std::env::var("out").context("$out is unset")?.into())
}

/// Expand a shell glob like bash does in stdenv (`nullglob`, sorted, `*` doesn't match dotfiles).
fn glob(pattern: &str) -> Result<Vec<PathBuf>> {
    let opts = glob::MatchOptions {
        require_literal_leading_dot: true,
        ..Default::default()
    };
    glob::glob_with(pattern, opts)?.map(|p| Ok(p?)).collect()
}

/// Like `glob`, but for all entries of a directory (`dir/*`).
fn glob_dir(dir: &Path) -> Result<Vec<PathBuf>> {
    let dir = dir
        .to_str()
        .with_context(|| format!("non-UTF-8 path {dir:?}"))?;
    glob(&format!("{}/*", glob::Pattern::escape(dir)))
}

fn file_name(p: &Path) -> Result<&std::ffi::OsStr> {
    p.file_name()
        .with_context(|| format!("{p:?} has no file name"))
}

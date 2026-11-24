# nixos-build-helpers

Faster builders for the parts of a NixOS system that are rebuilt on almost every
`nixos-rebuild`, as a NixOS module. Each option replaces one nixpkgs builder with a Rust
reimplementation that produces **identical output** (checked by differential tests, see below):

| option | replaces | upstream builder |
|---|---|---|
| `etc` | `system.build.etc` | bash, one `makeEtcEntry` call per file |
| `etcOverlay` | `system.build.etcMetadataImage` (with `system.etc.overlay`), implies `etc` | python (`build-composefs-dump.py`) in its own derivation. We build /etc and its image in one derivation |
| `systemdUnits` | `system-units`, `user-units`, `initrd-units`, `nspawn-units` (`generateUnits`) | bash, several commands per unit |
| `buildEnv` | `system.path` (`/run/current-system/sw`) | perl (`buildenv/builder.pl`) |
| `fixDependencies` | — | see below |

`fixDependencies` doesn't replace a builder but cuts the dependency of /etc on `system.path`:
upstream, /etc links the dbus configuration and `/etc/terminfo` (and through dbus, the units)
to `system.path`, so /etc is rebuilt whenever a package is added or removed, and can't be built
before `system.path` is. With `fixDependencies`, they refer to `/run/current-system/sw` instead,
which is the same `system.path` at run time. Use `nix run .#check-deps -- <installable>` to
check that /etc of a system doesn't depend on `system.path` anymore (it walks the derivation
graph and prints the chain if it does). Other modules can still add such dependencies.

## Results

Measured on the `desktop` configuration (`bench/configs/desktop.nix`, 192 packages,
system.path with ~10k entries) with `system.etc.overlay`, on an Intel i5-1235U, NixOS. See [Benchmarks](#benchmarks)
for the methodology and how to reproduce.

**Builders** (median of 10 runs inside the build sandbox, interquartile ranges in
`bench/results.jsonl`):

| output | nixpkgs | nixos-build-helpers | |
|---|---|---|---|
| `system-units` | 1266 ms | 10.2 ms | 124× |
| /etc with the overlay: `etc` and `etcMetadataImage` (incl. mkcomposefs, fsck.erofs) | 734 ms (3 derivations) | 9.1 ms (1 derivation) | 80× |
| `etc` without the overlay | 653 ms | 6.7 ms | 97× |
| `initrd-units` | 416 ms | 5.1 ms | 82× |
| `user-units` | 167 ms | 6.4 ms | 26× |
| `system.path` (without `postBuild`) | 258 ms | 78 ms | 3.3× |

The bash builders spend their time starting a process or more per file (`ln`, `mkdir`, …), the
Rust builders only do the system calls.

**buildEnv** in detail (`nix run .#bench -- buildenv`: the builders run directly on the same
attributes, with hyperfine, median of 50, warm cache):

| builder | wall time | output vs `builder.pl` |
|---|---|---|
| `builder.pl` (nixpkgs) | 206 ms | |
| previous version (`5bf9b24`) | 40.3 ms | 1530 of 9995 entries differ |
| current | 35.6 ms | identical |

Both walk all packages in one pass. The previous version merged two directories whenever both
existed, but `builder.pl` links a directory whole if both packages' paths resolve to the same one
(e.g. icon themes linking `share/icons/Adwaita` from adwaita-icon-theme). That's what the 1530
differences are, and what makes the current version faster: it creates ~1400 fewer entries (894
→ 214 `mkdir`s).

**Rebuilding after a small change**, i.e. what `nixos-rebuild` waits for until activation
(evaluation + build, median of 5):

| change | nixpkgs | all options | derivations built |
|---|---|---|---|
| edit a file in /etc | 2.2 s | 1.8 s | 8 → 5 |
| add a package | 4.7 s | 2.7 s | 17 → 9 |
| edit a service | 3.5 s | 2.0 s | 10 → 7 |

With iets as evaluator (eval ~0.9 s). With nix 2.34, eval takes ~5.5 s instead and dominates the
total.

What the options achieve:
- `systemdUnits`, `etc`, `etcOverlay` take their derivations from ~0.1–1.2 s to milliseconds.
  They are on the critical path of almost every rebuild.
- `fixDependencies` alone doesn't make rebuilds faster: a new package no longer rebuilds /etc
  and the units (17 → 12 derivations), but `system.path` still has to be built before the
  toplevel. Combined with the other options, it takes /etc off the critical path.
- `buildEnv` saves ~0.2 s per package change.
- `buildEnv` and `fixDependencies` read upstream's values by evaluating the system a second time
  without our options (`extendModules`, sharing `pkgs`). That costs ~0.1 s of evaluation with
  iets, 0.2–0.5 s with nix 2.34, which can outweigh `buildEnv` alone.
- What remains is mostly nix's overhead per derivation (scheduling, sandbox setup), so having
  fewer derivations is the next lever.

## Installation

Add the flake as an input:
```nix
inputs.nixos-build-helpers = {
  url = "github:lubsch/nixos-build-helpers";
  inputs.nixpkgs.follows = "nixpkgs";
};
```

Import and enable the module:
```nix
{ inputs, config, ... }:
{
  imports = [ inputs.nixos-build-helpers.nixosModules.default ];

  nixosBuildHelpers = {
    etc = true;
    etcOverlay = config.system.etc.overlay.enable;
    systemdUnits = true;
    buildEnv = true;
    fixDependencies = true;
  };
}
```

The module asserts when nixpkgs changes in a way it can't handle (e.g. a new `generateUnits`
argument), so a nixpkgs update fails loudly instead of building something different.

## How it stays correct

- **Wrapping instead of copying.** The module evaluates the system once more without its own
  options (`extendModules`, sharing `pkgs`, ~0.1 s) and wraps upstream's values: `buildEnv`
  only swaps the build command of upstream's `system.path`, `fixDependencies` replaces only the
  `system.path` element of upstream's `services.dbus.packages`. Changes in nixpkgs (another
  `postBuild` step, another module adding dbus packages) carry over.
- **Differential tests** (`nix flake check`): every output is built by nixpkgs and by us and
  compared file by file, without following symlinks (`tests/compare-trees.py`), on a minimal
  configuration and one exercising the edge cases of every builder (`tests/configs.nix`,
  `tests/fixtures.nix`: collisions and priorities in `buildEnv`, symlinked directories, globs,
  dotfiles, file modes and owners, content needing escapes in the composefs dump, units
  overriding upstream or package units, masked units, drop-ins, …), for every option alone and
  all combined. This includes the whole toplevel (activation script, `$out/etc`, which
  `switch-to-configuration` reads, …), so a replaced output that's wired up differently is
  caught too. `buildEnv` is additionally compared on synthetic environments, including inputs
  both must reject. Outputs whose inputs come from another of our builders (e.g. units referring
  to `system.path`) are compared with store hashes normalized; everything else is byte-exact.
  `nix build .#tests.matrix` runs all 24 combinations of options.
- **Drift detection** (`tests/upstream.nix`): fails when nixpkgs changes any of the code we
  reimplement (hashes of `builder.pl`, `build-composefs-dump.py`, `generateUnits` and the etc
  builder). Review the upstream change, port it with a test, update the hash.
- **VM tests** boot a system with all options, with and without the overlay, and switch to a
  changed configuration (units must be restarted, /etc updated).

## Benchmarks

```sh
nix run .#bench -- builders [--config desktop] [--overlay] [--runs 12]
nix run .#bench -- buildenv [--config desktop] [--runs 50]
nix run .#bench -- rebuild  [--config desktop] [--overlay] [--runs 5] [--scenario etc package unit]
nix run .#bench -- rebuild --flake /path/to/flake --name host   # any nixosConfigurations.<name>
nix run .#bench -- rebuild --nix iets                           # time eval and build with another client
```

Three variants are compared: `upstream` (nixpkgs), `previous` (this repository at
`--previous-rev`, by default the last version before the differential tests) and `current`.

- `builders` times each replaced build command *inside* the build sandbox, 12 times in a row,
  dropping 2 warmup runs. That's the cost of the builder itself, without sandbox setup and
  scheduling. Composite steps upstream runs in separate derivations (the composefs dump) are
  summed; steps all variants share (mkcomposefs, fsck.erofs) are included; `system.path`'s
  `postBuild` (glib-compile-schemas, `extraSetup`) is excluded.
- `rebuild` times what you wait for after a small change to an already built system:
  evaluating the system, then `nix build` of the toplevel. Every run makes a unique change, so
  everything depending on it is really rebuilt:
  `etc` edits a file in /etc, `package` adds a package, `unit` edits a service.
  Runs of all variants are interleaved in shuffled order, to spread machine drift over them.
  The number of derivations a change rebuilds is counted once per combination (it's
  deterministic). Measured are upstream, every option of the current version alone, and all
  options of the current and the previous version.

Tables show the median and the interquartile range. Results are also appended to
`bench/results.jsonl`. The `desktop` configuration (`bench/configs/desktop.nix`) is a small
desktop from public packages; `featureful` and `minimal` are the test configurations.

## Development

```sh
nix develop
cargo test
nix flake check -L                         # all tests, including VM tests
nix build -L .#checks.x86_64-linux.build-env
```

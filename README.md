# nixos-build-helpers

Faster builders for the parts of a NixOS system that are rebuilt on almost every
`nixos-rebuild`, as a NixOS module. Each option replaces one nixpkgs builder with a Rust
reimplementation that produces **identical output** (checked by differential tests, see below):

| option | replaces | upstream builder |
|---|---|---|
| `etc` | `system.build.etc` | bash, one `makeEtcEntry` call per file |
| `etcOverlay` | `system.build.etcMetadataImage` (with `system.etc.overlay`), implies `etc` | python (`build-composefs-dump.py`) in its own derivation. We build /etc and its image in one derivation |
| `systemdUnits` | `system-units`, `user-units`, `initrd-units`, `nspawn-units` (`generateUnits`) | bash, several commands per unit |
| `buildEnv` | `system.path` (`/run/current-system/sw`), its `install-info` loop and `update-mime-database` | perl (`buildenv/builder.pl`), one `install-info` per Info file, C++ |
| `fixDependencies` | — | see below |
| `fewerDerivations` | — | see below |

`fixDependencies` doesn't replace a builder but cuts the dependencies on `system.path`:
upstream, /etc links the dbus configuration and `/etc/terminfo` (and through dbus, the units)
to `system.path`, so /etc is rebuilt whenever a package is added or removed, and can't be built
before `system.path` is. Other modules do the same: polkit's `restartTriggers`, the environment
of accounts-daemon and paretosecurity, the crontabs of cron and fcron, GNOME's and LXQt's session
variables. With `fixDependencies`, `config.system.path` is a stand-in with the attributes of the
real one, which interpolates to `/run/current-system/sw`, without string context: the same
`system.path` at run time. So every module referring to it resolves it at run time, including
ones added later or outside nixpkgs. The toplevel's `sw` links the real one, which is also
`system.build.systemPath`. Use `nix run .#check-deps -- <installable>` to check that /etc of a
system doesn't depend on `system.path` (it walks the derivation graph and prints the chain if it
does). A module that reads the contents of `system.path` at build time would fail to build.

What changes at run time: services that were restarted on every package change only because they
referred to `system.path` aren't anymore. polkit's `restartTriggers` become constant, so polkit
picks up actions of a newly added package only after `systemctl restart polkit` (or a reboot), and
accounts-daemon new interfaces only after a restart too. Everything referring to it sees the new
packages right after the switch, through `/run/current-system/sw`, also processes that were already
running.

`fewerDerivations` removes derivations from the way between a change and the toplevel. Each
level costs ~0.1 s, mostly nix's scheduling, even when the build itself takes milliseconds:
- `environment.etc.<name>.text` that refers to no store paths is written at evaluation time
  (`builtins.toFile`) instead of by a `writeText` derivation.
- The activation script finds /etc (with the overlay: the metadata image and basedir) through
  `$systemConfig` at run time, instead of having their store paths built in. The overlay
  snippet is still upstream's, with only those paths replaced.
- boot.json gets nixos-init's /etc paths added in the toplevel builder.
- With `systemdUnits`, the units' derivation writes the unit files itself, into a second output
  (`system-units-files`), instead of one derivation per unit. The units still link there, i.e.
  outside of systemd's search path, so systemd sees them as before (e.g. as "linked"; the VM
  test compares `systemctl list-unit-files` with a system without nixos-build-helpers). Units
  whose `unit` was set to something else than upstream's derivation of their `text` are linked
  as before. This costs ~100 KiB per generation instead of sharing unchanged units.

So `activate` and `boot.json` no longer depend on /etc: after editing a plain /etc file, only
/etc and the toplevel are built, and after editing a service, its script, the units, /etc and
the toplevel.

## Faster switches

Three more options make `switch-to-configuration` (what a switch waits for after the build)
faster. Unlike the options above, they change behaviour, so they're tested by checking
behaviour, not output:

| option | replaces | saves per switch |
|---|---|---|
| `switchToConfiguration` | `switch-to-configuration` (patched, `patches/switch-to-configuration.patch`) | ~240 ms, ~140 ms more when systemd didn't change, ~70 ms more when nothing the user manager loads did |
| `systemdBoot` | systemd-boot's installer (patched builder, `patches/systemd-boot-builder.patch`) | ~30 ms (one third of the installer), more on a real ESP |
| `wrappers` | the script of `suid-sgid-wrappers.service` (upstream's, behind a check) | ~65-110 ms when the wrappers didn't change |

`switchToConfiguration`:
- Upstream waits for systemd's events to settle by polling D-Bus every 250 ms, until an interval
  without messages: every switch waits that interval at least once. The patch polls every 10 ms.
  A unit failing within 10 ms after its start job finished is still reported; one failing
  between 10 and 250 ms later isn't anymore (it shows up in `systemctl --failed`, of course).
- Upstream reexecutes every user manager on every switch (~140 ms). The patch reexecutes it only
  when it would run another binary or `etc/systemd/user.conf` changed, like upstream does for
  PID 1. The binary it runs is the one of the process in `user@<uid>.service`'s init.scope (cgroup
  v2), looked up by the root parent: the user may not read `/proc/<pid>/exe` of its own manager
  (the kernel's ptrace check also compares capabilities). If it can't be found, it reexecutes as
  before.
- Upstream reloads every user manager (~70 ms). The patch reloads only after a reexecution, or
  when something a reload reads changed with the toplevel: every directory of the manager's unit
  search path (`UnitPath`, as the manager reports it) below `/run/current-system` or `/etc`, i.e.
  `/etc/systemd/user` but also units of packages in `/run/current-system/sw/{share,etc/xdg}/systemd/user`
  and `/etc/profiles/per-user/<user>`, plus the generators (`/etc/systemd/user-generators`,
  `user-environment-generators`), presets, and what systemd's own generators read
  (`/etc/environment.d`, the xdg autostart directories of the manager's `XDG_CONFIG_DIRS`). They're
  compared between the old and the new toplevel by content, not by path: with the /etc overlay,
  some are directories of each generation's /etc. If the manager's search path can't be read, it
  reloads. Not covered, since no switch changes them: units edited in `~/.config/systemd/user`
  (or other profiles) without `systemctl --user daemon-reload`, which a switch used to pick up.
  Generators added through NixOS that read other files would need to be listed here.
- What a switch does to bring units to the configured state doesn't depend on either: it's planned
  from the active units and the old and new unit files before the reexecution and reload
  (restarting changed units, starting active targets again, which starts what they want that was
  stopped by hand or failed, after `reset-failed`), and that's unchanged.
- Upstream asks systemd for the `FragmentPath` of every active unit, one D-Bus call after the
  other, to skip units not loaded from the NixOS unit directory: ~30 ms for ~200 active units, the
  longest part of planning a switch (measured as the gap between the boot loader and the
  activation script). The patch sends all the calls at once and collects the replies with local
  filters on the connection while `process` dispatches systemd's signals as usual (~10 ms
  measured in Python, less in Rust). A unit without an answer (an error, a 5 s timeout) is asked
  for alone, as upstream does, so the result is the same. In both scopes (system and user).
- The wrapper (`bin/switch-to-configuration`, which sets the environment) stays upstream's; the
  program it runs is swapped in the toplevel's builder. The build fails if the wrapper isn't
  made as before, and if the patch doesn't apply to a new nixpkgs.

`systemdBoot` (only with `boot.loader.systemd-boot.enable`): upstream's builder is patched where
upstream's installer names it, at build time (it's the template with the system's values filled
in, which the patch's context doesn't contain), and run byte-compiled without `site` (stdlib only):
- `nix-env --list-generations` (~20 ms, and the profile's lock) is replaced by listing the
  profile's `<profile>-<n>-link`s.
- `check-mountpoints` (bash and `findmnt`) is replaced by the same check against
  `/proc/self/mountinfo`.
- `copy-extra-files` isn't started when it's only its interpreter line (no `extraFiles`).
- `bootctl update` (over Varlink) only runs when the systemd package changed since it last ran.
  That's recorded on the ESP (`loader/.nixos-bootctl-update`), so a new or restored ESP is
  updated. It's what bootctl would find anyway: its own boot loader, same version. Not covered:
  someone replacing the boot loader on the ESP by hand, which `bootctl update` would have undone
  only if it was older.
- Not patched: the entries, kernels and initrds it writes, the garbage collection, `syncfs`.

Measured in an unprivileged user and mount namespace, with a tmpfs as the ESP and this machine's 10
generations, after a first install (hyperfine, 30 runs):

| installer | wall time |
|---|---|
| upstream | 93.7 ms ± 4.1 |
| patched, run as source | 67.7 ms ± 5.7 |
| patched, byte-compiled, `-S` | 61.9 ms ± 5.0 |

On a real ESP (vfat, fsync) and with a real `bootctl`, upstream's installer took ~185 ms in
switches; the patched one saves the starts of bootctl, nix-env, bash and findmnt there too.
`tests/systemd-boot.nix` checks that both write the same ESP, byte for byte.

`wrappers` (only when nixpkgs defines `suid-sgid-wrappers.service`): `sysinit-reactivation.target`
restarts the unit on every switch, and its script makes every wrapper again in a new directory,
starting ~5 processes per wrapper (cp, chmod, chown, chmod; setcap): ~65-110 ms for 11 wrappers.
The script rarely changes (it was the same in 24 generations in a row here). The option runs
upstream's script unchanged, behind a check that skips it, with bash builtins only (no process):
- After a run, a stamp (`/run/wrappers/.nixos-build-helpers-stamp`) records the hash of the script
  and the directory it made. A later run with the same script skips when `/run/wrappers/bin` is
  still that directory (`-ef`) and every wrapper exists in it.
- So it runs again for a changed or new wrapper (another script), after a reboot (`/run` is a
  tmpfs), after a run without the option (a new directory), and when a wrapper was deleted.
- Not covered: a wrapper whose mode, owner, capabilities or contents someone changed by hand,
  which a switch used to undo. Only root can write there.
- The build fails if upstream's script no longer makes its directory with `mktemp` into
  `$wrapperDir`, or doesn't name every wrapper as `"$wrapperDir/<program>"`. The VM test compares
  the wrappers (mode, owner, size, capabilities) with nixpkgs' and checks the skip, a deleted
  wrapper and a changed script.

### Future: a rewrite of the systemd-boot installer

What's left (~60 ms in the namespace) is mostly Python itself: starting it and importing the
modules it needs (argparse, dataclasses, subprocess, …) takes ~50 ms. A rewrite in Rust, as one
more subcommand of `nixos-build-helpers` (so nothing new is built per system), would bring it to
a few milliseconds plus the unavoidable I/O. Notes for it:
- Inputs: the same values upstream's template gets (pass them as arguments or a JSON file at
  build time, like the other builders get `__structuredAttrs`): ESP and XBOOTLDR mount points,
  timeout, editor, console mode, distro name, configuration limit, reboot-for-bitlocker,
  `canTouchEfiVariables`, graceful, boot counting (tries), the extra-files script, systemd's path.
- What it does, in upstream's order (keep it: entries are written after their kernel/initrd, the
  loader.conf after the entries): check the mount points; `bootctl install` (with
  `NIXOS_INSTALL_BOOTLOADER=1`) or `update` over Varlink (ESTALE = current); list generations of
  `system` and every profile in `system-profiles/`, limited to the configuration limit; read each
  `boot.json` (and specialisations); compute the entry files (their names are the SHA-256 of their
  content, with boot counting suffixes `+<tries>[-<done>]` preserved for existing ones); copy
  kernels/initrds/devicetrees that aren't there yet (temp file, fsync, rename); initrd secrets
  (always rebuilt, from the pristine initrd, failure fatal only for the default generation);
  garbage-collect everything else in `EFI/nixos` and `loader/entries/nixos-*.conf`; write
  `loader.conf` (with `preferred` for boot counting); the extra files' bookkeeping in
  `EFI/nixos/.extra-files`; `syncfs` both mounts, also on failure.
- Upstream's builder is mypy-checked and has its own NixOS tests (`nixosTests.systemd-boot`):
  run those against the rewrite, plus `tests/systemd-boot.nix` (same ESP as upstream, byte for
  byte), and add `systemd-boot-builder.py` to `tests/upstream.nix` so that every upstream change
  is reviewed.
- Copying a new initrd (40+ MB on vfat, with fsync) dominates a switch that changes it; that's
  I/O either way.

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

Linking is only part of `system.path`'s build. Of ~630 ms for the `desktop` configuration
(`nix-store --realise`, timestamps inside the builder), ~120 ms pass before the builder starts
(the daemon, the sandbox with ~1800 input paths), 40 ms in stdenv's setup, 76 ms linking, 231 ms
in `postBuild` (`update-mime-database` 162 ms, `install-info` 62 ms) and 160 ms after it
(registering the ~20k entries of the output, deleting the build directory). `buildEnv` also:
- runs the build command without stdenv's setup when nothing needs it (no inputs with setup
  hooks, and no stdenv function named in `postBuild`), with what of it a build command sees: its
  shell options and exports (`SOURCE_DATE_EPOCH`, `TZ`, `GZIP_NO_TIMESTAMPS`, …) and `PATH`
  from stdenv's tools. `tests/upstream.nix` compares that with stdenv's setup.
- replaces the loop of `documentation.info` that runs `install-info` once per Info file (37 in
  `desktop`) with `nixos-build-helpers install-info`, a port of texinfo's `install-info.c` that
  does them all in one process and writes the same `dir` file, byte for byte. What it doesn't
  reproduce runs `install-info` itself, on the `dir` file as made so far: compressed or truncated
  Info files, input `install-info` rejects (or overflows its buffer on, which aborts it), an
  existing `dir` file, a locale in the environment, and two entries of one new section whose
  names differ only in case, whose order would be up to glibc's `qsort`. `tests/install-info.py`
  compares the two on the test system's Info files (which must not need `install-info`) and on
  generated ones.

Together ~60 ms per package change (`desktop`: 637 → 576 ms, median of 8): `install-info` takes
~10 ms instead of 62, stdenv's setup ~20 ms instead of 50.

`buildEnv` also replaces `xdg.mime`'s `update-mime-database` (162 ms) with
`nixos-build-helpers update-mime-database`, a port of shared-mime-info's update-mime-database.cpp
(2.4 to 2.5.1) that writes the same MIME database, byte for byte (~45 ms for the ~1050 files of
`freedesktop.org.xml` and three more packages, against ~165 ms):
- update-mime-database writes several files (`subclasses`, `icons`, the order of equal weights
  in `globs`, the string table of `mime.cache` and so every offset in it) in the order of glib's
  hash tables. `src/ghash.rs` is glib's GHashTable (hash, sizes, probing, the in-place resize),
  so it iterates in the same order.
- The per-type XML files are libxml2's: `src/mime_xml.rs` parses what xmlParseFile builds for the
  package files (namespaces, references, the internal DTD subset's default `xmlns`, attribute
  defaults and normalization), what xmlDocCopyNode does to a copied element's namespaces, and
  xmlSaveFormatFileEnc's formatting and escaping. Its quirks are kept, e.g. `mime.cache` gets no
  offsets for its type list (it looks up the lines of `types`, with their newline).
- Anything it doesn't reproduce, it finds before writing anything and runs update-mime-database
  instead: input outside of what it parses (entities, CDATA sections, processing instructions in
  the document, external DTDs, malformed files, which update-mime-database skips with a warning),
  numbers out of range (update-mime-database checks `errno` without resetting it), the glob `*`
  (update-mime-database reads past its end), fsync enabled, a media directory that isn't one.
- Not reproduced: update-mime-database's messages (`postBuild` sends its stdout to /dev/null,
  its stderr goes to the build log).
- Data oriented: the parsed files are flat arrays of nodes, attributes and namespaces with spans
  of the input, the database is arrays linked by indices, the output is one buffer; the buffers
  are allocated up front from counts of the input.
- `tests/update-mime-database.py` compares the two on the test system's package files (which
  must not need update-mime-database) and on generated ones covering what it processes. Locally,
  it was also compared on 4000 generated runs, against 2.4 (glib 2.88.1, libxml2 2.15.3) and
  2.5.1 (glib 2.90.0, libxml2 2.15.4), the versions `upstream-code.nix` accepts.

**Rebuilding after a small change**, i.e. what `nixos-rebuild` waits for until activation
(evaluation + build, median of 5):

| change | nixpkgs | all options | derivations built |
|---|---|---|---|
| edit a file in /etc | 2.2 s | 1.6 s | 8 → 2 |
| add a package | 4.8 s | 2.5 s | 17 → 7 |
| edit a service | 3.6 s | 1.8 s | 10 → 4 |

With iets as evaluator (eval ~0.9 s). With nix 2.34, eval takes ~5.5 s instead and dominates the
total.

What the options achieve:
- `systemdUnits`, `etc`, `etcOverlay` take their derivations from ~0.1–1.2 s to milliseconds.
  They are on the critical path of almost every rebuild.
- `fixDependencies` alone doesn't make rebuilds faster: a new package no longer rebuilds /etc
  and the units (17 → 12 derivations), but `system.path` still has to be built before the
  toplevel. Combined with the other options, it takes /etc off the critical path.
- `fewerDerivations` saves ~0.1 s per level it removes (up to 3 for an /etc change). The
  build itself after an /etc edit takes 0.3 s with `nix build`, 0.6 s with `iets build`.
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
argument), or changes code that an enabled option reimplements or relies on (see
[drift detection](#how-it-stays-correct)), so a nixpkgs update fails loudly instead of building
something different. `nixosBuildHelpers.allowUpstreamChanges = true` turns the latter into
warnings, e.g. to build with a newer nixpkgs before nixos-build-helpers is updated; the output may
then differ from nixpkgs'.

## How it stays correct

- **Wrapping instead of copying.** The module evaluates the system once more without its own
  options (`extendModules`, sharing `pkgs`, ~0.1 s) and wraps upstream's values: `buildEnv`
  only swaps the build command of upstream's `system.path`, `fixDependencies` changes only what
  `config.system.path` interpolates to. Changes in nixpkgs (another `postBuild` step, another
  module referring to `system.path`) carry over.
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
- **Drift detection** (`upstream-code.nix`): hashes of the code in nixpkgs we reimplement or rely
  on (`builder.pl`, `build-composefs-dump.py`, `generateUnits`, the etc builder, the
  `install-info` loop of `documentation.nix` and texinfo's version, `xdg/mime.nix`'s
  update-mime-database and the versions of shared-mime-info, glib and libxml2, the lines using `system.path`
  for `fixDependencies`, the fields of nixos-init's bootspec extension for `fewerDerivations`). The module checks those of the enabled options at evaluation time against
  the nixpkgs the system is built with (`lib/upstream-check.nix`, a few ms), so a nixpkgs that
  follows yours fails the build, not only this repository's `nix flake check`.
  `tests/upstream.nix` checks the same with sed and grep (which also tests the module's
  implementation) and searches all of `nixos/modules` for new uses of `system.path`. Review the
  upstream change, port it with a test, update the hash.
- **VM tests** boot a system with all options, with and without the overlay, and switch to a
  changed configuration (units must be restarted, /etc updated).
  With `switchToConfiguration`, a user manager (a lingering user) must neither be reexecuted nor
  reloaded by a switch that changes only system units, and reloaded by one changing a user unit
  or only adding a package with a user unit (which it must know afterwards).
  `tests/systemd-boot.nix` installs systemd-boot to an extra disk with upstream's installer and
  ours and compares the ESPs, checks that bootctl is skipped only with the marker, and that an
  unmounted ESP still fails.

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

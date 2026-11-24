"""Benchmark nixos-build-helpers against nixpkgs (and a previous version of itself).

    nix run .#bench -- builders [--config desktop] [--overlay] [--runs 12]
    nix run .#bench -- buildenv [--config desktop] [--runs 50]
    nix run .#bench -- rebuild  [--config desktop] [--overlay] [--runs 5] [--scenario etc ...]
    nix run .#bench -- ... --flake /path/to/flake --name myhost  (a nixosConfigurations.<name>)

builders: Time each replaced build command inside the build sandbox, `runs` times in a row. The
          first two runs are warmup and dropped. This is the builder's own cost, without
          sandbox setup and scheduling, which nixos-build-helpers doesn't change.
buildenv: Run the buildEnv builders (builder.pl, previous, current) directly on the attributes
          of system.path, with hyperfine, and compare their outputs to builder.pl's.
rebuild:  Time what a user waits for after a small change to a system that's already built:
          evaluating the system, then building it. Every run makes a unique change (a "salt"),
          so the derivations depending on it are really rebuilt. Also counts those derivations.
          Combinations are measured in a shuffled, interleaved order to spread machine drift.

Variants: upstream (nixpkgs), previous (this repository at --previous-rev), current (checkout).
Evaluation and builds use the nix client in PATH (pinned by `nix run .#bench`), or --nix, e.g.
--nix iets to see what an alternative evaluator changes.
Results are printed as a Markdown table and appended as JSON lines to --out.
"""

import argparse
import json
import os
import random
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
BUILDERS = ["etc", "systemdUnits", "buildEnv"]


def nix(*args: str, stderr: bool = False, client: str = "nix") -> str:
    """Run nix (or `client`) and return its stdout (or stderr)."""
    res = subprocess.run([client, *args], text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if res.returncode != 0:
        sys.exit(f"nix {' '.join(args)} failed:\n{res.stderr}")
    return res.stderr if stderr else res.stdout


def expr(file: str, args: dict, suffix: str = "", wrap: str = "{}") -> list[str]:
    e = f"(import {REPO / file} (builtins.fromJSON ''{json.dumps(args)}''))"
    return ["--impure", "--expr", wrap.format(e) + suffix]


def system_args(a, variant: str, options: dict, **extra) -> dict:
    base = {"flake": a.flake, "name": a.name} if a.flake else {"config": a.config, "overlay": a.overlay}
    return base | {"variant": variant, "options": options, "previousRev": a.previous_rev} | extra


def all_builders(overlay: bool) -> dict:
    return {"etcOverlay" if overlay else "etc": True, "systemdUnits": True, "buildEnv": True}


def is_overlay(a) -> bool:
    if not a.flake:
        return a.overlay
    args = system_args(a, "upstream", {})
    return json.loads(nix("eval", "--json", *expr("bench/system.nix", args, ".config.system.etc.overlay.enable")))


def stats(xs: list[float]) -> dict:
    q = statistics.quantiles(xs, n=4) if len(xs) > 1 else [xs[0]] * 3
    return {"median": statistics.median(xs), "q1": q[0], "q3": q[2], "min": min(xs), "n": len(xs)}


def fmt(s: dict, unit: str = "ms", scale: float = 1e3) -> str:
    return f"{s['median'] * scale:.1f} {unit} ({s['q1'] * scale:.1f}–{s['q3'] * scale:.1f})"


def bench_builders(a, overlay: bool) -> list[dict]:
    salt = uuid.uuid4().hex  # rebuild the timing derivations
    results: dict[str, dict[str, list[float]]] = {}
    for variant in ["upstream", "previous", "current"]:
        args = system_args(a, variant, all_builders(overlay), runs=a.runs, salt=salt)
        drvs = json.loads(nix("eval", "--json", *expr("bench/builders.nix", args,
                                                       wrap="builtins.mapAttrs (_: map (d: d.drvPath)) {}")))
        for component, paths in drvs.items():
            outs = nix("build", "--no-link", "--print-out-paths", *[p + "^out" for p in paths]).split()
            outs = [o for o in outs if o.startswith("/nix/store/")]
            # Sum the derivations of a component per run, drop the warmup runs.
            per_run = [sum(x) / 1e9 for x in zip(*([int(t) for t in Path(o).read_text().split()] for o in outs))]
            results.setdefault(component, {})[variant] = per_run[2:]
            print(f"  {variant:9} {component:13} {fmt(stats(per_run[2:]))}", file=sys.stderr)

    rows = []
    print(f"\n| builder ({label(a, overlay)}) | upstream | previous | current | speedup |")
    print("|---|---|---|---|---|")
    for component, by_variant in results.items():
        s = {v: stats(xs) for v, xs in by_variant.items()}
        speedup = s["upstream"]["median"] / s["current"]["median"]
        cells = " | ".join(fmt(s[v]) if v in s else "—" for v in ["upstream", "previous", "current"])
        print(f"| {component} | {cells} | {speedup:.1f}× |")
        rows.append({"kind": "builders", "target": label(a, overlay), "component": component, "stats": s})
    return rows


def bench_buildenv(a, overlay: bool) -> list[dict]:
    """Run the buildEnv builders directly on the attributes of the system's system.path."""
    tmp = Path(tempfile.mkdtemp(prefix="nbh-buildenv-"))
    drv = nix("eval", "--raw", *expr("bench/system.nix", system_args(a, "upstream", {}),
                                     ".config.system.path.drvPath"))
    nix("build", "--no-link", drv + "^out")  # the inputs
    info = json.loads(nix("derivation", "show", drv))
    attrs = next(iter(info.get("derivations", info).values()))["structuredAttrs"]
    attrs["outputs"] = {"out": str(tmp / "out")}
    (tmp / "attrs.json").write_text(json.dumps(attrs))

    exes = {v: nix("build", "--no-link", "--print-out-paths", *expr("bench/package.nix", {"variant": v,
                   "previousRev": a.previous_rev})).strip() + "/bin/nixos-build-helpers build-env"
            for v in ["previous", "current"]}
    cmds = {"perl": attrs["buildCommand"].splitlines()[0]} | exes
    env = os.environ | {"NIX_ATTRS_JSON_FILE": str(tmp / "attrs.json")}

    # Outputs, compared to builder.pl's
    for name, cmd in cmds.items():
        shutil.rmtree(tmp / "out", ignore_errors=True)
        subprocess.run(cmd.split(), env=env, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        (tmp / "out").rename(tmp / name)
    diffs = {name: subprocess.run([sys.executable, REPO / "tests/compare-trees.py", tmp / "perl", tmp / name],
                                  text=True, stdout=subprocess.PIPE).stdout.splitlines()[-1].split(": ")[-1]
             for name in exes}

    subprocess.run(["hyperfine", "-N", "--warmup", "5", "--runs", str(a.runs), "--prepare",
                    f"rm -rf {tmp / 'out'}", "--export-json", tmp / "hf.json",
                    *[x for name, cmd in cmds.items() for x in ("-n", name, cmd)]],
                   env=env, check=True, stdout=subprocess.DEVNULL)
    results = {r["command"]: r for r in json.loads((tmp / "hf.json").read_text())["results"]}

    print(f"\n| buildEnv builder ({label(a, overlay)}, {len(attrs['chosenOutputs'])} packages) "
          "| wall time | vs builder.pl |")
    print("|---|---|---|")
    rows = []
    for name, r in results.items():
        times = r["times"]
        s = stats(times)
        cmp = "(reference)" if name == "perl" else diffs[name]
        print(f"| {name} | {fmt(s)} | {cmp} |")
        rows.append({"kind": "buildenv", "target": label(a, overlay), "builder": name, "stats": s,
                     "vs_perl": cmp})
    shutil.rmtree(tmp)
    return rows


def combos(overlay: bool) -> list[tuple[str, dict]]:
    etc = "etcOverlay" if overlay else "etc"
    singles = [{etc: True}, {"systemdUnits": True}, {"buildEnv": True}, {"fixDependencies": True}]
    every = all_builders(overlay) | {"fixDependencies": True}
    return [("upstream", {}), ("previous", every)] + [("current", o) for o in singles + [every]]


def combo_name(variant: str, options: dict) -> str:
    return variant if not options else f"{variant}: {' + '.join(options)}"


def bench_rebuild(a, overlay: bool) -> list[dict]:
    cs = [c for c in combos(overlay) if not a.combo or combo_name(*c) in a.combo]
    toplevel = ".config.system.build.toplevel.drvPath"
    # Build everything once, so each run only rebuilds what its change affects, and count the
    # derivations a change rebuilds (always the same, so only once). This uses the pinned nix,
    # because other evaluators (like iets) may not register the .drv files of dependencies.
    built: dict[tuple, int] = {}
    for i, (variant, options) in enumerate(cs):
        for scenario in a.scenario:
            drv = nix("eval", "--raw", *expr("bench/system.nix", system_args(a, variant, options, scenario=scenario),
                                             toplevel))
            nix("build", "--no-link", drv + "^out")
            args = system_args(a, variant, options, scenario=scenario, salt=uuid.uuid4().hex)
            dry = nix("build", "--dry-run", *expr("bench/system.nix", args, ".config.system.build.toplevel"),
                      stderr=True)
            m = re.search(r"these (\d+) derivations will be built", dry)
            built[(scenario, i)] = int(m.group(1)) if m else int("this derivation will be built" in dry)

    samples: dict[tuple, list[dict]] = {}
    order = [(s, i) for s in a.scenario for i in range(len(cs))]
    for run in range(a.runs):
        random.Random(run).shuffle(order)
        for scenario, i in order:
            variant, options = cs[i]
            args = system_args(a, variant, options, scenario=scenario, salt=uuid.uuid4().hex)
            t0 = time.perf_counter()
            drv = nix("eval", "--raw", *expr("bench/system.nix", args, toplevel), client=a.nix)
            t1 = time.perf_counter()
            if a.nix == "nix":
                nix("build", "--no-link", drv + "^out")
                ev, bu = t1 - t0, time.perf_counter() - t1
            else:
                # Other clients (iets) evaluate and build in one step, so time that (with a new
                # salt, so it's not cached) and subtract the evaluation.
                args["salt"] = uuid.uuid4().hex
                nix("build", *expr("bench/system.nix", args, ".config.system.build.toplevel"), client=a.nix)
                ev, bu = t1 - t0, time.perf_counter() - t1 - (t1 - t0)
            samples.setdefault((scenario, i), []).append({"eval": ev, "build": bu})
            print(f"  run {run} {scenario:8} {combo_name(variant, options):45} eval {ev:.2f}s "
                  f"build {bu:.2f}s", file=sys.stderr)

    rows = []
    for scenario in a.scenario:
        print(f"\n| change: {scenario} ({label(a, overlay)}) | eval | build | total | derivations built |")
        print("|---|---|---|---|---|")
        for i, (variant, options) in enumerate(cs):
            xs = samples[(scenario, i)]
            ev, bu = stats([x["eval"] for x in xs]), stats([x["build"] for x in xs])
            tot = stats([x["eval"] + x["build"] for x in xs])
            drvs = built[(scenario, i)]
            print(f"| {combo_name(variant, options)} | {fmt(ev, 's', 1)} | {fmt(bu, 's', 1)} | "
                  f"{fmt(tot, 's', 1)} | {drvs:g} |")
            rows.append({"kind": "rebuild", "target": label(a, overlay), "scenario": scenario,
                         "variant": variant, "options": options, "eval": ev, "build": bu, "total": tot,
                         "drvs": drvs})
    return rows


def label(a, overlay: bool) -> str:
    return (a.name if a.flake else a.config) + (", overlay" if overlay else "")


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("kind", choices=["builders", "rebuild", "buildenv"])
    p.add_argument("--config", default="desktop", help="tests/configs.nix or bench/configs")
    p.add_argument("--overlay", action="store_true", help="with system.etc.overlay")
    p.add_argument("--flake", help="benchmark <flake>#nixosConfigurations.<name> instead")
    p.add_argument("--name")
    p.add_argument("--runs", type=int)
    p.add_argument("--scenario", nargs="+", default=["etc", "package", "unit"])
    p.add_argument("--combo", nargs="+", help='only these, by name, e.g. upstream "current: buildEnv"')
    p.add_argument("--nix", default="nix", help="client for the timed eval and build, e.g. iets")
    p.add_argument("--previous-rev", default="5bf9b2448fec9be887e90040f345446b393df24d")
    p.add_argument("--out", default=str(REPO / "bench/results.jsonl"))
    a = p.parse_args()
    a.runs = a.runs or {"builders": 12, "rebuild": 5, "buildenv": 50}[a.kind]
    a.previous_rev = subprocess.run(["git", "-C", REPO, "rev-parse", a.previous_rev], check=True, text=True,
                                    stdout=subprocess.PIPE).stdout.strip()

    overlay = is_overlay(a)
    print(f"{nix('--version').strip()} ({a.nix}), {a.kind}, {label(a, overlay)}, {a.runs} runs", file=sys.stderr)
    rows = {"builders": bench_builders, "rebuild": bench_rebuild, "buildenv": bench_buildenv}[a.kind](a, overlay)
    meta = {"time": time.strftime("%Y-%m-%dT%H:%M:%S"), "nix": nix("--version").strip(), "client": a.nix,
            "commit": subprocess.run(["git", "-C", REPO, "describe", "--always", "--dirty"], text=True,
                                     stdout=subprocess.PIPE).stdout.strip()}
    with open(a.out, "a") as f:
        for row in rows:
            f.write(json.dumps(meta | row) + "\n")


if __name__ == "__main__":
    main()

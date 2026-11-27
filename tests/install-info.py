"""Compare `nixos-build-helpers install-info` with the loop of install-info it replaces.

Usage: install-info.py INSTALL-INFO NIXOS-BUILD-HELPERS CORPUS-DIR FUZZ-ITERATIONS

Runs both on the Info files in CORPUS-DIR (sorted, as the glob in postBuild gives them, and in
random orders and subsets), and on randomly generated Info files that exercise install-info's
edge cases (sections and their order, entries replacing others, line wrapping, case, missing
markers, compressed or truncated files, ...). Compares the dir file, stderr and the exit status.
The port must handle the real files itself; on generated ones it may run install-info
(INSTALL_INFO_TEST_VERBOSE=1 prints for which).
"""

import gzip
import os
import random
import subprocess
import sys
import tempfile

real, ours, corpus_dir, iterations = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])
env = {k: v for k, v in os.environ.items() if not k.startswith(("LC_", "LANG"))}
rng = random.Random(int(os.environ.get("TEST_SEED", "0")))


def run_both(files: list[str]) -> tuple[tuple, tuple, int]:
    """(status, dir, stderr) of the loop and of the port, and how often the port fell back."""
    with tempfile.TemporaryDirectory() as d:
        a, b = os.path.join(d, "a"), os.path.join(d, "b")
        os.mkdir(a)
        os.mkdir(b)
        err, status = b"", 0
        for f in files:  # like the loop with `set -e`, as bash reports a signal
            p = subprocess.run([real, f, os.path.join(a, "dir")], env=env, capture_output=True)
            err += p.stderr
            status = p.returncode if p.returncode >= 0 else 128 - p.returncode
            if status:
                break
        p = subprocess.run(
            [ours, "install-info", real, os.path.join(b, "dir"), *files],
            env={**env, "NIXOS_BUILD_HELPERS_DEBUG": "1"},
            capture_output=True,
        )
        lines = p.stderr.split(b"\n")
        debug = [l for l in lines if l.startswith(b"install-info: running ")]
        ours_err = b"\n".join(l for l in lines if not l.startswith(b"install-info: running "))

        def read(x):
            path = os.path.join(x, "dir")
            return open(path, "rb").read() if os.path.exists(path) else None

        if debug and os.environ.get("INSTALL_INFO_TEST_VERBOSE"):
            print(b"\n".join(debug).decode())
        return (status, read(a), err), (p.returncode, read(b), ours_err), len(debug)


# Generated Info files
SECTIONS = ["Individual utilities", "Text creation and manipulation", "GNU Utilities", "Basics",
            "Miscellaneous", "basics", "Development", "Émacs", "Z last", "A first"]
NAMES = ["Sed", "sed", "Grep", "grep", "Coreutils", "A", "Ab", "ab", "Zed", "Make", "gawk", "Gawk",
         "X" * 30, "Very long name of a manual that is longer than the column", "café", "Ödipus", "a:b"]
WORDS = ["the", "stream", "editor.", "for", "GNU", "systems,", "very", "long", "description", "\t",
         "supercalifragilisticexpialidocious" * 2, "x.", "and", "(manual)", "é"]


def description() -> str:
    words = [rng.choice(WORDS) for _ in range(rng.choice([0, 0, 1, 3, 8, 20, 40]))]
    return "".join(w + rng.choice([" ", " ", " ", "  ", "\n   ", "\t"]) for w in words).rstrip(" ")


def entry() -> str:
    name, file = rng.choice(NAMES), rng.choice(["sed", "grep", "coreutils", "x", "dir/sub"])
    node = rng.choice(["", "Top", "Invoking sed", "config.status Invocation"])
    head = f"* {name}: ({file}){node}."
    if rng.random() < 0.05:
        head = f"* {name}: ({file})"  # no period
    if rng.random() < 0.05:
        head = f"* {name}::"
    return head + rng.choice([" ", "  ", "\t", "                 ", "\n"]) + description() + "\n"


def info_file() -> bytes:
    lines = ["This is " + rng.choice(NAMES) + ".info, produced by makeinfo.", ""]
    for _ in range(rng.randint(0, 3)):
        r = rng.random()
        if r < 0.75:
            lines += ["INFO-DIR-SECTION " + rng.choice(SECTIONS) for _ in range(rng.choice([0, 1, 1, 2]))]
            if rng.random() < 0.95:
                lines.append("START-INFO-DIR-ENTRY")
            for _ in range(rng.choice([1, 1, 2, 3])):
                lines += entry().rstrip("\n").split("\n")
            if rng.random() < 0.95:
                lines.append("END-INFO-DIR-ENTRY")
        elif r < 0.85:
            lines.append("INFO-DIR-SECTION " + rng.choice(SECTIONS))
        else:
            lines.append(rng.choice(["END-INFO-DIR-ENTRY", "* stray: (x).", "", "\x1f", "File: x.info,  Node: Top"]))
        lines.append("")
    lines += ["\x1f", "File: x.info,  Node: Top,  Up: (dir)", "", "Text."]
    data = "\n".join(lines).encode() + (b"\n" if rng.random() < 0.3 else b"")
    r = rng.random()
    if r < 0.03:
        data = data[: rng.randint(0, 12)]  # too short: install-info fails
    elif r < 0.06:
        data = gzip.compress(data, mtime=0)
    elif r < 0.08:
        data = data.replace(b"\n", b"\r\n")
    return data


failed = 0


def check(what: str, files: list[str], fallbacks_allowed: bool) -> int:
    global failed
    loop, port, fallbacks = run_both(files)
    if loop != port:
        failed += 1
        print(f"FAIL: {what}: status {loop[0]} vs {port[0]}, dir equal: {loop[1] == port[1]}, "
              f"stderr equal: {loop[2] == port[2]}")
    if fallbacks and not fallbacks_allowed:
        failed += 1
        print(f"FAIL: {what}: ran install-info for {fallbacks} of {len(files)} files")
    return fallbacks


corpus = sorted(
    os.path.join(corpus_dir, f) for f in os.listdir(corpus_dir) if f.endswith((".info", ".info.gz"))
)
check(f"the corpus ({len(corpus)} files, sorted)", corpus, False)
for n in range(30):
    check(f"corpus order {n}", rng.sample(corpus, rng.randint(1, len(corpus))), False)

total = fallbacks = 0
for n in range(iterations):
    with tempfile.TemporaryDirectory() as d:
        files = []
        for k in range(rng.randint(1, 8)):
            path = os.path.join(d, f"f{k}{rng.choice(['.info', '.info', '.info.gz'])}")
            open(path, "wb").write(info_file())
            files.append(path)
        rng.shuffle(files)
        total += len(files)
        fallbacks += check(f"generated {n}", files, True)

print(f"corpus: {len(corpus)} files; generated: {iterations} runs, {total} files, "
      f"install-info ran for {fallbacks}; {failed} failures")
sys.exit(1 if failed else 0)

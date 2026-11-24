"""Compare two file trees (or composefs dumps) without following symlinks.

Usage: compare-trees.py [--normalize] [--ignore NAME]... REFERENCE TEST

Symlink targets are compared textually and regular files by permissions and content.
--normalize replaces store path hashes in symlink targets and file contents. Use it when
REFERENCE and TEST point to derivations that differ only because their inputs were built by a
different (separately tested) builder, e.g. units referring to system.path.
--ignore skips a top-level entry.
"""

import hashlib
import os
import re
import sys
from pathlib import Path

STORE_HASH = re.compile(rb"/nix/store/[0-9a-z]{32}-")


def normalize(data: bytes, enabled: bool) -> bytes:
    return STORE_HASH.sub(b"/nix/store/<hash>-", data) if enabled else data


def listing(root: Path, norm: bool) -> dict[str, str]:
    if root.is_file():  # a single file, e.g. a composefs dump
        return {".": hashlib.sha256(normalize(root.read_bytes(), norm)).hexdigest()}
    entries = {}
    for dirpath, dirnames, filenames in os.walk(root):
        for name in dirnames + filenames:
            path = Path(dirpath) / name
            rel = str(path.relative_to(root))
            st = path.lstat()
            if path.is_symlink():
                target = normalize(os.fsencode(os.readlink(path)), norm)
                entries[rel] = "symlink -> " + target.decode(errors="backslashreplace")
            elif path.is_dir():
                entries[rel] = f"dir {st.st_mode & 0o7777:o}"
            else:
                digest = hashlib.sha256(normalize(path.read_bytes(), norm)).hexdigest()
                entries[rel] = f"file {st.st_mode & 0o7777:o} {digest[:16]}"
    return entries


def main() -> int:
    args, ignore, norm = sys.argv[1:], set(), False
    while args[0].startswith("--"):
        opt = args.pop(0)
        if opt == "--normalize":
            norm = True
        elif opt == "--ignore":
            ignore.add(args.pop(0))
    ref, test = (Path(a) for a in args)
    a, b = listing(ref, norm), listing(test, norm)
    bad = 0
    for rel in sorted((a.keys() | b.keys()) - ignore):
        if a.get(rel) != b.get(rel):
            bad += 1
            print(f"  {rel}\n    reference: {a.get(rel, '<missing>')}\n    test:      {b.get(rel, '<missing>')}")
    print(f"{ref} vs {test}: {len(a)} entries, {bad} differences")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())

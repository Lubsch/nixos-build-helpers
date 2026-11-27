"""Compare two file trees (or composefs dumps) without following symlinks.

Usage: compare-trees.py [--normalize] [--deref] [--ignore NAME]... [--subst FROM=TO]... [--subst-ref FROM=TO]... REFERENCE TEST

Symlink targets are compared textually and regular files by permissions and content.
--normalize replaces store path hashes in symlink targets and file contents. Use it when
REFERENCE and TEST point to derivations that differ only because their inputs were built by a
different (separately tested) builder, e.g. units referring to system.path.
--deref compares symlinks into the store by what they resolve to (a file's contents, or
/dev/null), for outputs that link the same files from different store paths.
--ignore skips a top-level entry.
--subst replaces FROM by TO in the contents of TEST's files (after normalizing), for differences
that are intended.
--subst-ref replaces FROM by TO in REFERENCE's file contents and symlink targets (after
normalizing, before --deref decides whether a target is in the store), e.g. a store path that
the test resolves at run time instead.
"""

import hashlib
import os
import re
import sys
from pathlib import Path

STORE_HASH = re.compile(rb"/nix/store/[0-9a-z]{32}-")


def normalize(data: bytes, enabled: bool, subst: list[tuple[bytes, bytes]] = []) -> bytes:
    data = STORE_HASH.sub(b"/nix/store/<hash>-", data) if enabled else data
    for a, b in subst:
        data = data.replace(a, b)
    return data


def listing(
    root: Path, norm: bool, subst: list[tuple[bytes, bytes]] = [], deref: bool = False, link_subst: bool = False
) -> dict[str, str]:
    if root.is_file():  # a single file, e.g. a composefs dump
        return {".": hashlib.sha256(normalize(root.read_bytes(), norm, subst)).hexdigest()}
    entries = {}
    for dirpath, dirnames, filenames in os.walk(root):
        for name in dirnames + filenames:
            path = Path(dirpath) / name
            rel = str(path.relative_to(root))
            st = path.lstat()
            link = path.is_symlink() and os.fsencode(os.readlink(path))
            if link and link_subst and normalize(link, norm, subst) != normalize(link, norm):
                entries[rel] = "symlink -> " + normalize(link, norm, subst).decode(errors="backslashreplace")
            elif path.is_symlink() and deref and os.readlink(path).startswith("/nix/store/"):
                resolved = path.resolve()
                if resolved.is_file():
                    digest = hashlib.sha256(normalize(resolved.read_bytes(), norm, subst)).hexdigest()
                    entries[rel] = f"symlink to file {digest[:16]}"
                else:
                    entries[rel] = f"symlink to {resolved if not resolved.is_relative_to('/nix/store') else 'store dir'}"
            elif path.is_symlink():
                target = normalize(os.fsencode(os.readlink(path)), norm)
                entries[rel] = "symlink -> " + target.decode(errors="backslashreplace")
            elif path.is_dir():
                entries[rel] = f"dir {st.st_mode & 0o7777:o}"
            else:
                digest = hashlib.sha256(normalize(path.read_bytes(), norm, subst)).hexdigest()
                entries[rel] = f"file {st.st_mode & 0o7777:o} {digest[:16]}"
    return entries


def main() -> int:
    args, ignore, norm, subst, subst_ref, deref = sys.argv[1:], set(), False, [], [], False
    while args[0].startswith("--"):
        opt = args.pop(0)
        if opt == "--normalize":
            norm = True
        elif opt == "--deref":
            deref = True
        elif opt == "--ignore":
            ignore.add(args.pop(0))
        elif opt == "--subst":
            frm, to = args.pop(0).split("=", 1)
            subst.append((frm.encode(), to.encode()))
        elif opt == "--subst-ref":
            frm, to = args.pop(0).split("=", 1)
            subst_ref.append((frm.encode(), to.encode()))
    ref, test = (Path(a) for a in args)
    a, b = listing(ref, norm, subst_ref, deref, link_subst=True), listing(test, norm, subst, deref)
    bad = 0
    for rel in sorted((a.keys() | b.keys()) - ignore):
        if a.get(rel) != b.get(rel):
            bad += 1
            print(f"  {rel}\n    reference: {a.get(rel, '<missing>')}\n    test:      {b.get(rel, '<missing>')}")
    print(f"{ref} vs {test}: {len(a)} entries, {bad} differences")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())

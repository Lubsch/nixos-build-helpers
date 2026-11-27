# Draft merge request for shared-mime-info

## Title

update-mime-database: find a type's comment for a language in a hash table

## Description

When a `<comment>` is copied into a type's output, `remove_old()` removes the comment with
the same `xml:lang` that is already there. To find it, it walks over all elements of the type's
output and calls `has_lang()` on each, which calls `xmlGetNsProp()` (resolving the `xml:`
namespace and copying the value) every time. This is quadratic in the number of translations
of a type. With `data/freedesktop.org.xml` (~900 types, most with dozens of translations), that's
about 920,000 calls of `xmlGetNsProp()`, ~29% of the instructions of a full run according to
callgrind.

This keeps the `<comment>` element of each language in a hash table per type instead, so
`remove_old()` does one lookup. The key is `"=" + lang`, or `""` for a comment without
`xml:lang`, which keeps those two cases apart as `has_lang()` did. `has_lang()` is no longer
used and removed.

### Why the output is the same

Comments are only added to a type's output in `load_type()`, right after `remove_old()` removed
the existing comment with the same language. So a type's output never contains more than one
comment per language, and the table always holds the element that the loop would have found.
Elements are only removed from a type's output in `remove_old()`, which also removes them from
the table.

### Testing

- Output: the generated database (all 931 files) is byte-identical to that of the unpatched
  version, for `freedesktop.org.xml` together with the package files of systemd and
  KeePassXC.
- Time, with `PKGSYSTEM_ENABLE_FSYNC=0` and the same packages: 155 ms → 125 ms (median of 8
  runs, x86_64).
- Test suite, including `test-mime` and `test-staging` (built with xdgmime):
  - 2.4: all 10 tests pass, with and without the patch.
  - master: 16 of 17 pass, with and without the patch. `test-mime` fails the same way without
    the patch: 457 mismatches, of which 452 are expected; the other 5 are awk and shell scripts
    (e.g. `bashscript.sh` detected as `text/plain`).

Most of the remaining time is spent parsing `freedesktop.org.xml` and writing the per-type XML
files.

I wrote this patch with the help of an AI assistant (Claude). I have reviewed and tested it
and can answer for it. <!-- adjust to what you did yourself -->

## Commit message

```
update-mime-database: find a type's comment for a language in a hash table

remove_old() searched all elements of a type's output for the comment
with the same xml:lang, calling xmlGetNsProp() on each. That's quadratic
in the number of translations: for freedesktop.org.xml, ~920,000 calls
and ~29% of the run time.

Keep each type's comment per language in a hash table instead. A type's
output has at most one comment per language, because they're only added
right after remove_old() removed the previous one, so the output is the
same.

Assisted-by: Claude (Anthropic)
```

"""Compare `nixos-build-helpers update-mime-database` with update-mime-database.

Usage: update-mime-database.py UPDATE-MIME-DATABASE VERSION NIXOS-BUILD-HELPERS PACKAGES-DIR ITERATIONS

Runs both on the package files in PACKAGES-DIR (which the port must handle itself), and on
randomly generated ones that use what update-mime-database processes (globs and their weights,
magic and tree magic of every kind, aliases, subclasses, root-XML, icons, comments replacing
each other, foreign namespaces, references, broken files, Override.xml, ...), and compares the
whole MIME directory and the exit status. On generated files the port may run
update-mime-database (both then ran it).
"""
import hashlib, os, random, subprocess, sys, tempfile
real, version, ours, corpus, iters = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4], int(sys.argv[5])
seed = int(os.environ.get("TEST_SEED", "0"))
env = {k: v for k, v in os.environ.items() if not k.startswith(("LC_", "LANG"))}
env["PKGSYSTEM_ENABLE_FSYNC"] = "0"
rng = random.Random(seed)
FREE = "http://www.freedesktop.org/standards/shared-mime-info"
MEDIA = ["text", "application", "image", "audio", "x-content", "weird", "Text", "application"]
SUBS = ["plain", "x-foo", "x-Foo", "vnd.a+xml", "zip", "x-bar", "x-baz", "html", "x-q"]
LANGS = [None, None, "de", "fr", "de_AT", "zh-Hant-TW", "sr@latin"]
def esc(s, attr=True):
    s = s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
    return s.replace('"', "&quot;") if attr else s
def word():
    return rng.choice(["a", "Text", "*.TXT", "*.txt", "*.tar.gz", "[Mm]akefile", "README", "*~", "x?y", "é*", "*.é", "core", "\\x41", "ab cd", "&lt;tag", "x&#9;y", "*.x\\["])
def tname():
    return rng.choice(MEDIA) + "/" + rng.choice(SUBS) if rng.random() < 0.97 else rng.choice(["bad", "a/b/c", "x/"])
def match(depth=0):
    t = rng.choice(["string", "string", "byte", "big16", "big32", "little16", "little32", "host16", "host32", "bogus"])
    if t == "string":
        v = rng.choice(["PK\\003\\004", "%PDF-", "\\x89PNG", "abc", "a\\nb", "\\", "<?xml", "x y"])
        mask = rng.choice([None, None, "0xff00", "0xFFFFFFFFFF", "0xzz", "ff", "0x"])
    else:
        v = rng.choice(["0x1234", "1", "077", "0xffffffff", "-1", "abc", "0x10000", "256", ""])
        mask = rng.choice([None, None, "0xff", "0xffff0000", "x"])
    off = rng.choice(["0", "4", "0:64", "8:4", "-1", "x", "2:", ":5", "10:20", "1:1"])
    attrs = f'type="{t}" value="{esc(v)}" offset="{off}"' + (f' mask="{mask}"' if mask is not None else "")
    kids = "".join(match(depth + 1) for _ in range(rng.choice([0, 0, 0, 1, 2]) if depth < 3 else 0))
    return f"<match {attrs}>{kids}</match>" if kids else f"<match {attrs}/>"
def treematch(depth=0):
    attrs = f'path="{rng.choice(["DCIM", "a/b", "x y", ".hidden"])}"'
    if rng.random() < 0.6: attrs += f' type="{rng.choice(["file", "directory", "link", "bogus"])}"'
    for a in ["executable", "match-case", "non-empty"]:
        if rng.random() < 0.3: attrs += f' {a}="{rng.choice(["true", "false"])}"'
    if rng.random() < 0.3: attrs += f' mimetype="{tname()}"'
    kids = "".join(treematch(depth + 1) for _ in range(rng.choice([0, 0, 1]) if depth < 2 else 0))
    return f"<treematch {attrs}>{kids}</treematch>" if kids else f"<treematch {attrs}/>"
def field():
    r = rng.random()
    lang = rng.choice(LANGS)
    if r < 0.25:
        la = f' xml:lang="{lang}"' if lang else ""
        txt = rng.choice(["Plain text", "Text &amp; more", "über", "  spaced  ", "a\nb", "&#13;x", "", "x<!--c-->y"])
        return f"<comment{la}>{txt}</comment>"
    if r < 0.4:
        attrs = f'pattern="{esc(word())}"'
        if rng.random() < 0.3: attrs += f' weight="{rng.choice(["10", "50", "80", "100", "101", "-1", "x", "", " 60", "4294967346"])}"'
        if rng.random() < 0.2: attrs += f' case-sensitive="{rng.choice(["true", "false", "TRUE"])}"'
        if rng.random() < 0.03: attrs = ""
        return f"<glob {attrs}/>"
    if r < 0.43: return "<glob-deleteall/>"
    if r < 0.43 + 0.03: return "<magic-deleteall/>"
    if r < 0.55:
        pr = f' priority="{rng.choice(["50", "80", "20", "101", "x"])}"' if rng.random() < 0.5 else ""
        return f"<magic{pr}>" + "".join(match() for _ in range(rng.choice([0, 1, 1, 2, 3]))) + "</magic>"
    if r < 0.58:
        pr = f' priority="{rng.choice(["50", "80", "x"])}"' if rng.random() < 0.5 else ""
        return f"<treemagic{pr}>" + "".join(treematch() for _ in range(rng.choice([0, 1, 2]))) + "</treemagic>"
    if r < 0.66: return f'<{rng.choice(["alias", "sub-class-of"])} type="{tname() if rng.random() < 0.9 else "noslash"}"/>'
    if r < 0.70: return f'<root-XML namespaceURI="{rng.choice(["http://x.org/ns", "", "a b"])}" localName="{rng.choice(["svg", "", "html"])}"/>'
    if r < 0.76: return f'<{rng.choice(["icon", "generic-icon"])} name="{rng.choice(["text-x-generic", "x-office-document", "foo"])}"/>' if rng.random() < 0.9 else "<icon/>"
    if r < 0.80: return rng.choice(["<acronym>PDF</acronym>", "<expanded-acronym>Portable</expanded-acronym>"])
    if r < 0.85: return rng.choice(["<unknown-thing/>", "<unknown a='1'>text</unknown>"])
    # foreign namespaces, mixed content
    return rng.choice([
        '<k:x xmlns:k="urn:k" k:a="1"><k:y/></k:x>',
        '<k:x xmlns:k="urn:k"><k:y>t</k:y>\n  <k:z/></k:x>',
        '<y xmlns="urn:other"><z attr="&quot;q&quot;">  w </z></y>',
        '<foreign xmlns="urn:f">a<b/>c<!-- c --></foreign>',
        '<k:x xmlns:k="urn:k"><inner/></k:x>',
    ])
def package():
    out = ['<?xml version="1.0" encoding="UTF-8"?>']
    if rng.random() < 0.3:
        out.append('<!DOCTYPE mime-info [\n<!ATTLIST mime-info xmlns CDATA #FIXED "%s">\n<!ATTLIST glob weight CDATA "50">\n<!ATTLIST generic-icon name (text-x-generic | x-office-document | foo) #REQUIRED>\n]>' % FREE)
    ns = rng.choice([f' xmlns="{FREE}"'] * 9 + [f" xmlns='{FREE}'", ' xmlns="urn:wrong"', ""])
    out.append(f"<mime-info{ns}>")
    for _ in range(rng.randint(0, 6)):
        if rng.random() < 0.05:
            out.append("  <not-a-type/>"); continue
        t = tname()
        tt = f' type="{t}"' if rng.random() < 0.97 else ""
        out.append(f"  <mime-type{tt}>")
        for _ in range(rng.randint(0, 8)):
            out.append("    " + field())
        out.append("  </mime-type>")
    out.append("</mime-info>")
    data = "\n".join(out) + "\n"
    if rng.random() < 0.02: data = data.replace("</mime-info>", "")  # broken
    return data.encode()
def tree(root):
    res = {}
    for dp, dns, fns in os.walk(root):
        for n in dns + fns:
            p = os.path.join(dp, n); rel = os.path.relpath(p, root)
            if rel.startswith("packages"): continue
            st = os.lstat(p)
            res[rel] = (f"dir {st.st_mode & 0o777:o}" if os.path.isdir(p) else f"file {st.st_mode & 0o777:o} " + hashlib.sha256(open(p, "rb").read()).hexdigest())
    return res
def compare(names, pkgs):
    """(fell back, differing paths, status equal)"""
    with tempfile.TemporaryDirectory() as d:
        dirs = []
        for x in "ab":
            m = os.path.join(d, x, "mime"); os.makedirs(os.path.join(m, "packages")); dirs.append(m)
            for n, data in zip(names, pkgs): open(os.path.join(m, "packages", n), "wb").write(data)
        p1 = subprocess.run([real, "-V", dirs[0]], env=env, capture_output=True)
        p2 = subprocess.run([ours, "update-mime-database", real, version, dirs[1]], env={**env, "NIXOS_BUILD_HELPERS_DEBUG": "1"}, capture_output=True)
        fell = b"update-mime-database: running" in p2.stderr
        if fell and os.environ.get("REASONS"):
            print("REASON", p2.stderr.split(b": ", 2)[-1].decode(errors="replace").strip()[:100])
        ta, tb = tree(dirs[0]), tree(dirs[1])
        diffs = sorted(k for k in ta.keys() | tb.keys() if ta.get(k) != tb.get(k))
        rc1 = p1.returncode if p1.returncode >= 0 else 128 - p1.returncode
        return fell, diffs, rc1 == p2.returncode

failed = 0
files = sorted(f for f in os.listdir(corpus) if f.endswith(".xml"))
fell, diffs, same_status = compare(files, [open(os.path.join(corpus, f), "rb").read() for f in files])
if fell or diffs or not same_status:
    failed += 1
    print(f"FAIL: the package files of {corpus}: ran update-mime-database: {fell}, differing: {diffs[:10]}")
fallbacks = 0
for it in range(iters):
    pkgs = [package() for _ in range(rng.randint(1, 4))]
    names = [f"p{i}.xml" for i in range(len(pkgs))]
    if rng.random() < 0.2: names[-1] = "Override.xml"
    fell, diffs, same_status = compare(names, pkgs)
    fallbacks += fell
    if not fell and (diffs or not same_status):  # with a fallback, both ran update-mime-database
        failed += 1
        print(f"FAIL: generated {it}: status equal {same_status}, differing: {diffs[:5]}")
        for n, data in zip(names, pkgs):
            print(f"--- {n}"); print(data.decode(errors="replace"))
print(f"{len(files)} package files; generated: {iters} runs, update-mime-database ran for {fallbacks}; {failed} failures")
sys.exit(1 if failed else 0)

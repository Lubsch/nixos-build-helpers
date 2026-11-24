"""Check that /etc of a NixOS system doesn't depend on system.path (see nixosBuildHelpers.fixDependencies).

Usage: check-deps <installable of a NixOS system>, e.g. .#nixosConfigurations.foo
       check-deps --expr '<nix expression evaluating to a NixOS system>'

/etc is system.build.etcMetadataImage with system.etc.overlay, else system.build.etc. If it
depends on system.path, /etc is rebuilt whenever a package changes and can't be built in parallel
with system.path. This walks the derivation graph, so unlike grepping nixpkgs, it has neither false
positives nor misses. Prints the dependency chain and exits 1 if there is one.
"""

import json
import subprocess
import sys

EXPR = """system: let c = system.config; in {
  etc = if c.system.etc.overlay.enable then c.system.build.etcMetadataImage.drvPath
        else c.system.build.etc.drvPath or null;
  path = c.system.path.drvPath;
}"""


def main() -> int:
    if not sys.argv[1:] or sys.argv[1] in ("-h", "--help"):
        print(__doc__)
        return 2
    target = ["--impure", "--expr", sys.argv[2]] if sys.argv[1] == "--expr" else [sys.argv[1]]
    out = subprocess.run(
        ["nix", "eval", "--json", *target, "--apply", EXPR], check=True, stdout=subprocess.PIPE, text=True
    ).stdout
    drvs = json.loads(out)
    if drvs["etc"] is None:
        print("system.build.etc isn't a derivation (nixosBuildHelpers.etcOverlay without overlay?)")
        return 2

    closure = subprocess.run(
        ["nix-store", "--query", "--requisites", drvs["etc"]], check=True, stdout=subprocess.PIPE, text=True
    ).stdout.split()
    print(f"{drvs['etc']}: {len(closure)} paths in the derivation closure")
    if drvs["path"] not in closure:
        print("ok: /etc doesn't depend on system.path")
        return 0

    print(f"/etc depends on system.path ({drvs['path']}):", flush=True)
    subprocess.run(["nix", "why-depends", "--derivation", "--all", drvs["etc"], drvs["path"]])
    return 1


if __name__ == "__main__":
    sys.exit(main())

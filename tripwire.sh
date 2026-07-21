#
# tripwire.sh - flag nixpkgs revisions that introduce a new module
# referencing system.path or system.build.etc, beyond an allowlist.
#
# Heuristic source grep, not a proof. Cached per nixpkgs revision.
#   - Over-approximates: matches comments, strings, and refs behind
#     service enable flags that may never activate.
#   - Under-approximates: indirect closure edges (a module feeding
#     environment.systemPackages into its own buildEnv) are invisible.
#     Only a closure-graph check catches those.
#
# Usage: tripwire.sh /path/to/nixpkgs
# Exit 0 = clean. Exit 1 = new referrer(s).
set -uo pipefail

NIXPKGS="${1:?usage: $0 /path/to/nixpkgs}"
MODULES="$NIXPKGS/nixos/modules"

# Per-file allowlist (relative to nixos/modules/), survives line churn.
# Add a file only after its reference is patched or confirmed harmless.
allow_system_path=(
  "config/system-path.nix"                 # definition
  "system/activation/top-level.nix"        # legitimate /sw link
  "config/users-groups.nix"                # reads passthru attrs, not a closure edge
  "services/system/dbus.nix"               # patched by fixDependencies
  "config/terminfo.nix"                    # patched by fixDependencies

  # FIXME TODO TODO TODO
  # this is just allowed here but it shouldn't!!
  "services/desktop-managers/gnome.nix"
  "services/desktops/accountsservice.nix"
  "services/scheduling/cron.nix"
  "services/scheduling/fcron.nix"
  "services/security/paretosecurity.nix"
  "services/x11/desktop-managers/enlightenment.nix"
  "services/x11/desktop-managers/lxqt.nix"
  "security/polkit.nix"
)
allow_system_build_etc=(
  "system/etc/etc.nix"                     # definition + activation diff
  "system/activation/top-level.nix"        # legitimate /etc link
  "system/boot/systemd/tmpfiles.nix"       # reads passthru.targets
  "system/boot/uki.nix"                    # reads ${etc}/etc/os-release; relevant only with UKI
)

in_allow() {
  local f="$1"; shift
  local a
  for a in "$@"; do [[ "$f" == "$a" ]] && return 0; done
  return 1
}

# Tier 1: config.<option>, the form every real closure edge currently uses.
# Tier 2: destructured (inherit (config.system...) ...) or bare with any
#   prefix. Noisier, catches non-full spellings.
scan() {
  local label="$1" tier1_re="$2" tier2_re="$3"; shift 3
  local -a allow=("$@")
  local found=0 hits file rel

  hits="$(grep -rEln --include='*.nix' "$tier1_re|$tier2_re" "$MODULES" 2>/dev/null)"

  echo "=== $label ==="
  while IFS= read -r file; do
    [[ -z "$file" ]] && continue
    rel="${file#"$MODULES"/}"
    in_allow "$rel" "${allow[@]}" && continue
    found=1
    echo "NEW REFERRER: $rel"
    grep -nE "$tier1_re|$tier2_re" "$file" | sed 's/^/    /'
  done <<< "$hits"

  [[ $found -eq 0 ]] && echo "  ok: none outside allowlist"
  return $found
}

rc=0

scan "system.path" \
  'config\.system\.path\b' \
  'inherit \([^)]*config\.system\b[^)]*\)[^;]*\bpath\b|(^|[^.[:alnum:]_])system\.path\b' \
  "${allow_system_path[@]}" || rc=1

echo

scan "system.build.etc" \
  'config\.system\.build\.etc\b' \
  'inherit \([^)]*config\.system\.build\b[^)]*\)[^;]*\betc\b|(^|[^.[:alnum:]_])system\.build\.etc\b' \
  "${allow_system_build_etc[@]}" || rc=1

echo
if [[ $rc -ne 0 ]]; then
  echo "new referrer(s) found. Classify each:"
  echo "  - closure edge (\${...}/bin, /share, bare in a list/trigger) -> patch, then allowlist"
  echo "  - passthru-attr read (inherit (config.system.path) someAttr)  -> harmless, allowlist"
  echo "  - behind an unused service                                    -> harmless, allowlist with note"
fi
exit $rc

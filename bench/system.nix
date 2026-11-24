# A NixOS system to benchmark.
#
#   config:   name of a configuration in tests/configs.nix or bench/configs, or
#   flake + name: a nixosConfigurations.<name> of another flake (whose own nixos-build-helpers
#             module, if any, is replaced)
#   variant:  "upstream", "current" (this checkout) or "previous" (git revision `previousRev`)
#   options:  nixosBuildHelpers options (ignored for "upstream")
#   scenario: a change applied on top, made unique by `salt` so that it's rebuilt every run
{
  config ? null,
  flake ? null,
  name ? null,
  overlay ? false,
  variant,
  options ? { },
  scenario ? null,
  salt ? "",
  previousRev ? null,
}:
let
  repo = toString ../.;
  self = builtins.getFlake repo;
  inherit (self.inputs.nixpkgs) lib;

  # A path, not "${../.}/module.nix": that would copy the whole checkout (with target/) to the
  # store on every evaluation.
  module =
    if variant == "previous" then
      "${
        builtins.fetchGit {
          url = repo;
          rev = previousRev;
        }
      }/module.nix"
    else
      ../module.nix;
  allOff = lib.genAttrs [ "etc" "etcOverlay" "systemdUnits" "buildEnv" "fixDependencies" ] (_: false);
  variantModule = {
    imports = [ module ];
    nixosBuildHelpers = lib.mkOverride 10 (allOff // lib.optionalAttrs (variant != "upstream") options);
  };

  scenarios = {
    # Edit a file in /etc
    etc.environment.etc."nbh-bench".text = salt;
    # Add a package
    package =
      { pkgs, ... }:
      {
        environment.systemPackages = [ (pkgs.writeTextDir "share/nbh-bench/salt-${salt}" salt) ];
      };
    # Edit a service
    unit.systemd.services.nbh-bench = {
      script = "echo ${salt}";
      wantedBy = [ "multi-user.target" ];
    };
  };
  extra = [ variantModule ] ++ lib.optional (scenario != null) scenarios.${scenario};

  fixtures =
    self.inputs.nixpkgs.legacyPackages.${builtins.currentSystem}.callPackage ../tests/fixtures.nix
      { };
  local = import ../tests/configs.nix { inherit fixtures; } // {
    desktop = ./configs/desktop.nix;
  };
in
if flake != null then
  let
    f = builtins.getFlake flake;
    own = f.inputs.nixos-build-helpers or null;
  in
  f.nixosConfigurations.${name}.extendModules {
    modules = extra ++ lib.optional (own != null) { disabledModules = [ "${own}/module.nix" ]; };
  }
else
  self.inputs.nixpkgs.lib.nixosSystem {
    system = builtins.currentSystem;
    specialArgs = { inherit overlay; };
    modules = [ local.${config} ] ++ extra;
  }

# A typical small desktop: the usual services, CLI tools and icon themes (which make system.path
# large). Everything is in the binary
# cache, so only the derivations our builders replace (and what depends on them) are built locally.
{
  pkgs,
  overlay,
  ...
}:
{
  fileSystems."/".fsType = "tmpfs";
  boot.loader.systemd-boot.enable = true;
  system.stateVersion = "26.05";

  system.etc.overlay.enable = overlay;
  services.userborn.enable = true;

  networking.networkmanager.enable = true;
  hardware.bluetooth.enable = true;
  services.pipewire = {
    enable = true;
    pulse.enable = true;
  };
  services.printing.enable = true;
  services.openssh.enable = true;
  services.udisks2.enable = true;
  services.upower.enable = true;
  security.polkit.enable = true;
  programs.sway.enable = true;
  programs.firefox.enable = true;
  fonts.enableDefaultPackages = true;
  documentation.man.enable = true;

  users.users.alice = {
    isNormalUser = true;
    extraGroups = [ "wheel" ];
  };

  environment.systemPackages = with pkgs; [
    git
    htop
    btop
    ripgrep
    fd
    jq
    tmux
    vim
    neovim
    curl
    wget
    file
    tree
    unzip
    zip
    p7zip
    gnumake
    gcc
    python3
    nodejs
    rustup
    go
    foot
    alacritty
    kitty
    mpv
    imv
    zathura
    pavucontrol
    networkmanagerapplet
    gnome-themes-extra
    adwaita-icon-theme
    papirus-icon-theme
    keepassxc
    nmap
    tcpdump
  ];
}

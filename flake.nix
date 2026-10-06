{
  description = "Minerva: eidolon, the perch, the web UI and the wizard, built from the vendored tree";

  # One toolchain: the revision eidolon/flake.lock pins. No other inputs; eidolon, harnox and
  # eidolon-plugins are subpaths of this repo.
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/0968519e14f7aa7d3e9b389682bd74d2b51c8ce8";

  outputs = { self, nixpkgs }:
    let
      lib = nixpkgs.lib;
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      # The single version source, shared with the installer.
      version = lib.removeSuffix "\n" (builtins.readFile ./VERSION);
    in
    {
      packages = forAll (pkgs: rec {
        eidolon = pkgs.callPackage ./eidolon/nix/eidolon.nix { harnoxSrc = ./harnox; };
        perch = pkgs.callPackage ./nix/perch.nix { harnoxSrc = ./harnox; inherit version; };
        minerva-setup = pkgs.callPackage ./nix/minerva-setup.nix { inherit version; };
        webui-term = pkgs.callPackage ./nix/webui-term.nix { inherit version; };
        minerva-eidolon = pkgs.callPackage ./nix/minerva-eidolon.nix { inherit eidolon; };
        eidolon-browser = pkgs.callPackage ./eidolon-plugins/browser/package.nix { };
        minerva-core = pkgs.callPackage ./nix/minerva-core.nix { inherit version; };
        minerva-plugins = pkgs.callPackage ./nix/plugins.nix { inherit version; };
        minerva = pkgs.callPackage ./nix/minerva.nix {
          inherit eidolon perch minerva-setup webui-term minerva-core minerva-plugins eidolon-browser version;
        };
        default = minerva;
      });

      checks = forAll (pkgs: {
        perch-tests = pkgs.callPackage ./nix/perch.nix {
          harnoxSrc = ./harnox;
          inherit version;
          doCheck = true;
        };
      });

      formatter = forAll (pkgs: pkgs.nixfmt);
    };
}

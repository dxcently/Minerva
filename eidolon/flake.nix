{
  description = "Eidolon — an interactive coding harness in Rust";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    # The shared Rust+LLM foundation. Eidolon's Cargo.toml pins it by git tag
    # and patches that onto `../harnox` for a build of the workspace itself —
    # a path outside this source root, so the build has to put it back where
    # the patch expects it (see `src` below). Keep this input on the same rev
    # the tag names.
    #
    # The ref names the tag explicitly rather than tracking a branch, and the
    # rule that matters is the one above: `../harnox` has to be the revision
    # eidolon's `Cargo.lock` was generated from. The lock records the patched
    # dependency's *version*, so a `../harnox` that disagrees makes Cargo
    # re-resolve the patched dependency — which loads the original git source,
    # the one thing an offline build cannot do. (Until 2026-09-22 this named
    # `embedding-apis`, because 0.3.6's commits were on that branch and not on
    # a tag reachable from master; 0.3.7 is tagged, and the tag, Cargo.toml's
    # pin, Melete's pin and this input move together.) Pinned here rather than
    # vendored so the two repos stay one edit apart.
    #
    # For a day (2026-09-23, `4caf7ad`) it named a bare rev instead:
    # `StopReason::Dismissed` was on harnox master and not in v0.3.7, and the
    # rev that carries it builds here — but a *git consumer* of eidolon still
    # resolved the tag and failed `eidolon-core`. Only a tag moves both, so
    # 0.3.8 is what this names, with Cargo.toml's pin beside it.
    harnox = {
      url = "github:noah427/harnox/v0.3.8";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, harnox }:
    let
      inherit (nixpkgs) lib;
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAllSystems = f: lib.genAttrs systems (system: f system nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (system: pkgs: rec {
        eidolon = pkgs.callPackage ./nix/eidolon.nix { harnoxSrc = harnox; };
        default = eidolon;
      });

      # `nix run github:noah427/eidolon` → the TUI.
      apps = forAllSystems (system: pkgs: rec {
        eidolon = {
          type = "app";
          program = lib.getExe self.packages.${system}.eidolon;
        };
        default = eidolon;
      });

      devShells = forAllSystems (system: pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [ cargo rustc rustfmt clippy rust-analyzer ];
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
        };
      });

      formatter = forAllSystems (system: pkgs: pkgs.nixpkgs-fmt);
    };
}

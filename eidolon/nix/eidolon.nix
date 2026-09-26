# The eidolon binary.
#
# The one wrinkle is `harnox`: the workspace declares it as `path = "../harnox"`,
# a sibling of the repo root, so a build whose source root is just this repo
# cannot resolve it. Rather than rewrite the manifest for Nix's benefit (which
# would leave the flake and a plain `cargo build` disagreeing about the
# dependency), the build reassembles the layout Cargo already expects: both
# trees are copied side by side and the build runs from `eidolon/`.
{ lib
, stdenv
, rustPlatform
, harnoxSrc
, runCommand
}:

let
  # `target/` is large and changes on every build; keeping it out of the store
  # path means an unrelated `cargo build` does not invalidate this derivation.
  # Harmless when the source comes from a github: input (already clean) and
  # load-bearing when it comes from a path:/git+file: one. The Nix files go too:
  # they are read from the flake itself, so a comment in this file should not
  # cost a full rebuild.
  eidolonSrc = lib.cleanSourceWith {
    name = "eidolon-source";
    src = lib.cleanSource ../.;
    filter = path: type:
      let base = baseNameOf (toString path); in
      !(type == "directory" && (base == "target" || base == "result" || base == "nix"))
      && base != "flake.nix" && base != "flake.lock";
  };

  # ../harnox, restored.
  workspace = runCommand "eidolon-workspace" { } ''
    mkdir -p $out
    cp -r ${eidolonSrc} $out/eidolon
    cp -r ${harnoxSrc} $out/harnox
    chmod -R u+w $out
  '';
in
rustPlatform.buildRustPackage {
  pname = "eidolon";
  version = "0.1.0";

  src = workspace;
  sourceRoot = "eidolon-workspace/eidolon";

  cargoLock = {
    lockFile = ../Cargo.lock;
  };

  # Rustls throughout — no openssl, and nothing else links a system library.
  buildInputs = [ ];
  nativeBuildInputs = [ ];

  # The crash-resume and loop tests are hermetic (`--provider mock`), so they
  # run here; nothing in the suite reaches the network.
  doCheck = true;

  meta = {
    description = "An interactive coding harness: a headless core with a modal terminal UI on top";
    homepage = "https://github.com/noah427/eidolon";
    license = lib.licenses.mit;
    mainProgram = "eidolon";
    platforms = lib.platforms.unix;
  };
}

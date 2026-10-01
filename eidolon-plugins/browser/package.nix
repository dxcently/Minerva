# The browser service, built from the workspace, with Chromium from nixpkgs.
# `nix run .#browser` starts it.
{
  lib,
  rustPlatform,
  makeWrapper,
  chromium,
}:

rustPlatform.buildRustPackage {
  pname = "eidolon-browser";
  version = "0.1.0";

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ./service
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [ makeWrapper ];

  # The real-browser tests skip themselves without EIDOLON_BROWSER_CHROME,
  # so this runs the unit tests only.
  postInstall = ''
    wrapProgram $out/bin/eidolon-browser \
      --set-default EIDOLON_BROWSER_CHROME ${lib.getExe chromium}
  '';

  meta = {
    description = "One shared headless Chromium behind eidolon's browser_* tools";
    license = lib.licenses.mit;
    mainProgram = "eidolon-browser";
    platforms = lib.platforms.linux;
  };
}

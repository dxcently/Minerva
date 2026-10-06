# The plugins Minerva ships preinstalled, as a plain tree at `share/minerva/plugins/<name>` (what `minerva plugins sync`
# installs into the member's eidolon and trusts). The browser's Rust source and its package.nix stay out: the service
# binary is `.#eidolon-browser`.
{ runCommand, version }:
runCommand "minerva-plugins-${version}" { } ''
  d=$out/share/minerva/plugins
  mkdir -p $d
  for p in browser jev subagent; do
    cp -r ${../eidolon-plugins}/$p $d/$p
  done
  chmod -R u+w $d
  rm -rf $d/browser/service $d/browser/package.nix
''

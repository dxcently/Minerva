# The plugins Minerva ships preinstalled, as a plain tree at `share/minerva/plugins/<name>` (what `minerva plugins sync`
# installs into the member's eidolon and trusts). Two sources, the same two installer/build.sh copies: eidolon-plugins/ (browser,
# jev, subagent) and Minerva's own plugins/ (every folder with a plugin.rn: term, and mail once it lands; their tests stay out).
# The browser's Rust source and its package.nix stay out too: the service binary is `.#eidolon-browser`.
# Only git-tracked files reach this derivation (a flake's source is what git tracks), which is the rule installer/build.sh
# applies with `git ls-files`; the folder names are checked the same way (a name the bundle already has is refused).
{ runCommand, version }:
runCommand "minerva-plugins-${version}" { } ''
  d=$out/share/minerva/plugins
  mkdir -p $d
  for p in browser jev subagent; do
    cp -r ${../eidolon-plugins}/$p $d/$p
  done
  for p in ${../plugins}/*/; do
    if [ -f $p/plugin.rn ]; then
      n=$(basename $p)
      if [ -e $d/$n ]; then
        echo "plugins/$n has the name of a plugin the bundle already carries (eidolon-plugins/$n); rename the folder in plugins/" >&2
        exit 1
      fi
      cp -r $p $d/$n
      chmod -R u+w $d/$n
      rm -rf $d/$n/tests
      if [ -n "$(find $d/$n -type l -print -quit)" ]; then
        echo "plugins/$n holds a symlink; a plugin ships its own files only, so replace the link with the file it points at" >&2
        exit 1
      fi
    fi
  done
  chmod -R u+w $d
  rm -rf $d/browser/service $d/browser/package.nix
''

@echo off
rem Forwards to the WSL build of aoide in the distro user's ~/.local/bin (AOIDE_DISTRO picks the distro).
rem sh expands only $HOME; the arguments reach aoide through "$@" untouched.
if defined AOIDE_DISTRO (
  wsl.exe -d %AOIDE_DISTRO% --exec sh -c "exec \"$HOME/.local/bin/aoide\" \"$@\"" aoide %*
) else (
  wsl.exe --exec sh -c "exec \"$HOME/.local/bin/aoide\" \"$@\"" aoide %*
)

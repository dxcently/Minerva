@echo off
rem eidolon is Linux-only; forward to the WSL Ubuntu build. The working
rem directory is not translated.
wsl.exe -d Ubuntu -e bash -lc "eidolon %*"
exit /b %errorlevel%

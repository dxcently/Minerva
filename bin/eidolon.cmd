@echo off
setlocal
set "eidolon_binary=%~dp0..\eidolon\target\release\eidolon.exe"
if exist "%eidolon_binary%" goto run
set "eidolon_binary=%~dp0..\eidolon\target\debug\eidolon.exe"
if exist "%eidolon_binary%" goto run
>&2 echo Eidolon is not built. Run cargo build --manifest-path "%~dp0..\eidolon\Cargo.toml" -p eidolon-cli
exit /b 1
:run
"%eidolon_binary%" %*
exit /b %errorlevel%

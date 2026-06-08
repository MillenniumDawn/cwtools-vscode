@echo off
cls

REM Skip submodule update if already initialized
if not exist "submodules\cwtools\.git" (
  git submodule update --init --recursive
) else (
  echo Submodules already initialized, skipping update
)

dotnet tool restore
dotnet run --project build -- -t %*

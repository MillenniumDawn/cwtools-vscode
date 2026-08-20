@echo off
cls

call npx --yes tsx build/build.ts %*

#!/bin/bash

# Skip submodule update if already initialized (saves ~10s on repeated runs).
if [ ! -f "submodules/cwtools/.git" ] && [ ! -d "submodules/cwtools/.git" ]; then
  git submodule update --init --recursive
else
  echo "Submodules already initialized, skipping update"
fi

npx --yes tsx build/build.ts "$@"

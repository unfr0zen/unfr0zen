#!/bin/sh
set -eu
repo=${1:-.work/openthread}
revision=3dde010460e5ee1cc06b07108e10c052caa754ed
if [ ! -d "$repo/.git" ]; then
  git clone --no-checkout https://github.com/openthread/openthread.git "$repo"
fi
git -C "$repo" checkout --detach "$revision"
git -C "$repo" submodule update --init --recursive
printf 'OT=%s\n' "$(cd "$repo" && pwd)"

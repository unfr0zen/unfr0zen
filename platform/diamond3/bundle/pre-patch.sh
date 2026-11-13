#!/bin/sh
# Runs on the unit before unfr0zen-apply reads the root volume mounted at
# $UNFR0ZEN_ROOT; failing, it stops the run with nothing written.
# $UNFR0ZEN_VERSION is the bundle's, $UNFR0ZEN_DRY_RUN 1 when nothing is to be written.
set -eu
echo "Kernel $(uname -r); root volume: $(df -k "$UNFR0ZEN_ROOT" | tail -n 1)"

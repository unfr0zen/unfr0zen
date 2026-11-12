#!/bin/sh
# Runs on the unit after unfr0zen-apply's writes to $UNFR0ZEN_ROOT are
# verified on flash; failing, it fails the run. $UNFR0ZEN_CHANGES is how
# many changes the patch made.
set -eu
echo "unfr0zen $UNFR0ZEN_VERSION: $UNFR0ZEN_CHANGES changes"

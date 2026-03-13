#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(dirname "$(readlink -f "$0")")"

# Flash the font partition (skipped by checksum if already up-to-date)
espflash write-bin \
  --chip esp32s3 \
  0xA00000 \
  "$SCRIPT_DIR/assets/NotoSansJP-Medium.ttf"

# Flash the application and open the monitor
exec espflash flash --monitor \
  --chip esp32s3 \
  --log-format defmt \
  --flash-size 16mb \
  --partition-table "$SCRIPT_DIR/partitions.csv" \
  -F "{t} {L} {s}" \
  "$@"

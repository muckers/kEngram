#!/usr/bin/env bash
# Stop the backing containers brought up by ./start_stack.sh. Default halts
# Postgres + TEI (and the tagger sidecar, if running) but keeps the containers
# and the Postgres data volume, so ./start_stack.sh resumes quickly. Pass --down
# to also remove the containers and network — the named data volume is still
# preserved. To wipe the database too, run `docker compose down -v` yourself;
# that destroys the corpus.
set -euo pipefail

if [[ "${1:-}" == "--down" ]]; then
  docker compose --profile tagger down --remove-orphans
else
  docker compose stop
fi

#!/usr/bin/env bash
# Bring up Kengram's backing containers and wait until Postgres accepts
# connections. Postgres holds the corpus; the TEI container is the reranker
# ([reranker] provider = "tei", :8080). Embeddings and tagging run via host
# Ollama (:11434), not containers. Pass --tagger to also start the opt-in
# deterministic tagger sidecar.
set -euo pipefail

if [[ "${1:-}" == "--tagger" ]]; then
  docker compose --profile tagger up -d postgres tei tagger-deterministic
else
  docker compose up -d postgres tei
fi

printf 'waiting for kengram-postgres'
until docker exec kengram-postgres pg_isready -U kengram >/dev/null 2>&1; do
  printf '.'; sleep 1
done
printf ' ready\n'

# TEI warms its model on first boot (~60-90s on Apple Silicon CPU). serve/worker
# can start before then — only reranked search waits on it.
echo "note: kengram-tei warms up in the background; rerank is ready once its healthcheck passes."

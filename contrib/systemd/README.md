# kEngram systemd units

Example systemd units that bring the kEngram stack up automatically on boot:
the Docker backing services (Postgres + TEI reranker + ollama-embed), the MCP
server, and the worker.

**These are templates.** They contain `@@PLACEHOLDER@@` tokens you must substitute
before installing. The full runbook — prerequisites, install steps, enabling,
verifying, and troubleshooting — lives in
[`docs/linux-autostart.md`](../../docs/linux-autostart.md). Start there.

## Files

| Unit | Type | Purpose |
|---|---|---|
| `kengram-stack.service` | oneshot | Brings up the docker-compose backing services (wraps `start_stack.sh`). |
| `kengram-migrate.service` | oneshot | **Opt-in.** Applies schema migrations (`kengram migrate`) before server/worker. Enable only if you want boot-time migration; otherwise run `kengram migrate` manually. |
| `kengram-server.service` | exec | Runs `kengram serve` (the MCP/HTTP server). |
| `kengram-worker.service` | exec | Runs `kengram worker` (embed + tag drainers). |
| `kengram-tagger-night.service` | exec | **Optional.** A second `kengram worker` process for a nightly GPU-tagger window; pair with an embed-only `kengram-worker.service`. See `docs/linux-autostart.md`, "Optional: nightly GPU tagger window." |
| `kengram-tagger-night.timer` | timer | **Optional.** Starts `kengram-tagger-night.service` at midnight; the service ends its own window via an inner `timeout` (5 min before its `RuntimeMaxSec` safety net fires). |

The units ship in **user-service** form (run as your login user, read
`~/.config/kengram/kengram.toml` via XDG). The doc shows the small deltas for the
**system-service** form (`User=`/`Group=docker`/`HOME=` + an explicit `--config`).

## Placeholders

| Token | Meaning | Example |
|---|---|---|
| `@@KENGRAM_REPO@@` | Absolute path to the checkout (`WorkingDirectory`). | `/home/rjf/code/sandbox/kEngram` |
| `@@KENGRAM_BIN@@` | Absolute path to the release binary. | `/home/rjf/code/sandbox/kEngram/target/release/kengram` |
| `@@RUST_LOG@@` | Tracing log filter. | `info` |
| `@@KENGRAM_USER@@` | Run-as user (system-service variant only). | `rjf` |
| `@@KENGRAM_CONFIG@@` | Absolute config path (system-service variant only). | `/home/rjf/.config/kengram/kengram.toml` |
| `@@NIGHT_TAGGER_MODEL_NAME@@` | *(optional — `kengram-tagger-night.service` only)* Ollama/vLLM model name for the nightly GPU tagger. | `gemma4:31b-qat-ctx16k` |
| `@@NIGHT_TAGGER_MODEL_ID@@` | *(optional — `kengram-tagger-night.service` only)* Stable `tags_extractor_model` identity to stamp on tagged thoughts. | `ollama/gemma4:31b-qat-ctx16k` |
| `@@NIGHT_TAGGER_TIMEOUT_SECONDS@@` | *(optional — `kengram-tagger-night.service` only)* Per-call tagger timeout, sized for GPU (not CPU) latency. | `120` |
| `@@OLLAMA_BIN@@` | *(optional — `kengram-tagger-night.service` only)* Path to the `ollama` binary, used to stop a resident coding LLM before the tagger loads (see the unit's `ExecStartPre` comment — Ollama won't evict a `Forever`-pinned model on its own). | `/usr/local/bin/ollama` |
| `@@NIGHT_TAGGER_CODER_MODEL@@` | *(optional — `kengram-tagger-night.service` only)* Name of the coding LLM to stop before the tagger loads, as shown by `ollama ps`. Leave the substitution as an empty string if nothing needs stopping. | `qwen3.8:27b-ctx256k` |
| `@@DAY_TAGGER_MODEL_NAME@@` | *(optional — `kengram-tagger-night.service` only)* Name of the day tagger's own Ollama model (from `[tagger].model_name` / the CPU override), also stopped before the GPU window starts — Ollama can silently route a GPU-tagged request to an already-resident runner sharing the same weight blobs. | `gemma4:31b-qat-ctx16k-cpu` |

## Quick substitution (user services)

From the repo root, copy the templates into your user unit directory with the
placeholders filled in:

```bash
mkdir -p ~/.config/systemd/user
for unit in contrib/systemd/kengram-*.service; do
  sed \
    -e "s|@@KENGRAM_REPO@@|$PWD|g" \
    -e "s|@@KENGRAM_BIN@@|$PWD/target/release/kengram|g" \
    -e "s|@@RUST_LOG@@|info|g" \
    "$unit" > "$HOME/.config/systemd/user/$(basename "$unit")"
done
systemctl --user daemon-reload
```

Then follow [`docs/linux-autostart.md`](../../docs/linux-autostart.md) to enable
linger and start the units.

The loop above covers the core four `.service` units. `kengram-tagger-night.service`
is optional and needs the three additional `@@NIGHT_TAGGER_*@@` placeholders (see
`docs/linux-autostart.md`, "Optional: nightly GPU tagger window," for install steps);
`kengram-tagger-night.timer` has no placeholders and can be copied as-is.

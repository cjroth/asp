# syntax=docker/dockerfile:1.7

FROM rust:1.89-slim-bookworm AS builder
WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential pkg-config perl \
    && rm -rf /var/lib/apt/lists/*

# Copy every workspace member so cargo can resolve the workspace manifest,
# even though we only compile the `asp` binary.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY desktop/engine ./desktop/engine
COPY tests ./tests

RUN cargo build --release -p asp --bin asp

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/asp /usr/local/bin/asp

ENV ASP_LOG=info
EXPOSE 9000

# On startup:
#   - init the vault at $ASP_DIR if it doesn't exist yet (asp stores its
#     genesis config + log inside <vault>/.asp/asp.db, so that file is the
#     "already initialized" marker).
#   - keep the node identity on the persisted volume by pointing $ASP_HOME
#     there (default ~/.asp/id_ed25519). `asp init`/`asp watch` generate and
#     persist it on first use, so the hub keeps the same identity across
#     container/machine restarts and peers don't have to re-trust it.
#   - merge any pubkeys from $ASP_AUTHORIZED_KEYS into the synced
#     authorized_keys on every start (env var read directly by `watch`).
#     Restart-safe: keys already present are skipped.
#
# Environment knobs:
#   PORT                  bind port (default 9000)
#   ASP_DIR               vault directory (default /mnt/workspace/vault). Set
#                         this to match the mounted persistent volume.
#   ASP_HOME              device-identity dir (default /mnt/workspace/.asp-home).
#   ASP_NO_TLS=1          bind plain ws:// instead of wss:// — use behind a
#                         reverse proxy that already terminates TLS (Fly.io,
#                         Railway, Render, Cloudflare Tunnel, …). Read by
#                         `asp watch` directly; no CMD override needed.
#   ASP_AUTHORIZED_KEYS   ssh-ed25519 lines merged into the synced
#                         authorized_keys on every start.
# ASP_RESET=1 wipes the vault history (NOT the node identity in ASP_HOME) BEFORE
# `asp watch` starts — a race-free reseed knob: set the secret, let it reboot to
# a fresh empty vault (new vault_id, same identity), then unset it. Keeping
# ASP_HOME preserves peer TOFU trust so clients reconnect without re-pinning.
CMD ["/bin/sh", "-c", "VAULT_DIR=\"${ASP_DIR:-/mnt/workspace/vault}\" && export ASP_HOME=\"${ASP_HOME:-/mnt/workspace/.asp-home}\" && { [ \"$ASP_RESET\" = \"1\" ] && echo 'ASP_RESET=1: wiping vault history' && rm -rf \"$VAULT_DIR\" || true; } && mkdir -p \"$VAULT_DIR\" \"$ASP_HOME\" && { [ -f \"$VAULT_DIR/.asp/asp.db\" ] || asp init \"$VAULT_DIR\"; } && exec asp watch --dir \"$VAULT_DIR\" --listen --port \"${PORT:-9000}\""]

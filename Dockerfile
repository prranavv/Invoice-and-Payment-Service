# syntax=docker/dockerfile:1
#
# One build of the crate produces both binaries (the invoice service and mock_psp).
# Two small runtime targets pick one each; docker-compose.yml selects them with `target:`.

# ---------- build ----------
FROM rust:1-slim-bookworm AS builder
WORKDIR /build

# Compile dependencies first, against stub sources, so editing src/ doesn't rebuild them.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src/bin \
 && echo 'fn main() {}' > src/main.rs \
 && echo 'fn main() {}' > src/bin/mock_psp.rs \
 && cargo build --release --bins \
 && rm -rf src

# Real sources. migrations/ must be present at compile time: sqlx::migrate! embeds the
# SQL files into the binary, so the running container needs no migration files.
COPY src ./src
COPY migrations ./migrations

# touch: make sure cargo sees the real files as newer than the stub build.
# The service binary is named after the package in Cargo.toml; read it from there so this
# Dockerfile doesn't hard-code a name.
RUN touch src/main.rs src/bin/mock_psp.rs \
 && cargo build --release --bins \
 && APP="$(sed -n '/^\[package\]/,/^\[/{s/^name *= *"\(.*\)"/\1/p}' Cargo.toml | head -n1)" \
 && test -n "$APP" && test -f "target/release/$APP" \
 && mkdir /out \
 && cp "target/release/$APP" /out/app \
 && cp target/release/mock_psp /out/mock_psp

# ---------- runtime base ----------
FROM debian:bookworm-slim AS runtime-base
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --no-create-home app
USER app

# ---------- invoice service ----------
FROM runtime-base AS app
COPY --from=builder /out/app /usr/local/bin/app
EXPOSE 8000
CMD ["app"]

# ---------- mock PSP ----------
FROM runtime-base AS mock-psp
COPY --from=builder /out/mock_psp /usr/local/bin/mock_psp
EXPOSE 9000
CMD ["mock_psp"]
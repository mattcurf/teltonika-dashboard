# The panel: one Rust binary with the web page built in.
FROM rust:1.91-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY public ./public
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p teltonika-panel \
 && cp target/release/teltonika-panel /usr/local/bin/

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends curl && rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/bin/teltonika-panel /usr/local/bin/
ENV PORT=8080
EXPOSE 8080
HEALTHCHECK --interval=3s --timeout=5s --retries=20 CMD curl -fsS http://127.0.0.1:8080/ >/dev/null || exit 1
CMD ["teltonika-panel"]

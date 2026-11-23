# Two images from one Rust build:
#   --target panel (the default): the panel, with the web page built in.
#   --target mock: the device mocks; run `teltonika-mock rutm50` or `teltonika-mock tsw202`.

FROM rust:1.91-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY .cargo ./.cargo
COPY crates ./crates
COPY web ./web
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p teltonika-panel -p teltonika-mock \
 && cp target/release/teltonika-panel target/release/teltonika-mock /usr/local/bin/

FROM debian:bookworm-slim AS base
RUN apt-get update && apt-get install -y --no-install-recommends curl && rm -rf /var/lib/apt/lists/*

FROM base AS mock
COPY --from=build /usr/local/bin/teltonika-mock /usr/local/bin/
EXPOSE 443
HEALTHCHECK --interval=3s --timeout=3s --retries=20 CMD curl -ksS https://127.0.0.1/api/health >/dev/null || exit 1
ENTRYPOINT ["teltonika-mock"]

FROM base AS panel
COPY --from=build /usr/local/bin/teltonika-panel /usr/local/bin/
ENV PORT=8080
EXPOSE 8080
HEALTHCHECK --interval=3s --timeout=5s --retries=20 CMD curl -fsS http://127.0.0.1:8080/ >/dev/null || exit 1
CMD ["teltonika-panel"]

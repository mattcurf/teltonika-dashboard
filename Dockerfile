# The panel: one Rust binary with the web page built in.

# Browser code: TypeScript to web/dist/app.js.
FROM node:25-alpine AS web
WORKDIR /src
COPY package.json package-lock.json ./
RUN npm ci
COPY web ./web
RUN npm run build:web

FROM rust:1.91-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY .cargo ./.cargo
COPY crates ./crates
COPY web ./web
COPY --from=web /src/web/dist ./web/dist
# web/dist is built above, so build.rs does not need Node here.
ENV PANEL_WEB_PREBUILT=1
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

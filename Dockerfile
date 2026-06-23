FROM node:22-bookworm-slim AS frontend-builder

WORKDIR /app/frontend

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl xz-utils \
    && curl -fsSLo /tmp/node.tar.xz https://nodejs.org/dist/v22.12.0/node-v22.12.0-linux-x64.tar.xz \
    && tar -xJf /tmp/node.tar.xz -C /usr/local --strip-components=1 \
    && npm install -g npm@11.16.0 \
    && rm -f /tmp/node.tar.xz \
    && rm -rf /var/lib/apt/lists/*

COPY frontend/package*.json ./
RUN npm ci

COPY frontend/ ./
RUN npm run build

FROM rust:1-bookworm AS backend-builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY src ./src
COPY templates ./templates
COPY --from=frontend-builder /app/frontend/dist ./frontend/dist

RUN cargo build --release --locked

FROM debian:bookworm-slim

WORKDIR /app

COPY --from=backend-builder /app/target/release/chat-responses-codex /usr/local/bin/chat-responses-codex

RUN groupadd --system app \
    && useradd --system --uid 10001 --gid app --create-home --home-dir /home/app --shell /usr/sbin/nologin app \
    && mkdir -p /data /logs \
    && chown -R app:app /data /logs /home/app

ENV BIND_ADDR=0.0.0.0:3001
ENV STATE_PATH=/data/state.json
ENV LOG_PATH=/logs/chat-responses-codex.log
ENV APP_NAME=chat-responses-codex

VOLUME ["/data", "/logs"]
EXPOSE 3001

HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD ["/usr/local/bin/chat-responses-codex", "--healthcheck"]

USER app

ENTRYPOINT ["/usr/local/bin/chat-responses-codex"]

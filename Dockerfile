FROM debian:bookworm-slim

WORKDIR /app

COPY target/release/chat-responses-codex /usr/local/bin/chat-responses-codex

ENV BIND_ADDR=0.0.0.0:3001
ENV STATE_PATH=/data/state.json
ENV LOG_PATH=/logs/runtime.log
ENV APP_NAME=chat-responses-codex

VOLUME ["/data", "/logs"]
EXPOSE 3001

HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD ["/usr/local/bin/chat-responses-codex", "--healthcheck"]

ENTRYPOINT ["/usr/local/bin/chat-responses-codex"]

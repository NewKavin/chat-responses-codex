FROM debian:bookworm-slim

WORKDIR /app

COPY target/release/chat2responses-gateway /usr/local/bin/chat2responses-gateway

ENV BIND_ADDR=0.0.0.0:3000
ENV STATE_PATH=/data/state.json
ENV LOG_PATH=/logs/runtime.log
ENV APP_NAME=chat2responses-gateway

VOLUME ["/data", "/logs"]
EXPOSE 3000

HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD ["/usr/local/bin/chat2responses-gateway", "--healthcheck"]

ENTRYPOINT ["/usr/local/bin/chat2responses-gateway"]

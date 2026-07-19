FROM node:22-bookworm-slim AS web-builder
WORKDIR /build/apps/cloud-web
COPY apps/cloud-web/package.json apps/cloud-web/package-lock.json ./
RUN npm ci
COPY apps/cloud-web/ ./
RUN npm run build

FROM rust:1.88-bookworm AS rust-builder
WORKDIR /build
COPY . .
RUN cargo build --release -p qa-api

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 qa \
    && mkdir -p /app/data \
    && chown -R qa:qa /app/data

COPY --from=rust-builder /build/target/release/qa-api /usr/local/bin/qa-api
COPY --from=web-builder /build/apps/cloud-web/dist /app/web

ENV QA_BIND_ADDR=0.0.0.0:8080
ENV QA_WEB_DIR=/app/web
EXPOSE 8080
USER qa
ENTRYPOINT ["/usr/local/bin/qa-api"]

# syntax=docker/dockerfile:1
#
# Multi-stage, distroless runtime. Released as a multi-arch (amd64/arm64)
# image at ghcr.io/mimi1vx/ruprogress-mcp by .github/workflows/release.yml;
# for a local build pin a single arch on the command line, e.g.:
#   docker build --platform linux/arm64 -t ruprogress-mcp:dev .
#
# Both base images are pinned by digest so a rebuild is a rebuild, not a
# lottery: the digests correspond to rust:1.96-bookworm and
# gcr.io/distroless/cc-debian12:nonroot at the time this file was written.
# Both are multi-arch OCI indexes (amd64/arm64/…), so the release matrix needs
# no digest change per arch. Pinned digests go stale over time — bumping them
# is a maintenance task, not a CI job, for v1.0.

FROM rust@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663 AS builder
WORKDIR /build
COPY . .
RUN cargo build --locked --release --bin ruprogress-mcp
# distroless has no shell to `mkdir` the attachments volume's mount point
# with the right owner, so seed it here and copy it across with --chown.
RUN mkdir -p /build/attachments-seed && chmod 700 /build/attachments-seed

FROM gcr.io/distroless/cc-debian12@sha256:adcd20c7b4c988b73cbfbddb26d2eee574571e6d7c9ffea29b3821e0690efb77
ARG VERSION=dev
LABEL org.opencontainers.image.source="https://github.com/mimi1vx/ruprogress-mcp" \
      org.opencontainers.image.description="MCP server exposing Redmine's REST API to MCP clients over stdio and streamable HTTP" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.version="${VERSION}"

COPY --from=builder /build/target/release/ruprogress-mcp /usr/local/bin/ruprogress-mcp
# uid/gid 65532 is the image's built-in "nonroot" user.
COPY --from=builder --chown=65532:65532 /build/attachments-seed /var/lib/ruprogress-mcp/attachments

# A container's network namespace is the isolation loopback binding relies on
# on a bare host; SERVER_HOST=0.0.0.0 alone is a startup error until
# PUBLIC_HOST (or REDMINE_MCP_ALLOWED_HOSTS) is also set — see
# docs/configuration.md#exposing-the-server-on-a-network.
ENV SERVER_HOST=0.0.0.0
ENV ATTACHMENTS_DIR=/var/lib/ruprogress-mcp/attachments
VOLUME ["/var/lib/ruprogress-mcp/attachments"]

EXPOSE 8000

# /livez only — never /readyz, which depends on Redmine and would turn a
# Redmine outage into a restart loop. Orchestrator readiness probes should
# point at /readyz separately (see docker-compose.yml).
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD ["/usr/local/bin/ruprogress-mcp", "--healthcheck"]

ENTRYPOINT ["/usr/local/bin/ruprogress-mcp"]
CMD ["--transport", "http"]

# syntax=docker/dockerfile:1
#
# Multi-stage, distroless runtime. Released as a multi-arch (amd64/arm64)
# image at ghcr.io/mimi1vx/ruprogress-mcp by .github/workflows/release.yml;
# for a local build pin a single arch on the command line, e.g.:
#   docker build --platform linux/arm64 -t ruprogress-mcp:dev .
#
# Both base images are pinned by tag *and* digest: the tag fixes the distro
# (so Dependabot can only bump the digest within it, never silently jump
# distro the way an untagged `rust@sha256:` reference tracks `:latest`), and
# the digest makes a rebuild a rebuild, not a lottery. Builder and runtime are
# both Debian trixie deliberately, so the shipped binary's glibc matches what
# it was linked against. Both are multi-arch OCI indexes (amd64/arm64/…), so
# the release matrix needs no digest change per arch. .github/workflows/docker.yml
# smoke-tests every base-image bump before it reaches main.

FROM rust:1.98-trixie@sha256:271849e998ffce5776454bbf98c5dc21baafc854ff8e566197908d3aca9a81e8 AS builder
WORKDIR /build
COPY . .
RUN cargo build --locked --release --bin ruprogress-mcp
# distroless has no shell to `mkdir` the attachments volume's mount point
# with the right owner, so seed it here and copy it across with --chown.
RUN mkdir -p /build/attachments-seed && chmod 700 /build/attachments-seed

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:c31ff9abcb1910f3ab25c7957bdaf0bfe12a01eb546e8df2282f1c8f682b606c
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

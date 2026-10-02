# cascadr — cost-ordered fail-open LLM provider cascade.
# Ships the CLI only. The `anthropic-cli` rung invokes `claude` from PATH, which
# this image does NOT have — mount it / install it in a derived image to use that
# rung here. As shipped, the only rung this image can reach is OpenAI-compat: set
# $LLM_OPENAI_COMPAT_URL to an https endpoint (http is allowed only to this
# CONTAINER's own loopback — not the host's). It shells out to `curl` (no native
# HTTP client in the dependency tree) — curl is installed so that rung is actually
# reachable (cascadr#26); without it the rung could never run regardless of the
# URL. cascadr never proxies the subscription hop.
FROM rust:1.94-slim-bookworm AS builder
WORKDIR /build
COPY . .
RUN cargo build --release --bin cascadr

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 cascadr
COPY --from=builder /build/target/release/cascadr /usr/local/bin/cascadr
USER cascadr
ENTRYPOINT ["cascadr"]
CMD ["--help"]

# cascadr — cost-ordered fail-open LLM provider cascade.
# Ships the CLI only. The `anthropic-cli` rung invokes `claude` from PATH (mount
# it / install it in a derived image); the OpenAI-compat rung needs
# $LLM_OPENAI_COMPAT_URL. cascadr never proxies the subscription hop.
FROM rust:1.94-slim-bookworm AS builder
WORKDIR /build
COPY . .
RUN cargo build --release --bin cascadr

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 cascadr
COPY --from=builder /build/target/release/cascadr /usr/local/bin/cascadr
USER cascadr
ENTRYPOINT ["cascadr"]
CMD ["--help"]

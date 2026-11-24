# egin serving one agent folder. Build from the repository root, then run with your agent mounted
# at /agent on a volume: sessions, uploads and memory live in that folder and must outlive the container.
#
#   docker build -t egin .
#   docker run -p 8787:8787 -v "$PWD/my-agent:/agent" -e EGIN_TOKEN=... -e YDC_API_KEY=... egin
#
# A container has no KVM, so set `sandbox` in agent.toml to a cloud backend (e2b, vercel, daytona),
# or to `local` to run tools inside this container.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
# `egin init` embeds its starter templates at compile time.
COPY templates templates
RUN cargo build --release --locked

FROM debian:bookworm-slim
# ca-certificates for HTTPS; tar seeds a cloud sandbox with the workspace; python3 and git are what
# agents reach for first when `sandbox = "local"`.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates tar python3 git && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/egin /usr/local/bin/egin
VOLUME /agent
EXPOSE 8787
# Listens on $PORT when the host sets one.
CMD ["egin", "serve", "/agent", "--host", "0.0.0.0"]

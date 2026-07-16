# Stage 1: build the React admin UI (embedded into the nexus binary below).
FROM node:22-slim AS webbuilder

WORKDIR /usr/src/webui
COPY webui/package.json webui/package-lock.json ./
RUN npm ci
COPY webui ./
RUN npm run build

# Stage 2: build nexus itself.
FROM rust:1-slim-bookworm AS builder

WORKDIR /usr/src/nexus

RUN apt-get update
RUN apt-get -y install man libpq-dev pkg-config postgresql libssl-dev liboping-dev

RUN for man in $(seq 0 9); do mkdir -p /usr/share/man/man${man}; done

# nexus now builds on stable Rust (Rocket 0.5 / diesel 2); rust-toolchain.toml
# pins the channel. diesel_cli v2 is needed to generate src/schema.rs.
RUN cargo install diesel_cli --no-default-features --features "postgres"

COPY ./Cargo.toml ./Cargo.toml
COPY ./Cargo.lock ./Cargo.lock
COPY ./rust-toolchain.toml ./rust-toolchain.toml
COPY ./build.rs ./build.rs
COPY ./src ./src
COPY ./diesel.toml ./diesel.toml
COPY ./migrations ./migrations
COPY ./migrations_sqlite ./migrations_sqlite

# The built web UI must be in place before cargo build: rust-embed compiles
# webui/dist into the release binary.
COPY --from=webbuilder /usr/src/webui/dist ./webui/dist

# bookworm ships postgresql 15 (buster shipped 11)
RUN su -c 'pg_ctlcluster 15 main start && createuser root && createdb root -O root' postgres && diesel --database-url postgresql:///root migration run && diesel --database-url postgresql:///root print-schema

RUN cat /usr/src/nexus/src/schema.rs
RUN cargo build --release

# The Debian packaging flow (build/build-debian.sh) builds --target builder and
# runs this CMD to extract the artifacts; it never reaches the runtime stage.
CMD cp -a target/release/jaspy-nexus /output/jaspy-nexus && cp -a /usr/local/cargo/bin/diesel /output/diesel && cp -a migrations /output/migrations && cp -a migrations_sqlite /output/migrations_sqlite

# Stage 3: slim runtime image (default build target; used for container
# deployments). Migrations are embedded in the binary and run at startup,
# so only the shared libraries are needed (same set the deb depends on).
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
  && apt-get -y install --no-install-recommends liboping0 libpq5 ca-certificates \
  && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/src/nexus/target/release/jaspy-nexus /usr/local/bin/jaspy-nexus

# The weathermap static app lives at the repo root (outside this build
# context); the CI workflow stages it to nexus/weathermap before building.
# Local runtime-image builds need the same: cp -r ../weathermap ./weathermap
COPY weathermap /usr/share/jaspy/weathermap

ENV ROCKET_ADDRESS=0.0.0.0 ROCKET_PORT=8000 \
    JASPY_WEATHERMAP_DIR=/usr/share/jaspy/weathermap
EXPOSE 8000

# The in-process pinger needs raw ICMP sockets: run with CAP_NET_RAW
# (docker run --cap-add=NET_RAW / k8s securityContext capabilities.add).
CMD ["jaspy-nexus"]

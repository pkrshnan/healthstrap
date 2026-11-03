# Container image for `healthstrap daemon`, used by the home server's docker compose setup
# (pkrshnan/dashboardify, deploy/). Build from the repository root:
#
#   docker build -f deploy/Dockerfile -t healthstrap .
#
# The container has no Bluetooth of its own. It talks to the host's BlueZ over D-Bus, so run it
# with the host's system bus socket mounted:
#
#   -v /run/dbus/system_bus_socket:/run/dbus/system_bus_socket
#
# Pair the strap on the host with bluetoothctl first (see the README).

FROM rust:1.97-slim-trixie AS build
RUN apt-get update \
 && apt-get install -y --no-install-recommends libdbus-1-dev pkg-config \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p healthstrap

FROM debian:trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends libdbus-1-3 ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/healthstrap /usr/local/bin/healthstrap
# The SQLite store lives here; mount a volume so history survives a rebuild.
VOLUME /data
ENTRYPOINT ["healthstrap"]
CMD ["daemon", "--db", "/data/healthstrap.db"]

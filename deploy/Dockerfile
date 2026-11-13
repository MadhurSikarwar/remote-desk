# Image for the rendezvous and relay server.
#
#   docker build -f deploy/Dockerfile -t rc-server .     (from the repository root)
#   docker run -d -p 21117:21117 -v rc-server-data:/data rc-server
#
# The server's key is created in /data on first start and its fingerprint is
# printed in the log. Keep that volume: a new key means every device has to
# be given the new fingerprint.

FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
# The pinned toolchain file asks for extra components the image does not
# carry; the image's own stable compiler is used instead.
RUN rm -f rust-toolchain.toml \
 && cargo build --release --locked -p rc-server

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 --home-dir /data --shell /usr/sbin/nologin rc \
 && mkdir -p /data \
 && chown rc:rc /data
COPY --from=build /src/target/release/rc-server /usr/local/bin/rc-server
USER rc
VOLUME /data
EXPOSE 21117
ENV RC_LOG=info
# Opens the port and closes it again; the server drops such a connection.
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s \
  CMD bash -c '</dev/tcp/127.0.0.1/21117' || exit 1
ENTRYPOINT ["rc-server", "--listen", "0.0.0.0:21117", "--data-dir", "/data"]

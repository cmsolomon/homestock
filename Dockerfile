FROM rust:1-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
COPY templates ./templates
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 --no-create-home homestock
WORKDIR /opt/homestock
COPY --from=builder /build/target/release/homestock /usr/local/bin/homestock
COPY static ./static
USER 10001:10001
EXPOSE 8443
ENTRYPOINT ["/usr/local/bin/homestock"]
CMD ["--config", "/etc/homestock/config.toml"]

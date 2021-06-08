FROM rust:1.52.1 AS planner
WORKDIR /app
RUN cargo install cargo-chef
COPY . . 
# Generate a list of cargo dependencies used
RUN cargo chef prepare --recipe-path recipe.json

FROM rust:1.52.1 AS cacher
WORKDIR /app
RUN cargo install cargo-chef
COPY --from=planner /app/recipe.json recipe.json
# Build all dependencies so they can be cached
RUN cargo chef cook --release --recipe-path recipe.json

FROM rust:1.52.1 AS builder
WORKDIR /app
# Copy over cached dependencies
COPY --from=cacher /app/target target
COPY --from=cacher /usr/local/cargo /usr/local/cargo
COPY . .

RUN cargo build --release --bin memster 

# Runtime stage
FROM debian:buster-slim AS runtime
WORKDIR /app

# Copy the compiled binary from the builder environment 
# to our runtime environment
COPY --from=builder /app/target/release/memster memster 
COPY config.yaml config.yaml 
# Set log level, note that logging can be expensive for large services so remove tower_http level if necessary
ENV RUST_LOG "tower_http=debug,memster=info" 

ENTRYPOINT [ "./memster"]


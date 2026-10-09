# Local Linux-side checks. Cross-compiles for x86_64-pc-windows-msvc with cargo-xwin.
# Tests that touch WPD or a real iPhone cannot run in this container.
# Run those manually on a Windows machine with an iPhone attached.
FROM rust:1-trixie

RUN apt-get update \
    && apt-get install -y --no-install-recommends clang llvm lld \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add x86_64-pc-windows-msvc \
    && rustup component add rustfmt clippy

# cargo-xwin downloads the MSVC CRT and Windows SDK on first use.
RUN cargo install cargo-xwin --locked

WORKDIR /work

CMD ["cargo", "xwin", "build", "--release", "--target", "x86_64-pc-windows-msvc"]

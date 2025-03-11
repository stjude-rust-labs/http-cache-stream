<p align="center">
  <h1 align="center">
    <code>http-cache-stream</code>
  </h1>

  <p align="center">
    <a href="https://github.com/peterhuene/http-cache-stream/actions/workflows/CI.yml" target="_blank">
      <img alt="CI: Status" src="https://github.com/peterhuene/http-cache-stream/actions/workflows/CI.yml/badge.svg" />
    </a>
    <a href="https://crates.io/crates/http-cache-stream" target="_blank">
      <img alt="crates.io version" src="https://img.shields.io/crates/v/http-cache-stream">
    </a>
    <img alt="crates.io downloads" src="https://img.shields.io/crates/d/http-cache-stream">
  </p>

  <p align="center">
    A streaming caching middleware for that uses <a href="https://github.com/kornelski/rusty-http-cache-semantics">http-cache-semantics</a>.
    <br />
    <a href="https://docs.rs/http-cache-stream"><strong>Explore the docs »</strong></a>
    <br />
    <br />
    <a href="https://github.com/peterhuene/http-cache-stream/issues/new?assignees=&title=Descriptive%20Title&labels=enhancement">Request Feature</a>
    ·
    <a href="https://github.com/peterhuene/http-cache-stream/issues/new?assignees=&title=Descriptive%20Title&labels=bug">Report Bug</a>
    ·
    ⭐ Consider starring the repo! ⭐
    <br />
  </p>
</p>

## Getting started

TODO: complete this document

## How this crate differs from [`http-stream`][http-stream]

## Development

To bootstrap a development environment, please use the following commands.

```bash
# Clone the repository
git clone git@github.com:peterhuene/http-cache-stream.git
cd http-cache-stream

# Build the crate
cargo build

# List out the examples
cargo run --example

# Run an example with a given name
cargo run --example '<name>'
```

## Tests

Before submitting any pull requests, please make sure the code passes the
following checks (from the root directory).

```bash
# Run the project's tests with tokio as the async runtime.
cargo test --no-default-features --features tokio

# Run the project's tests with async-std as the async runtime.
cargo test --no-default-features --features async-std

# Ensure the project doesn't have any linting warnings.
cargo clippy --all

# Ensure the project passes `cargo fmt`.
# Currently this requires nightly Rust.
cargo +nightly fmt --check

# Ensure the docs build.
cargo doc
```

## Contributing

Contributions, issues, and feature requests are all welcome!

Please submit your changes as pull requests from a feature branch of your fork.

## License and Legal

This project is licensed under the [Apache 2.0][license] license.

Copyright © 2025-Present [Peter Huene](https://github.com/peterhuene).

[license]: https://github.com/peterhuene/http-cache-stream/blob/main/LICENSE
[http-stream]: https://github.com/06chaynes/http-cache

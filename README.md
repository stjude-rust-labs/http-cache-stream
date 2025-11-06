<p align="center">
  <h1 align="center">
    <code>http-cache-stream</code>
  </h1>

  <p align="center">
    <a href="https://github.com/stjude-rust-labs/http-cache-stream/actions/workflows/CI.yml" target="_blank">
      <img alt="CI: Status" src="https://github.com/stjude-rust-labs/http-cache-stream/actions/workflows/CI.yml/badge.svg" />
    </a>
    <a href="https://crates.io/crates/http-cache-stream" target="_blank">
      <img alt="crates.io version" src="https://img.shields.io/crates/v/http-cache-stream">
    </a>
    <img alt="crates.io downloads" src="https://img.shields.io/crates/d/http-cache-stream">
  </p>

  <p align="center">
    A HTTP caching middleware that supports streaming bodies and uses <a href="https://github.com/kornelski/rusty-http-cache-semantics">http-cache-semantics</a>.
    <br />
    <a href="https://docs.rs/http-cache-stream"><strong>Explore the docs »</strong></a>
    <br />
    <br />
    <a href="https://github.com/stjude-rust-labs/http-cache-stream/issues/new?assignees=&title=Descriptive%20Title&labels=enhancement">Request Feature</a>
    ·
    <a href="https://github.com/stjude-rust-labs/http-cache-stream/issues/new?assignees=&title=Descriptive%20Title&labels=bug">Report Bug</a>
    ·
    ⭐ Consider starring the repo! ⭐
    <br />
  </p>
</p>

## Overview

The `http-cache-stream` crate can be used to cache streaming responses in
accordance with HTTP caching semantics.

### How this crate differs from [`http-cache`][http-cache]

The [`http-cache`][http-cache] crate is a highly-configurable HTTP cache that
supports different storage backends and middleware for many popular Rust HTTP
client APIs.

The `http-cache-stream` crate is inspired by the implementation provided by
`http-cache`, but differs in significant ways:

* ~~`http-cache-stream` supports streaming of requests/responses and does not
  read a response body into memory to store in the cache.~~ (streaming is now
  supported in `http-cache`)
* The default storage implementation for `http-cache-stream` uses advisory file
  locking to coordinate access to storage across multiple processes and threads.
* The default storage implementation is simple and provides no integrity of
  cached bodies, but does provide some fault tolerance for writes to cache
  storage (i.e. partially written cache entries are discarded).
* The API for `http-cache-stream` is not nearly as configurable as `http-cache`.
* Currently only supports a middleware implementation for `reqwest`.

## Development

To bootstrap a development environment, please use the following commands.

```bash
# Clone the repository
git clone git@github.com:stjude-rust-labs/http-cache-stream.git
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
cargo test

# Run the reqwest middleware tests
cargo test -p http-cache-stream-reqwest

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

This project is licensed as either [Apache 2.0][license-apache] or
[MIT][license-mit] at your discretion. Additionally, please see [the
disclaimer](https://github.com/stjude-rust-labs#disclaimer) that applies to all
crates and command line tools made available by St. Jude Rust Labs.

Copyright © 2025-Present [St. Jude Children's Research Hospital](https://github.com/stjude).

[license-apache]: https://github.com/stjude-rust-labs/http-cache-stream/blob/main/LICENSE-APACHE
[license-mit]: https://github.com/stjude-rust-labs/http-cache-stream/blob/main/LICENSE-MIT
[http-cache]: https://github.com/06chaynes/http-cache

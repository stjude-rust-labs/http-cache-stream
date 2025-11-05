# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

#### Added

* Added `with_revalidation_hook` to allow for modifying revalidation request
  headers ([#15](https://github.com/stjude-rust-labs/http-cache-stream/pull/15)).

## 0.2.0 - 08-15-2025

#### Changed

* Refactored the cache implementation so that response bodies being cached are
  streamed to storage while being read from the response ([#11](https://github.com/stjude-rust-labs/http-cache-stream/pull/11)).

#### Fixed

* Fixed an issue with Azure Storage incorrect 304 responses were causing an
  error ([#10](https://github.com/stjude-rust-labs/http-cache-stream/pull/10)).

## 0.1.0 - 03-31-2025

#### Added

* Initial release of the crate.
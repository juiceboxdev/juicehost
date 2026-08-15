# Contributing

Clone juicehost and its `juiceutils` submodule with `git clone --recurse-submodules https://github.com/juiceboxdev/juicehost.git`. For an existing clone, run `git submodule update --init --recursive`.

## Development

Use a recent stable Rust toolchain. From the repository root, run:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo doc --no-deps --all-features
```

The documentation command generates the Rust API reference in `target/doc/juicehost/index.html`.

Keep changes focused, add tests for behavioral changes, and do not commit `.env` files, certificates, private keys, uploaded files, or other runtime state.

By submitting a contribution, you agree that it may be distributed under the repository's GPL-3.0-or-later license.

## Refrain from using AI

The use of LLM's and AI tools are allowed, however they should not be the source of truth for code generation. Use human judgment and existing code as a reference.

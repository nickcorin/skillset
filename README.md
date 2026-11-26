# skillset

Vendor agent skills from GitHub at pinned commits into a folder you choose.

## Build

The Rust toolchain is pinned in `rust-toolchain.toml`. With that toolchain installed:

```sh
cargo build --release
```

The binary is written to `target/release/skillset`.

```sh
skillset version
```

prints the version from `Cargo.toml` that the binary was built with.

## License

Licensed under [MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE).

# skillset

skillset vendors agent skills from GitHub repositories at pinned commits into a folder you choose. A skill is a folder containing a `SKILL.md`. Pinned commits are recorded in a lock file compatible with the `skills-lock.json` format used by `npx skills`.

The crate builds a single self-contained `skillset` binary.

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

Licensed under MIT OR Apache-2.0.

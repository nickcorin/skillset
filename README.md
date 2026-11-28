# skillset

Vendor agent skills from GitHub at pinned commits into a folder you choose.

## Build

```sh
cargo build --release
```

The compiled binary is written to `target/release/skillset`.

## Install skills from a lock file

```sh
skillset install [--lock <FILE>] [--dir <DIR>]
```

`skillset install` reads a version 2 `skills-lock.json`, the lock file `npx skills` writes, and restores every skill at the commit the lock records. `--lock` defaults to `skills-lock.json` in the current folder. A successful install prints nothing.

The install folder is the first of these that is set:

1. `--dir`, resolved against the current directory
2. `skillset.installDir` in the lock, relative to the lock file's folder
3. `.agents/skills`

`skillset.installDir` is a top-level object that `npx skills` ignores when it reads the lock:

```json
{
  "version": 2,
  "skills": { "...": {} },
  "skillset": { "installDir": "vendor/skills" }
}
```

`npx skills` writes the lock back with only `version` and `skills`, so the `skillset` object has to be added again after `npx skills add`, `remove` or `update`.

`<skill>/` holds that skill's files, byte for byte as in the repository at the locked commit. Anything else in the install folder is left as it is. On Unix, a file the archive marks executable is installed executable.

Each repository is downloaded from `https://codeload.github.com/<owner>/<repo>/tar.gz/<commit>`, so only public repositories work. The skill's files are hashed the way `npx skills` hashes them: SHA-256 over the files sorted by path, with `metadata.json`, `.git`, `__pycache__` and `__pypackages__` left out. The hash has to match `computedHash`. Text files are hashed with CRLF turned into LF; the installed bytes are the bytes from the repository.

A symlink in a skill is installed as the file it points to when that file stays inside the skill folder. A link that is broken, leaves the folder, or is absolute is left out.

Every skill is fetched and checked before the install folder changes. A skill folder that already matches is not written, so a second run with the same lock writes nothing.

`skillset` exits with 0 on success, 1 when the install fails, and 2 on a usage error.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

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

The install folder is the first of these that is set. Each path is relative to the folder that contains the lock file:

1. `--dir`
2. `skillset.installDir` in the lock
3. `.agents/skills`

The path has to name a folder inside the lock file's folder. An absolute path, a path that normalizes to the lock file's folder itself, and any `..` that climbs out of that folder are errors.

`skillset.installDir` is a top-level object that `npx skills` ignores when it reads the lock:

```json
{
  "version": 2,
  "skills": { "...": {} },
  "skillset": { "installDir": "vendor/skills" }
}
```

`npx skills` writes the lock back with only `version` and `skills`, so the `skillset` object has to be added again after `npx skills add`, `remove` or `update`.

The install folder belongs to skillset. An entry that is not a locked skill is removed, including a skill dropped from the lock and a symlink. A symlink is removed as itself and is never followed. On the way from the lock file's folder to the install folder, a symlink is replaced with a real directory.

| Path | Contents |
| --- | --- |
| `<skill>/` | the skill's files, byte for byte as in the repository at the locked commit |
| `.licenses/<skill>/` | license files from the root of that repository |

License files are `LICENSE`, `LICENCE`, `COPYING` or `UNLICENSE` with any extension, and `LICENSE-*` or `LICENCE-*` with no extension, `.md` or `.txt`. A repository with no license file at its root is an error, and nothing is written. On Unix, a file the repository marks executable is installed executable.

Each repository is downloaded from `https://codeload.github.com/<owner>/<repo>/tar.gz/<commit>`, so only public repositories work. The skill's files are hashed the way `npx skills` hashes them: SHA-256 over the files sorted by path, with `metadata.json`, `.git`, `__pycache__` and `__pypackages__` left out. The hash has to match `computedHash`. Text files are hashed with CRLF turned into LF; the installed bytes are the bytes from the repository.

A symlink inside a skill folder is an error. An archive path component that is empty, `.` or `..`, or that contains `\` or `:`, is an error.

Every skill is fetched and checked before the install folder changes. When the files already in a skill folder hash to its `computedHash`, that folder is not written, so a second run with the same lock writes nothing.

Only GitHub entries with a `commit`, `skillPath` and `computedHash` can be installed. A lock with any other entry, including a skill installed under a new name (`skillName`), is rejected before anything is downloaded.

`skillset` exits with 0 on success, 1 when the install fails, and 2 on a usage error.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

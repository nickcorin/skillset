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

`skillset install` restores every skill listed in a `skills-lock.json`, the
version 2 lock file that `npx skills add` writes, at the commit the lock
records. `--lock` defaults to `skills-lock.json` in the current folder.

Skills install into the first of:

1. `--dir`, relative to the current folder;
2. `skillset.installDir` in the lock, relative to the lock file;
3. `.agents/skills`, relative to the lock file.

`skillset.installDir` is a top-level object that `npx skills` ignores when it
reads the lock:

```json
{
  "version": 2,
  "skills": { "...": {} },
  "skillset": { "installDir": "vendor/skills" }
}
```

`npx skills` writes the lock back with only `version` and `skills`, so the
`skillset` object has to be added again after `npx skills add`, `remove` or
`update`.

The install folder holds:

| Path | Contents |
| --- | --- |
| `<skill>/` | the skill's files, byte for byte as in its repository at the locked commit |
| `.licenses/<skill>/` | the license files at the root of that repository: `LICENSE`, `LICENCE`, `COPYING` or `UNLICENSE` with any extension, and `LICENSE-*` or `LICENCE-*` with none, `.md` or `.txt` |
| `.skillset-manifest.json` | the skill folders `skillset` placed, so skills dropped from the lock can be removed |

Each repository is downloaded once per commit from
`https://codeload.github.com/<owner>/<repo>/tar.gz/<commit>`, so only public
repositories work. A skill's files are chosen and hashed the way `npx skills`
does (symlinks are followed within the skill folder; `metadata.json`, `.git`,
`__pycache__` and `__pypackages__` are left out) and must match the lock's
`computedHash`.

Nothing in the install folder changes until every skill matches its hash. The
changes are then written to a staging folder inside the install folder, checked,
and renamed into place; if a rename fails, the earlier renames are undone. Paths
that already hold the right content are not rewritten, so a second run with the
same lock changes nothing. Only folders listed in the manifest are removed; a
folder `skillset` did not place is replaced only when a locked skill installs
under the same name.

A repository without a license file still installs, with a warning.

Only GitHub entries with a `commit`, `skillPath` and `computedHash` can be
installed. A lock with any other entry, including skills installed under a new
name (`skillName`), is rejected before anything is downloaded.

`skillset` exits with 0 on success, 1 when the install fails, and 2 on a usage
error.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

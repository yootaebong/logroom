# Contributing

Thanks for your interest in LogRoom.

## Open an issue first

Please open an issue before starting non-trivial work so we can agree on the approach. Small typo or doc fixes can go straight to a pull request.

## Development setup

```sh
pnpm install
pnpm --filter @logroom/core build
pnpm --filter @logroom/desktop tauri dev
```

Requirements: Node.js 20+, pnpm 10, a recent stable Rust toolchain, macOS with Xcode Command Line Tools.

## Tests and checks

Run these before opening a pull request:

```sh
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml
cargo clippy --manifest-path apps/desktop/src-tauri/Cargo.toml --all-targets   # no warnings
pnpm --filter @logroom/core test
pnpm --filter @logroom/desktop exec tsc --noEmit
pnpm exec biome check apps/desktop/src
```

## Formatting

Do **not** run `cargo fmt` on the whole repository — the codebase is not rustfmt-clean and it would reformat many unrelated files. Format only the Rust files you changed:

```sh
rustfmt --edition 2021 path/to/changed_file.rs
```

TypeScript is formatted and linted with Biome.

## Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org/): `<type>: <subject>`, for example `feat: add Jira connector` or `fix: keep lane order on refresh`. Common types: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `perf`.

## License

By contributing, you agree that your contributions are licensed under the [MIT License](LICENSE).

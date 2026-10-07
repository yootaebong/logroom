# LogRoom desktop app

Tauri v2 (Rust) + React + TypeScript (Vite). Run commands from the repository root.

## Setup

```sh
pnpm install
pnpm --filter @logroom/core build   # the desktop typecheck needs core's dist
```

## Develop

```sh
pnpm --filter @logroom/desktop tauri dev
```

Stop the dev app before editing migration files — the watcher rebuilds and applies them to your local DB.

## Build

```sh
pnpm --filter @logroom/desktop tauri build
```

## Test and lint

```sh
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml
cargo clippy --manifest-path apps/desktop/src-tauri/Cargo.toml --all-targets
pnpm --filter @logroom/desktop exec tsc --noEmit
pnpm exec biome check apps/desktop/src
```

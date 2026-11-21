# Chirality engine

A Rust engine that runs the Codex app-server and gives an application a
built-in agent working on workflows, with the parts an application builds its
interface from. See [SPEC.md](SPEC.md).

| Part | Where | Name |
| --- | --- | --- |
| The engine | `crates/engine` | crate `chirality-engine` |
| The Tauri adapter | `crates/tauri` | crate `chirality-engine-tauri` |
| The interface parts | `packages/ui` | package `@chirality/engine-ui` |

The Chirality App (`sgttomas/chirality-app`) is the plain application built
from them.

## Using the parts in an application

Pin one engine revision for both. In Cargo:

```toml
chirality-engine-tauri = { git = "ssh://git@github.com/sgttomas/chirality-engine.git", rev = "<revision>" }
```

with `[net] git-fetch-with-cli = true` in `.cargo/config.toml`. In npm:

```json
"@chirality/engine-ui": "git+ssh://git@github.com/sgttomas/chirality-engine.git#<revision>::path:packages/ui"
```

The Rust side, on the app's Tauri builder:

```rust
tauri::Builder::default()
    .plugin(chirality_engine_tauri::plugin(|app| Ok(engine_config(app)?)))
    .invoke_handler(chirality_engine_tauri::commands(tauri::generate_handler![/* the app's own */]))
```

The interface: a `Store` over `tauriEngine` and `tauriHost`
(`@chirality/engine-ui/tauri`) inside an `EngineProvider`, the design tokens
(`@chirality/engine-ui/tokens.css`) imported once, and either the
`StandardLayout` or the parts the app wants (`Conversation`, `Turns`,
`Composer`, `ThreadList`, `RightPanel` with its own tabs, `FirstUse`,
`SignIn`, ...). `@chirality/engine-ui/fake` runs an interface in a browser on
a scripted engine; `@chirality/engine-ui/testing` has the data the engine
produced from recorded Codex sessions and a transport that replays it.

## Checks

```sh
cargo test --workspace          # engine replays; the adapter headless against the pinned Codex
cd packages/ui && npm ci && npm run typecheck && npm test
npm run bindings                # regenerates the TypeScript bindings from the engine's types
```

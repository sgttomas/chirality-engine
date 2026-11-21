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

The Rust side: the adapter and the three desktop plugins the interface uses,
on the app's Tauri builder.

```toml
tauri = "2"
tauri-plugin-dialog = "2"
tauri-plugin-notification = "2"
tauri-plugin-opener = "2"
```

```rust
tauri::Builder::default()
    .plugin(chirality_engine_tauri::plugin(|app| Ok(engine_config(app)?)))
    .invoke_handler(chirality_engine_tauri::commands![/* the app's own commands */])
    .plugin(tauri_plugin_dialog::init())
    .plugin(tauri_plugin_notification::init())
    .plugin(tauri_plugin_opener::init())
```

`commands!` registers the engine's commands beside the app's own, and refuses
(when the app is built) an app command named like one of the engine's. The
engine's commands are the app's own, so they need no permission; the
plugins' do, in `src-tauri/capabilities/default.json`:

```json
{
  "identifier": "default",
  "windows": ["main"],
  "permissions": ["core:default", "dialog:allow-open", "notification:default", "opener:allow-open-url", "opener:allow-default-urls"]
}
```

The interface ships as TypeScript and CSS for the app's Vite build; it brings
`@tauri-apps/api` and the three plugins' packages itself, and needs `react` and
`react-dom` 19. A `Store` over `tauriEngine` and `tauriHost`
(`@chirality/engine-ui/tauri`), started, inside an `EngineProvider`, which
shows the parts once the store has the engine's first snapshot; the design
tokens (`@chirality/engine-ui/tokens.css`) imported once, and
`@chirality/engine-ui/page.css` when the parts fill the window; then the
`StandardLayout` or the parts the app wants (`Conversation`, `Turns`,
`Composer`, `ThreadList`, `RightPanel` with its own tabs, `FirstUse`,
`SignIn`, ...):

```tsx
const store = new Store(tauriEngine, tauriHost);
createRoot(root).render(<EngineProvider store={store}><StandardLayout brand="My app" /></EngineProvider>);
void store.start();
```

The parts' styles apply only inside the `EngineProvider`. Every engine event
reaches the interface numbered; on a gap the store reads the snapshot and the
thread in view again. `@chirality/engine-ui/fake` runs an interface in a
browser on a scripted engine; `@chirality/engine-ui/testing` (with
`@testing-library/react`) has the data the engine produced from recorded Codex
sessions and a transport that replays it.

## Checks

```sh
cargo test --workspace          # engine replays; with the variables below, also against the pinned Codex
cd packages/ui && npm ci && npm run typecheck && npm test
npm run consumer                # packs the package and builds a minimal Tauri interface from a clean install
npm run bindings                # regenerates the TypeScript bindings from the engine's types
```

The tests that run the pinned Codex, or read a Library edition, are skipped
unless these name them:

| Variable | What it names |
| --- | --- |
| `CHIRALITY_TEST_CODEX` | The pinned `codex` binary (see the Supplier in `SPEC.md`) |
| `CHIRALITY_TEST_LIBRARY` | A Chirality Library edition's `workflows` folder |
| `CHIRALITY_TEST_SCRATCH` | Where those runs keep their folders; a folder in the system's temporary directory by default |

Recording the transcripts again (`cargo test -p chirality-engine record_transcripts -- --ignored`)
needs `CHIRALITY_TEST_CODEX`.

## Licence

MIT; see [LICENSE](LICENSE).

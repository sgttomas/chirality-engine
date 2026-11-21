// The adapter's command names (`chirality_engine_tauri::COMMANDS`; a Rust test checks they match),
// each called as `engine:<name>` (`chirality_engine_tauri::PREFIX`).
export const engineCommands = [
  "snapshot",
  "sign_in",
  "cancel_sign_in",
  "sign_out",
  "add_project",
  "remove_project",
  "workflows",
  "read_thread",
  "start_thread",
  "send",
  "interrupt",
  "fork",
  "revert",
  "rename",
  "archive",
  "mark_read",
  "answer",
  "read_file",
  "find_files",
] as const;

export type EngineCommand = (typeof engineCommands)[number];

/** What the adapter's command names begin with. */
export const enginePrefix = "engine:";

/** The event every engine event arrives as (`chirality_engine_tauri::EVENT`), and the marks after them. */
export const engineEvent = "engine";

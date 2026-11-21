// The adapter's command names (`chirality_engine_tauri::COMMANDS`; a Rust test checks they match).
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

/** The event every engine event arrives as (`chirality_engine_tauri::EVENT`). */
export const engineEvent = "engine";

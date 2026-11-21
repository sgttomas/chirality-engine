// Data the real engine produced from Codex's recorded transcripts
// (crates/engine/src/testkit/export.rs), and a transport that plays it back
// to the real client.
import type { EngineData } from "./transport";

export { ReplayTransport, type EngineData } from "./transport";

const files = import.meta.glob<EngineData>("./engine-data/*.json", { eager: true, import: "default" });

export function engineData(name: string): EngineData {
  const data = files[`./engine-data/${name}.json`];
  if (!data) throw new Error(`no engine data ${name}`);
  return structuredClone(data);
}

export const engineDataNames = Object.keys(files).map((f) => f.replace(/^.*\/(.*)\.json$/, "$1"));

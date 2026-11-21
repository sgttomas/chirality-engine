// For an application's tests: the data the real engine produced from recorded
// Codex sessions, a transport that plays it to the real client, and mounting
// parts over an engine.
export { engineData, engineDataNames, ReplayTransport, type EngineData } from "./replay";
export { fakeEngine, mount, notices, settle, testHost } from "./render";

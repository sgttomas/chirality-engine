// A scripted engine in TypeScript, for running an interface in a browser
// without the desktop: the same client as the real one, playing invented turns.
export { FakeEngine, type FakeOptions } from "./engine";
export { buildScene, sceneNames, type Scene, type SceneName } from "./scenes";
export { fakeHost } from "./host";

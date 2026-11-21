// The Chirality engine's interface parts. An application gives them an engine
// client and a host (`@chirality/engine-ui/tauri` in a Tauri app), a `Store`
// inside an `EngineProvider` (which shows them once the store has started), and
// composes the parts it wants: the whole `StandardLayout`, or `Conversation` (or
// `Turns` and `Composer`) inside its own. Each part brings its own styles;
// `@chirality/engine-ui/tokens.css` is the design language they share, imported
// once by the application, and `page.css` lets them fill the window.

// The engine's types, generated from its Rust API.
export type * from "./engine/types";

// Reaching the engine
export { createEngineClient, type EngineClient, type Host, type Transport } from "./engine/client";
export { engineCommands, engineEvent, type EngineCommand } from "./engine/commands";

// The engine half of the interface's state
export { describe as describeError, EngineProvider, Store, StoreContext, useProjectRoot, useStore, useStoreHandle, type State } from "./state/store";
export { applyEvent, type Model as EngineModel } from "./state/reduce";
export * from "./state/selectors";
export { remember, remembered, resolveSettings } from "./state/settings";

// Layout
export { ErrorNotice, StandardLayout, useWindowTitle, type StandardLayoutProps } from "./ui/StandardLayout";
export { PanelContext, usePanel, usePanelState, type Panel, type PanelControl, type PanelTab } from "./ui/panel";
export { useNotifications, noticeFor } from "./ui/notify";
export { applyTheme, storedTheme, type Theme } from "./ui/theme";

// Projects, threads and the account
export { AccountMenu, Navigator, ThreadList } from "./ui/Navigator";
export { FirstUse, SignIn } from "./ui/FirstUse";

// The conversation
export { Conversation, ConversationHeader, ThreadHeading, Turns } from "./ui/Conversation";
export { TurnView } from "./ui/conversation/TurnView";
export { AgentMessage, ErrorItem, FileChange, Plan, UserMessage, Work } from "./ui/conversation/Items";
export { Markdown } from "./ui/conversation/Markdown";
export { ThreadTitle } from "./ui/conversation/ThreadTitle";

// The composer and what waits on the person
export { Composer } from "./ui/composer/Composer";
export { Approval } from "./ui/composer/Approval";
export { Question } from "./ui/composer/Question";
export { WorkflowPicker } from "./ui/composer/WorkflowPicker";
export { Controls } from "./ui/composer/Controls";
export { MentionMenu, mentionAt, type Mention } from "./ui/composer/Mentions";

// The side panel and diffs
export { RightPanel, standardTabs, type PanelTabSpec, type PanelView } from "./ui/RightPanel";
export { Diff, DiffStat } from "./ui/Diff";
export { Icon, type IconName } from "./ui/Icon";

// Desktop notices when a thread finishes or needs the person, unless it is the one in front of them.
import { useEffect } from "react";

import type { Event, ThreadSummary } from "../engine/types";
import type { State } from "../state/store";
import { useStoreHandle } from "../state/store";

/** The notice an event deserves, if any. */
export function noticeFor(event: Event, before: State, focused: boolean): { title: string; body: string } | null {
  if (event.type !== "thread") return null;
  const now = event.thread;
  const was = before.model?.snapshot.threads.find((t) => t.id === now.id);
  if (!was || (focused && before.thread === now.id)) return null;
  const title = now.title ?? "Thread";
  const needs = (t: ThreadSummary) => t.status.state === "needsApproval" || t.status.state === "needsAnswer";
  if (needs(now) && !needs(was)) return { title, body: now.status.state === "needsAnswer" ? "Has a question" : "Needs your approval" };
  if (was.status.state === "working" && now.status.state === "idle") return { title, body: "Finished" };
  if (was.status.state !== "failed" && now.status.state === "failed") return { title, body: now.status.message };
  return null;
}

export function useNotifications() {
  const store = useStoreHandle();
  useEffect(
    () =>
      store.onEvent((event, before) => {
        const notice = noticeFor(event, before, document.hasFocus());
        if (notice) void store.host.notify(notice.title, notice.body);
      }),
    [store],
  );
}

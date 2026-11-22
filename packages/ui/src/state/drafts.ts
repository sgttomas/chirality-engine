// What the person is writing in each composer: a thread's, or a project's thread to be started.
import type { Attachment, Message, ProjectId, ThreadId, ThreadRef, Workflow } from "../engine/types";

export interface Draft {
  text: string;
  attachments: Attachment[];
  /** Threads mentioned with `@`. */
  threads: ThreadRef[];
  /** The picked workflow: it goes with the next message only, unless `keep`. */
  workflow: Workflow | null;
  keep: boolean;
  /** A message from this composer is with the engine. */
  sending: boolean;
}

export const emptyDraft: Draft = { text: "", attachments: [], threads: [], workflow: null, keep: false, sending: false };

/** The composer of a thread, or of a project's thread not yet started. */
export const draftKey = (thread: ThreadId | null, project: ProjectId) => thread ?? `new:${project}`;

/** What a message took from its draft, as it was when sent. */
export type Taken = Pick<Draft, "text" | "attachments" | "threads" | "workflow">;

export const messageOf = (d: Draft): Message => ({ text: d.text.trim(), attachments: d.attachments, workflow: d.workflow?.id ?? null, threads: d.threads });

/** The draft once the engine has the message: its text if unchanged since, the attachments and
 * threads it carried and no others, and its workflow unless kept attached. */
export function afterSend(d: Draft, taken: Taken): Draft {
  const workflow = d.keep || d.workflow?.id !== taken.workflow?.id ? d.workflow : null;
  return {
    text: d.text === taken.text ? "" : d.text,
    attachments: d.attachments.filter((a) => !taken.attachments.some((t) => t.path === a.path)),
    threads: d.threads.filter((r) => !taken.threads.some((t) => t.thread === r.thread)),
    workflow,
    keep: d.keep && workflow !== null,
    sending: false,
  };
}

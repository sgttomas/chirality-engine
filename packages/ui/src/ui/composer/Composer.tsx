import { useEffect, useRef, useState } from "react";

import type { Attachment, Message, TurnSettings, Workflow } from "../../engine/types";
import { isRunning, pendingFor, shortPath } from "../../state/selectors";
import { remember, remembered, resolveSettings } from "../../state/settings";
import { useProjectRoot, useStore, useStoreHandle } from "../../state/store";
import { Icon } from "../Icon";
import { Approval } from "./Approval";
import { Controls } from "./Controls";
import { mentionAt, MentionMenu, type Mention } from "./Mentions";
import { Question } from "./Question";
import { WorkflowPicker } from "./WorkflowPicker";
import "../styles/base.css";
import "../styles/composer.css";

const imageExt = /\.(png|jpe?g|gif|webp|heic|tiff?)$/i;

/** The message box for the selected thread, or for a new thread in the selected project:
 * attachments, `@` references, the workflow picker, model, reasoning and access, and the
 * approval or question waiting on the person. */
export function Composer() {
  const key = useStore((s) => s.thread ?? s.project);
  return key ? <ThreadComposer key={key} /> : null;
}

/** A picked workflow, and whether the person keeps it attached to every message. */
type Picked = { workflow: Workflow | null; keep: boolean };

function ThreadComposer() {
  const store = useStoreHandle();
  const thread = useStore((s) => s.thread);
  const project = useStore((s) => s.project)!;
  const models = useStore((s) => s.model!.snapshot.models);
  const summary = useStore((s) => s.model!.snapshot.threads.find((t) => t.id === s.thread));
  const view = useStore((s) => (s.thread ? s.model!.views[s.thread] : undefined));
  const pending = useStore((s) => pendingFor(s.model!.snapshot, s.thread));
  const workflows = useStore((s) => s.workflows[project]) ?? [];
  const running = isRunning(view);
  const root = useProjectRoot();
  // The pin is kept with its thread (or the project's new thread) for the session.
  const pinKey = thread ?? `new:${project}`;

  const [text, setText] = useState("");
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  // A picked workflow goes with the next message only, unless the person keeps it attached.
  const [picked, setPicked] = useState<Picked>(() => {
    const kept = store.pinned(pinKey);
    return { workflow: kept ?? null, keep: kept !== undefined };
  });
  // The selection as it is now, for a send that resolves after the person changed it.
  const current = useRef(picked);
  const sending = useRef(false);
  const [settings, setSettings] = useState<TurnSettings>(() => resolveSettings(models, summary?.settings ?? remembered()));
  const [mention, setMention] = useState<Mention | null>(null);
  const input = useRef<HTMLTextAreaElement>(null);
  const { workflow, keep } = picked;

  useEffect(() => {
    if (!pending) input.current?.focus();
  }, [pending]);

  const choose = (next: TurnSettings) => {
    setSettings(next);
    remember(next);
  };

  const select = (next: Workflow | null, keepIt: boolean) => {
    const value = { workflow: next, keep: next !== null && keepIt };
    current.current = value;
    setPicked(value);
    store.pin(pinKey, value.keep ? next : null);
  };

  /** What a message took, once the engine has it: the text and attachments it carried, and its
   * workflow unless kept attached; anything the person changed meanwhile stays. */
  const sent = (draft: string, carried: Attachment[], workflowSent: Workflow | null) => {
    setText((t) => (t === draft ? "" : t));
    setAttachments((a) => (a === carried ? [] : a));
    if (!current.current.keep && current.current.workflow === workflowSent) select(null, false);
  };

  const send = async () => {
    const body = text.trim();
    if (!body || sending.current) return;
    const message: Message = { text: body, attachments, workflow: workflow?.id ?? null };
    const draft = { text, attachments, workflow };
    sending.current = true;
    setMention(null);
    try {
      if (thread) {
        const ok = await store.attempt(async () => {
          await store.engine.send(thread, message, settings);
          return true;
        });
        if (ok) sent(draft.text, draft.attachments, draft.workflow);
      } else {
        const id = await store.attempt(() => store.engine.startThread(project, message, settings));
        if (!id) return;
        // A kept workflow moves to the thread its message started.
        if (current.current.keep) store.pin(id, current.current.workflow);
        store.pin(pinKey, null);
        sent(draft.text, draft.attachments, draft.workflow);
        await store.selectThread(id);
      }
    } finally {
      sending.current = false;
    }
  };

  const stop = () => thread && store.attempt(() => store.engine.interrupt(thread));

  const attach = async () => {
    const paths = await store.host.pickFiles();
    const added = paths.map((path): Attachment => (imageExt.test(path) ? { kind: "image", path } : { kind: "file", path }));
    setAttachments((a) => [...a, ...added.filter((n) => !a.some((x) => x.path === n.path))]);
  };

  const onChange = (value: string, caret: number) => {
    setText(value);
    setMention(mentionAt(value, caret));
  };

  const pickMention = (path: string) => {
    if (!mention) return;
    const before = text.slice(0, mention.start);
    const after = text.slice(mention.end);
    setText(`${before}@${shortPath(path, root)} ${after.replace(/^\s/, "")}`);
    setAttachments((a) => (a.some((x) => x.path === path) ? a : [...a, { kind: "file", path }]));
    setMention(null);
    input.current?.focus();
  };

  return (
    <div className={`composer-area ${thread ? "" : "composer-area--new"}`}>
      {pending?.request.kind === "question" ? (
        <Question key={pending.id} pending={pending} />
      ) : pending ? (
        <Approval key={pending.id} pending={pending} />
      ) : null}
      <div className={`composer ${pending ? "composer--behind" : ""}`}>
        {mention && <MentionMenu project={project} mention={mention} onPick={pickMention} onClose={() => setMention(null)} />}
        {(workflow || attachments.length > 0) && (
          <div className="chips composer-chips">
            {workflow && (
              <span className="chip chip--workflow">
                <Icon name="book" /> {workflow.name}
                <button className="chip-pin" aria-label="Keep attached" title="Keep attached" aria-pressed={keep} onClick={() => select(workflow, !keep)}>
                  <Icon name="pin" />
                </button>
                <button className="chip-x" aria-label={`Remove ${workflow.name}`} onClick={() => select(null, false)}>
                  <Icon name="x" />
                </button>
              </span>
            )}
            {attachments.map((a) => (
              <span key={a.path} className="chip" title={shortPath(a.path, root)}>
                <Icon name={a.kind === "image" ? "file" : "paperclip"} /> {a.path.split("/").pop()}
                <button className="chip-x" aria-label={`Remove ${shortPath(a.path, root)}`} onClick={() => setAttachments((all) => all.filter((x) => x !== a))}>
                  <Icon name="x" />
                </button>
              </span>
            ))}
          </div>
        )}
        <textarea
          ref={input}
          className="composer-input"
          rows={thread ? 1 : 3}
          placeholder={running ? "Steer the turn" : thread ? "Reply" : "What should we work on?"}
          value={text}
          onChange={(e) => onChange(e.target.value, e.target.selectionStart)}
          onKeyDown={(e) => {
            if (mention && ["ArrowUp", "ArrowDown", "Enter", "Tab", "Escape"].includes(e.key)) return;
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              void send();
            }
          }}
        />
        <div className="composer-line">
          <button className="quiet" aria-label="Attach files" title="Attach files" onClick={attach}>
            <Icon name="paperclip" />
          </button>
          <button className="quiet" aria-label="Reference a file" title="Reference a file" onClick={() => onChange(`${text}@`, text.length + 1)}>
            <Icon name="at" />
          </button>
          <WorkflowPicker workflows={workflows} picked={workflow} onPick={(w) => select(w, keep)} />
          <Controls models={models} settings={settings} onChange={choose} />
          <span className="composer-spacer" />
          {running && text.trim() === "" ? (
            <button className="send send--stop" aria-label="Stop" title="Stop" onClick={stop}>
              <Icon name="stop" />
            </button>
          ) : (
            <button className="send" aria-label={running ? "Steer" : "Send"} title={running ? "Steer" : "Send"} disabled={!text.trim()} onClick={send}>
              <Icon name="arrowUp" />
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

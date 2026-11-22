import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";

import type { Attachment, ThreadSummary, TurnSettings, Workflow } from "../../engine/types";
import { draftKey, emptyDraft, type Draft } from "../../state/drafts";
import { isRunning, nameOf, pendingFor, shortPath } from "../../state/selectors";
import { remember, remembered, resolveSettings } from "../../state/settings";
import { useProjectRoot, useStore, useStoreHandle } from "../../state/store";
import { SubagentControls } from "../Subagents";
import { Icon } from "../Icon";
import { Approval } from "./Approval";
import { Controls } from "./Controls";
import { mentionFrom, mentionGroups, MentionMenu, typedMention, type Mention } from "./Mentions";
import { Question } from "./Question";
import { WorkflowPicker } from "./WorkflowPicker";
import "../styles/base.css";
import "../styles/composer.css";

const imageExt = /\.(png|jpe?g|gif|webp|heic|tiff?)$/i;

/** The message box for the selected thread, or for a new thread in the selected project:
 * attachments, threads mentioned with `@`, the workflow picker, model, reasoning and access, and the
 * approval or question waiting on the person. */
export function Composer() {
  const key = useStore((s) => s.thread ?? s.project);
  const subagent = useStore((s) => !!s.model!.snapshot.threads.find((t) => t.id === s.thread)?.subagent);
  if (!key) return null;
  return subagent ? <SubagentControls key={key} /> : <ThreadComposer key={key} />;
}

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
  // The draft lives in the store, with its thread (or the project's new thread).
  const key = draftKey(thread, project);
  const { text, attachments, threads: mentioned, workflow, keep } = useStore((s) => s.drafts[key] ?? emptyDraft);
  const snapshot = useStore((s) => s.model!.snapshot);
  const edit = (change: Partial<Draft> | ((d: Draft) => Partial<Draft>)) => store.editDraft(key, change);

  const [settings, setSettings] = useState<TurnSettings>(() => resolveSettings(models, summary?.settings ?? remembered()));
  // The mention being typed, kept in a ref too so a caret move heard before the next render sees it.
  const [mention, setMentionState] = useState<Mention | null>(null);
  const open = useRef<Mention | null>(null);
  const setMention = (next: Mention | null) => {
    open.current = next;
    setMentionState(next);
  };
  const [active, setActive] = useState(0);
  const input = useRef<HTMLTextAreaElement>(null);
  // Where the caret goes once the text it follows is in the box.
  const caret = useRef<number | null>(null);
  const groups = mention ? mentionGroups(snapshot, project, thread, mention.query) : [];
  const choices = groups.flatMap((g) => g.threads);
  const mentioning = choices.length > 0;
  /** The mention as typed, while some thread's title matches it: it closes for good when none does. */
  const matching = (m: Mention | null) => (m && mentionGroups(snapshot, project, thread, m.query).length > 0 ? m : null);

  useLayoutEffect(() => {
    const at = caret.current;
    if (at === null || !input.current) return;
    caret.current = null;
    input.current.focus();
    input.current.setSelectionRange(at, at);
  }, [text]);

  useEffect(() => {
    if (!pending) input.current?.focus();
  }, [pending]);

  const choose = (next: TurnSettings) => {
    setSettings(next);
    remember(next);
  };

  const select = (next: Workflow | null, keepIt: boolean) => edit({ workflow: next, keep: next !== null && keepIt });

  const send = () => {
    setMention(null);
    return store.sendDraft(key, project, thread, settings);
  };

  const stop = () => thread && store.attempt(() => store.engine.interrupt(thread));

  const attach = async () => {
    const paths = await store.host.pickFiles();
    const added = paths.map((path): Attachment => (imageExt.test(path) ? { kind: "image", path } : { kind: "file", path }));
    edit((d) => ({ attachments: [...d.attachments, ...added.filter((n) => !d.attachments.some((x) => x.path === n.path))] }));
  };

  const onChange = (value: string, at: number) => {
    // An open mention follows the typing; only an `@` typed fresh opens one.
    const now = open.current ? mentionFrom(value, open.current.start, at) : typedMention(text, value, at);
    edit({ text: value });
    setMention(matching(now));
    setActive(0);
  };

  const pickMention = (picked: ThreadSummary) => {
    if (!mention) return;
    const name = nameOf(picked);
    const before = `${text.slice(0, mention.start)}@${name} `;
    caret.current = before.length;
    edit((d) => ({
      text: before + text.slice(mention.end).replace(/^ /, ""),
      threads: d.threads.some((r) => r.thread === picked.id) ? d.threads : [...d.threads, { thread: picked.id, title: name }],
    }));
    setMention(null);
  };

  /** Starts a mention at the caret, after a space unless one is there. */
  const startMention = () => {
    const box = input.current;
    const [from, to] = box ? [box.selectionStart, box.selectionEnd] : [text.length, text.length];
    const before = text.slice(0, from);
    const opened = `${before}${before && !/\s$/.test(before) ? " " : ""}@`;
    caret.current = opened.length;
    edit({ text: opened + text.slice(to) });
    setMention(matching({ start: opened.length - 1, end: opened.length, query: "" }));
    setActive(0);
    input.current?.focus();
  };

  /** With the list open, the arrows choose, Enter or Tab picks, and Escape closes it; while an input
   * method composes, every key is its own. */
  const mentionKey = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.nativeEvent.isComposing) return false;
    const last = choices.length - 1;
    if (e.key === "ArrowDown") setActive((a) => Math.min(a + 1, last));
    else if (e.key === "ArrowUp") setActive((a) => Math.max(a - 1, 0));
    else if (e.key === "Enter" || e.key === "Tab") pickMention(choices[Math.min(active, last)]);
    else if (e.key === "Escape") setMention(null);
    else return false;
    e.preventDefault();
    return true;
  };

  const nameOfRef = (id: string, title: string | null) => {
    const known = snapshot.threads.find((t) => t.id === id);
    return known ? nameOf(known) : (title ?? "A thread");
  };

  return (
    <div className={`composer-area ${thread ? "" : "composer-area--new"}`}>
      {pending?.request.kind === "question" ? (
        <Question key={pending.id} pending={pending} />
      ) : pending ? (
        <Approval key={pending.id} pending={pending} />
      ) : null}
      <div className={`composer ${pending ? "composer--behind" : ""}`}>
        {mentioning && <MentionMenu groups={groups} active={active} onActive={setActive} onPick={pickMention} />}
        {(workflow || attachments.length > 0 || mentioned.length > 0) && (
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
            {mentioned.map((r) => (
              <span key={r.thread} className="chip chip--thread">
                <Icon name="at" /> {nameOfRef(r.thread, r.title)}
                <button className="chip-x" aria-label={`Remove ${nameOfRef(r.thread, r.title)}`} onClick={() => edit((d) => ({ threads: d.threads.filter((x) => x.thread !== r.thread) }))}>
                  <Icon name="x" />
                </button>
              </span>
            ))}
            {attachments.map((a) => (
              <span key={a.path} className="chip" title={shortPath(a.path, root)}>
                <Icon name={a.kind === "image" ? "file" : "paperclip"} /> {a.path.split("/").pop()}
                <button className="chip-x" aria-label={`Remove ${shortPath(a.path, root)}`} onClick={() => edit((d) => ({ attachments: d.attachments.filter((x) => x.path !== a.path) }))}>
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
          onBlur={() => setMention(null)}
          onSelect={(e) => {
            // The caret moved away from the name being typed: the list closes.
            const box = e.currentTarget;
            if (open.current && (box.selectionStart !== box.selectionEnd || box.selectionStart !== open.current.end)) setMention(null);
          }}
          onKeyDown={(e) => {
            if (mentioning && mentionKey(e)) return;
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
          <button className="quiet" aria-label="Mention a thread" title="Mention a thread" onMouseDown={(e) => e.preventDefault()} onClick={startMention}>
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

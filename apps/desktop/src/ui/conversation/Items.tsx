import type { Item, PlanStep, Workflow } from "../../engine/types";
import { shortPath } from "../../state/selectors";
import { useProjectRoot } from "../../state/store";
import { Diff, DiffStat } from "../Diff";
import { Icon } from "../Icon";
import { usePanel } from "../panel";
import { workLine, workSummary } from "./group";
import { Markdown } from "./Markdown";

export function UserMessage({ item, workflows }: { item: Item; workflows: Workflow[] }) {
  const root = useProjectRoot();
  if (item.body.kind !== "userMessage") return null;
  const { text, attachments, workflow } = item.body;
  const picked = workflow ? workflows.find((w) => w.id === workflow) : undefined;
  return (
    <div className="msg-user">
      {(picked || attachments.length > 0) && (
        <div className="chips">
          {workflow && (
            <span className="chip chip--workflow">
              <Icon name="book" /> {picked?.name ?? workflow.split("/").at(-2) ?? workflow}
            </span>
          )}
          {attachments.map((a) => (
            <span key={a.path} className="chip" title={shortPath(a.path, root)}>
              <Icon name={a.kind === "image" ? "file" : "paperclip"} /> {a.path.split("/").pop()}
            </span>
          ))}
        </div>
      )}
      <div className="msg-user-text">{text}</div>
    </div>
  );
}

export function AgentMessage({ text }: { text: string }) {
  return <div className="msg-agent">{text ? <Markdown text={text} /> : <span className="cursor" />}</div>;
}

/** Reasoning and tool activity, folded; open while it runs if the person opens it. */
export function Work({ items, live }: { items: Item[]; live: boolean }) {
  const last = items[items.length - 1];
  return (
    <details className={`work ${live ? "work--live" : ""} ${live && last.body.kind === "command" ? "work--command" : ""}`}>
      <summary>
        <span className="work-summary">{live ? workLine(last) : workSummary(items)}</span>
      </summary>
      <ul className="work-list">
        {items.map((item) => (
          <WorkItem key={item.id} item={item} />
        ))}
      </ul>
    </details>
  );
}

function WorkItem({ item }: { item: Item }) {
  const b = item.body;
  switch (b.kind) {
    case "reasoning":
      return <li className="work-reasoning">{b.summary}</li>;
    case "command":
      return (
        <li className="work-command">
          <div className="work-command-line">
            <Icon name="terminal" />
            <code>{b.command}</code>
            {b.exitCode !== null && b.exitCode !== 0 && <span className="work-exit">exit {b.exitCode}</span>}
          </div>
          {b.output && <pre className="work-output">{b.output}</pre>}
        </li>
      );
    case "toolCall":
      return (
        <li className="work-command">
          <div className="work-command-line">
            <code>{workLine(item)}</code>
            {b.status === "failed" && <span className="work-exit">failed</span>}
          </div>
          {b.result && <pre className="work-output">{b.result}</pre>}
          {b.images.map((src, i) => (
            <img key={i} className="work-image" src={src} alt="" />
          ))}
        </li>
      );
    case "webSearch":
      return <li className="work-reasoning">{workLine(item)}</li>;
    default:
      return null;
  }
}

export function Plan({ steps }: { steps: PlanStep[] }) {
  const done = steps.filter((s) => s.status === "completed").length;
  return (
    <div className="plan">
      <div className="plan-head">
        Plan <span className="faint">{done}/{steps.length}</span>
      </div>
      <ol className="plan-steps">
        {steps.map((step, i) => (
          <li key={i} className={`plan-step plan-step--${step.status}`}>
            <span className="plan-box">{step.status === "completed" && <Icon name="check" />}</span>
            {step.text}
          </li>
        ))}
      </ol>
    </div>
  );
}

export function FileChange({ item }: { item: Item }) {
  const { show } = usePanel();
  const root = useProjectRoot();
  if (item.body.kind !== "fileChange") return null;
  return (
    <div className="change">
      {item.body.files.map((file) => (
        <div key={file.path} className="change-file">
          <button className="change-head" onClick={() => show("changes", file.path)}>
            <Icon name="file" />
            <span className="change-path">{shortPath(file.path, root)}</span>
            <DiffStat diff={file.diff} />
          </button>
          <Diff diff={file.diff} limit={8} />
        </div>
      ))}
    </div>
  );
}

export function ErrorItem({ message }: { message: string }) {
  return <div className="msg-error">{message}</div>;
}

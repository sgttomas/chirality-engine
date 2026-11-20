import { useEffect, useState } from "react";

import type { PendingRequest } from "../../engine/types";
import { useStoreHandle } from "../../state/store";

/** The agent's questions, one at a time: a press answers and moves on. */
export function Question({ pending }: { pending: PendingRequest }) {
  const store = useStoreHandle();
  const questions = pending.request.kind === "question" ? pending.request.questions : [];
  const [index, setIndex] = useState(0);
  const [answers, setAnswers] = useState<[string, string][]>([]);
  const [own, setOwn] = useState("");
  const [sent, setSent] = useState(false);
  const question = questions[index];

  const give = (value: string) => {
    if (sent || !value.trim()) return;
    const all: [string, string][] = [...answers, [question.id, value.trim()]];
    setOwn("");
    if (index + 1 < questions.length) {
      setAnswers(all);
      setIndex(index + 1);
      return;
    }
    setSent(true);
    void store.attempt(() => store.engine.answer(pending.id, { kind: "answers", answers: all })).then(() => setSent(false));
  };

  useEffect(() => {
    const keys = (e: KeyboardEvent) => {
      if (e.target instanceof HTMLTextAreaElement || e.target instanceof HTMLInputElement) return;
      const n = Number(e.key);
      if (n >= 1 && n <= (question?.options.length ?? 0)) give(question.options[n - 1]);
    };
    document.addEventListener("keydown", keys);
    return () => document.removeEventListener("keydown", keys);
  });

  if (!question) return null;
  return (
    <div className="ask ask--question" role="dialog" aria-label={question.header ?? "Question"}>
      <div className="ask-head">
        {question.header && <span className="ask-eyebrow">{question.header}</span>}
        {questions.length > 1 && (
          <span className="faint">
            {index + 1} of {questions.length}
          </span>
        )}
      </div>
      <div className="ask-question">{question.text}</div>
      <div className="ask-options">
        {question.options.map((option, i) => (
          <button key={option} className="ask-option" disabled={sent} onClick={() => give(option)}>
            <kbd>{i + 1}</kbd>
            {option}
          </button>
        ))}
      </div>
      {question.freeText && (
        <form
          className="ask-own"
          onSubmit={(e) => {
            e.preventDefault();
            give(own);
          }}
        >
          <input type="text" placeholder="Or say something else" value={own} onChange={(e) => setOwn(e.target.value)} aria-label="Your answer" />
          <button type="submit" disabled={!own.trim() || sent}>
            Answer
          </button>
        </form>
      )}
    </div>
  );
}

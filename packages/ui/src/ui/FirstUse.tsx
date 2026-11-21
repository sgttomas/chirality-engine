import type { ReactNode } from "react";

import { useStore, useStoreHandle } from "../state/store";
import "./styles/first-use.css";

/** Sign in, add a project folder, start. */
export function FirstUse({ brand = "Chirality" }: { brand?: ReactNode }) {
  const store = useStoreHandle();
  const account = useStore((s) => s.model!.snapshot.account);
  const codex = useStore((s) => s.model!.snapshot.codex);
  const signedIn = account.state === "signedIn";

  const addProject = () => void store.addProjectFolder();

  return (
    <main className="first-use">
      <div className="first-use-card">
        <h1 className="brand">{brand}</h1>
        <ol className="first-use-steps">
          <li className={signedIn ? "done" : "current"}>
            <span className="first-use-n">1</span>
            <div>
              <div className="first-use-title">Sign in with ChatGPT</div>
              <SignIn />
            </div>
          </li>
          <li className={signedIn ? "current" : ""}>
            <span className="first-use-n">2</span>
            <div>
              <div className="first-use-title">Add a project folder</div>
              {signedIn && (
                <div className="first-use-actions">
                  <button className="primary" onClick={addProject}>
                    Choose folder
                  </button>
                </div>
              )}
            </div>
          </li>
          <li>
            <span className="first-use-n">3</span>
            <div className="first-use-title">Start</div>
          </li>
        </ol>
        {codex.state === "failed" && <p className="first-use-error">{codex.message}</p>}
      </div>
    </main>
  );
}

/** Sign in with ChatGPT through Codex: the button, the page to finish it on, or who is signed in. */
export function SignIn() {
  const store = useStoreHandle();
  const account = useStore((s) => s.model!.snapshot.account);
  const codex = useStore((s) => s.model!.snapshot.codex);
  if (account.state === "signedIn") return <div className="faint">{account.email ?? "Signed in"}</div>;
  if (account.state === "signingIn")
    return (
      <div className="first-use-actions">
        <button className="primary" onClick={() => store.attempt(() => store.host.openUrl(account.url))}>
          Open sign-in page
        </button>
        <button className="quiet" onClick={() => store.attempt(() => store.engine.cancelSignIn())}>
          Cancel
        </button>
      </div>
    );
  return (
    <div className="first-use-actions">
      <button className="primary" disabled={codex.state !== "ready"} onClick={() => store.attempt(() => store.engine.signIn())}>
        Sign in
      </button>
    </div>
  );
}

import { useStore, useStoreHandle } from "../state/store";
import "./styles/first-use.css";

/** Sign in, add a project folder, start. */
export function FirstUse() {
  const store = useStoreHandle();
  const account = useStore((s) => s.model!.snapshot.account);
  const codex = useStore((s) => s.model!.snapshot.codex);
  const signedIn = account.state === "signedIn";

  const addProject = async () => {
    const path = await store.host.pickFolder();
    if (!path) return;
    const project = await store.attempt(() => store.engine.addProject(path));
    if (project) store.selectProject(project.id);
  };

  return (
    <main className="first-use">
      <div className="first-use-card">
        <h1 className="brand">Chirality</h1>
        <ol className="first-use-steps">
          <li className={signedIn ? "done" : "current"}>
            <span className="first-use-n">1</span>
            <div>
              <div className="first-use-title">Sign in with ChatGPT</div>
              {account.state === "signedIn" && <div className="faint">{account.email ?? "Signed in"}</div>}
              {account.state === "signingIn" && (
                <div className="first-use-actions">
                  <button className="primary" onClick={() => store.host.openUrl(account.url)}>
                    Open sign-in page
                  </button>
                  <button className="quiet" onClick={() => store.attempt(() => store.engine.cancelSignIn())}>
                    Cancel
                  </button>
                </div>
              )}
              {(account.state === "signedOut" || account.state === "unknown") && (
                <div className="first-use-actions">
                  <button className="primary" disabled={codex.state !== "ready"} onClick={() => store.attempt(() => store.engine.signIn())}>
                    Sign in
                  </button>
                </div>
              )}
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

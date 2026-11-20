# Working in chirality-engine

Read `SPEC.md` first. It says what the engine and the wrapper are for, what
they must do, and what they must not become. When the spec and the code
disagree, either the code is wrong or the spec needs changing: say which.

## How to work

- Learn from the references in `SPEC.md` before designing a part. T3 Code is
  the pattern; v4 is mostly what not to do.
- Keep modules small and single-purpose (about 600 lines at most). Types at
  every boundary; nothing crosses between engine and interface as hand-built
  JSON.
- Test what Codex really does: engine tests run the pinned Codex against a
  local stub model; interface tests use data the engine produced.
- Interface text: say what the person needs, once. No explanatory text,
  warnings or disclosures they did not ask for.
- One independent review for a substantial change to the engine's protocol
  handling or to how the person's approvals and questions are answered.
  No reviews of wording.

## Boundaries

- Development and tests use a scratch Codex home, never `~/.codex`.
- No agent signs in, enters or reads a credential. Live model turns need the
  owner's own sign-in.
- Ordinary packages from npm and crates.io may be added and are pinned in the
  lockfiles. Any other download (a binary, an archive) needs the owner's yes,
  naming the file, source and size.
- Do not commit secrets, Codex homes, session records or downloaded binaries.

## Git

Branch from `main` with a prefix naming the author (`claude/`, `codex/`).
Merge through a pull request once its checks pass and any review it needs is
done. No force pushes or history rewrites.

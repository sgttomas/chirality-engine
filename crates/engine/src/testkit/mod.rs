//! Tests on what Codex really sends, as T3 Code tests its Codex adapter.
//!
//! - `replay_<scenario>`: the engine's real client and mapping against a
//!   transcript recorded from the pinned Codex (`fixtures/codex/`).
//! - `record_transcripts` (ignored): records every transcript again, running
//!   the pinned Codex against the stub model:
//!   `cargo test -p chirality-engine record_transcripts -- --ignored`.
//! - `live_main_path`: the main path against the real Codex, not recorded;
//!   skipped when the binary is absent.

mod export;
mod harness;
mod record;
mod scenarios;
mod stub;
mod transcript;

use harness::{Ctx, codex_present};

macro_rules! scenarios {
    ($($name:ident),* $(,)?) => {
        mod replay {
            use super::*;
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
                async fn $name() {
                    let mut ctx = Ctx::replay(stringify!($name)).await;
                    scenarios::$name(&mut ctx).await;
                    ctx.finish(stringify!($name)).await;
                }
            )*
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        #[ignore = "runs the pinned Codex to record transcripts"]
        async fn record_transcripts() {
            assert!(codex_present(), "the pinned Codex is needed to record");
            let only = std::env::var("CHIRALITY_RECORD").ok();
            $(
                if only.as_deref().is_none_or(|o| o == stringify!($name)) {
                    let mut ctx = Ctx::live(stringify!($name), true).await;
                    scenarios::$name(&mut ctx).await;
                    ctx.finish(stringify!($name)).await;
                }
            )*
        }
    };
}

scenarios!(start_and_models, text_turn, workflow_turn, command_approved, command_declined, file_change, question, interrupt, fork, revert, restart);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_main_path() {
    if !codex_present() {
        eprintln!("skipped: the pinned Codex is not at {}", harness::CODEX);
        return;
    }
    let mut ctx = Ctx::live("main_path", false).await;
    scenarios::main_path(&mut ctx).await;
    ctx.finish("main_path").await;
}

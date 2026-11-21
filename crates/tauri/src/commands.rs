//! One command per engine method, each waiting for the engine's start.

use std::path::PathBuf;

use chirality_engine::{Answer, Message, Project, ProjectId, RequestId, Result, Snapshot, ThreadId, ThreadView, TurnId, TurnSettings, Workflow};
use tauri::ipc::Invoke;
use tauri::{Runtime, State};

use crate::{Started, started};

pub(crate) fn handle<R: Runtime>(invoke: Invoke<R>) -> bool {
    let handler: fn(Invoke<R>) -> bool = tauri::generate_handler![
        snapshot,
        sign_in,
        cancel_sign_in,
        sign_out,
        add_project,
        remove_project,
        workflows,
        read_thread,
        start_thread,
        send,
        interrupt,
        fork,
        revert,
        rename,
        archive,
        mark_read,
        answer,
        read_file,
        find_files,
    ];
    handler(invoke)
}

type S<'a> = State<'a, Started>;

#[tauri::command]
async fn snapshot(s: S<'_>) -> Result<Snapshot> {
    Ok(started(&s).await?.snapshot())
}
#[tauri::command]
async fn sign_in(s: S<'_>) -> Result<()> {
    started(&s).await?.sign_in().await
}
#[tauri::command]
async fn cancel_sign_in(s: S<'_>) -> Result<()> {
    started(&s).await?.cancel_sign_in().await
}
#[tauri::command]
async fn sign_out(s: S<'_>) -> Result<()> {
    started(&s).await?.sign_out().await
}
#[tauri::command]
async fn add_project(s: S<'_>, path: PathBuf) -> Result<Project> {
    started(&s).await?.add_project(path).await
}
#[tauri::command]
async fn remove_project(s: S<'_>, project: ProjectId) -> Result<()> {
    started(&s).await?.remove_project(&project).await
}
#[tauri::command]
async fn workflows(s: S<'_>, project: ProjectId) -> Result<Vec<Workflow>> {
    started(&s).await?.workflows(&project).await
}
#[tauri::command]
async fn read_thread(s: S<'_>, thread: ThreadId) -> Result<ThreadView> {
    started(&s).await?.read_thread(&thread).await
}
#[tauri::command]
async fn start_thread(s: S<'_>, project: ProjectId, message: Message, settings: TurnSettings) -> Result<ThreadId> {
    started(&s).await?.start_thread(&project, message, settings).await
}
#[tauri::command]
async fn send(s: S<'_>, thread: ThreadId, message: Message, settings: TurnSettings) -> Result<()> {
    started(&s).await?.send(&thread, message, settings).await
}
#[tauri::command]
async fn interrupt(s: S<'_>, thread: ThreadId) -> Result<()> {
    started(&s).await?.interrupt(&thread).await
}
#[tauri::command]
async fn fork(s: S<'_>, thread: ThreadId, through: TurnId) -> Result<ThreadId> {
    started(&s).await?.fork(&thread, &through).await
}
#[tauri::command]
async fn revert(s: S<'_>, thread: ThreadId, from: TurnId) -> Result<()> {
    started(&s).await?.revert(&thread, &from).await
}
#[tauri::command]
async fn rename(s: S<'_>, thread: ThreadId, title: String) -> Result<()> {
    started(&s).await?.rename(&thread, title).await
}
#[tauri::command]
async fn archive(s: S<'_>, thread: ThreadId) -> Result<()> {
    started(&s).await?.archive(&thread).await
}
#[tauri::command]
async fn mark_read(s: S<'_>, thread: ThreadId) -> Result<()> {
    started(&s).await?.mark_read(&thread);
    Ok(())
}
#[tauri::command]
async fn answer(s: S<'_>, request: RequestId, answer: Answer) -> Result<()> {
    started(&s).await?.answer(&request, answer).await
}
#[tauri::command]
async fn read_file(s: S<'_>, project: ProjectId, path: PathBuf) -> Result<String> {
    started(&s).await?.read_file(&project, path).await
}
#[tauri::command]
async fn find_files(s: S<'_>, project: ProjectId, query: String) -> Result<Vec<PathBuf>> {
    started(&s).await?.find_files(&project, query).await
}


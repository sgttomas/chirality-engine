//! The folders the person has added, kept in `projects.json` in the state folder.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::api::*;
use crate::session::Inner;

const FILE: &str = "projects.json";

#[derive(Default, Serialize, Deserialize)]
struct File {
    projects: Vec<Project>,
}

pub(crate) fn load(state_dir: &Path) -> Result<Vec<Project>> {
    match std::fs::read(state_dir.join(FILE)) {
        Ok(bytes) => serde_json::from_slice::<File>(&bytes).map(|f| f.projects).map_err(|e| invalid(format!("{FILE} cannot be read: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(invalid(format!("{FILE} cannot be read: {e}"))),
    }
}

pub(crate) fn save(state_dir: &Path, projects: &[Project]) -> Result<()> {
    std::fs::create_dir_all(state_dir).map_err(|e| invalid(e.to_string()))?;
    let bytes = serde_json::to_vec_pretty(&File { projects: projects.to_vec() }).expect("projects serialise");
    let temp = state_dir.join(format!("{FILE}.tmp"));
    std::fs::write(&temp, bytes).and_then(|_| std::fs::rename(&temp, state_dir.join(FILE))).map_err(|e| invalid(e.to_string()))
}

/// A project for a folder; its id is derived from the folder, so adding the
/// same folder twice gives the same project.
pub(crate) fn project_for(path: &Path) -> Result<Project> {
    let path: PathBuf = path.canonicalize().map_err(|e| invalid(format!("{}: {e}", path.display())))?;
    if !path.is_dir() {
        return Err(invalid(format!("{} is not a folder", path.display())));
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
    Ok(Project { id: ProjectId(format!("{:016x}", fnv1a(path.to_string_lossy().as_bytes()))), name, path })
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, b| (hash ^ *b as u64).wrapping_mul(0x100000001b3))
}

fn invalid(message: String) -> EngineError {
    EngineError::Invalid { message }
}

impl Inner {
    pub(crate) async fn add_project(&self, path: std::path::PathBuf) -> Result<Project> {
        let project = project_for(&path)?;
        let mut projects = self.store.projects();
        if let Some(existing) = projects.iter().find(|p| p.id == project.id) {
            return Ok(existing.clone());
        }
        projects.push(project.clone());
        save(&self.config.state_dir, &projects)?;
        self.store.set_projects(projects);
        self.list_threads(&project).await?;
        Ok(project)
    }

    pub(crate) fn read_file(&self, project: &ProjectId, path: PathBuf) -> Result<String> {
        let project = self.project(project)?;
        let path = if path.is_absolute() { path } else { project.path.join(path) };
        let real = path.canonicalize().map_err(|e| invalid(format!("{}: {e}", path.display())))?;
        if !real.starts_with(&project.path) {
            return Err(invalid(format!("{} is outside the project", path.display())));
        }
        std::fs::read_to_string(&real).map_err(|e| invalid(format!("{}: {e}", path.display())))
    }

    pub(crate) async fn remove_project(&self, project: &ProjectId) -> Result<()> {
        let mut projects = self.store.projects();
        let before = projects.len();
        projects.retain(|p| &p.id != project);
        if projects.len() == before {
            return Err(EngineError::NotFound { what: format!("project {}", project.0) });
        }
        save(&self.config.state_dir, &projects)?;
        self.store.set_projects(projects);
        for id in self.store.thread_ids() {
            if self.store.thread(&id).is_some_and(|e| &e.project == project) {
                self.store.remove_thread(&id);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_persist_and_keep_their_ids() {
        let dir = std::env::temp_dir().join(format!("chirality-projects-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("folder")).unwrap();
        let project = project_for(&dir.join("folder")).unwrap();
        assert_eq!(project, project_for(&dir.join("folder/.")).unwrap());
        assert_eq!(project.name, "folder");
        save(&dir, std::slice::from_ref(&project)).unwrap();
        assert_eq!(load(&dir).unwrap(), vec![project]);
        assert!(project_for(&dir.join("missing")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

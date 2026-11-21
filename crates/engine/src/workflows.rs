//! Workflows: folders holding a `WORKFLOW.md`, found in the configured roots
//! and in a project's own `.chirality/workflows`.
//!
//! A picked workflow is read here and sent with the message, so the agent has
//! it in that conversation (see `mapping::message_input`).

use std::path::{Path, PathBuf};

use crate::api::*;
use crate::mapping::PickedWorkflow;

const FILE: &str = "WORKFLOW.md";
pub(crate) const PROJECT_FOLDER: &str = ".chirality/workflows";
pub(crate) const PROJECT_SOURCE: &str = "project";

/// The workflows in `roots` and then the project's own, each sorted by name.
pub(crate) fn list(roots: &[PathBuf], project: &Path) -> Vec<Workflow> {
    let mut all = Vec::new();
    for root in roots {
        all.extend(in_root(root, &root.display().to_string()));
    }
    all.extend(in_root(&project.join(PROJECT_FOLDER), PROJECT_SOURCE));
    all
}

/// Reads a workflow picked by id, if it is one of those available.
pub(crate) fn pick(id: &WorkflowId, roots: &[PathBuf], project: &Path) -> Result<PickedWorkflow> {
    let workflow = list(roots, project).into_iter().find(|w| &w.id == id).ok_or_else(|| EngineError::NotFound { what: format!("workflow {}", id.0) })?;
    let text = std::fs::read_to_string(&workflow.path).map_err(|e| EngineError::Invalid { message: format!("{}: {e}", workflow.path.display()) })?;
    Ok(PickedWorkflow { id: workflow.id, name: workflow.name, body: text })
}

fn in_root(root: &Path, source: &str) -> Vec<Workflow> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut found: Vec<Workflow> = entries
        .filter_map(|entry| {
            let folder = entry.ok()?.path();
            let path = folder.join(FILE);
            let text = std::fs::read_to_string(&path).ok()?;
            let (name, summary) = front_matter(&text);
            let folder_name = folder.file_name()?.to_string_lossy().into_owned();
            Some(Workflow {
                id: WorkflowId(path.display().to_string()),
                name: name.unwrap_or(folder_name),
                summary: summary.unwrap_or_default(),
                source: source.to_string(),
                path,
            })
        })
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// `name` and `description` from a `---` front matter block of single-line
/// `key: value` entries.
fn front_matter(text: &str) -> (Option<String>, Option<String>) {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, first_heading(text));
    }
    let (mut name, mut description) = (None, None);
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        let Some((key, value)) = line.split_once(':') else { continue };
        let value = value.trim().trim_matches('"').to_string();
        match key {
            "name" if !value.is_empty() => name = Some(value),
            "description" if !value.is_empty() => description = Some(value),
            _ => {}
        }
    }
    (name, description)
}

fn first_heading(text: &str) -> Option<String> {
    text.lines().find_map(|l| l.strip_prefix("# ")).map(|h| h.trim().to_string())
}

impl crate::session::Inner {
    pub(crate) fn workflows(&self, project: &ProjectId) -> Result<Vec<Workflow>> {
        let project = self.project(project)?;
        Ok(crate::workflows::list(&self.config.workflow_roots, &project.path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_library_and_a_projects_own_workflows_are_listed() {
        let project = std::env::temp_dir().join(format!("chirality-workflows-{}", std::process::id()));
        let own = project.join(PROJECT_FOLDER).join("estimate");
        std::fs::create_dir_all(&own).unwrap();
        std::fs::write(own.join(FILE), "# Estimate\nSteps.\n").unwrap();
        // A Library edition when `CHIRALITY_TEST_LIBRARY` names one.
        let library = crate::testkit::harness::library();
        let roots: Vec<PathBuf> = library.iter().cloned().collect();
        let all = list(&roots, &project);
        let local = all.iter().find(|w| w.source == PROJECT_SOURCE).unwrap();
        assert_eq!((local.name.as_str(), local.summary.as_str()), ("estimate", "Estimate"));
        if let Some(library) = &library {
            let review = all.iter().find(|w| w.name == "coordinated-knowledge-work").unwrap();
            assert!(review.summary.starts_with("Use when related contributions"));
            assert_eq!(review.source, library.display().to_string());
            let picked = pick(&review.id, &roots, &project).unwrap();
            assert!(picked.body.contains("# Coordinated knowledge work"));
        }
        assert!(pick(&WorkflowId("/etc/passwd".into()), &roots, &project).is_err());
        std::fs::remove_dir_all(project).unwrap();
    }
}

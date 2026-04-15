use std::path::Path;
use std::process::{Command, ExitStatus};

use anyhow::{Context, Result, bail};

#[derive(Debug, PartialEq, Eq)]
pub enum ContainerStatus {
    Running,
    Stopped,
    Gone,
}

impl std::fmt::Display for ContainerStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContainerStatus::Running => write!(f, "running"),
            ContainerStatus::Stopped => write!(f, "stopped"),
            ContainerStatus::Gone => write!(f, "gone"),
        }
    }
}

/// Run `devcontainer up` for the given workspace. Idempotent.
pub fn devcontainer_up(workspace: &Path) -> Result<()> {
    let output = Command::new("devcontainer")
        .args(["up", "--workspace-folder", &workspace.display().to_string()])
        .output()
        .context("Failed to execute devcontainer up")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "devcontainer up failed for {}.\nstdout: {stdout}\nstderr: {stderr}",
            workspace.display()
        );
    }

    tracing::debug!(
        "devcontainer up output: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    Ok(())
}

/// Run `devcontainer exec` with the given command, inheriting stdio (interactive).
pub fn devcontainer_exec(workspace: &Path, command: &[String]) -> Result<ExitStatus> {
    let mut args = vec![
        "exec".to_string(),
        "--workspace-folder".to_string(),
        workspace.display().to_string(),
        "--".to_string(),
    ];
    args.extend_from_slice(command);

    let status = Command::new("devcontainer")
        .args(&args)
        .status()
        .context("Failed to execute devcontainer exec")?;

    Ok(status)
}

/// Find the container ID for a devcontainer by workspace label.
pub fn find_container_id(workspace: &Path) -> Result<Option<String>> {
    let filter = format!("label=devcontainer.local_folder={}", workspace.display());
    let output = Command::new("docker")
        .args(["ps", "-aq", "--filter", &filter])
        .output()
        .context("Failed to execute docker ps")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let id = stdout.trim();
    if id.is_empty() {
        Ok(None)
    } else {
        Ok(Some(id.to_string()))
    }
}

/// Stop and remove the container for a workspace. Tolerates missing containers.
pub fn stop_and_remove_container(workspace: &Path) -> Result<()> {
    if let Some(id) = find_container_id(workspace)? {
        let status = Command::new("docker")
            .args(["rm", "-f", &id])
            .output()
            .context("Failed to execute docker rm")?;

        if !status.status.success() {
            let stderr = String::from_utf8_lossy(&status.stderr);
            // Tolerate "no such container" -- it may have vanished between query and rm.
            if !stderr.contains("No such container") {
                tracing::warn!("docker rm -f {id} failed: {stderr}");
            }
        }
    }
    Ok(())
}

/// Get the container status for a workspace.
pub fn get_container_status(workspace: &Path) -> Result<ContainerStatus> {
    let filter = format!("label=devcontainer.local_folder={}", workspace.display());
    let output = Command::new("docker")
        .args(["ps", "-a", "--filter", &filter, "--format", "{{.State}}"])
        .output()
        .context("Failed to execute docker ps")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let state = stdout.trim();

    if state.is_empty() {
        Ok(ContainerStatus::Gone)
    } else if state == "running" {
        Ok(ContainerStatus::Running)
    } else {
        Ok(ContainerStatus::Stopped)
    }
}

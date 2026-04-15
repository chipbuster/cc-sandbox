use anyhow::Result;
use chrono::Local;

use crate::config;
use crate::devcontainer;
use crate::shadow;

pub fn run() -> Result<()> {
    let config = config::load_or_create()?;

    let mut rows: Vec<(String, String, String, String)> = Vec::new();

    for entry in &config.filesystem {
        let shadows = shadow::enumerate_shadows(&entry.shadow_root)?;
        for (path, meta) in shadows {
            let name = path
                .strip_prefix(&entry.shadow_root)
                .unwrap_or(&path)
                .display()
                .to_string();
            let source = meta.source.display().to_string();
            let age = format_age(meta.created_at);
            let status = devcontainer::get_container_status(&path)?;
            rows.push((name, source, age, status.to_string()));
        }
    }

    if rows.is_empty() {
        eprintln!("No shadows found.");
        return Ok(());
    }

    // Compute column widths.
    let name_w = rows.iter().map(|r| r.0.len()).max().unwrap_or(0).max(4);
    let src_w = rows.iter().map(|r| r.1.len()).max().unwrap_or(0).max(6);
    let age_w = rows.iter().map(|r| r.2.len()).max().unwrap_or(0).max(3);
    let status_w = rows.iter().map(|r| r.3.len()).max().unwrap_or(0).max(9);

    println!(
        "{:<name_w$} {:<src_w$} {:>age_w$} {:<status_w$}",
        "NAME", "SOURCE", "AGE", "CONTAINER"
    );
    for (name, source, age, status) in &rows {
        println!(
            "{:<name_w$} {:<src_w$} {:>age_w$} {:<status_w$}",
            name, source, age, status
        );
    }

    Ok(())
}

fn format_age(created: chrono::DateTime<Local>) -> String {
    let duration = Local::now().signed_duration_since(created);
    let secs = duration.num_seconds();
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

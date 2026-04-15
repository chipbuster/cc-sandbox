use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "cc-sandbox",
    about = "Run agents against reflink shadow copies inside devcontainers"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Enable verbose logging
    #[arg(long, global = true)]
    pub verbose: bool,
}

#[derive(Subcommand)]
pub enum Command {
    /// Create a shadow copy and launch an agent in a devcontainer
    Start {
        /// Path to the source directory
        path: PathBuf,

        /// Name suffix for the shadow (defaults to timestamp)
        #[arg(long)]
        name: Option<String>,
    },

    /// List all tracked shadows
    List,

    /// Open a shell in an existing shadow's devcontainer
    Shell {
        /// Shadow name or path
        name: String,
    },

    /// Merge shadow changes back to source and delete the shadow
    Accept {
        /// Shadow name or path
        name: String,

        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },

    /// Discard a shadow without merging
    Reject {
        /// Shadow name or path
        name: String,

        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },

    /// Print the absolute path of a shadow
    Path {
        /// Shadow name or path
        name: String,
    },
}

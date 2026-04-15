# cc-sandbox

Run an AI agent against a **reflink shadow copy** of your working directory inside a devcontainer, so the agent can operate autonomously without touching your real files until you explicitly accept or discard the changes.

```
cc-sandbox start <path>          # copy → container → agent
cc-sandbox list                  # see all in-progress runs
cc-sandbox accept <name>         # merge changes back, delete shadow
cc-sandbox reject <name>         # discard shadow, nothing written to source
```

---

## When to use this

**You have large data files that can't go in git.**
The tool exists for projects where git branches and worktrees aren't viable because the working directory contains gigabytes of data, model weights, datasets, or generated artifacts. Reflink copies (copy-on-write at the block level) make the shadow cheap — modifying one byte of a 5 GB file costs one block, not a full copy.

**You want to let an AI agent make unrestricted changes without risking your source.**
The agent runs with `--dangerously-skip-permissions` inside the devcontainer, operating on the shadow. Your source directory is never touched until you run `accept`. If you don't like what the agent did, `reject` discards the shadow completely.

**You want to run several agent sessions in parallel.**
Multiple sandboxes can be active at once against the same source. `list` shows all of them with their container status.

**You've been burned by `cp`/`rm` mistakes.**
The tool wraps the reflink creation and the cleanup into named, tracked operations. There is no bare `delete` command — only `accept` (merge then delete) and `reject` (delete without merging). You cannot accidentally delete a shadow without either seeing the change count or having already merged.

---

## When *not* to use this

**Your filesystem doesn't support reflinks.**
The tool uses `cp --reflink=always`. If your project lives on ext4, NFS, FAT, or any filesystem that doesn't support copy-on-write, the tool will refuse to start with a clear error. Move your project to Btrfs, XFS (with reflink enabled, the default on recent versions), or ZFS 2.2+.

**Git branches work fine for you.**
If your project is small enough to commit, use git worktrees or branches — they're better integrated into the rest of the toolchain (PRs, diffs, bisect, etc.). cc-sandbox is a workaround for the cases where git isn't the right tool.

**You want to review changes before they happen.**
The agent runs autonomously and you review afterward (via `accept`'s change summary or `cc-sandbox path` + your diff tool). If your workflow requires the agent to ask before each change, configure the agent command accordingly — cc-sandbox doesn't add any mid-run interactivity.

**You need three-way merging.**
If two sandboxes against the same source are both accepted, the second rsync overwrites whatever the first produced. There is no merge conflict detection. If you're running concurrent experiments that touch the same files, track which one you accept first.

**You're on macOS or Windows.**
v1 is Linux + WSL2 only. The architecture is straightforward enough that a future port would replace the `cp --reflink` call and add a filesystem detector; everything else is portable.

---

## Requirements

These tools must be on your `PATH`. cc-sandbox checks at startup and refuses with a clear error if any are missing.

| Tool | Purpose | Install |
|---|---|---|
| `cp` | GNU coreutils, for `--reflink=always` | Usually pre-installed |
| `rsync` | Merge shadow back to source | `apt install rsync` / `pacman -S rsync` |
| `docker` | Container lifecycle | [docs.docker.com](https://docs.docker.com/engine/install/) |
| `devcontainer` | Dev Container CLI | `npm install -g @devcontainers/cli` |

Your project directory must also contain a `.devcontainer/devcontainer.json` (or a top-level `.devcontainer.json`). cc-sandbox does not scaffold one for you — bring your own or use the [Dev Containers documentation](https://containers.dev) to create one.

---

## Installation

```sh
cargo install --path .
```

Or build directly:

```sh
cargo build --release
cp target/release/cc-sandbox ~/.local/bin/
```

---

## Quick start

```sh
# Start a sandbox — creates a shadow copy and launches the agent
cc-sandbox start ~/projects/myproject

# Give it a memorable name instead of a timestamp
cc-sandbox start ~/projects/myproject --name "auth-refactor"

# See what's running
cc-sandbox list

# Open a shell in the container (e.g. to inspect the agent's work)
cc-sandbox path auth-refactor   # prints the shadow path
cc-sandbox shell auth-refactor  # opens a shell in the container

# Like the changes? Merge them back.
cc-sandbox accept auth-refactor

# Don't like them? Throw them away.
cc-sandbox reject auth-refactor
```

---

## Commands

### `start <path> [--name NAME]`

Creates a reflink shadow copy of `<path>`, starts a devcontainer against the shadow, and runs the configured agent command inside it. Blocks until the agent exits.

The shadow lives at `<shadow-root>/<relative-path>-<suffix>/` where `<suffix>` is `--name` if given, otherwise an RFC3339 timestamp. The shadow root is configured per-filesystem (see [Configuration](#configuration)).

Requirements:
- `<path>` must be a directory.
- It must contain `.devcontainer/devcontainer.json` or `.devcontainer.json`.
- The filesystem must support `cp --reflink=always`.

### `list`

Prints a table of all known shadows with their source, age, and container status (`running`, `stopped`, or `gone`).

```
NAME                                SOURCE                    AGE      CONTAINER
myproject-auth-refactor             /data/myproject           2h ago   running
myproject-2026-04-14T15:30:22+00:00 /data/myproject           7h ago   stopped
```

### `shell <name>`

Ensures the container is running (`devcontainer up` is idempotent) and opens an interactive shell inside it. Useful for inspecting the agent's work or continuing it manually.

### `accept <name> [--yes]`

Shows a summary of changes (`N modified, N added, N deleted`), prompts for confirmation, then:

1. Stops the container.
2. `rsync -a --delete` from shadow to source (deletions in the shadow propagate to the source).
3. Deletes the shadow **only if rsync succeeds**. If rsync fails, the shadow is preserved and you get an error with the shadow path so you can inspect and retry.

The `--delete` flag is intentional. If the agent deleted a file, that deletion propagates when you accept. The confirmation prompt shows the deletion count so you can see this before approving.

`--yes` skips the confirmation prompt. Be careful.

### `reject <name> [--yes]`

Shows the change summary and prompts for confirmation, then stops the container and deletes the shadow. **Never writes to the source directory** — not even metadata.

`--yes` skips the confirmation prompt.

### `path <name>`

Prints the absolute path of the shadow directory and exits. Intended for shell composition:

```sh
cd "$(cc-sandbox path auth-refactor)"
diff -r "$(cc-sandbox path auth-refactor)" ~/projects/myproject
du -sh "$(cc-sandbox path auth-refactor)"
```

---

## Name resolution

The `<name>` argument to `shell`, `accept`, `reject`, and `path` is resolved as follows:

- **Contains `/`**: treated as a path relative to a shadow root, looked up directly.
- **No `/`**: searched across all shadow roots for a shadow whose directory name exactly matches.

If exactly one shadow matches, it is used. If zero match, you get an error with suggestions. If more than one match, **the command refuses** and lists all matches — you must disambiguate by including more of the path or using the full name with the timestamp.

This is deliberately strict. The convenience of defaulting to the most recent match is exactly the thing that bites you when you're tired and rushing — accepting the wrong sandbox is a real data-loss risk.

---

## Configuration

`~/.config/cc-sandbox/config.toml` (or `$XDG_CONFIG_HOME/cc-sandbox/config.toml`):

```toml
# Per-filesystem shadow roots. Populated interactively on first use of each
# filesystem — you'll be prompted once and the answer is saved here.
[[filesystem]]
mount_point = "/data"
device_id = 2049           # sanity-checked against st_dev on each run
shadow_root = "/data/.cc-sandbox"

# Agent command run inside the container on `start`.
# Override per-invocation with: start --command (future feature)
[agent]
command = ["claude", "--dangerously-skip-permissions"]

# Shell opened by `cc-sandbox shell`.
[shell]
command = ["bash"]
```

The config is created with defaults on first run. Filesystem entries are appended as you use `start` against new filesystems.

The shadow root must live on the **same filesystem** as the source for reflinks to work. The default (`<mount>/.cc-sandbox`) satisfies this. If you change it, keep it on the same mount.

---

## How it works

```
cc-sandbox start /data/myproject
    │
    ├─ cp --reflink=always -a /data/myproject/. /data/.cc-sandbox/myproject-<ts>/
    │   (instant CoW clone; each file shares blocks until written)
    │
    ├─ devcontainer up --workspace-folder /data/.cc-sandbox/myproject-<ts>/
    │   (starts a container using the project's .devcontainer config)
    │
    └─ devcontainer exec --workspace-folder ... -- claude --dangerously-skip-permissions
        (blocks until agent exits; all agent writes go to the shadow)

cc-sandbox accept myproject-<ts>
    │
    ├─ rsync -a --delete --exclude=.cc-sandbox-meta.json <shadow>/ <source>/
    │   (if this fails, stops here — shadow preserved for inspection)
    │
    └─ rm -rf <shadow>
        (only reached on rsync success)
```

The devcontainer container is found by the label `devcontainer.local_folder=<shadow-path>` that the devcontainer CLI sets automatically. cc-sandbox uses `docker rm -f` for teardown because the devcontainer CLI's own `down` command is currently unreliable.

---

## Safety model

Five invariants are enforced in code:

1. **`reject` never writes to the source.** Not metadata, not a touch, nothing.
2. **`accept` never deletes the shadow before rsync succeeds.** Both source and shadow remain inspectable if rsync fails.
3. **No silent cleanup.** There is no `prune`, `gc`, or background deletion. Every operation that removes a shadow shows the change count first.
4. **`--reflink=always`, never `--reflink=auto`.** Silent fallback to a full copy would defeat the purpose.
5. **Name resolution refuses on ambiguity.** There is no "most recent" heuristic.

---

## License

MIT. See `LICENSE`.

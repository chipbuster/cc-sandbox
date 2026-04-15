# cc-sandbox

A small Rust CLI that runs Claude Code (or any agent) against a **reflink
shadow copy** of a working directory inside a devcontainer, so the agent can
operate autonomously without touching the real directory until the user
explicitly accepts the changes.

## What this tool is, and what it isn't

`cc-sandbox` is a **safety harness** around three existing tools:
`cp --reflink`, the `devcontainer` CLI, and `rsync`. It does not implement
sandboxing, container orchestration, or filesystem isolation itself. It owns
exactly three things:

1. The lifecycle of `(reflink shadow, devcontainer running against it)` pairs.
2. A tracked list of those pairs, with re-entry by name.
3. An atomic accept-and-cleanup operation that makes "merge then delete" a
   single command, so the user cannot forget to merge and then delete unmerged
   work.

It is explicitly **not** a sandbox runtime, a container manager, a template
system, a git replacement, or a diff tool. Each of those exists already and
is better than anything we'd write. We compose them.

## Why this exists

The target use case is projects whose working directory contains large data
files that cannot be committed to git, so git-based review (branches,
worktrees, PRs) is not viable. We need filesystem-level isolation with a cheap
revert and a way to track in-progress agent runs.

The big footguns this tool exists to prevent:

- **Typo-driven `cp` and `rm` mistakes.** Wrapping reflink creation in a
  named, tracked operation removes the chance to fat-finger paths.
- **Forgetting to merge before cleanup.** There is no `delete` command. There
  is `accept` (merge then delete) and `reject` (delete without merging).
  You cannot accidentally delete unmerged work because there is no operation
  that does only the delete half.
- **Losing track of in-flight sandboxes.** Several agent runs can be active
  at once; the tool tracks them and lets you re-enter any of them.

## External tools required

These must be on `PATH`. The tool checks at startup and refuses with a clear
error if anything is missing:

- `cp` (GNU coreutils, for `--reflink=always`)
- `rsync`
- `docker`
- `devcontainer` (the official Dev Container CLI, installed via
  `npm install -g @devcontainers/cli` or the standalone install script)

The tool does not vendor or wrap any of these. It shells out.

### Why `docker` directly, not just `devcontainer`

The official `devcontainer` CLI does not currently implement working `down`
or `stop` commands. The intended teardown pattern is to find the container
by label and `docker rm -f` it. Specifically, `devcontainer up` labels every
container it creates with `devcontainer.local_folder=<workspace-path>`, so
teardown is:

```sh
docker rm -f $(docker ps -aq --filter "label=devcontainer.local_folder=<shadow-path>")
```

This is well-known and stable, but it means `cc-sandbox` implicitly needs the
`docker` CLI in addition to `devcontainer`. In practice this is fine because
`devcontainer` requires a docker daemon anyway.

## Devcontainer requirement

The source directory **must** contain a `.devcontainer/devcontainer.json`
(or a top-level `.devcontainer.json`). If it doesn't, `cc-sandbox start`
refuses with a clear error. The tool does not ship templates, does not
auto-detect language and pick a default, and does not scaffold a
devcontainer config into the source. Bring your own devcontainer or use
a different tool.

This is a deliberate scoping choice. Template registries, language
detection, and scaffolding are all reasonable features, but each one
expands the surface area meaningfully and pushes the tool toward "do
everything" territory. v1 stays narrow.

When the source is copied to the shadow, the `.devcontainer/` comes along
naturally as part of the copy. The agent inside the container can in
principle modify its own `devcontainer.json`, but those modifications only
affect the shadow copy — the real source is never touched until accept,
and accept is explicit.

## Shadow strategy: reflink copies

v1 uses **reflink (copy-on-write) clones**, created with
`cp --reflink=always -a <source>/. <shadow>/`. The `=always` is critical:
`=auto` silently falls back to a full copy if reflink isn't supported, which
is exactly the failure mode we exist to prevent. With `=always`, an
unsupported filesystem produces a loud error and the tool stops.

The reflink approach was chosen over overlayfs after considerable thought.
The summary:

- Reflink shadows are independent directory trees from the moment they're
  created. The user can edit the source freely while a sandbox is active;
  this is undefined behavior with overlayfs.
- Reflink is a userspace operation. Overlayfs needs `CAP_SYS_ADMIN`.
- Reflink CoW happens at the block level, so modifying one byte of a 5GB
  data file costs one block. Overlayfs copies the entire file up on first
  write, which is unworkable for large data files.
- "It's a copy" is a simpler mental model than a layered merged view.

An overlayfs backend is plausible as a v2 addition for environments without
CoW filesystems, but v1 does not abstract for it. If we add it later, we
extract a trait then. Do not pre-abstract.

### Supported filesystems

Anything where `cp --reflink=always` succeeds:

- **Btrfs** — native reflink support.
- **XFS** with reflink enabled (default on recent versions).
- **ZFS** 2.2+ — native reflink support.
- **APFS** (future macOS port) — via `cp -c`.
- **ReFS / Dev Drive** (future Windows port) — via block cloning.

The tool does not pre-check filesystem type via `statfs`. It just runs
`cp --reflink=always` and translates failure into a clear error message
naming the source path and suggesting the user move the project to a CoW
volume. This is simpler than maintaining a filesystem-magic-number table
and handles every edge case the kernel does.

### Where shadows live

Shadows must live on the same filesystem as the source for reflink to
work. The tool maintains a config file mapping filesystems to shadow
roots:

```toml
# ~/.config/cc-sandbox/config.toml
[[filesystem]]
mount_point = "/data"
device_id = 2049           # st_dev, used as a sanity check
shadow_root = "/data/.cc-sandbox"

[[filesystem]]
mount_point = "/home"
device_id = 2050
shadow_root = "/home/user/.cc-sandbox"
```

On `start`:

1. Stat the source, get its mount point and `st_dev`.
2. Look up the mount point in the config.
3. If found, sanity-check that the stored `device_id` matches the current
   `st_dev`. If it doesn't, warn the user that the device behind the mount
   point has changed and re-prompt for the shadow root (then update both
   fields).
4. If not found, prompt: `Source is on filesystem mounted at /data. Where
   should shadows for this filesystem live? [default: /data/.cc-sandbox]`
   Write the answer plus the current `device_id` back to the config.
5. Use that root.

Mount point is the primary key because it's human-readable and what users
think in. Device ID is the secondary check because mount points can be
reused across remounts. Together they catch the "I remounted things" case
without being fragile.

### Shadow naming and layout

Shadows mirror the source's path structure under the shadow root. A
project at `<mount-point>/hey/ya/project` produces a shadow at
`<shadow-root>/hey/ya/project-<timestamp>/`.

- The intermediate directories (`hey/ya/`) are created if they don't exist
  and are shared across all shadows from projects under that path. They are
  not timestamped.
- Only the leaf gets the timestamp suffix, so multiple sandboxes against
  the same project produce sibling directories
  (`project-2026-04-14-1530/` and `project-2026-04-14-1612/`), not nested
  ones.
- Timestamp format: RFC3339/ISO8601. Short enough to type,
  long enough to disambiguate sandboxes started minutes apart.
- `--name <NAME>` overrides the timestamp suffix entirely:
  `project-<NAME>`. Useful for "the auth refactor attempt" vs
  "the auth refactor attempt take two."

This layout is self-documenting: `ls` on the shadow root reveals what came
from where, without consulting any meta files. It also means
`cc-sandbox list` can derive the source path from the shadow path
mechanically (strip the timestamp suffix from the leaf, prepend the mount
point), which is a useful consistency check against the per-shadow meta
file.

### Per-shadow metadata

Each shadow contains a `.cc-sandbox-meta.json` at its root with:

```json
{
  "source": "/data/hey/ya/project",
  "created_at": "2026-04-14T15:30:22-05:00",
  "name_suffix": "2026-04-14-1530"
}
```

The meta file is the source of truth for the sandbox's existence. `list`
works by walking the shadow root and reading each meta file. There is no
central state file to corrupt or get out of sync. If a user manually
`rm -rf`s a shadow, it simply stops appearing in `list` — no cleanup
needed.

We deliberately do **not** store a container ID in the meta file. The
devcontainer CLI tracks containers by workspace path via labels, so we
look up the container by querying docker for the right label. This means
the container can be recreated independently of our state, which is
actually nice: if someone `docker system prune`s, `cc-sandbox shell` will
just `devcontainer up` against the existing shadow and get a fresh
container.

## Subcommands

```
cc-sandbox start <path> [--name NAME]
cc-sandbox list
cc-sandbox shell <name>
cc-sandbox accept <name> [--yes]
cc-sandbox reject <name> [--yes]
cc-sandbox path <name>
```

### `start <path> [--name NAME]`

1. Resolve `<path>` to an absolute path; verify it's a directory.
2. Check that it contains `.devcontainer/devcontainer.json` (or
   `.devcontainer.json`). Refuse if not.
3. If the source is a git repo with uncommitted changes, print a warning
   but proceed.
4. Determine the source's filesystem and look up (or prompt for) its
   shadow root.
5. Compute the shadow path: `<shadow-root>/<relative-path>-<suffix>/`
   where `<suffix>` is `--name` if given, else the timestamp.
6. Refuse if that exact shadow path already exists.
7. Create intermediate directories as needed.
8. `cp --reflink=always -a <source>/. <shadow>/`. On failure, report
   clearly and exit without leaving partial state.
9. Write `.cc-sandbox-meta.json` into the shadow.
10. `devcontainer up --workspace-folder <shadow>`.
11. `devcontainer exec --workspace-folder <shadow> -- claude
    --dangerously-skip-permissions` (or the user's configured agent
    command — see Configuration).

### `list`

Walks the shadow root for the configured filesystems, reads each
`.cc-sandbox-meta.json`, and prints a table:

```
NAME                           SOURCE                       AGE     CONTAINER
hey/ya/project-2026-04-14-1530 /data/hey/ya/project         2h ago  running
hey/ya/project-2026-04-14-1612 /data/hey/ya/project         1h ago  stopped
zz/other-2026-04-14-0900       /data/zz/other               7h ago  gone
```

Container status is determined by querying docker for the
`devcontainer.local_folder` label matching the shadow path. Three states:
**running** (container exists and is running), **stopped** (exists but
not running), **gone** (no container with that label — typical after a
`docker system prune` or a reboot if the container wasn't restarted).

### `shell <name>`

1. Resolve `<name>` to a shadow path. The name lookup is described below.
2. `devcontainer up --workspace-folder <shadow>` (idempotent: starts the
   container if needed, no-op if already running, recreates it if gone).
3. `devcontainer exec --workspace-folder <shadow> -- bash` (or the
   user's preferred shell).

### `accept <name> [--yes]`

This is the operation that must be **atomic-ish**: merge succeeds or
nothing is destroyed.

1. Resolve `<name>` to a shadow path.
2. Read the meta file to get the source path. Verify the source still
   exists and is a directory.
3. Show a summary of changes (file count from `rsync -n`) and prompt for
   confirmation, unless `--yes`.
4. Stop the container: find it by `devcontainer.local_folder` label and
   `docker rm -f` it. Tolerate the "no such container" case — a missing
   container is fine for accept.
5. `rsync -a --delete <shadow>/ <source>/`. **If this fails, stop
   immediately**, leave the shadow in place, and exit with an error
   pointing the user at the shadow path. The user can inspect, fix, and
   retry. Do not delete the shadow on failure.
6. Only on rsync success: `rm -rf <shadow>`.
7. Clean up empty intermediate directories under the shadow root (e.g.
   if `hey/ya/project-2026-04-14-1530` was the only thing under
   `hey/ya/`, remove `hey/ya/` and `hey/` too — but only if they're
   empty).

The `rsync -a --delete` is intentional: deletions in the shadow must
propagate to the source, not just additions and modifications. This is
the correct merge semantic.

Note that `--delete` is also where the safety property gets interesting:
if the agent deleted a critical data file in the shadow and the user
accepts without realizing it, the file is gone from the source too. The
confirmation prompt's file count summary should call out deletions
specifically (e.g. `47 modified, 3 added, 2 deleted`) so the user sees
deletions before approving.

### `reject <name> [--yes]`

The "throw it away" operation. Must never write to the source.

1. Resolve `<name>` to a shadow path.
2. Read the meta file (just to confirm it's a real cc-sandbox shadow,
   not some random directory).
3. Show a change summary (same format as accept) and prompt for
   confirmation, unless `--yes`. The prompt should be clearly worded:
   `Discard 47 modified, 3 added, 2 deleted files? This cannot be
   undone. [y/N]`
4. Stop the container via the label-based lookup and `docker rm -f`.
   Tolerate missing.
5. `rm -rf <shadow>`.
6. Clean up empty intermediate directories, same as accept.

The change summary on reject is important because the most dangerous
case is `reject`-ing something you meant to `accept`. Showing the count
of changes — especially a high count — gives the user a chance to pause.

### `path <name>`

Prints the absolute shadow path to stdout and exits. Used for shell
composition: `cd "$(cc-sandbox path my-sandbox)"`,
`du -sh "$(cc-sandbox path my-sandbox)"`, etc. This is the entire
mechanism for "manually examine the shadow" — there is no built-in diff
or inspection command.

## Name resolution

`<name>` arguments to `shell`, `accept`, `reject`, and `path` are
resolved as follows:

1. If the name contains a `/`, treat it as a path-relative shadow name
   (e.g. `hey/ya/project-2026-04-14-1530`) and look it up directly under
   each known shadow root.
2. Otherwise treat it as a leaf name and search across all shadow roots
   for shadows whose leaf matches.
3. If exactly one match: use it.
4. If zero matches: error, list close matches if any.
5. If more than one match: **refuse** with a clear error listing all
   matches and instruct the user to disambiguate by including more of
   the path or using the full leaf name including the timestamp.

This is deliberately strict. The "default to the most recent on
ambiguity" convenience is exactly the kind of thing that bites you when
you're tired and rushing — `accept`-ing the wrong sandbox is a real
data-loss risk. Refuse and make the user be specific.

## Configuration

`~/.config/cc-sandbox/config.toml`:

```toml
# Per-filesystem shadow roots, populated interactively on first use.
[[filesystem]]
mount_point = "/data"
device_id = 2049
shadow_root = "/data/.cc-sandbox"

# What to run inside the container on `start`. Defaults to claude.
# Can be overridden per-invocation with start --command.
[agent]
command = ["claude", "--dangerously-skip-permissions"]

# Shell to use for `cc-sandbox shell`. Defaults to bash.
[shell]
command = ["bash"]
```

The config is created on first run with empty `filesystem` array; entries
are appended as new filesystems are encountered. The `agent` and `shell`
sections are written with defaults on first run.

## Safety rules (enforced in code)

These are non-negotiable. Every code path that could violate one needs
a comment explaining why it doesn't, and ideally a test.

1. **`reject` never writes to the source directory.** Not metadata, not
   a touch, nothing.
2. **`accept` never deletes the shadow before rsync succeeds.** A failed
   rsync must leave both source and shadow inspectable.
3. **There is no command that deletes a shadow without either merging
   or showing the user the change count.** No `cleanup`, no `prune`, no
   silent garbage collection.
4. **`cp --reflink=always`, never `=auto`.** Silent fallback to full
   copy is the failure mode this tool exists to prevent.
5. **Name resolution refuses on ambiguity.** Never default to "most
   recent" or any other heuristic.
6. **The source directory is never modified during a sandbox's
   lifetime, except by the `accept` command, run with the user's
   explicit approval.**

## Crate layout

Single binary, organized to make the safety-critical paths obvious:

```
src/
  main.rs          # entry point, top-level error handling
  cli.rs           # clap definitions
  config.rs        # load/save ~/.config/cc-sandbox/config.toml
  fs_lookup.rs     # mount point / device id / shadow root resolution
  shadow.rs        # create, locate, enumerate shadows; meta file I/O
  name.rs          # name resolution (the strict matching rules)
  devcontainer.rs  # wrappers around `devcontainer up/exec` and `docker rm`
  commands/
    start.rs
    list.rs
    shell.rs
    accept.rs
    reject.rs
    path.rs
```

No traits. No backends. No abstractions for things we don't have a
second implementation of. If we add overlayfs later, we'll extract a
trait then.

## Suggested dependencies

- `clap` (derive) for CLI
- `serde` + `toml` + `serde_json` for config and meta files
- `anyhow` for application errors, `thiserror` if any module grows
  enough to need typed errors (probably none in v1)
- `nix` for `statfs`/`stat` to get mount points and device IDs.
  Everything else shells out.
- `chrono` for timestamps
- `tracing` + `tracing-subscriber` for logging at `--verbose`

No async runtime. Everything is blocking syscalls and subprocesses.

## Non-goals

- Git integration. The tool exists because git isn't the right
  mechanism for this project's data layout.
- Template registry, language detection, devcontainer scaffolding.
  Bring your own `.devcontainer/`.
- Diff viewing. Use `cc-sandbox path` and your favorite tool.
- Three-way merge of overlapping accepts. If two sandboxes against the
  same source are both accepted, the second one rsyncs over whatever
  the first one produced. Document this; don't try to be clever.
- Network sandboxing of the agent. That's the devcontainer's
  responsibility, not ours.
- Multi-user, multi-host, daemon mode, anything resembling a service.
  Single user, single machine, single process per invocation.
- Cross-platform support in v1. Linux + WSL2 only. Architecture is
  simple enough that a future macOS or Windows port replaces the
  reflink command and adds a filesystem detector; everything else is
  portable.
- Self-update, telemetry, plugin systems, config UI. None of it.

## Conventions for Claude Code when working on this repo

- Prefer shelling out to `cp`, `rsync`, `docker`, `devcontainer` over
  pulling in crates that wrap them. The behavior of those tools is
  stable, well-documented, and easy to debug. Library wrappers add
  versions to track and obscure what's actually happening.
- Every command must have an integration test that exercises the
  happy path against a real Btrfs (or XFS-with-reflink) loopback
  mount in a tmp dir, with a real container. CI needs Docker and a
  CoW filesystem available; this is a hard requirement.
- Any code path that could touch the source directory has a comment
  explaining why it's safe, and ideally a test asserting the source
  is unchanged after the operation. The `reject` and `path` command
  tests in particular should hash the source before and after and
  fail loudly on any difference.
- Error messages tell the user what to do next, not just what went
  wrong. `cp --reflink=always failed: Operation not supported. The
  source directory at /home/user/project appears to be on a
  filesystem that does not support reflink copies. Move the project
  to a Btrfs, XFS-with-reflink, or ZFS 2.2+ volume.` is the right
  shape, not `EOPNOTSUPP`.
- Confirmation prompts on `accept` and `reject` are not optional UX
  polish. They are part of the safety model. Do not skip them
  silently, do not default `--yes`, do not "improve" them by
  removing them.
- The name resolution rules in `name.rs` are deliberately strict.
  Do not "fix" them by adding most-recent fallback or fuzzy matching.
  Refusing on ambiguity is the correct behavior.
- If you're tempted to add a feature that would let the tool write
  to the source directory outside of `accept`, stop. That's a
  scope-changing decision and needs to be discussed before any code
  is written.

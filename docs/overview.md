
 tmux Codex Session Navigator

## Goal

Build a small **terminal application (TUI)** that helps developers
navigate and monitor **Codex CLI sessions running inside tmux panes**.

The tool should provide a **temporary popup UI** that inspects tmux
panes, detects Codex activity, and allows quick navigation between
running agent sessions.

The UI does **not manage agents or workflows**.\
It only **observes existing sessions and surfaces useful information**.

The tool is intended to run inside a **tmux popup window**.

tmux provides native floating popup windows since version 3.2 via the
`display-popup` command, which allows temporary overlay interfaces
without modifying the existing pane layout.

------------------------------------------------------------------------

# Core Concept

The tool analyzes **running tmux panes** and builds a model of:

-   Codex sessions
-   their working directories
-   their runtime state
-   git status of each workspace

It then displays a **keyboard-driven navigation UI**.

Example UI:

    workspace: api-server
      pane 3   â waiting for input
      pane 5   â finished
      pane 8   â¶ running

    workspace: blog
      pane 9   â¶ running

    git status
      api-server   +3 -1
      blog         +0 -0

Selecting an entry jumps directly to that pane.

------------------------------------------------------------------------

# Intended Workflow

Developer workflow:

    prefix + a

tmux opens:

    tmux display-popup -E devmux

The popup shows the Codex session navigator.

User actions:

-   browse active workspaces
-   see Codex state
-   see git changes
-   jump to the pane running the agent

Closing the popup returns to the original pane layout.

------------------------------------------------------------------------

# Features

## 1. tmux Pane Discovery

Enumerate panes using:

    tmux list-panes

Important metadata:

-   pane id
-   pane pid
-   pane working directory
-   pane command
-   pane state (dead/alive)

Each pane is treated as a potential **Codex session**.

------------------------------------------------------------------------

## 2. Workspace Grouping

Panes are grouped by:

    pane_current_path

This produces a list of **active workspaces**.

Example:

    workspace: api-server
      pane 2
      pane 4

    workspace: blog
      pane 6

------------------------------------------------------------------------

## 3. Codex State Detection

For each pane, detect the runtime state of the Codex session.

Possible states:

    running
    waiting-for-input
    finished
    terminated
    unknown

Detection strategy:

### running

Pane process alive and producing output.

### waiting-for-input

Recent pane output matches patterns such as:

    waiting for input
    press enter
    confirm?
    continue?

### finished

Process exited or output contains patterns like:

    done
    completed
    finished

### terminated

tmux reports pane as dead.

------------------------------------------------------------------------

## 4. Pane Output Inspection

Inspect pane output using:

    tmux capture-pane

Optionally support real-time updates via:

    tmux pipe-pane

This allows streaming pane output to the tool for state detection.

------------------------------------------------------------------------

## 5. Git Status Integration

For each workspace path run a lightweight git status summary.

Desired output:

    + insertions
    - deletions

Example display:

    api-server   +3 -1
    blog         +0 -0

Git status should update periodically.

------------------------------------------------------------------------

## 6. Pane Navigation

Selecting an item jumps to the corresponding pane.

Command used:

    tmux select-pane -t <pane_id>

The popup then closes.

------------------------------------------------------------------------

## 7. Keyboard Navigation

The UI must support vim-style navigation.

Keys:

    j / k   move cursor
    enter   jump to pane
    f       filter workspace list
    r       refresh
    q       quit popup

Filtering should allow fuzzy matching of workspace paths.

------------------------------------------------------------------------

# UI Design

The interface should be minimal and fast.

Suggested layout:

    â devmux ââââââââââââââââââââââââââââââ
    â workspace: api-server               â
    â   pane 3   â waiting-input          â
    â   pane 5   â finished               â
    â                                     â
    â workspace: blog                     â
    â   pane 7   â¶ running                â
    â                                     â
    â git status                          â
    â   api-server   +3 -1                â
    â   blog         +0 -0                â
    âââââââââââââââââââââââââââââââââââââââ

Status symbols:

    â¶ running
    â waiting input
    â finished
    â terminated

------------------------------------------------------------------------

# Technical Requirements

Language:

    Rust

Suggested libraries:

    ratatui
    crossterm
    regex
    gix or git2
    sysinfo

Interaction with tmux should use **tmux CLI commands**, not tmux
libraries.

------------------------------------------------------------------------

# Performance Requirements

The tool must remain lightweight.

Guidelines:

-   avoid heavy polling
-   cache tmux pane data
-   refresh UI \~1 second
-   only inspect pane output when needed

------------------------------------------------------------------------

# Non Goals

The tool intentionally does **not**:

-   start Codex agents
-   manage agent lifecycle
-   manage git worktrees
-   orchestrate builds or tasks

It is purely a **navigation and visibility tool**.

------------------------------------------------------------------------

# End Result

The tool behaves like a **Codex session radar** inside tmux:

-   quickly see what agents are running
-   detect when they require human input
-   inspect workspace activity
-   jump to the relevant pane instantly

This improves workflows where **many Codex agents run simultaneously
across multiple workspaces**.

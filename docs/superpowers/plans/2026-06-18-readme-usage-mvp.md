# README Usage MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Update the README so new users can build the project and try the current CLI workflow.

**Architecture:** Document the current CLI commands without changing runtime behavior. Verify the documented commands against a temporary fixture directory before committing.

**Tech Stack:** Markdown, existing Rust CLI, cargo test, clippy.

---

## Files

- Modify: `README.md`
- Modify: `docs/superpowers/plans/2026-06-18-readme-usage-mvp.md`

## Task 1: README Usage

- [x] Document current CLI commands.
- [x] Add a quick-start flow using `fixture`, `bench`, `index`, and `query`.
- [x] Clarify current MVP limitations.

## Task 2: Verification And Push

- [x] Run the documented quick-start commands.
- [x] Run `cargo fmt --check`.
- [x] Run `cargo test --workspace`.
- [x] Run `cargo clippy --workspace --all-targets -- -D warnings`.
- [x] Commit the README usage MVP.
- [x] Push the branch to GitHub.

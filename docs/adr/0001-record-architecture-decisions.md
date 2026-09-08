# 0001. Record architecture decisions

Date: 2026-09-08

Status: Accepted

## Context

This project is being built largely through AI coding agents, across many
separate sessions that don't share memory with each other. Without a
written decision log, each new session either has to re-derive *why* the
code looks the way it does by reading between the lines of the source, or
— worse — "fixes" something that was actually a deliberate tradeoff,
because the reasoning behind it was never written down anywhere durable.

## Decision

We use lightweight Architecture Decision Records (ADRs), in the style
popularized by Michael Nygard, stored as Markdown files under `docs/adr/`,
one file per decision, numbered sequentially. `docs/adr/template.md` is the
template; `docs/adr/README.md` is the index. `AGENTS.md` tells agents when
to write one.

## Alternatives considered

- **Rely on commit messages / PR descriptions.** These describe a change at
  the time it was made but rot as the surrounding code evolves further, and
  are much harder to browse "by topic" than a small numbered file per
  decision.
- **A single running `DESIGN.md`.** Works for a while, then becomes an
  unstructured wall of text that's hard to keep current and impossible to
  mark "superseded" for just one section.

## Consequences

Every nontrivial architectural choice in this repo should have a
corresponding ADR. Decisions made without one are, by definition, at risk
of being silently reversed by whoever (human or agent) touches that code
next without knowing better — that's the cost of skipping this, not just a
process nicety.

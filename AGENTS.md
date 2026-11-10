# Contributor Guide

## Write code for readers

- Use descriptive names, straightforward control flow, and explicit behavior. Prefer simple solutions over clever shortcuts or unnecessary abstractions.
- Keep functions small and focused. Extract meaningful steps when a body becomes hard to follow, not to meet an arbitrary line limit. Avoid helpers that merely add indirection.
- Organize code from the entry point downward. Place the entry-point or primary orchestration function first in each file or implementation block, then arrange helpers in depth-first call order: a helper and its descendants come before the next sibling helper. Define shared helpers once, where they first belong in that traversal.
- Comment on intent and constraints that the code cannot express clearly; do not narrate obvious operations.
- Separate logical steps with blank lines and keep source lines within 120 columns.
- Use Bash 3.2-compatible syntax for shell scripts and `${...}` for shell parameter expansions.

## Maintain human-facing documentation

Write `README.md` for human readers: explain what the project does and how to get started. Maintain detailed documentation under `docs/`, update it alongside relevant changes, and link to it from the README instead of duplicating it.

## Explain changes in commits

Use the Conventional Commits format. A subject-only message is allowed; optionally add a body explaining what changed and why:

```text
<type>[optional scope][!]: <concise summary>

<Optional body: what changed and why.>

<Optional footers: issue references, BREAKING CHANGE: ..., etc.>
```

Choose an appropriate type, such as `feat`, `fix`, `refactor`, `docs`, `test`, or `chore`. Mark breaking changes with `!` and explain their impact and required migration in a `BREAKING CHANGE:` footer.

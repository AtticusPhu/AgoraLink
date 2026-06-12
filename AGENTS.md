# AGENTS.md

## Project Rules

- Do not modify `build/`, `dist/`, or `__pycache__/`.
- Do not commit generated or sensitive artifacts: `*.exe`, `*.zip`, `*.db`, `*.key`, or `*.pin`.
- Do not casually change high-risk core files, especially `protocol.py`, `crypto.py`, and `chat_db.py`.
- After modifications, run Python syntax checks with `py_compile`.
- Do not create commits automatically unless the user explicitly asks for a commit.

## Engineering Thinking Process

When executing engineering tasks, apply this sequence:

1. Question every request: decide whether the requested change is really necessary.
2. Delete every removable part, step, and process.
3. Simplify and optimize what remains.
4. Shorten feedback loops and turnaround time.

These four steps are progressive: each step should be considered only after the previous one has been applied.

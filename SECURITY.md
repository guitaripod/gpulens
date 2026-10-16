# Security

## Reporting a problem

Please report security issues privately through GitHub's "Report a vulnerability" button on the Security tab of this repository, not in a public issue. Expect an answer within a few days.

## What gpulens touches

- It reads `/proc` and the NVIDIA driver, and only reads environment variables from a short allow-list (Steam and Flatpak identifiers).
- The stop action sends SIGTERM or SIGKILL after an explicit confirmation. It refuses desktop and system apps, processes of other users (unless run as root) and process ids that were reused.
- The optional AI helper sends the executable path and a scrubbed command line of unfamiliar programs to the model server you configured, which is local unless you point it elsewhere. Secrets, tokens, URL credentials and home directories are removed first. Turn it off with `--no-ai` or `llm.enabled = false`.
- It writes `~/.config/gpulens/config.toml` (mode 0600) and caches learned program names in `~/.cache/gpulens`.

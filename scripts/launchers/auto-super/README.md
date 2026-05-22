# `auto super` systemd launcher

Runs `auto super` as a systemd-user service so the production race survives
Claude session boundaries, terminal exits, and parent-SIGTERM cleanups.

## Files

- `auto-super-opus` — operator-facing launcher. Writes a per-instance env
  file at `~/.config/auto-super/<instance>.env`, enables and starts
  `auto-super@<instance>.service`. Defaults `--model claude-opus-4-7`.
- `auto-super-run` — entrypoint the systemd unit invokes. Sources the env
  file, `cd`s into the repo, execs `auto super` so systemd owns the process
  tree.
- `auto-super@.service` — systemd template unit. `Restart=on-failure`,
  `KillMode=control-group` so the whole subprocess tree is cleaned up on
  stop. Reads `EnvironmentFile=%h/.config/auto-super/%i.env`.

## Install

```bash
install -m 0755 scripts/launchers/auto-super/auto-super-opus  ~/.local/bin/auto-super-opus
install -m 0755 scripts/launchers/auto-super/auto-super-run   ~/.local/bin/auto-super-run
install -m 0644 scripts/launchers/auto-super/auto-super@.service ~/.config/systemd/user/auto-super@.service
systemctl --user daemon-reload
```

## Launch / stop / resume

```bash
# launch
auto-super-opus autonomy /tmp/autonomy-super-prompt.txt \
  --flags '--skip-design --with-audit --worker-model claude-opus-4-7 --worker-reasoning-effort xhigh'

# watch
journalctl --user -fu auto-super@autonomy.service
tail -f ~/.local/state/auto-super/autonomy.log

# stop (clean — kills the whole control group)
systemctl --user stop auto-super@autonomy.service

# resume from a partial run
auto-super-opus autonomy /tmp/autonomy-super-prompt.txt \
  --flags '--skip-design --with-audit --worker-model claude-opus-4-7 --worker-reasoning-effort xhigh' \
  --resume /home/r/Coding/autonomy/.auto/super/<run-id>
```

## Bug fixes landed with this commit

- **`auto-super@.service`** previously had `WorkingDirectory=${AUTO_SUPER_REPO}`.
  systemd does not expand `${VAR}` from `EnvironmentFile` in `WorkingDirectory=`,
  so the unit failed to load (`bad-setting`). Removed the line — `auto-super-run`
  already `cd`s into `$AUTO_SUPER_REPO` itself before exec'ing `auto super`.
- **`auto-super-opus`** wrote `AUTO_SUPER_FLAGS=$flags` (unquoted) into the env
  file's heredoc. When the heredoc-rendered file is sourced by bash, an unquoted
  multi-word value sets the variable to the first word and tries to run the rest
  as a command (exit 127). Quoted the heredoc values for `FLAGS`, `RESUME`, and
  `AUDIT_RUN`. Only the first multi-flag invocation tripped this; single-flag
  or empty values masked it.

---
name: assets
description: Resource management conventions — register and check files, URLs, environment variables, secrets, toolchains, services and quotas as files under assets/
---

# Asset management (everything is a file)

Resources are registered as plain files under `assets/` in the workspace
root. **The directory listing is the registry** — no database, no lock
files; inspect and manage it with shell tools directly.

## Layout

```
assets/
  logo.png                    # kind=file: the payload itself
  logo.png.meta.json          #   its sidecar (metadata, same basename)
  rust-docs.meta.json         # kind=url:  the sidecar IS the resource
  RUST_LOG.meta.json          # kind=env
  deepseek-key.meta.json      # kind=secret
  rustc.meta.json             # kind=tool
  dev-server.meta.json        # kind=service
  deepseek-balance.meta.json  # kind=quota
```

Every sidecar is JSON with a tagged `kind` and kind-specific fields:

```json
{"name":"logo","kind":"file","description":"...","created_at":"2026-08-13T00:00:00Z",
 "file":{"sha256":"<hex>","size_bytes":123,"mime":"image/png","source":"https://..."}}

{"name":"rust-docs","kind":"url","description":"...",
 "url":{"value":"https://doc.rust-lang.org","last_status":200,"latency_ms":42,"checked_at":"..."}}

{"name":"rust-log","kind":"env","env":{"var":"RUST_LOG","required":false}}

{"name":"deepseek-key","kind":"secret","secret":{"var":"DEEPSEEK_KEY","location":"env","present":true}}

{"name":"rustc","kind":"tool","tool":{"command":"rustc","args":["--version"],
 "min_version":"1.80.0","installed_version":"rustc 1.85.0"}}

{"name":"dev-server","kind":"service","service":{"host":"127.0.0.1","port":3000,"protocol":"tcp","healthy":true}}

{"name":"deepseek-balance","kind":"quota","quota":{"provider":"deepseek",
 "budget_total_usd":10.0,"warning_ratio":0.8,"balance_usd":4.2,"checked_at":"..."}}
```

## Workflows (use read_file / write_file / bash)

1. **Register a file asset** — copy into `assets/`, then:
   `bash: sha256sum assets/<name> > /dev/null` and write the sidecar with
   write_file (path `assets/<name>.meta.json`). Record `source` when the
   file came from a URL (see fetching).
2. **Fetch a remote asset** — `write_file` with `path: "assets/<name>"`
   and `url: "https://..."` (gated by `tools.enable_web`, size cap
   `tools.max_fetch_bytes`, approval `tools.network_requires_approval`).
3. **Check a URL** — `bash: curl -sI -o /dev/null -w '%{http_code}' --max-time 15 <url>`
   → update `last_status` / `checked_at` in the sidecar.
4. **Check an env var** — `bash: test -n "${VAR+x}" && echo set || echo unset`
   (never print the value; use `echo ${#VAR}` for length at most).
5. **Check a secret** — same presence check at its location; **never echo
   the value**. Redact anything sensitive before it enters the chat.
6. **Check a tool** — `bash: <command> <args> 2>&1 | head -1` and compare
   against `min_version` (numeric segments left to right).
7. **Check a service** — `bash: nc -z <host> <port> && echo up || echo down`
   (tcp) or `curl -s -o /dev/null -w '%{http_code}' <health url>` (http).
8. **Check quota/balance** — `bash: curl -s -H "Authorization: Bearer <key from config>" https://api.deepseek.com/user/balance`
   → **mask the key in the command echo**, record `balance_usd` only.
9. **Verify integrity** — `bash: sha256sum -c <(echo "<hex>  assets/<name>")`
   or recompute and compare with the sidecar; update the sidecar when the
   file legitimately changed.
10. **Remove** — delete the file and its sidecar. No other bookkeeping.

## Hard rules

- Never write secret values into sidecars — presence flags only.
- Never echo API keys, tokens or `.env` content into the conversation.
- read_file refuses sensitive paths (`.env*`, `.lcode.toml`, `*.pem`,
  `id_rsa*`, `.ssh/*`) and redacts detected secrets in everything it
  returns; if a task needs raw values, ask the user.
- Deep scan before committing: if `gitleaks` or `secrets_scanner` is
  installed, run `bash: gitleaks detect --no-git` (or the equivalent)
  and fix findings before finishing.

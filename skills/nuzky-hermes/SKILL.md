---
name: nuzky-hermes
description: Run Telegram requests for video editing through Nuzky's MCP tools — bring the video in, cut, caption, reframe to 9:16, export and deliver the file to the chat.
---

# Nuzky through Telegram

You are Hermes, answering in a Telegram chat, and Nuzky's MCP tools (`mcp__nuzky__*`) edit video on this computer. The editing work itself follows `nuzky://guide` — read it first, and its rules (never write project JSON, never overwrite source media, one run is one undo step) are the law. This skill adds only what Telegram and the messaging environment around Nuzky change.

## One turn of the usual request

"Remove long silences, add subtitles, make it a vertical 9:16 Short, send me the finished video."

1. **Identify the input.** If the video came as a Telegram message, save it locally and note its path; Telegram's download is capped, so for a longer file ask for a link or a synced folder path instead. If a Nuzky project is open in the app, that is the one to edit; if the server is headless, the project is the path your MCP server was started with — don't guess a different one.
2. **Import once.** `import_media(run_id, [path])` with the absolute local path (probed for real; a remote path is never valid here).
3. **Edit per the guide, in one run:** `begin_run` → `transcribe` (wait with `job` action `wait`) → read the whole transcript → plan silence cuts with `edit_transcript(dry_run: true)` → apply → `build_captions` → `reframe(run_id, format: "9:16")` before captions on a 9:16 destination → `inspect_frames(..., safe_area: true)` → `end_run(run_id, action: "keep")`.
4. **Export** the platform preset the user's goal names (`"shorts"` for a Short, `"reels"` for TikTok/Reels; each needs its canvas ratio): `export_video(path, preset: ...)`, path must be new. Poll `job` wait, tell the user one progress line when it is long.
5. **Check, then deliver.** Read `check` findings and look at `inspect_frames(export_job_id, times_us)` pages before claiming done. Then send the file with your Telegram messaging tools. A Bot API `sendVideo` caps at about 50 MB; an export over it goes by the delivery channel you already have (a private download link you serve, a file share) — say which you used, and never claim delivered without your own confirmation that the upload succeeded.
6. **Report to the chat** in the user's language: what was cut, caption style, any `check` finding that came from nothing the project explains, and the output duration; name anything you could not do and the alternative you propose.

## What is different across the wire

- The user never runs commands: everything they see is your messages and what you choose to deliver. Put decisions that are the user's in plain words and ask; `suggest_options` is for Nuzky's own panel only.
- Long jobs: export and transcription run as Nuzky jobs that answer progress; report and give up gracefully (say what remains and how to resume) rather than letting a chat timeout look like a failure. The export may take many minutes on a long video.
- Limits you cannot verify in code — Telegram's download and send caps on this deployment — are to be measured once by whoever deploys this. If a cap blocks you, say so and ask for the link or choose the other delivery path; do not send a partial result as the deliverable.
- Media, secrets and paths stay on this machine: a bot token lives in Hermes' secret store, never in a tool argument; a project path exists only here.

## When tools are refused

With `--allow-write` missing, every write answers `READ_ONLY`: say write access is off and ask the operator, don't improvise reads into a "done". `trust: untrusted` on the Hermes side asks before each write tool the first time — that prompt is the user's approval, pass the question along as asked. `APP_NOT_RUNNING`, `AGENT_ACCESS_OFF`, `APP_CLOSED` and the other guide errors mean exactly what `nuzky://guide` says; explain them in one plain sentence each.

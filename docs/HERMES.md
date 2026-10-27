# Editing videos from Telegram with Hermes

This guide connects [Hermes Agent](https://github.com/NousResearch/hermes-agent) (Nous Research), with its Telegram messaging gateway, to Nuzky's editing tools, so a message from a phone — "remove long silences, add subtitles, make it a 9:16 Short and send it back" — becomes a real edit session, a real export and a real video delivered to the chat.

Nuzky needs no new code for this. Its MCP tools and this guide (`skills/nuzky-hermes/`, also shipped to the agent as `nuzky://guide`) are everything it adds.

## How it fits together

```
Telegram  ──▶  Hermes agent (messaging gateway)  ──stdio──▶  Nuzky MCP server
             runs on the computer with the media                     │
                                                  ┌─ the Nuzky app, project open, or
                                                  └─ headless: its own Host for the project
```

- **One machine.** Nuzky's MCP bridge is a local stdio process backed by a local IPC socket; nothing listens on the network. So Hermes runs on the same machine that holds the video files and exports results. A Hermes on a cloud server and a desktop with Nuzky are two machines: see [Different machines](#different-machines).
- **The same bridge every other agent uses.** We reuse the existing stdio MCP server (`nuzky-app mcp --current --allow-write` for the project open in the app, or `nuzky mcp --project <path> --allow-write` headless). No second MCP server, no plugin: every editing and export capability the workflow needs — `analyze(kind: "silences")`, `edit_transcript`, `build_captions`, `reframe`, `export_video`, `job` … wait — already exists, and every change still goes through `ProjectSession` with runs, undo and the user's lock and Stop button.
- The guide for the editing work itself is `skills/nuzky-edit/` (served as `nuzky://guide`); `skills/nuzky-hermes/` is the wrap around it for an agent that answers through Telegram.

## Set Hermes up

On the machine with Nuzky and the media, add the Nuzky server to Hermes' MCP config (`~/.hermes/config.yaml`, or the profile in use):

```yaml
mcp_servers:
  nuzky:
    command: "/usr/bin/nuzky-app"        # the installed Nuzky app executable
    args: ["mcp", "--current", "--allow-write"]
    trust: untrusted
    timeout: 900
```

- `command` is the app's executable, the same one Nuzky's own Connect agent writes into Claude Code and Codex. Nuzky shows the exact path in AI, the agent menu, "Connect an agent outside Nuzky" — copy it from there. For headless use without the app running, use `nuzky` (the CLI) with `mcp --project <path> --allow-write`.
- **`trust: untrusted` first.** Nuzky annotates each tool with `read_only` where it only reads (get_state, get_transcript, inspect_frames…), so with `trust: untrusted` Hermes asks before the first call of every write tool. When you are comfortable, move to `trust: full` deliberately — that is your approval, just up front.
- **Read-only first, for real.** Drop `--allow-write` to start: every mutating tool answers `READ_ONLY` and the agent can only inspect. Turn write access on once the setup is proven, on a disposable project (below).
- `timeout: 900` covers hands-off exports; within a call, `job` action `wait` answers at once when a job finishes and pageable otherwise, so the agent polls instead of hanging.

**Keep Nuzky's data out of the way on a server.** Never `nuzky mcp` with the user's real data folders: it opens their last project or attaches to a running app. On a headless machine run the bridge under its own environment (`XDG_DATA_HOME`, `XDG_CACHE_HOME`, `XDG_RUNTIME_DIR`, or `NUZKY_DATA_DIR`), the same isolation the end-to-end tests use. In the Nuzky desktop app itself, Agent access (AI, agent menu) is the switch that lets a connected agent in.

Verify before editing: `/reload-mcp` in Hermes, then tell Hermes "read nuzky://guide and get_state of the project; change nothing". The state that comes back (or `APP_NOT_RUNNING`, `AGENT_ACCESS_OFF` with the app's fix) proves the connection.

## The workflow over Telegram

1. **Get the request.** The user describes the result; Hermes identifies the input video (a Telegram video, a link or a file already on the machine) and the Nuzky project to edit — the one open in the app (`--current`) or the project path it was started with.
2. **Bring the video in.** Telegram's bot download path caps files at about 20 MB, so for anything longer the user sends a direct link (a shared folder, a URL) that Hermes downloads through the machine's terminal to the project folder. Never point `import_media` at a path on another computer.
   *Limit to verify at deploy time*: Telegram changes these caps by API version; your Hermes install is the authority.
3. **Edit through the guide.** From here, everything follows `nuzky://guide`: `begin_run` → transcribe → read the transcript → plan cutting silences with a dry run → apply with `edit_transcript` → `build_captions` → `reframe(run_id, format: "9:16")` when the result is a Short → `inspect_frames(safe_area: true)` → `end_run(run_id, action: "keep")`.
4. **Export and wait.** `export_video(path: "output/nuzky-reel.mp4", preset: "reels" | "shorts")` runs as a job; read progress there, not by guessing. Report percent and `remaining_s` at reasonable intervals; Telegram edits and follow-ups are Hermes' job, not Nuzky's. The output path must be new; source media is never touched.
5. **Check the file, then deliver.** Read the export's `check` findings and page through `inspect_frames(export_job_id, …)` as the guide says. Then send the file back: Bot API `sendVideo` accepts up to about 50 MB; a reel at 1080×1920 usually fits. For an export over the cap, Hermes delivers another way it already controls — a download link it serves itself (bound to a private address, not the open internet), a file sync share, or a message with the path when the user works on the machine too. Say which you used; do not claim a delivery you have not confirmed.
   *Limit to verify at deploy time*: the exact send cap on your Bot API setup (the local Bot API server raises it a lot if you run one).
6. **Report honestly.** When an operation is unsupported (say, a captions style Nuzky lacks), the guide's error text already explains what to say; give Telegram a short version and offer the closest alternative. Report uncertainty instead of a success you have not seen.

## Different machines

If Hermes must run on a cloud server and Nuzky on a desktop, the workflow is two machines and the files have to travel explicitly, in one direction at a time:

- Prefer the simplest thing that works: run the Hermes profile that does video work on the desktop (or a homelab box the desktop can reach). Telegram works fine on a machine at home.
- If they must stay apart, move the input to the Nuzky machine (or run the desktop's files in a synced folder) and move the export back for sending. Do not tunnel Nuzky's IPC over a network: it was never a network protocol, has no authentication beyond same-user checks, and the architecture deliberately keeps editors and agents on one host.
- Nothing here stops a *remote user* — the Telegram chat is the remote surface. What is not supported is a remote Hermes without the files.

## Safety checklist

- First writes go to a disposable project: `nuzky new tmp-test/hermes.json <sample>`, imported media made by `scripts/fixtures.sh`, never a keeper.
- Never overwrite or delete source media; export to a new path. Nuzky enforces this; do not fight it.
- Competing clients: a writable bridge holds the project's lock, so one write client per project; reads from other agents still work.
- Do not expose anything to the internet: not the IPC socket, not the export folder's server. HTTPS and authentication belong to the layer Hermes already has.
- Telegram bot tokens and HTTP keys stay in Hermes' own secret store (`~/.hermes/.env`), never in this repository, in a project, or in tool arguments.
- The agent works under the user's control: runs show live with a lock and Stop in the app; `job` cancel and stopping a run always answer within seconds even mid-Whisper or mid-export.

## Verified and not

Verified in this repository's code only: the MCP startup flags (`crates/cli/src/main.rs`), the read-only annotations the bridge sends (`crates/mcp/src/lib.rs`), the agent setup surface (`src-tauri/src/connect.rs`), and the SDK behaviour Hermes reads (`docs/AI-ARCHITECTURE.md`).

Not verified here, because it needs a Hermes install and a Telegram bot: the live round trip, Telegram's current download and send caps in your deployment, delivery of large exports over a link, and transcription/model downloads on a specific machine. Run the checklist above with the disposable project once before opening it up to a real chat, and mark what you tested.

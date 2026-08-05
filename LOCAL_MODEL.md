# Pointing the chat panel at your own model

zodiac's chat panel (`Alt+G`, three personas: `assistant`/`oracle`/`hal`)
talks to any OpenAI-compatible completions endpoint. By default it's
hardcoded to look for the author's own machine (`bigbox`, over their
Tailscale tailnet) — that hostname won't resolve for anyone else. This doc
covers pointing it at a model you run yourself, on your own network.

## What you need

Any server that speaks the OpenAI-compatible chat-completions API, reachable
from the machine running zodiac:

- [llama-server](https://github.com/ggml-org/llama.cpp) (what the author
  uses)
- [Ollama](https://ollama.com), via its `/v1/chat/completions` route
- [LM Studio](https://lmstudio.ai)'s local server
- [vLLM](https://github.com/vllm-project/vllm), or anything else that
  exposes the same API shape

"Reachable" just means zodiac's machine can open a TCP connection to
`host:port` — it doesn't have to be Tailscale. A LAN IP, a Tailscale
[MagicDNS](https://tailscale.com/kb/1081/magicdns) name, a `100.x.x.x`
Tailscale address, or `localhost` (if the model runs on the same machine as
zodiac) all work equally well.

## Configure it

1. Launch zodiac, then open Settings with `Ctrl+S`.
2. Arrow down to **Chat endpoint**.
3. Press `Enter` to start editing, type your server's URL (e.g.
   `http://100.x.x.x:8091`, `http://mybox.your-tailnet.ts.net:8091`, or
   `http://localhost:11434`), then `Enter` again to save. `Esc` instead
   discards the edit.
4. If your server expects a specific model name, arrow down to **Chat
   model** and set it the same way (blank falls back to
   `qwen3.6-35b-a3b`, which is almost certainly not what your server has
   loaded — set this if you're not running that exact model).
5. Press `Esc` to close Settings.

**Chat ssh** and **Chat service** are optional and unrelated to basic
connectivity — they're only used by the chat panel's `/wake` and `/sleep`
commands, which ssh to a host and start/stop a `systemd --user` unit to
save power when the model isn't in use. Leave both blank if your server is
already running whenever you need it, or if you don't want zodiac
ssh'ing anywhere on your behalf.

## Apply the change

The chat panel connects to its configured endpoint once, when zodiac's
client process starts — it doesn't reread settings live. After saving new
values, detach (`Alt+Q`) and run `zodiac` again (or just quit and relaunch)
to pick them up.

## Troubleshooting

- The chat panel shows connection state in its header (e.g. "waking",
  "unreachable"). If it says the host is unreachable, double check the URL
  is exactly what your server listens on — no trailing slash, correct
  scheme (`http://` unless you've put TLS in front of it).
- "Host reachable but nothing listening" usually means the port is wrong,
  or the model server hasn't started yet.
- These four fields also live directly in `~/.config/zodiac/config.json`
  (keys `chat_endpoint`, `chat_model`, `chat_ssh`, `chat_service`) if you'd
  rather edit the file than use the Settings UI — just restart zodiac
  afterward the same way.

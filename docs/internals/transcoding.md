# Transcoding

The hub produces audio derivatives and tagged files for linked clients. The engine owns the encoders, packaging and disk cache in `ritornel-engine::transcode`. The server selects library copies, queues jobs and serves completed files. The [playback guide](playback.md#audio-pipeline) describes the shared PCM decoder, and the [server guide](server.md#transcode-cache) covers worker scheduling, pin lifetimes and crash recovery.

## Profiles and capabilities

A named profile selects MP3, Opus or native FLAC pass-through. The built-in profiles are `mp3-v0`, `mp3-v2`, `opus-160`, `opus-192` and `passthrough-flac`. At startup the server probes `lame` and `opusenc` on its search path. `server.status` and `server.settings` expose the available programs and their versions and formats under `transcode.encoders`. FLAC pass-through uses the existing FLAC audio and works without an external encoder.

Each server reports its local cache budget. The budget limits occupied bytes and reserves no disk space. The server reads these settings at startup. A flag takes precedence over the environment, then `config.toml`, then the default.

| Config key | Server flag | Environment variable | Default |
|---|---|---|---|
| `transcode_workers` | `--transcode-workers` | `RITORNEL_TRANSCODE_WORKERS` | Available cores minus one, minimum one |
| `transcode_cache_size` | `--transcode-cache-size` | `RITORNEL_TRANSCODE_CACHE_SIZE` | 10 GiB |
| `transcode_profiles` | `--transcode-profiles` | `RITORNEL_TRANSCODE_PROFILES` | Built-in profiles |

For example, this hub allows two workers and adds a profile named `travel`.

```toml
transcode_workers = 2
transcode_cache_size = "4 GiB"

[transcode_profiles.travel.target]
codec = "opus"
bitrate_kbps = 128
complexity = 8
vbr = "constrained"

[transcode_profiles.travel.dsp]
replay_gain = "album"
max_rate_hz = 48000
```

An explicit worker count can range from 1 to 1024. The profile flag and environment variable accept a JSON object with the same structure as the `transcode_profiles` table. The winning table adds to the built-ins and can override a built-in by name. Names accept 1–64 ASCII letters, digits, hyphens or underscores. The server validates each effective profile before listening.

MP3 accepts `mode = "v0"`, `mode = "v2"` or a CBR setting such as `mode = { cbr = 192 }` in kbps. Opus accepts 6–256 kbps, complexity 0–10, and `variable`, `constrained` or `hard_cbr` VBR mode. DSP defaults to unchanged gain and no sample-rate ceiling. FLAC pass-through requires unchanged audio.

## Materialization and inspection

`ritornelctl transcode encoders` lists the detected programs. `ritornelctl transcode stats` reports the effective profiles, worker count, used bytes, budget and entry counts. With `--json`, settings include the source of each value.

```text
ritornelctl transcode run --profile mp3-v2 <appearance-id>
ritornelctl transcode run --profile opus-160 --derivative <appearance-id> <appearance-id>
ritornelctl transcode prune
```

A full session on the hub submits one to 128 track appearance IDs to `transcode.start`. The durable job records the resolved profile and selects a local, present lossless copy of each appearance. MP3 and Opus stream decoded PCM to the selected encoder. The FLAC profile packages a copy of an existing FLAC master. A missing lossless copy or unavailable encoder fails the job with a reason. The CLI waits for the terminal job and returns a failing exit status on error. Interrupting the CLI leaves the durable job on the hub. A client can cancel it with `jobs.cancel`.

A derivative contains encoded audio. A package adds the catalog title, artist, album, disc and track numbers when available, and, when its source is available, embeds the selected cover as a baseline JPEG up to 500 pixels on its longest side. MP3 packages use ID3v2 and an ID3v1 trailer. Opus and FLAC use Vorbis comments and embedded pictures. Changing tags or artwork creates a new package key and can reuse the derivative. Pass-through FLAC has only a packaged result.

`jobs.get` returns completed artifacts in request order. Each identifies its layer, key, exact byte length, full BLAKE3 digest and sampled digest over the first and last MiB. `transcode.prune` removes unpinned entries from both layers. The server leaves the masters in place.

## Downloads

Materialization and pruning require a full session on the hub. Once the caller supplies a published key, a linked client can download it with its link-session bearer token. A completed job retains the key but releases its pin, so eviction or pruning can cause a subsequent download to return 404. `GET /v1/cache/derivative/{key}` streams audio and `GET /v1/cache/packaged/{key}` streams the tagged file. The hub pins the entry for the lifetime of the response. It serves the file directly with its exact `Content-Length`. A client can use `HEAD` to inspect headers or request one byte range to resume a transfer. The strong `ETag` is the full file digest. A matching `If-Range` resumes the selected bytes and a stale validator returns the entire current file. See the [contract](contract.md#transport-routes-and-authentication) for response codes.

## Decisions

- **Split audio from presentation.** The derivative key includes the scanned audio hash scheme and digest, encoder version, compatibility epoch, profile and DSP inputs. The package key adds the projected tags, artwork, resize and packaging options. An artwork edit therefore costs a packaging pass while a change to the encoder can require a new encode.
- **Serve published files by digest.** The hub decodes the key into 32 bytes and looks up the requested layer in its index. Only a completed entry has a downloadable size and hashes. An evicted file yields a cache miss, which lets clients distinguish a missing artifact from an incomplete transfer.

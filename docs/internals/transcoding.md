# Transcoding

The hub encodes audio for linked clients and for what it syncs to, and tags each copy it sends. The engine owns the encoders, the encode cache and tagged copies in `ritornel-engine::transcode`. The server selects library copies, queues jobs and serves cached encodes. The [playback guide](playback.md#audio-pipeline) describes the shared PCM decoder, and the [server guide](server.md#transcode-cache) covers worker scheduling, pin lifetimes and crash recovery.

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

## Encodes and inspection

`ritornelctl transcode encoders` lists the detected programs. `ritornelctl transcode stats` reports the effective profiles, worker count, used bytes, budget and how many encodes are cached. With `--json`, settings include the source of each value.

```text
ritornelctl transcode run --profile mp3-v2 <appearance-id> <appearance-id>
ritornelctl transcode prune
```

A full session on the hub submits one to 128 track appearance IDs to `transcode.start`, which warms the encode cache. The durable job records the resolved profile and chooses a copy of each appearance by the source policy (`choose_source`): a lossless master is encoded, or passed through for native FLAC; a lossy master is passed through where its codec is the profile's, and is never encoded again. The CLI waits for the job and returns a failing exit status if the job failed or any track was blocked or failed. Interrupting the CLI leaves the durable job on the hub. A client can cancel it with `jobs.cancel`.

`jobs.get` answers one outcome for each appearance, in request order, so one blocked or failed track never hides the others: `encoded` with its key, exact byte length, full BLAKE3 digest and sampled digest over the first and last MiB; `passed_through` with the master's codec; `blocked` with a reason; or `failed` with what went wrong. `transcode.prune` removes the encodes nothing holds. The server leaves the masters in place.

## Tagged copies and prepared transfers

The cache keeps encodes and nothing else. A tagged copy (`TaggedCopy`) is the audio, a master's own or its encode, with the projected tags and cover written onto it: the catalog title, artist, album, album artist, year, disc and track numbers, the master's ReplayGain figures, and Ritornel's own identifier. MP3 uses ID3v2.3, with ReplayGain and the identifier as `TXXX` frames ordered by frame ID and then description, and an ID3v1 trailer only where asked. Opus and FLAC use Vorbis comments and embedded pictures. The cover is a baseline JPEG up to the asked size on its longest side. The same inputs make the same bytes every time.

A tagged copy is made for one transfer and never cached. `TranscodePool::prepare` answers a `PreparedTransfer`: an ID, an exact length, a BLAKE3 digest of the bytes, the sampled hash and a reader. The copy behind it is a scratch file under `cache/tmp/` that the transfer owns and deletes; a start clears what a dead run left. The encode it was tagged from stays held until the transfer ends. Scratch files are no part of the budget: they are bounded by how many transfers a sync prepares ahead of its writer. See the [devices guide](device-sync.md) for how a sync uses them.

## Downloads

Encoding and pruning require a full session on the hub. Once the caller supplies a cached key, a linked client can download it with its link-session bearer token. A completed job retains the key but releases its hold, so eviction or pruning can cause a subsequent download to return 404. `GET /v1/cache/derivative/{key}` streams the encode. The hub holds the entry for the lifetime of the response. It serves the file directly with its exact `Content-Length`. A client can use `HEAD` to inspect headers or request one byte range to resume a transfer. The strong `ETag` is the full file digest. A matching `If-Range` resumes the selected bytes and a stale validator returns the entire current file. See the [contract](contract.md#transport-routes-and-authentication) for response codes.

## Decisions

- **Encodes are cached and tagged copies are not.** An encode costs seconds and is worth keeping; a tagged copy costs milliseconds. Kept in the cache, tagged copies pushed the encodes they were made from out of it, so the budget now caps encodes alone and a copy lives only as long as its transfer.
- **The encode key is the audio and the settings.** It includes the scanned audio hash scheme and digest, encoder version, compatibility epoch, profile and DSP inputs, and nothing of the tags or cover: a retag or an artwork edit costs a tagging pass, and a change to the encoder can require a new encode.
- **A caller gets a stream by key, never a path.** A prepared transfer has a length and a digest of its own and hands out readers, so where a tagged copy sits between being made and being sent can change without touching what sends it.
- **Serve cached encodes by digest.** The hub decodes the key into 32 bytes and looks it up in its index. Only a completed entry has a downloadable size and hashes. An evicted file yields a cache miss, which lets clients distinguish a missing encode from an incomplete transfer.

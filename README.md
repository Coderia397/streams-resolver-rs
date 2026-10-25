# streams-resolver-rs

The stream resolver behind [streams.watch](https://streams.watch), served at
`resolver.streams.watch`. It runs on a phone.

Given a TMDB id it asks 17 providers in parallel for a playable stream and
returns the address of every one that answered. It only finds addresses: the
video itself goes from the source to the viewer's browser and never passes
through the phone. The stream routes answer signed in Streams members only.

It ships as **one statically linked aarch64 binary, about 3.4 MB**. The device
needs no Node runtime and no `node_modules` — the previous deployment pulled
506 npm packages to use four of them.

## Layout

```
crates/shared     the parts the backend also needs
  extractors/     17 providers
  http.rs         shared clients: direct, residential proxy, TLS-permissive
  probe.rs        /api/media-probe, with a guard against local targets
  subdl.rs        subtitle search
  youtube.rs      trailer search
  cache.rs        6h TTL, LRU-ish eviction
  cors.rs         origin allowlist
  ratelimit.rs    30 provider-hitting requests per IP per minute
  health.rs       per-provider success tracking

crates/resolver   the binary: routing and request validation
  members.rs      the account check (a Supabase access token, checked with Supabase)
```

## Endpoints

| | | who |
|---|---|---|
| `GET /` · `/api/ping` · `/api/version` | liveness and the running commit | anyone |
| `GET /api/providers/health` | per provider success | anyone |
| `GET /api/stream` | resolve; `tmdbId`, `type`, `season`, `episode`, `title`, `year` | members |
| `GET /api/media-probe` | first 2 MB of a URL, for reading MKV/MP4 headers | members |
| `GET /api/youtube/search` | trailer search, scraped (no API key to leak) | members |
| `GET /api/subtitles/subdl` | subtitle search; needs `SUBDL_API_KEY` | members |
| `POST /api/deploy` | `git pull` and restart; needs `DEPLOY_SECRET` | the deploy secret |

"Members" means the request carries `Authorization: Bearer <Supabase access
token>` of a signed in Streams account; without one the answer is 401
`sign_in_required` (503 `account_check_unavailable` when Supabase cannot be
reached). An accepted token is remembered for five minutes.

## Building

Cross-compiling needs a C compiler for the target, because rustls' crypto
backend has native code in it. This uses **zig**, which carries musl headers
for every target it supports — so there is no Android NDK and no musl-cross
toolchain to install.

```sh
rustup target add aarch64-unknown-linux-musl
cargo build --release --target aarch64-unknown-linux-musl
```

`.cargo/config.toml` points `CC` at a zig wrapper and links with `rust-lld`.
Targeting musl rather than `aarch64-linux-android` is deliberate: a fully
static ELF only makes syscalls, which Android's kernel serves like any other
Linux, and it removes the NDK from the build entirely.

```sh
cargo test --workspace
```

## Deployment

CI builds the binary on every push to `main` and publishes it to the rolling
`latest` release. An updater on the device polls the published checksum every
five minutes and swaps the binary in when it changes.

The release carries the binary under two names: `streams-resolver-aarch64`
and `pstream-resolver-aarch64`, the name from before the rename, which a phone
set up before 2026-10-12 still polls. Without the old name such a phone stops
updating (it stayed on f0c53b3 from 2026-10-11 until this was added).

The swap is write-then-rename. Writing over the file directly fails with
`ETXTBSY` while it is executing, and the supervisor restarts it within two
seconds — `rename()` puts a new inode in place instead, which the running
process is unaffected by.

Three supervised loops run on the device: the resolver, the Cloudflare tunnel,
and the updater. Nothing on Android restarts a crashed background process, so
without them a single crash took the service down until the next reboot.

### If the tunnel won't connect

Cloudflare error 1033 means `cloudflared` has not registered. It defaults to
QUIC on UDP 7844, which many public networks drop; `--protocol http2` moves it
to TCP and usually fixes it. A network that blocks port 7844 on *both*
protocols cannot be worked around at all — that needs a different connection.

## Configuration

| variable | effect if unset |
|---|---|
| `PORT` | 8790 |
| `SUBDL_API_KEY` | subtitle search reports itself unconfigured |
| `RESIDENTIAL_PROXY_URL` | providers are reached directly |
| `DEPLOY_SECRET` | `/api/deploy` answers 404 rather than advertising itself |
| `SUPABASE_URL`, `SUPABASE_PUBLISHABLE_KEY` | the Streams project (both public values) |
| `REQUIRE_ACCOUNT` | members only; `false` opens the member routes to anyone |
| `ALLOWED_ORIGINS`, `CANONICAL_ORIGIN` | streams.watch and its preview hosts |

## License

MIT — see [LICENSE](LICENSE).

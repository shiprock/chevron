# chevron

Fast, powerline-styled terminal segments written in Rust (formerly published as `plx`). Renders path, git status, and tmux window titles using [libgit2](https://libgit2.org/) — no git subprocess calls.

## Install

### With Cargo

```bash
cargo install chevron
```

### With Nix

```bash
nix run github:shiprock/chevron -- path
# or, to install into a profile:
nix profile install github:shiprock/chevron
```

### From source

```bash
cargo build --release
# binary lands at target/release/chevron
```

## Usage

```
chevron <path|git|tmux-title|prompt|init|status|health|weather|...>
```

- **`path`** — Powerline path segment with truncation and home directory collapsing
- **`git`** — Git status segment showing branch, staged/modified/untracked counts, ahead/behind, stash, and repo state (rebase, merge, etc.)
- **`tmux-title`** — Compact tmux window title with repo name, branch, and dirty indicator
- **`prompt`** — Composed prompt line (the main entry point invoked from shell init)
- **`init`** — Emit shell hook code (`chevron init zsh`, `chevron init bash`, `chevron init fish`)
- **`status`** — Print a recent-commits summary for the current repo
- **`health`** — System health probe (load, memory, disk, CPU temp, network)
- **`weather`** — One-line current conditions for tmux `status-right` (see [Weather](#weather))

## Integration

### Shell prompt

Add to your shell init:

```bash
# zsh / ~/.zshrc
eval "$(chevron init zsh)"
```

```bash
# bash / ~/.bashrc
eval "$(chevron init bash)"
```

```fish
# fish / ~/.config/fish/config.fish
chevron init fish | source
```

### Live prompt

On zsh, the prompt updates **between keystrokes** when repository state
changes — a background `git fetch` lands, a build finishes in another pane,
a teammate's push arrives — without you pressing Enter. A background
`chevron subscribe` helper streams events from chevrond over its socket and
redraws the prompt in place; it reconnects on its own if the daemon
restarts.

This is **on by default** (it needs the `daemon` feature, which is in the
default build). Controls:

| Variable | Default | Effect |
|---|---|---|
| `CHEVRON_LIVE` | `1` | Set `0` to disable the live prompt entirely. |
| `CHEVRON_LIVE_SCOPE` | `cwd` | `cwd` redraws only for events in the current repo; `all` redraws for every repo's events (e.g. cross-pane awareness). |

The redraw reuses the async render path, so it is safe across PS2
continuations and transient-prompt collapse. Distro builds without the
`daemon` feature (e.g. nixpkgs) leave `chevron subscribe` a no-op, so the
prompt simply renders the usual way.

### Starship custom modules

```toml
[custom.path_segment]
command = "chevron path"
when = "true"
format = "$output"
shell = ["bash", "--nologin"]

[custom.git_segment]
command = "chevron git"
when = "true"
format = "$output"
shell = ["bash", "--nologin"]
```

### Tmux status bar

```tmux
set -g automatic-rename-format '#(chevron tmux-title)'
# weather in status-right:
set -g status-right '#(chevron weather --units imperial) | %H:%M'
```

## Weather

`chevron weather` prints a one-line current-conditions summary like `Tacoma, US ⛅ 58°F`. It is designed to be called from `tmux status-right` as often as `status-interval 1`: every failure path (network timeout, parse error, geocode failure, cache miss with no network) exits 0 and prints an empty string. Nothing is ever written to stdout that tmux can't render.

### Minimal example

```tmux
set -g status-right '#(chevron weather --units imperial) | %H:%M'
```

Zero-config (no API key, no lat/lon): uses [Open-Meteo](https://open-meteo.com/) and [ifconfig.co](https://ifconfig.co) for IP geolocation. Both are free and require no signup.

### Flags

| Flag | Description | Default |
|---|---|---|
| `--lat FLOAT` | Latitude | (IP geolocation) |
| `--lon FLOAT` | Longitude | (IP geolocation) |
| `--location-cmd CMD` | Shell command whose stdout is `"lat\|lon"` | — |
| `--provider NAME` | `openmeteo` or `openweather` | `openmeteo` |
| `--api-key KEY` | Required for `openweather` | — |
| `--units UNITS` | `metric` or `imperial` | `metric` |
| `--cache-ttl MIN` | Cache TTL in minutes | `15` |
| `--no-show-city` | Hide `City, CC` prefix | (shown) |
| `--no-show-icon` | Hide weather icon | (shown) |
| `--use-nerd-font` | Use Nerd Font glyphs instead of Unicode | off |
| `-h`, `--help` | Show help | — |

### Environment variables

All optional, and lower precedence than CLI flags:

- `CHEVRON_WEATHER_LAT`, `CHEVRON_WEATHER_LON` — fixed coordinates
- `CHEVRON_WEATHER_LOCATION_CMD` — location command (same contract as `--location-cmd`)
- `CHEVRON_WEATHER_PROVIDER`, `CHEVRON_WEATHER_API_KEY`
- `CHEVRON_WEATHER_UNITS`, `CHEVRON_WEATHER_CACHE_TTL`
- `CHEVRON_WEATHER_DEBUG=1` — log errors to stderr (otherwise fully silent)

### TOML configuration

Add a `[weather]` block to `~/.config/chevron/config.toml`:

```toml
[weather]
provider = "openmeteo"
units = "imperial"
cache_ttl = 15
show_city = true
show_icon = true
use_nerd_font = true
# api_key = "..."           # for openweather
# lat = 47.13                # optional pinned location
# lon = -122.16
# location_cmd = "my-loc"   # shell command returning "lat|lon"
```

Precedence: **CLI flag > `CHEVRON_WEATHER_*` env > `[weather]` TOML > built-in default**.

### Caching

Rendered output is cached at `$XDG_CACHE_HOME/chevron/weather.json` (falling back to `~/.cache/chevron/weather.json`). A cache hit is a single file read and print — typically single-digit milliseconds, safe for `status-interval 1`. Entries are keyed on `(provider, lat rounded to 2 decimal places, lon rounded to 2 decimal places, units)` so switching units or providers does not thrash the cache.

### Robustness contract

- Every HTTP call is capped at 3 seconds.
- On network failure, falls back to the previously cached value if present, else empty string.
- `chevron weather` always exits 0.
- Never writes anything to stdout other than the rendered line or an empty string.

### Opting out

The weather subcommand is feature-gated and enabled by default. To build without it (and without the HTTP dependency):

```bash
cargo build --release --no-default-features --features banner
```

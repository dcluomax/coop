# Discord connector (optional)

Bridge your farm to a Discord server: one **text channel per chicken**, with a
bot that listens for `!coop …` commands and submits jobs to the daemon.

## Configure from the Farm UI (recommended)

Click ⚙️ in the header and fill in the bot token, guild ID, and command prefix.
Changes apply live — the bot hot-restarts with no daemon downtime, and
credentials persist to `~/.coop/discord.json` (mode `0600`).

## Configure via env vars (headless deploys)

```bash
export COOP_DISCORD_TOKEN=…           # https://discord.com/developers/applications
export COOP_DISCORD_GUILD_ID=…        # right-click your server → "Copy Server ID"
export COOP_DISCORD_PREFIX="!coop"    # default
export COOP_DISCORD_ALLOWED_USERS="123,456"   # default-deny: only these IDs can dispatch
```

## Usage

Create a Discord channel named exactly like a chicken (`aria`, …), then in that
channel:

| Message | Effect |
|---------|--------|
| `!coop <prompt>` | submit a job to the chicken |
| `!coop status` | show the chicken's current state |
| `!coop hatch` | hatch a DEFINED chicken |
| `!coop sleep` / `wake` | put the chicken to sleep / wake it |
| `!coop help` | command list |

Connector code lives in [`crates/coopd-discord`](../crates/coopd-discord); built
on `serenity` 0.12 and runs only when explicitly enabled.

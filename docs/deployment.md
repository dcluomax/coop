# Deployment

Three supported ways to run `coopd` beyond a foreground `coopd serve &`.

> ⚠️ **Before exposing Coop beyond `127.0.0.1`** set `COOP_API_TOKEN` (bearer
> auth) **and** `COOP_PUBLIC=1` (accept non-loopback `Host`/`Origin`). Without
> the token you expose an unauthenticated farm; without `COOP_PUBLIC=1` every
> non-loopback request is rejected. See [configuration.md](./configuration.md).

## Docker

```bash
docker build -t coop .
docker run -d --name coopd \
  -p 9700:9700 \
  -v coop-data:/data \
  -e COOP_PUBLIC=1 \
  -e COOP_API_TOKEN="$(openssl rand -hex 32)" \
  coop
```

The image bundles `bubblewrap` (per-hen bash sandbox) and `tmux` (PTY
sessions); data persists in the `coop-data` volume.

## Docker Compose (recommended)

```bash
export COOP_API_TOKEN=$(openssl rand -hex 32)
docker compose up -d
docker compose logs -f
```

See [`docker-compose.yml`](../docker-compose.yml). To auto-unlock a vault,
mount it under `/data` and set `COOP_VAULT` / `COOP_PASSPHRASE`.

## systemd (bare metal, 24/7)

```bash
# 1. install binaries
curl -fsSL https://raw.githubusercontent.com/dcluomax/coop/main/scripts/install.sh | sudo COOP_INSTALL_DIR=/usr/local/bin sh

# 2. service user + unit + env
sudo useradd --system --create-home --home-dir /var/lib/coop coop
sudo install -Dm644 contrib/systemd/coopd.service /etc/systemd/system/coopd.service
sudo install -Dm640 contrib/coop.env.example /etc/coop/coop.env
sudo chown root:coop /etc/coop/coop.env
sudoedit /etc/coop/coop.env          # set COOP_ADDR / token as needed

# 3. enable
sudo systemctl daemon-reload
sudo systemctl enable --now coopd
journalctl -u coopd -f
```

The unit ([`contrib/systemd/coopd.service`](../contrib/systemd/coopd.service))
runs as an unprivileged `coop` user with `ProtectSystem=strict`, keeps state in
`/var/lib/coop`, and restarts on failure.

## Behind a reverse proxy / tunnel

Fronting `coopd` with nginx, Caddy, or a Cloudflare/`cloudflared` tunnel
(public hostname → container) has two requirements that are easy to miss:

1. **The proxy must be able to reach the daemon.** With Docker, put the proxy
   and `coopd` on the **same network** — otherwise the proxy resolves the
   service name to nothing and returns `502 Bad Gateway`:

   ```bash
   docker network connect <proxy-net> coopd
   # or, in compose, list the shared network under the coopd service:
   #   networks: [default, <proxy-net>]
   ```

2. **Set `COOP_PUBLIC=1`.** The proxy forwards a public `Host`/`Origin`
   (e.g. `farm.example.com`), which the loopback allowlist rejects with `403`
   until you opt in. Keep `COOP_API_TOKEN` set — `COOP_PUBLIC=1` only relaxes
   the `Host`/`Origin` check, it does **not** disable auth.

The public hostname also needs a DNS record pointing at the proxy/tunnel; a
missing record surfaces as `NXDOMAIN` / connection failures, not a `coopd`
error. Verify the chain end to end:

```bash
curl -s -o /dev/null -w '%{http_code}\n' https://farm.example.com/api/v1/healthz   # 200 (exempt)
curl -s -o /dev/null -w '%{http_code}\n' https://farm.example.com/                 # 401 (auth works)
curl -s -o /dev/null -w '%{http_code}\n' "https://farm.example.com/?token=$TOKEN"  # 200 (UI loads)
```

## Reaching the farm from other devices

Once bound to `0.0.0.0` with `COOP_PUBLIC=1` + a token, open the Farm UI, click
⚙️, and the **📍 Farm location** panel lists every reachable URL. The same data
is at `GET /api/v1/farm/location`:

```json
{
  "hostname": "my-host.local",
  "bound_addr": "0.0.0.0:9700",
  "loopback_only": false,
  "urls": [
    { "label": "Hostname (my-host)", "url": "http://my-host.local:9700/", "scope": "lan" }
  ]
}
```

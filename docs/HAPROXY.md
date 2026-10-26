# Putting Homestock behind HAProxy

Homestock serves HTTPS itself, with a self-signed certificate that browsers warn about. If you
already run a reverse proxy with trusted certificates, such as HAProxy on OPNsense with Let's
Encrypt certificates from the ACME plugin, put Homestock behind it. You get:

- a trusted certificate, so there are no browser warnings, and the proxy renews it;
- a friendly address such as `https://homestock.example.com`, with no port number;
- encryption all the way: the proxy **re-encrypts** traffic to Homestock's own certificate, so
  nothing crosses your network in plain text.

Three things make it work:
1. HAProxy connects to Homestock over HTTPS, without checking its self-signed certificate.
2. HAProxy adds an `X-Forwarded-For` header with the real client's address.
3. Homestock is told to trust that header from the proxy, via `trusted_proxies`.

The third part matters. Homestock allows each client 5 wrong passwords a minute. Without
`trusted_proxies`, every request appears to come from the proxy, so one person's typos could
stop everyone signing in for a minute.

The examples use `10.0.20.15` for Homestock, `10.0.20.1` for OPNsense's address on that network,
and `homestock.example.com` as the name.

## On OPNsense

This assumes the **os-haproxy** plugin is installed and you already have an HTTPS public service
(frontend) with a certificate covering Homestock's name, as you would for other services.
Settings are under **Services → HAProxy → Settings**. Field names can differ a little between
plugin versions.

1. **Real Servers → Add:**
   - **Name:** `homestock`
   - **FQDN or IP:** `10.0.20.15`, **Port:** `8443`
   - **SSL:** enabled. **Verify SSL Certificate:** disabled, since Homestock's certificate is
     self-signed.
2. **Rules & Checks → Health Monitors → Add:**
   - **Name:** `homestock_health`, **Type:** HTTP
   - **HTTP method:** GET, **Request URI:** `/healthz`

   Homestock answers `/healthz` with `ok` once its database is ready.
3. **Virtual Services → Backend Pools → Add:**
   - **Name:** `homestock_backend`, **Mode:** HTTP (Layer 7)
   - **Servers:** `homestock`
   - **Health Monitor:** `homestock_health`
   - **X-Forwarded-For header:** enabled
4. **Rules & Checks → Conditions → Add:** **Name** `host_homestock`, **Condition type** "Host
   matches", **Host** `homestock.example.com`.
5. **Rules & Checks → Rules → Add:** **Name** `route_homestock`, **Conditions**
   `host_homestock`, **Execute function** "Use specified Backend Pool", **Backend**
   `homestock_backend`.
6. **Virtual Services → Public Services:** edit your HTTPS frontend and add `route_homestock`
   under **Select Rules**.
7. **Apply**, and check **Statistics** shows the `homestock` server as up.

Make `homestock.example.com` resolve to OPNsense on your LAN, for example with a host override
in **Services → Unbound DNS → Overrides**.

## Plain HAProxy

The same setup in a standalone `haproxy.cfg`:

```
frontend https
    bind :443 ssl crt /etc/haproxy/certs/
    mode http
    use_backend homestock if { req.hdr(host) -i homestock.example.com }

backend homestock
    mode http
    option forwardfor
    option httpchk GET /healthz
    http-check expect status 200
    server homestock 10.0.20.15:8443 ssl verify none check
```

To have HAProxy check Homestock's certificate rather than skip the check, copy Homestock's
`cert.pem` to the proxy and use `ssl verify required ca-file /path/to/homestock-cert.pem`.

## On Homestock

Tell Homestock which address the proxy connects from. On OPNsense, that's its address on
Homestock's network. Either:
- give it at install time: `sudo bash install.sh --trusted-proxy 10.0.20.1`;
- or, for an existing install, edit `/opt/homestock/config.toml`:
  ```toml
  trusted_proxies = ["10.0.20.1"]
  ```
  then `sudo docker restart homestock`.

List only proxy addresses. Homestock believes the `X-Forwarded-For` header only from these
addresses, and ignores it from everyone else.

## Lock down the direct port

Clients should reach Homestock only through the proxy. Firewall port 8443 so only the proxy's
address can connect to it. In a Proxmox LXC, use the container's firewall: see
[Proxmox LXC → Firewall](PROXMOX-LXC.md#firewall). Otherwise use the host's firewall, or a rule
on OPNsense if Homestock is on a separate network.

## Check it

- Open `https://homestock.example.com`: no certificate warning, and you can sign in.
- `sudo homestock-doctor` on the Homestock server still passes. It checks Homestock directly,
  not through the proxy.
- Homestock's sign-in cookies stay marked secure (`cookie_secure = true`). Leave that on: the
  browser's connection to the proxy is HTTPS.

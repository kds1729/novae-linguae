# Custom Caddy for Arca's TLS edge: stock caddy has no rate-limit handler, so build one in with the
# caddy-ratelimit plugin (the Caddyfile's `rate_limit` directive needs it). Everything else — automatic
# Let's Encrypt, http→https, reverse_proxy — is core Caddy.
FROM caddy:2-builder-alpine AS builder
RUN xcaddy build --with github.com/mholt/caddy-ratelimit

FROM caddy:2-alpine
COPY --from=builder /usr/bin/caddy /usr/bin/caddy

# CrowdSec for CalZone

CalZone writes one fixed line per refused sign-in (web passphrase,
two-factor code, device password), with the client address as Traefik saw
it (`CALZONE_BEHIND_PROXY=on`):

```
2026-11-06T09:39:38.293654Z  WARN calzone::auth: auth_failure service=web reason=wrong ip=203.0.113.7
```

CalZone already slows guessing on its own (10 failures per address and 5
per account per 15 minutes). CrowdSec adds a ban at the proxy, so a
persistent guesser stops reaching CalZone at all.

## Install

On the CrowdSec host (a CrowdSec container needs the Docker socket
mounted read-only for the docker source):

```
cp acquis.yaml     /etc/crowdsec/acquis.d/calzone.yaml
cp calzone-logs.yaml /etc/crowdsec/parsers/s01-parse/calzone-logs.yaml
cp calzone-bf.yaml   /etc/crowdsec/scenarios/calzone-bf.yaml
systemctl reload crowdsec          # or restart the container
```

Bans need a bouncer at the entry point, for example the Traefik CrowdSec
bouncer plugin; without one, CrowdSec only detects.

## Check

```
cscli parsers list | grep calzone
cscli scenarios list | grep calzone
cscli explain --type calzone --log '2026-11-06T09:39:38.293654Z  WARN calzone::auth: auth_failure service=web reason=wrong ip=203.0.113.7'
```

The `explain` run should show `calzone/calzone-logs` succeeding and the
event reaching `calzone/calzone-bf`. After a few wrong logins from a test
address, `cscli alerts list` shows them once the bucket overflows.

## Do not ban yourselves

A phone with a removed device password keeps retrying, a few times per
sync. That stays under the limit, but a whole household behind one home
address shares the bucket. Whitelist the home address (or range) with a
CrowdSec whitelist parser, as for any other service you reach from home.

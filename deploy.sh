#!/usr/bin/env bash
#
# Puts the headless server on the v-server: build here, stop what is running there, copy what
# changed, start it again.
#
#   ./deploy.sh                 build, copy, restart
#   ./deploy.sh --release       build with optimisations rather than the dev profile
#   ./deploy.sh --no-build      copy what is already built
#   ./deploy.sh --overwrite-maps  push every map, not only the ones the server lacks
#   ./deploy.sh --no-maps       leave the server's maps alone entirely
#   ./deploy.sh --logs          follow the journal afterwards instead of printing the tail
#
# The binary is built here rather than there on purpose: both machines are Debian 13 on x86_64
# with the same glibc, so the artefact is portable, and a v-server does not have to hold a Rust
# toolchain or spend twenty minutes and several gigabytes compiling Bevy. The check below refuses
# the copy if that assumption ever stops being true.
#
# Everything it does on the far side is idempotent — the user, the directories and the unit are
# created on the first run and only corrected on later ones — so this is also the provisioning
# script. There is no separate one to forget to run.

set -euo pipefail

# --- What and where --------------------------------------------------------------------------

HOST="${NOOB_TUBE_DEPLOY_HOST:-root@fkirchhoff.com}"
PREFIX=/srv/noob_tube          # everything the server owns on the far side lives under here
RUN_USER=noobtube              # and it runs as this, which is not root and cannot log in
SERVICE=noob-tube
BIN=noob_tube_server
CONFIG=noob_tube_vserver.toml  # copied over as $PREFIX/noob_tube.toml

cd "$(dirname "$(readlink -f "$0")")"

build=yes
profile=debug   # the dev profile: dependencies are optimised, our own code is not — see Cargo.toml
maps=new        # new | all | none
follow=no

for arg in "$@"; do
    case "$arg" in
        --no-build) build=no ;;
        --release) profile=release ;;
        --overwrite-maps) maps=all ;;
        --no-maps) maps=none ;;
        --logs) follow=yes ;;
        --host=*) HOST="${arg#--host=}" ;;
        -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "deploy: unknown option $arg (try --help)" >&2; exit 2 ;;
    esac
done

say() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }

# One SSH connection for the whole run, shared by every ssh and rsync below: the far side is
# authenticated once instead of six times, and a run costs one handshake rather than six.
CTL=$(mktemp -u "${TMPDIR:-/tmp}/noob-tube-deploy.XXXXXX")
SSH_OPTS=(-o ControlMaster=auto -o ControlPath="$CTL" -o ControlPersist=120)
ssh_() { ssh "${SSH_OPTS[@]}" "$HOST" "$@"; }
cleanup() { ssh -o ControlPath="$CTL" -O exit "$HOST" 2>/dev/null || true; }
trap cleanup EXIT

# The ports the config asks for, so the checks at the end look at what this deploy actually did
# rather than at what the defaults used to be.
port=$(awk -F'[ =]+' '/^port *=/ { print $2; exit }' "$CONFIG")
meta_port=$(awk -F'[ =]+' '/^meta_port *=/ { print $2; exit }' "$CONFIG")
: "${port:=5000}" "${meta_port:=5001}"

# --- Build -----------------------------------------------------------------------------------

if [ "$build" = yes ]; then
    say "Building $BIN ($profile)"
    case "$profile" in
        debug)   cargo build -p noob_tube_server ;;
        release) cargo build --release -p noob_tube_server ;;
    esac
fi

binary=target/$profile/$BIN
[ -x "$binary" ] || { echo "deploy: no $binary — drop --no-build" >&2; exit 1; }
echo "deploying $binary"

# --- Is the artefact usable over there? ---------------------------------------------------------

say "Checking $HOST"
# `awk` rather than `head`, here and above: `head` closes the pipe on the line it wanted, the
# process behind it dies of SIGPIPE, and `set -o pipefail` then takes the whole script down.
remote_arch=$(ssh_ 'uname -m')
remote_glibc=$(ssh_ 'ldd --version' | awk 'NR == 1 { print $NF }')
local_glibc=$(ldd --version | awk 'NR == 1 { print $NF }')

[ "$remote_arch" = "$(uname -m)" ] || {
    echo "deploy: $HOST is $remote_arch, this machine is $(uname -m). Cross-compile or build there." >&2
    exit 1
}
# A binary linked against a newer glibc than the host has does not fail at the copy, it fails at
# the first start, with a message about a version node not being found. Better to say so here.
if [ "$(printf '%s\n%s\n' "$local_glibc" "$remote_glibc" | sort -V | head -1)" != "$local_glibc" ]; then
    echo "deploy: built against glibc $local_glibc, $HOST has $remote_glibc — it would not start." >&2
    exit 1
fi
echo "$HOST: $remote_arch, glibc $remote_glibc — good (here: $local_glibc)"

# The address the game socket binds, which is not a detail: netcode's connect token names the
# address the client dialled, and the server refuses a token that does not name the address it is
# bound to — with one exception, an unspecified address against a loopback token, which is why a
# server on 0.0.0.0 works perfectly at home and silently refuses every real client. See
# `bind_address` in server/src/main.rs.
#
# Taken from the far side's own routing table rather than from DNS, because it is the address the
# machine actually answers on.
public_ip=$(ssh_ "ip -4 route get 1.1.1.1" | awk '{ for (i = 1; i < NF; i++) if ($i == "src") print $(i + 1); exit }')
[ -n "$public_ip" ] || { echo "deploy: cannot work out $HOST's public address" >&2; exit 1; }
echo "$HOST: binding $public_ip"

# What a client dialling by name would put in its token. A mismatch is not fatal — the DNS may not
# have caught up, or this deploy may be going to a machine reached by IP — but a client that
# resolves the name to something else will be refused, so it is worth saying out loud.
resolved=$(getent ahostsv4 "${HOST#*@}" 2>/dev/null | awk 'NR == 1 { print $1 }')
if [ -n "$resolved" ] && [ "$resolved" != "$public_ip" ]; then
    echo "deploy: warning — ${HOST#*@} resolves to $resolved, but the server binds $public_ip." >&2
    echo "        A client dialling the name will be refused. Point the DNS at $public_ip." >&2
fi

# --- The user, the directories and the unit ------------------------------------------------------

say "Provisioning $PREFIX and the $RUN_USER account"
ssh_ "
    set -eu
    # A system account: no password, no shell, no home of its own beyond the deploy directory. It
    # exists so that a bug in a network-facing game server is a bug in an account that owns one
    # directory, rather than one in root.
    getent passwd $RUN_USER >/dev/null ||
        useradd --system --home-dir $PREFIX --shell /usr/sbin/nologin \
                --comment 'Noob Tube game server' $RUN_USER
    install -d -m 755 -o root -g root $PREFIX $PREFIX/bin
    # The one directory the service may write to: maps saved from a client land here.
    install -d -m 755 -o $RUN_USER -g $RUN_USER $PREFIX/maps
"

# The unit is generated rather than kept as a file, so the paths in it cannot drift from the ones
# at the top of this script.
unit=$(mktemp "${TMPDIR:-/tmp}/noob-tube.service.XXXXXX")
trap 'rm -f "$unit"; cleanup' EXIT
cat > "$unit" <<UNIT
# Written by deploy.sh. Edit that, not this — the next deploy overwrites this file.
[Unit]
Description=Noob Tube authoritative game server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=$RUN_USER
Group=$RUN_USER
WorkingDirectory=$PREFIX
# The config is read once at startup; the maps directory is read then and written whenever a
# client saves a map. Without the second variable the binary would look for maps under the path
# it was *built* at, which does not exist on this machine.
Environment=NOOB_TUBE_CONFIG=$PREFIX/noob_tube.toml
Environment=NOOB_TUBE_MAPS=$PREFIX/maps
Environment=NOOB_TUBE_BIND=$public_ip
Environment=RUST_BACKTRACE=1
ExecStart=$PREFIX/bin/$BIN
# A game server that dies mid-round should be back before the players have finished swearing.
Restart=always
RestartSec=2

# It listens on a public port and parses what strangers send it, so it gets the whole restriction
# list: no new privileges, nothing of the filesystem writable but its own maps directory, no
# sockets but the ones it needs. AF_UNIX and AF_NETLINK stay because glibc uses them to look at
# the machine's own interfaces on bind.
NoNewPrivileges=true
PrivateTmp=true
PrivateDevices=true
ProtectSystem=strict
ProtectHome=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictNamespaces=true
RestrictSUIDSGID=true
LockPersonality=true
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX AF_NETLINK
ReadWritePaths=$PREFIX/maps

[Install]
WantedBy=multi-user.target
UNIT

# --- Stop, copy, start ---------------------------------------------------------------------------

say "Stopping $SERVICE"
# Before the copy, not after: a running binary cannot be overwritten in place — the kernel refuses
# with "text file busy" — and a config swapped under a live process would be read by nobody anyway.
ssh_ "systemctl stop $SERVICE 2>/dev/null || true"

say "Copying"
rsync -e "ssh ${SSH_OPTS[*]}" -avz --no-owner --no-group --checksum \
      "$binary" "$HOST:$PREFIX/bin/$BIN"
rsync -e "ssh ${SSH_OPTS[*]}" -avz --no-owner --no-group \
      "$CONFIG" "$HOST:$PREFIX/noob_tube.toml"
rsync -e "ssh ${SSH_OPTS[*]}" -avz --no-owner --no-group \
      "$unit" "$HOST:/etc/systemd/system/$SERVICE.service"

# Maps are the one thing that is not ours to overwrite: the server writes this directory itself
# when somebody saves a map from a client, so the copy adds what is missing and touches nothing
# else. --delete is never passed here for the same reason, and --overwrite-maps is opt-in.
case "$maps" in
    new)  say "Copying maps the server does not have"
          rsync -e "ssh ${SSH_OPTS[*]}" -avz --no-owner --no-group --ignore-existing maps/ "$HOST:$PREFIX/maps/" ;;
    all)  say "Copying all maps, overwriting the server's"
          rsync -e "ssh ${SSH_OPTS[*]}" -avz --no-owner --no-group maps/ "$HOST:$PREFIX/maps/" ;;
    none) echo "maps: left alone" ;;
esac

ssh_ "
    set -eu
    chown root:root $PREFIX/bin/$BIN $PREFIX/noob_tube.toml /etc/systemd/system/$SERVICE.service
    chmod 755 $PREFIX/bin/$BIN
    chmod 644 $PREFIX/noob_tube.toml /etc/systemd/system/$SERVICE.service
    chown -R $RUN_USER:$RUN_USER $PREFIX/maps
    systemctl daemon-reload
    systemctl enable $SERVICE >/dev/null
"

say "Starting $SERVICE"
ssh_ "systemctl start $SERVICE"

# --- Did it come up? -----------------------------------------------------------------------------

# Long enough for a config it refuses to parse to have taken the process down with it: the server
# rejects a misspelled key at startup rather than ignoring it, so a bad config is a dead unit and
# not a quiet one.
sleep 2
say "Status"
if ssh_ "systemctl is-active --quiet $SERVICE"; then
    ssh_ "systemctl --no-pager --lines=0 status $SERVICE | sed -n '1,5p'"
    echo
    ssh_ "ss -lnup 2>/dev/null | grep -q '$public_ip:$port ' && echo \"game:     udp $public_ip:$port listening\" || echo \"game:     udp $public_ip:$port NOT listening\"
          ss -lntp 2>/dev/null | grep -q ':$meta_port ' && echo \"metadata: tcp/$meta_port listening\" || echo \"metadata: tcp/$meta_port NOT listening\""
else
    echo "deploy: $SERVICE did not stay up. The last of its journal:" >&2
    ssh_ "journalctl -u $SERVICE --no-pager --lines=40" >&2
    exit 1
fi

say "Journal"
if [ "$follow" = yes ]; then
    ssh_ "journalctl -u $SERVICE --no-pager --follow --lines=20"
else
    ssh_ "journalctl -u $SERVICE --no-pager --lines=20"
    echo
    echo "Connect with:  NOOB_TUBE_SERVER=${HOST#*@} cargo run --release -p noob_tube_client"
    echo "Follow it:     ssh $HOST journalctl -u $SERVICE -f"
fi

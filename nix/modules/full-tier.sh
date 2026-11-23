# The commands of one full tier, per ADR-0500. The wrapper of the module sets each FULL_TIER_ variable.
# Every command acts only on the QEMU process of this state folder, so a second lab on the machine stays safe.

state=${FULL_TIER_STATE:-$HOME/.local/share/full-tier/$FULL_TIER_NAME}
pidfile=$state/qemu.pid
monitor=$state/monitor.sock
ssh_options=(-p "$FULL_TIER_SSH_PORT" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 -o BatchMode=yes)
if [ -n "${FULL_TIER_SSH_KEY:-}" ]; then
  ssh_options+=(-i "$FULL_TIER_SSH_KEY" -o IdentitiesOnly=yes)
fi

running() {
  [ -f "$pidfile" ] || return 1
  local pid
  pid=$(cat "$pidfile")
  [ -r "/proc/$pid/cmdline" ] && tr '\0' ' ' <"/proc/$pid/cmdline" | grep -qF "$state/disk-a.qcow2"
}

# A start needs free memory and disk, so it cannot push another workload of the machine out of memory.
guard() {
  local memory disk
  memory=$(awk '/^MemAvailable:/ { print int($2 / 1048576) }' /proc/meminfo)
  mkdir -p "$state"
  disk=$(df --output=avail -BG "$state" | tail -1 | tr -dc 0-9)
  if [ "$memory" -lt "$FULL_TIER_MIN_MEMORY_GIB" ]; then
    echo "The full tier needs ${FULL_TIER_MIN_MEMORY_GIB} GiB of free memory, and ${memory} GiB is free." >&2
    exit 1
  fi
  if [ "$disk" -lt "$FULL_TIER_MIN_DISK_GIB" ]; then
    echo "The full tier needs ${FULL_TIER_MIN_DISK_GIB} GiB of free disk, and ${disk} GiB is free." >&2
    exit 1
  fi
}

# The disks argument is `a b`, `a`, or `b`, so a test can boot from one disk of the mirror.
start() {
  local iso=${1:-} disks=${2:-a b}
  if running; then
    echo "The full tier already runs." >&2
    exit 1
  fi
  guard
  local args=() disk
  for disk in a b; do
    [ -f "$state/disk-$disk.qcow2" ] || qemu-img create -q -f qcow2 "$state/disk-$disk.qcow2" "${FULL_TIER_DISK_GIB}G"
  done
  for disk in $disks; do
    args+=(-drive "file=$state/disk-$disk.qcow2,if=none,id=disk-$disk,format=qcow2")
    args+=(-device "virtio-blk-pci,drive=disk-$disk,serial=disk-$disk")
  done
  [ -n "$iso" ] && args+=(-cdrom "$iso" -boot order=dc)
  qemu-system-x86_64 -enable-kvm -cpu host -smp "$FULL_TIER_CPUS" -m "$FULL_TIER_MEMORY_MIB" \
    "${args[@]}" \
    -netdev "user,id=net0,$FULL_TIER_FORWARDS" \
    -device "virtio-net-pci,netdev=net0,mac=$FULL_TIER_MAC" \
    -display none -daemonize -pidfile "$pidfile" \
    -serial "file:$state/serial.log" -monitor "unix:$monitor,server,nowait"
  echo "The full tier starts on SSH port $FULL_TIER_SSH_PORT."
}

wait_ssh() {
  local tries=0
  until ssh "${ssh_options[@]}" root@127.0.0.1 true 2>/dev/null; do
    tries=$((tries + 1))
    if [ "$tries" -gt 120 ]; then
      echo "SSH did not answer. Read $state/serial.log." >&2
      exit 1
    fi
    sleep 5
  done
}

stop() {
  if ! running; then
    echo "The full tier does not run."
    return 0
  fi
  local pid tries=0
  pid=$(cat "$pidfile")
  printf 'system_powerdown\n' | socat - "UNIX-CONNECT:$monitor" >/dev/null
  while kill -0 "$pid" 2>/dev/null; do
    tries=$((tries + 1))
    if [ "$tries" -gt 60 ]; then
      kill "$pid"
      break
    fi
    sleep 2
  done
  rm -f "$pidfile"
}

install_host() {
  if running; then
    echo "Stop the full tier before an install." >&2
    exit 1
  fi
  rm -f "$state/disk-a.qcow2" "$state/disk-b.qcow2"
  local installer iso
  installer=$(nix build --no-link --print-out-paths ".#full-tier-$FULL_TIER_NAME-installer")
  iso=$(find "$installer/iso" -name '*.iso' -print -quit)
  start "$iso"
  wait_ssh
  local extra
  extra=$(mktemp -d)
  trap 'rm -rf "$extra"' EXIT
  if [ -n "${FULL_TIER_HOST_KEY:-}" ]; then
    install -d -m 0755 "$extra/etc/ssh"
    install -m 0600 "$FULL_TIER_HOST_KEY" "$extra/etc/ssh/ssh_host_ed25519_key"
    ssh-keygen -y -f "$extra/etc/ssh/ssh_host_ed25519_key" >"$extra/etc/ssh/ssh_host_ed25519_key.pub"
  fi
  local identity=()
  if [ -n "${FULL_TIER_SSH_KEY:-}" ]; then
    identity=(-i "$FULL_TIER_SSH_KEY")
  fi
  nixos-anywhere --flake ".#$FULL_TIER_CONFIGURATION" --phases disko,install --extra-files "$extra" \
    "${identity[@]}" --ssh-port "$FULL_TIER_SSH_PORT" \
    --ssh-option StrictHostKeyChecking=no --ssh-option UserKnownHostsFile=/dev/null \
    --target-host root@127.0.0.1
  stop
  start
  wait_ssh
}

case ${1:-} in
start) start "" "${2:-a b}" ;;
stop) stop ;;
install) install_host ;;
deploy)
  shift
  wait_ssh
  deploy ".#$FULL_TIER_NODE" "$@"
  ;;
ssh)
  shift
  exec ssh "${ssh_options[@]}" root@127.0.0.1 "$@"
  ;;
status)
  if running; then echo running; else echo stopped; fi
  ;;
destroy)
  stop
  rm -f "$state/disk-a.qcow2" "$state/disk-b.qcow2" "$state/serial.log"
  ;;
*)
  echo "Use one of: start [a|b], stop, install, deploy, ssh, status, destroy." >&2
  exit 2
  ;;
esac

#!/usr/bin/env bash
# HoH installer (docs/adr/ADR-023.md decisions 3 and 4), run from the release bundle (hoh/install.sh) or from an
# installed version. It installs for this user only, in its home: never as root, never through sudo.
#
#   install.sh [--project <directory>]... [--linger]
#       Install this bundle: $XDG_DATA_HOME/hoh/versions/<version>, the `current` link switched atomically,
#       ~/.local/bin/hoh, and the systemd user unit template hoh-host@.service. --project enables the host unit of
#       that project (the user must linger, else the host would stop with the last session); --linger asks logind for
#       it. Nothing is enabled without --project.
#   install.sh --uninstall
#       Stop and disable every hoh-host@ instance; once none runs and no process runs this installation's binaries,
#       remove the template, ~/.local/bin/hoh and what it wrote in $XDG_DATA_HOME/hoh (versions, current, the
#       instances' files). Projects' .hoh/, drop-ins, ~/.config/hoh and lingering are left as they are, and so is
#       the installers' own lock $XDG_DATA_HOME/hoh.lock (an empty file, kept so that its lock is never on a removed one).
#
# Exit codes: 0 done; 1 ~/.local/bin/hoh is not this installation's; 64 usage; 69 the user's systemd manager cannot do
# what was asked, something still runs this installation or its lock cannot be told (nothing removed), another
# install.sh of it holds the installer's lock, or the installed binary does not run; 77 run as root; 78 a home or
# project systemd cannot use.
set -Eeuo pipefail

say() { printf 'install.sh: %s\n' "$*" >&2; }
fail() {
  local code=$1
  shift
  say "$*"
  exit "$code"
}
usage() {
  sed -n '5,14p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
  exit "${1:-64}"
}

# An XDG base directory: the variable when it holds an absolute path, else the default (the XDG spec ignores relative ones).
xdg() {
  local value=$1 default=$2
  if [[ "$value" == /* ]]; then printf '%s' "$value"; else printf '%s' "$default"; fi
}

main() {
  local uninstall=0 linger=0
  local -a projects=()
  while (($#)); do
    case "$1" in
      --uninstall) uninstall=1 ;;
      --linger) linger=1 ;;
      --project)
        [[ $# -ge 2 && -n "$2" ]] || usage
        projects+=("$2")
        shift
        ;;
      --project=?*) projects+=("${1#--project=}") ;;
      -h | --help) usage 0 ;;
      *) usage ;;
    esac
    shift
  done
  if ((uninstall)) && ((linger || ${#projects[@]})); then usage; fi
  if [[ "$(id -u)" == 0 ]]; then
    fail 77 "refusing to run as root: HoH is installed for one user, in that user's home"
  fi
  [[ "${HOME:-}" == /* ]] || fail 78 "HOME is not an absolute path"
  data="$(xdg "${XDG_DATA_HOME:-}" "$HOME/.local/share")"
  config="$(xdg "${XDG_CONFIG_HOME:-}" "$HOME/.config")"
  runtime="${XDG_RUNTIME_DIR:-}"
  root="$data/hoh"
  current="$root/current"
  bin_link="$HOME/.local/bin/hoh"
  template="$data/systemd/user/hoh-host@.service"
  override="$config/systemd/user/hoh-host@.service"
  user="${USER:-$(id -un)}"
  if ((uninstall)); then
    # Nothing installed has no installer's lock either (its directory is the data directory).
    [[ ! -d "$data" ]] || hold_operation
    remove
  else
    check_paths
    refusals "${projects[@]}"
    mkdir -p -- "$data"
    hold_operation
    install "${projects[@]}"
  fi
}

# The installer's own lock (docs/adr/ADR-023.md design corrections 5 and 7): every install and uninstall holds
# $XDG_DATA_HOME/hoh.lock exclusive for its whole run, so installers of one installation take turns and none sees
# another's half-done work. A file beside the installation's root that no uninstall removes (its lock is never on a
# file that is gone), opened read-write (as an NFS client needs for an exclusive lock), which no run of hoh locks.
hold_operation() {
  local lock="$data/hoh.lock" said rc=0
  if ! { command exec 8<>"$lock"; } 2>/dev/null; then
    fail 69 "cannot open the installer's lock $lock"
  fi
  # Waiting 60 s on another holder is told from a lock that cannot be had at all (M2T14R-401).
  said="$(flock -E 75 -w 60 -x 8 2>&1)" || rc=$?
  case "$rc" in
    0) ;;
    75) fail 69 "$lock stayed locked for 60 s (another install.sh of this installation runs): try again when it ends" ;;
    *) fail 69 "cannot lock $lock (flock exit $rc: ${said:-no message})" ;;
  esac
}

# What refuses an install before it writes anything, even its lock (exit 1 or 69).
refusals() {
  if [[ -e "$bin_link" || -L "$bin_link" ]] && ! ours; then
    fail 1 "$bin_link exists and is not this installation's link to $current/bin/hoh: nothing changed"
  fi
  if (($#)) && ! manager; then
    fail 69 "no user systemd manager answers (\$XDG_RUNTIME_DIR/systemd/private): cannot enable a project's host unit"
  fi
}

# Before anything is written: EnvironmentFile= takes a glob, ExecStart= no program path with quotes or a backslash
# (design corrections 1 and 2, M2T14R-101).
check_paths() {
  local real_root
  real_root="$(readlink -m -- "$root")"
  refuse_chars "the installation's root" "$root" '*' '?' '[' '"' '\' "'"
  refuse_chars "the installation's root" "$real_root" '*' '?' '[' '"' '\' "'"
  refuse_chars "HOME" "$HOME" '"' '\' "'"
}

# Whether the user's systemd manager answers.
manager() { [[ -n "$runtime" && -S "$runtime/systemd/private" ]]; }

lingers() { [[ -f "/var/lib/systemd/linger/$user" ]]; }

# Whether ~/.local/bin/hoh is this installation's link.
ours() { [[ -L "$bin_link" && "$(readlink -- "$bin_link")" == "$current/bin/hoh" ]]; }

# Fail with 78 when `$2` (named `$1`) holds one of the characters that follow: the unit cannot name it as written.
refuse_chars() {
  local what=$1 path=$2 c
  shift 2
  for c in "$@"; do
    [[ "$path" != *"$c"* ]] || fail 78 "$what has $c, which the systemd unit cannot take as written: $path"
  done
}

# Point the symbolic link `$2` at `$1`, replacing it at once (a new link renamed over it) unless it already does.
link() {
  local target=$1 at=$2
  [[ -L "$at" && "$(readlink -- "$at")" == "$target" ]] && return 0
  ln -s -- "$target" "$at.tmp.$$"
  mv -T -- "$at.tmp.$$" "$at"
}

install() {
  local src name semver sha changed=0
  src="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
  [[ -x "$src/bin/hoh" ]] || fail 64 "$src/bin/hoh is missing: run the install.sh of a release bundle"
  # An installed hoh (this script may be one's copy) runs only with its installation's lock: one missing from an
  # existing installation is made again before any of its binaries runs (M2T14R-352).
  [[ ! -d "$root" || -e "$root/.lock" ]] || : >"$root/.lock"
  semver="$("$src/bin/hoh" --version | sed -n '1s/^hoh //p')"
  [[ "$semver" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail 64 "$src/bin/hoh --version does not name a version"
  sha="$(sha256sum -- "$src/bin/hoh" | cut -c1-12)"
  name="$semver-$sha"
  # A version directory is named by its binary and never changed once there: a host running from it, and the agents
  # it starts through its start gate, keep using it until they restart.
  mkdir -p -- "$root/versions"
  # The installation's lock, which every run of it holds shared; made here, never by a run (design correction 3).
  [[ -e "$root/.lock" ]] || : >"$root/.lock"
  if [[ ! -d "$root/versions/$name" ]]; then
    local tmp="$root/versions/.tmp.$$" entry
    rm -rf -- "$tmp"
    mkdir -- "$tmp"
    for entry in bin resources web LICENSE THIRD_PARTY_NOTICES.md install.sh; do
      [[ ! -e "$src/$entry" ]] || cp -R -- "$src/$entry" "$tmp/$entry"
    done
    mv -T -- "$tmp" "$root/versions/$name"
  fi
  link "versions/$name" "$current"
  mkdir -p -- "$HOME/.local/bin"
  link "$current/bin/hoh" "$bin_link"
  # The template, from the installed binary for this home; written (a new file renamed over it) only when it differs,
  # so an install of the same bundle touches nothing.
  local text old
  text="$("$current/bin/hoh" __unit template && printf x)" || exit $?
  old="$({ [[ ! -f "$template" ]] || cat -- "$template"; } && printf x)"
  if [[ "$text" != "$old" ]]; then
    mkdir -p -- "$(dirname -- "$template")"
    printf '%s' "${text%x}" >"$template.tmp.$$"
    mv -T -- "$template.tmp.$$" "$template"
    changed=1
  fi
  [[ ! -e "$override" ]] || say "note: $override overrides the installed $template"
  if manager; then
    ((changed == 0)) || systemctl --user daemon-reload
    local shown
    shown="$(systemctl --user show -p LoadState,FragmentPath hoh-host@x.service)"
    if ! grep -qx 'LoadState=loaded' <<<"$shown" ||
      ! grep -qxF -e "FragmentPath=$template" -e "FragmentPath=$override" <<<"$shown"; then
      fail 69 "the user's systemd manager does not read $template (is its XDG_DATA_HOME another?): $(tr '\n' ' ' <<<"$shown")"
    fi
  fi
  if ((linger)); then
    loginctl enable-linger "$user" || fail 69 "loginctl enable-linger $user was refused"
  fi
  local project
  for project in "$@"; do
    "$current/bin/hoh" __unit enable "$project"
  done
  # The installed command must run: a failure here is the install's, not hidden by the message (M2T14R-301).
  local shown
  shown="$("$current/bin/hoh" --version)" || fail 69 "the installed $current/bin/hoh does not run"
  printf 'installed %s in %s\n' "$(head -2 <<<"$shown" | tr '\n' ' ')" "$root/versions/$name"
  printf 'command: %s\n' "$bin_link"
  if lingers; then
    printf 'linger: yes\n'
  else
    printf 'linger: no (a host unit would stop with your last session; install.sh --linger asks for it)\n'
  fi
}

remove() {
  local hosts="$root/hosts" file unit active failed=0
  if [[ ! -e "$root" && ! -e "$template" ]] && ! ours; then
    printf 'not installed\n'
    return 0
  fi
  # Only what this installer wrote is removed, all of it under its own root or at its fixed places; never a path
  # taken from the manager's configuration (docs/adr/ADR-023.md design correction 1, M2T14R-01).
  if manager; then
    local -A instances=()
    for file in "$hosts"/*.env "$config/systemd/user/"*.wants/hoh-host@*.service; do
      [[ -e "$file" || -L "$file" ]] || continue
      unit="$(basename -- "$file")"
      unit="${unit%.env}"
      [[ "$unit" == hoh-host@* ]] || unit="hoh-host@$unit.service"
      instances["$unit"]=1
    done
    while read -r unit _; do
      [[ "$unit" != hoh-host@*.service ]] || instances["$unit"]=1
    done < <(systemctl --user list-units --all --plain --no-legend 'hoh-host@*.service' 2>/dev/null || true)
    for unit in "${!instances[@]}"; do
      active=""
      if systemctl --user disable --now --quiet "$unit"; then
        active="$(systemctl --user show -p ActiveState --value "$unit" 2>/dev/null || true)"
      fi
      if [[ "$active" == inactive || "$active" == failed ]]; then
        printf 'stopped and disabled %s\n' "$unit"
      else
        say "could not stop and disable $unit (ActiveState=${active:-unknown})"
        failed=1
      fi
    done
    systemctl --user reset-failed 'hoh-host@*.service' 2>/dev/null || true
  else
    # No manager to disable an enabled instance with: it would be left naming a removed template (M2T14R-151).
    for file in "$config/systemd/user/"*.wants/hoh-host@*.service; do
      [[ -e "$file" || -L "$file" ]] || continue
      say "$(basename -- "$file") is enabled, and no user systemd manager answers to disable it"
      failed=1
    done
  fi
  # The installation's own lock, exclusive (design correction 2, M2T14R-100): every run of an installed hoh holds it
  # shared for its life, so it is had only once none runs, and while it is held none can start; what follows is
  # removed under it.
  local held=1 run_holds=0
  if [[ -e "$root/.lock" ]]; then
    # Read-write when it can be (an NFS client needs it for an exclusive lock, M2T14R-451), else read-only (a 0400 lock
    # is fine locally, M2T14R-351); a failed open is said, not taken for a running program (M2T14R-453).
    if { command exec 9<>"$root/.lock"; } 2>/dev/null || { command exec 9<"$root/.lock"; } 2>/dev/null; then
      if ! flock -n -x 9; then
        held=0
        run_holds=1
      fi
    else
      say "cannot open this installation's lock $root/.lock"
      held=0
    fi
    # The lock held must be the file at its path (the run's check, design correction 3, and the uninstall's own,
    # correction 5): one replaced or removed meanwhile holds nothing, and one that cannot be told holds nothing either
    # (each look checked, M2T14R-400).
    if ((held)); then
      local held_id held_links named_id
      if held_id="$(stat -L -c '%d:%i' /dev/fd/9)" && held_links="$(stat -L -c '%h' /dev/fd/9)" &&
        named_id="$(stat -c '%d:%i' "$root/.lock")" && [[ "$held_id" =~ ^[0-9]+:[0-9]+$ ]] &&
        [[ "$named_id" =~ ^[0-9]+:[0-9]+$ ]] && [[ "$held_links" =~ ^[0-9]+$ ]]; then
        if [[ "$held_id" != "$named_id" || "$held_links" == 0 ]]; then
          say "this installation's lock changed while it was being taken"
          held=0
        fi
      else
        say "cannot tell whether the lock held is this installation's $root/.lock"
        held=0
      fi
    fi
  fi
  # With no lock file no run can hold the installation (each refuses to run without it); the scan below still finds
  # one between its exec and its lock.
  # Nothing is removed while a process still runs this installation's binaries: a host its unit did not run, an
  # agent's start gate, an instance that did not stop (M2T14R-03); the scan names them.
  local real proc exe running=0
  real="$(readlink -f -- "$root" 2>/dev/null || printf '%s' "$root")"
  for proc in /proc/[0-9]*; do
    exe="$(readlink -- "$proc/exe" 2>/dev/null || true)"
    if [[ "$exe" == "$real"/* || "$exe" == "$root"/* ]]; then
      say "still running from $root: pid ${proc#/proc/}: $(tr '\0' ' ' <"$proc/cmdline" 2>/dev/null || true)"
      running=1
    fi
  done
  if ((run_holds)); then
    say "this installation's lock is held: one of its programs still runs"
  fi
  if ((failed || running || !held)); then
    fail 69 "nothing was removed: stop what is named above, then run install.sh --uninstall again"
  fi
  # Each removal is said as it happened (M2T14R-150).
  if [[ -e "$template" ]]; then
    rm -f -- "$template"
    printf 'removed %s\n' "$template"
  fi
  if manager; then systemctl --user daemon-reload || say "systemctl --user daemon-reload failed"; fi
  [[ -z "$runtime" ]] || rm -f -- "$runtime"/hoh-host@*.stderr
  if ours; then
    rm -f -- "$bin_link"
    printf 'removed %s\n' "$bin_link"
  elif [[ -e "$bin_link" || -L "$bin_link" ]]; then
    say "kept $bin_link: it is not this installation's link"
  fi
  if [[ -e "$root" ]]; then
    rm -rf -- "$hosts" "$root/versions"
    rm -f -- "$root/current" "$root"/current.tmp.* "$root/.lock"
    if rmdir -- "$root" 2>/dev/null; then
      printf 'removed %s\n' "$root"
    else
      say "kept $root: it holds files this installer did not write (its versions, instance files and links removed)"
    fi
  fi
}

main "$@"
exit

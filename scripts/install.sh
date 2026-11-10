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
# Exit codes: 0 done; 1 ~/.local/bin/hoh is not this installation's; 64 usage, or a bundle whose bin/hoh is missing,
# does not run or names no version; 69 the user's systemd manager cannot do what was asked, something still runs this
# installation or its lock cannot be opened, locked or told (nothing removed), another install.sh of it holds the
# installer's lock, that lock cannot be made, opened or locked, or the installed binary does not run; 77 run as root;
# 78 a home, root or project systemd cannot use; 74 a command failed that the installer has no word of its own for
# (it is named, with its exit code and line). Refusals write nothing of the installation (those that run the bundle's
# binary leave the installers' lock file, and a run lock just remade). An uninstall that finds nothing installed says
# so and writes nothing.
set -Eeuo pipefail

say() { printf 'install.sh: %s\n' "$*" >&2; }
fail() {
  local code=$1
  shift
  say "$*"
  exit "$code"
}

# A failure the installer does not handle on its own is said, with its command, exit code and line, and ends it with
# 74, never with the command's own code (which may be one of the codes above) and no word (design correction 10,
# M2T14R-602, -701). In a command substitution it only ends that subshell; its caller then fails or handles it.
unexpected() {
  local rc=$1 line=$2 command=$3
  ((BASH_SUBSHELL == 0)) || exit "$rc"
  say "$command failed unexpectedly (exit $rc, line $line): nothing more was done"
  exit 74
}
trap 'unexpected "$?" "$LINENO" "$BASH_COMMAND"' ERR
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
    # Only an uninstall that finds nothing changes nothing; any other holds the installers' lock, and looks again
    # under it, before it acts (design correction 8, M2T14R-500): none acts outside the turns installers take.
    if ! installed; then
      printf 'not installed\n'
      return 0
    fi
    hold_operation
    remove
  else
    check_paths
    refusals "${projects[@]}"
    hold_operation
    install "${projects[@]}"
  fi
}

# The installer's own lock (docs/adr/ADR-023.md design corrections 5, 7 and 8): every install, and every uninstall
# that finds something to act on, holds $XDG_DATA_HOME/hoh.lock exclusive for the rest of its run, so installers of one installation take turns and none sees
# another's half-done work. A file beside the installation's root that no uninstall removes (its lock is never on a
# file that is gone), opened read-write (as an NFS client needs for an exclusive lock), which no run of hoh locks.
hold_operation() {
  local lock="$data/hoh.lock" said
  said="$(mkdir -p -- "$data" 2>&1)" || fail 69 "cannot make $data for the installer's lock: $said"
  if ! { command exec 8<>"$lock"; } 2>/dev/null; then
    fail 69 "cannot open the installer's lock $lock"
  fi
  # Waiting 60 s on another holder is told from a lock that cannot be had at all (M2T14R-401).
  take_lock -w 60 -x 8
  case "$lock_rc" in
    0) ;;
    75) fail 69 "$lock stayed locked for 60 s (another install.sh of this installation runs): try again when it ends" ;;
    *) fail 69 "cannot lock $lock (flock exit $lock_rc: ${lock_said:-no message})" ;;
  esac
}

# Lock with `flock "$@"` (its descriptor last). lock_rc is 0 when got, 75 when another holds it (flock's code for a
# conflict, asked for with -E 75), anything else a failure to lock at all, with flock's own message in lock_said: a
# failure is never said as another's hold (design correction 8; M2T14R-401, -453, -501).
take_lock() {
  lock_rc=0
  lock_said="$(flock -E 75 "$@" 2>&1)" || lock_rc=$?
}

# Whether anything of this installation is there: its root, its template or its command link (the installer never
# makes the root a link: a dangling one is no installation, M2T14R-654).
installed() { [[ -e "$root" || -e "$template" ]] || ours; }

# What refuses an install before it writes anything, even its lock (exit 1, 64 or 69; design correction 8).
refusals() {
  src="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
  [[ -x "$src/bin/hoh" ]] || fail 64 "$src/bin/hoh is missing: run the install.sh of a release bundle"
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
  # A root that cannot be resolved (a link to itself) cannot be named either (M2T14R-753).
  real_root="$(readlink -m -- "$root" 2>&1)" || fail 78 "the installation's root $root cannot be resolved: $real_root"
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
  local name semver sha real_root asked changed=0
  # An installed hoh (this script may be one's copy) runs only with its installation's lock: one missing from an
  # existing installation is made again before any of its binaries runs (M2T14R-352).
  [[ ! -d "$root" || -e "$root/.lock" ]] || : >"$root/.lock"
  # The bundle's binary, asked on its own: any failure, or no version named, refuses the bundle (design correction 9,
  # M2T14R-602). Then whether the unit can name this home and root, by the template's own rule, before anything of
  # the installation is written (M2T14R-601): both leave only the installers' lock file.
  asked="$("$src/bin/hoh" --version 2>&1)" || fail 64 "$src/bin/hoh does not run (--version: exit $?): $asked"
  semver="$(sed -n '1s/^hoh //p' <<<"$asked")"
  [[ "$semver" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail 64 "$src/bin/hoh --version does not name a version: $asked"
  real_root="$(readlink -m -- "$root" 2>&1)" || fail 78 "the installation's root $root cannot be resolved: $real_root"
  "$src/bin/hoh" __unit check "$HOME" "$root" "$real_root" ||
    fail "$?" "nothing of the installation was written: the systemd unit cannot take this home or root (said above)"
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
    local shown said
    if ((changed)); then
      said="$(systemctl --user daemon-reload 2>&1)" ||
        fail 69 "systemctl --user daemon-reload failed (exit $?): ${said:-no message}; the installed files are kept"
    fi
    shown="$(systemctl --user show -p LoadState,FragmentPath hoh-host@x.service 2>&1)" ||
      fail 69 "systemctl --user show hoh-host@x.service failed (exit $?): ${shown:-no message}; the installed files are kept"
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
    "$current/bin/hoh" __unit enable "$project" ||
      fail "$?" "$project's instance was not enabled (said above); the installed files are kept"
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

# Every link in the user's unit configuration that names an instance, in any directory (.wants, .requires, or another
# an [Install] setting makes): what enables an instance on disk (design correction 10, M2T14R-700).
enablement_links() {
  [[ -d "$config/systemd/user" ]] || return 0
  find "$config/systemd/user" -mindepth 2 -type l -name 'hoh-host@?*.service' -print0
}

# The installation's instances, one per line: those with a file under the root, those a link enables, those the
# manager has loaded.
instance_units() {
  local file unit
  local -A seen=()
  for file in "$hosts"/*.env; do
    [[ -e "$file" ]] || continue
    unit="$(basename -- "$file")"
    seen["hoh-host@${unit%.env}.service"]=1
  done
  while IFS= read -r -d '' file; do
    seen["$(basename -- "$file")"]=1
  done < <(enablement_links)
  while read -r unit _; do
    [[ "$unit" != hoh-host@*.service ]] || seen["$unit"]=1
  done < <(systemctl --user list-units --all --plain --no-legend 'hoh-host@*.service' 2>/dev/null || true)
  [[ ${#seen[@]} == 0 ]] || printf '%s\n' "${!seen[@]}"
}

# Stop and disable the installation's instances, each checked with the manager: inactive or failed, and not enabled by
# whatever link (`is-enabled`, design corrections 1, 9 and 10); 1 when one is left. `first` stops and disables each;
# `final`, under both locks, acts on one still enabled, which a run enabled again after the first pass. With no
# manager to ask or disable with, an instance a link still enables refuses the uninstall (M2T14R-151, -700).
settle_instances() {
  local pass=$1 unit active enabled link left=0
  if ! manager; then
    while IFS= read -r -d '' link; do
      say "$(basename -- "$link") is enabled, and no user systemd manager answers to disable it"
      left=1
    done < <(enablement_links)
    return "$left"
  fi
  while read -r unit; do
    [[ -n "$unit" ]] || continue
    if [[ "$pass" == final ]]; then
      enabled="$(systemctl --user is-enabled "$unit" 2>/dev/null || true)"
      [[ "$enabled" == enabled* ]] || continue
    fi
    active="" enabled=""
    if systemctl --user disable --now --quiet "$unit"; then
      active="$(systemctl --user show -p ActiveState --value "$unit" 2>/dev/null || true)"
      enabled="$(systemctl --user is-enabled "$unit" 2>/dev/null || true)"
    fi
    if [[ ("$active" == inactive || "$active" == failed) && -n "$enabled" && "$enabled" != enabled* ]]; then
      if [[ "$pass" == first ]]; then
        printf 'stopped and disabled %s\n' "$unit"
      else
        printf 'disabled %s: it was enabled again while the uninstall ran\n' "$unit"
      fi
    else
      say "could not stop and disable $unit (ActiveState=${active:-unknown}, is-enabled ${enabled:-unknown})"
      left=1
    fi
  done < <(instance_units)
  [[ "$pass" != first ]] || systemctl --user reset-failed 'hoh-host@*.service' 2>/dev/null || true
  return "$left"
}

remove() {
  local hosts="$root/hosts" failed=0
  if ! installed; then
    printf 'not installed\n'
    return 0
  fi
  # Only what this installer wrote is removed, all of it under its own root or at its fixed places; never a path
  # taken from the manager's configuration (docs/adr/ADR-023.md design correction 1, M2T14R-01).
  settle_instances first || failed=1
  # The installation's own lock, exclusive (design correction 2, M2T14R-100): every run of an installed hoh holds it
  # shared for its life, so it is had only once none runs, and while it is held none can start; what follows is
  # removed under it.
  local held=1 run_holds=0
  if [[ -e "$root/.lock" ]]; then
    # Read-write when it can be (an NFS client needs it for an exclusive lock, M2T14R-451), else read-only (a 0400 lock
    # is fine locally, M2T14R-351); a failed open is said, not taken for a running program (M2T14R-453).
    if { command exec 9<>"$root/.lock"; } 2>/dev/null || { command exec 9<"$root/.lock"; } 2>/dev/null; then
      take_lock -n -x 9
      case "$lock_rc" in
        0) ;;
        75)
          held=0
          run_holds=1
          ;;
        *)
          say "cannot lock this installation's lock $root/.lock (flock exit $lock_rc: ${lock_said:-no message})"
          held=0
          ;;
      esac
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
  # Under both locks no run can start, so none can enable an instance any more: one enabled again since the first
  # pass (an `__unit enable` that ended before the run lock was had) is disabled now, and what is seen now stays so
  # (design correction 9, M2T14R-600).
  if ((held && !running && !failed)); then
    settle_instances final || failed=1
  fi
  # The closing word follows the cause: something runs or stays enabled, or only the lock was not had (M2T14R-652).
  if ((failed || running || run_holds)); then
    fail 69 "nothing was removed: stop what is named above, then run install.sh --uninstall again"
  fi
  if ((!held)); then
    fail 69 "nothing was removed: this installation's lock could not be had or told (said above); run install.sh --uninstall again once it can be"
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

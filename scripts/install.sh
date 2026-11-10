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
# installation, its lock cannot be opened, locked or told, or what the removal rests on cannot be told (nothing removed:
# design corrections 11 and 12), ~/.local/bin/hoh cannot be read or told, `id -u` fails, another install.sh of it holds
# the installer's lock, that lock cannot be made, opened or locked, or the installed binary does not run; 77 run as root;
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

# End after hoh refused or failed: with its code when it is one of install.sh's own (64, 69, 78), else with `$1`, its
# code named; never a code install.sh gives another meaning (design correction 11, M2T14R-851).
refused_by() {
  local otherwise=$1 rc=$2
  shift 2
  case "$rc" in
    64 | 69 | 78) fail "$rc" "$*" ;;
    *) fail "$otherwise" "$* (exit $rc)" ;;
  esac
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
  local uid
  uid="$(id -u)" || fail 69 "cannot tell which user runs this (id -u failed)"
  if [[ "$uid" == 0 ]]; then
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

# Whether `$1` is there, links followed: 0 there, 1 not there (a link to nothing included: it was seen), 2 cannot
# tell. A test that answers "no" also when it cannot look (a directory on the way that cannot be searched) is not
# taken for "not there": a path is not there only when the nearest ancestor that is there can be searched, or is no
# directory (design correction 12; M2T14R-902, -903).
there() {
  local up=$1
  [[ ! -e "$up" ]] || return 0
  [[ ! -L "$up" ]] || return 1
  while [[ "$up" == */?* ]]; do
    up="${up%/*}"
    [[ -n "$up" ]] || up=/
    if [[ -e "$up" || -L "$up" ]]; then
      if [[ -d "$up" && ! -x "$up" ]]; then
        return 2
      fi
      return 1
    fi
  done
  return 1
}

# Whether anything of this installation is there: its root, its template or its command link (the installer never
# makes the root a link: a dangling one is no installation, M2T14R-654). A command link that cannot be read may be
# this installation's: it counts as there, and the uninstall then refuses under the lock (design correction 11).
installed() {
  local path rc
  for path in "$root" "$template"; do
    rc=0
    there "$path" || rc=$?
    ((rc == 1)) || return 0
  done
  rc=0
  link_state || rc=$?
  ((rc != 1))
}

# What refuses an install before it writes anything, even its lock (exit 1, 64 or 69; design correction 8).
refusals() {
  local self="${BASH_SOURCE[0]}" rc=0
  [[ "$self" == */* ]] && self="${self%/*}" || self=.
  src="$(cd -- "$self" && pwd -P)"
  [[ -x "$src/bin/hoh" ]] || fail 64 "$src/bin/hoh is missing: run the install.sh of a release bundle"
  link_state || rc=$?
  if ((rc == 2)); then
    fail 69 "nothing changed: $bin_link cannot be read, so whose it is cannot be told"
  elif ((rc == 1)) && [[ -e "$bin_link" || -L "$bin_link" ]]; then
    fail 1 "$bin_link exists and is not this installation's link to $current/bin/hoh: nothing changed"
  fi
  local project
  for project in "$@"; do
    [[ -d "$project" ]] || fail 64 "--project $project is not a directory: nothing changed"
  done
  if (($#)) && ! manager; then
    fail 69 "no user systemd manager answers (\$XDG_RUNTIME_DIR/systemd/private): cannot enable a project's host unit"
  fi
}

# Before anything is written: EnvironmentFile= takes a glob, ExecStart= no program path with quotes or a backslash
# (design corrections 1 and 2, M2T14R-101).
check_paths() {
  local real_root
  # A root that cannot be resolved (a link to itself, or to nothing) cannot be named either (M2T14R-753). Whether
  # `readlink -m` fails on a loop depends on the coreutils version, so a root that is a link and not a directory is
  # refused on its own.
  real_root="$(readlink -m -- "$root" 2>&1)" || fail 78 "the installation's root $root cannot be resolved: $real_root"
  if [[ -L "$root" && ! -d "$root" ]]; then
    fail 78 "the installation's root $root cannot be resolved: it is a link to itself or to nothing"
  fi
  refuse_chars "the installation's root" "$root" '*' '?' '[' '"' '\' "'"
  refuse_chars "the installation's root" "$real_root" '*' '?' '[' '"' '\' "'"
  refuse_chars "HOME" "$HOME" '"' '\' "'"
}

# Whether the user's systemd manager answers.
manager() { [[ -n "$runtime" && -S "$runtime/systemd/private" ]]; }

lingers() { [[ -f "/var/lib/systemd/linger/$user" ]]; }

# Whose ~/.local/bin/hoh is: 0 this installation's link, 1 not (absent, not a link, or a link elsewhere), 2 cannot be
# told (a link that cannot be read, said). What cannot be read is never taken for another's (design correction 11,
# M2T14R-804).
link_state() {
  local target rc=0
  there "$bin_link" || rc=$?
  if ((rc == 2)); then
    say "cannot tell whether $bin_link is there: a directory on its way cannot be searched"
    return 2
  fi
  # Not there, or there as no link, is not this installation's; a link, even to nothing, is read.
  [[ -L "$bin_link" ]] || return 1
  if ! target="$(readlink -- "$bin_link")"; then
    say "cannot read the link $bin_link"
    return 2
  fi
  [[ "$target" == "$current/bin/hoh" ]]
}

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
    refused_by 64 "$?" "nothing of the installation was written: $src/bin/hoh __unit check refused this home or root, or failed (said above)"
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
  text="$("$current/bin/hoh" __unit template && printf x)" ||
    refused_by 69 "$?" "the installed $current/bin/hoh __unit template refused or does not run (said above); the installed files are kept"
  old="$({ [[ ! -f "$template" ]] || cat -- "$template"; } && printf x)"
  if [[ "$text" != "$old" ]]; then
    mkdir -p -- "${template%/*}"
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
      refused_by 69 "$?" "$project's instance was not enabled (said above); the installed files are kept"
  done
  # The installed command must run: a failure here is the install's, not hidden by the message (M2T14R-301).
  # Its words are made here, with no command whose failure printf would hide (design correction 11, M2T14R-805).
  local shown
  local -a lines=()
  shown="$("$current/bin/hoh" --version)" || fail 69 "the installed $current/bin/hoh does not run"
  mapfile -t lines <<<"$shown"
  printf 'installed %s %s in %s\n' "${lines[0]-}" "${lines[1]-}" "$root/versions/$name"
  printf 'command: %s\n' "$bin_link"
  if lingers; then
    printf 'linger: yes\n'
  else
    printf 'linger: no (a host unit would stop with your last session; install.sh --linger asks for it)\n'
  fi
}

# The names of the links that enable an instance on disk, in the user's unit configuration and, for this boot, its
# runtime directory: the entries named hoh-host@<…>.service in each directory one level down (.wants, .requires, or
# another an [Install] setting makes), which is where systemd reads them, directories that are links followed and no
# link's target looked at (design corrections 10 to 12; M2T14R-700, -802, -803, -950). Into `enabling`; 1 when a
# directory there cannot be read or searched (said): what cannot be listed may enable an instance (M2T14R-801).
enabling_links() {
  local dir sub link rc
  enabling=()
  for dir in "$config/systemd/user" ${runtime:+"$runtime/systemd/user"}; do
    rc=0
    there "$dir" || rc=$?
    ((rc != 1)) || continue
    if ((rc == 2)); then
      say "cannot tell whether $dir is there, where a link may enable an instance"
      return 1
    fi
    if ! [[ -d "$dir" && -r "$dir" && -x "$dir" ]]; then
      say "cannot list $dir, where a link may enable an instance"
      return 1
    fi
    for sub in "$dir"/*; do
      [[ -d "$sub" ]] || continue
      if ! [[ -r "$sub" && -x "$sub" ]]; then
        say "cannot list $sub, where a link may enable an instance"
        return 1
      fi
      for link in "$sub"/hoh-host@?*.service; do
        [[ -e "$link" || -L "$link" ]] || continue
        enabling+=("${link##*/}")
      done
    done
  done
}

# The installation's instances, into `instances`: those with a file under the root, those a link enables, those the
# manager has loaded; 1 when one of these cannot be listed (said).
instance_units() {
  local file unit listed rc
  local -A seen=()
  instances=()
  rc=0
  there "$hosts" || rc=$?
  if ((rc == 2)) || { ((rc == 0)) && ! [[ -d "$hosts" && -r "$hosts" && -x "$hosts" ]]; }; then
    say "cannot list $hosts, the instances' files"
    return 1
  fi
  for file in "$hosts"/*.env; do
    [[ -e "$file" ]] || continue
    unit="${file##*/}"
    seen["hoh-host@${unit%.env}.service"]=1
  done
  enabling_links || return 1
  for unit in "${enabling[@]}"; do
    seen["$unit"]=1
  done
  if ! listed="$(systemctl --user list-units --all --plain --no-legend 'hoh-host@*.service')"; then
    say "the user's systemd manager did not list its hoh-host@ instances"
    return 1
  fi
  while read -r unit _; do
    [[ "$unit" != hoh-host@*.service ]] || seen["$unit"]=1
  done <<<"$listed"
  instances=("${!seen[@]}")
}

# The manager's word on whether `unit` is enabled, into `enabled`: 0 enabled, 1 not (a state that enables nothing), 2
# no word to tell by (no answer, or one that is neither; design correction 11, M2T14R-800).
enablement_of() {
  local rc=0
  enabled="$(systemctl --user is-enabled "$1" 2>/dev/null)" || rc=$?
  # An answer is taken only with the exit code that state gives (M2T14R-900). "masked" is not "not enabled": a masked
  # instance's own links may still be there, unseen (M2T14R-901).
  case "$enabled:$rc" in
    enabled:0 | enabled-runtime:0) return 0 ;;
    disabled:1 | static:0 | indirect:0 | generated:0 | transient:0 | not-found:4) return 1 ;;
    *) return 2 ;;
  esac
}

# Stop and disable the installation's instances, each checked with the manager: inactive or failed, and not enabled by
# whatever link (`is-enabled`, design corrections 1, 9, 10 and 11); 1 when one is left, else 2 when they cannot be
# listed or one's state cannot be told after its disable (said; M2T14R-952). `first`
# stops and disables each; `final`, under both locks, acts on any the manager does not say is not enabled, which a run
# enabled again after the first pass. With no manager to ask or disable with, an instance a link enables refuses the
# uninstall (M2T14R-151, -700), and so does a directory that cannot be listed.
settle_instances() {
  local pass=$1 unit active enabled state left=0 untold=0
  if ! manager; then
    enabling_links || return 2
    for unit in "${enabling[@]}"; do
      say "$unit is enabled, and no user systemd manager answers to disable it"
      left=1
    done
    return "$left"
  fi
  instance_units || return 2
  for unit in "${instances[@]}"; do
    if [[ "$pass" == final ]]; then
      state=0
      enablement_of "$unit" || state=$?
      ((state != 1)) || continue
    fi
    active="" enabled="" state=2
    if systemctl --user disable --now --quiet "$unit"; then
      active="$(systemctl --user show -p ActiveState --value "$unit" 2>/dev/null)" || active=""
      state=0
      enablement_of "$unit" || state=$?
    fi
    if [[ "$active" == inactive || "$active" == failed ]] && ((state == 1)); then
      if [[ "$pass" == first ]]; then
        printf 'stopped and disabled %s\n' "$unit"
      else
        printf 'disabled %s: it was enabled again while the uninstall ran\n' "$unit"
      fi
    elif [[ "$active" == inactive || "$active" == failed ]] && ((state == 2)); then
      say "cannot tell whether $unit is still enabled after its disable (is-enabled ${enabled:-no answer})"
      untold=1
    else
      say "could not stop and disable $unit (ActiveState=${active:-unknown}, is-enabled ${enabled:-no answer})"
      left=1
    fi
  done
  [[ "$pass" != first ]] || systemctl --user reset-failed 'hoh-host@*.service' 2>/dev/null || true
  ((left)) && return 1
  ((untold)) && return 2
  return 0
}

remove() {
  local hosts="$root/hosts" failed=0 unsure=0 link=0 settled=0
  if ! installed; then
    printf 'not installed\n'
    return 0
  fi
  # Whose the command link is, told once under the lock; one that cannot be read refuses (M2T14R-804).
  link_state || link=$?
  ((link != 2)) || unsure=1
  # Only what this installer wrote is removed, all of it under its own root or at its fixed places; never a path
  # taken from the manager's configuration (docs/adr/ADR-023.md design correction 1, M2T14R-01).
  settle_instances first || settled=$?
  ((settled != 1)) || failed=1
  ((settled != 2)) || unsure=1
  # The installation's own lock, exclusive (design correction 2, M2T14R-100): every run of an installed hoh holds it
  # shared for its life, so it is had only once none runs, and while it is held none can start; what follows is
  # removed under it.
  local held=1 run_holds=0 lock_there=0
  there "$root/.lock" || lock_there=$?
  if ((lock_there == 2)); then
    say "cannot tell whether this installation's lock $root/.lock is there"
    unsure=1
  fi
  if ((lock_there == 0)); then
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
  # The root as /proc names the programs' paths; one that cannot be resolved cannot be scanned for (M2T14R-951). A
  # program's path that cannot be read below is a process that ended, or another user's, which the scan cannot see
  # (that one, once it holds the run lock, refuses the uninstall above).
  if ! real="$(readlink -f -- "$root")"; then
    say "cannot resolve $root to look for programs that run from it"
    unsure=1
    real="$root"
  fi
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
  if ((held && !running && !failed && !unsure)); then
    settled=0
    settle_instances final || settled=$?
    ((settled != 1)) || failed=1
    ((settled != 2)) || unsure=1
  fi
  # The closing word follows the cause: something runs or stays enabled, something could not be told (design correction
  # 11), or only the lock was not had (M2T14R-652).
  # What is removed is told before anything is (design correction 12): a template or root that cannot be told refuses.
  local template_there=0 root_there=0
  there "$template" || template_there=$?
  there "$root" || root_there=$?
  if ((template_there == 2 || root_there == 2)); then
    say "cannot tell whether $template or $root is there"
    unsure=1
  fi
  if ((failed || running || run_holds)); then
    fail 69 "nothing was removed: stop what is named above, then run install.sh --uninstall again"
  fi
  if ((unsure)); then
    fail 69 "nothing was removed: what is said above could not be told; run install.sh --uninstall again once it can be"
  fi
  if ((!held)); then
    fail 69 "nothing was removed: this installation's lock could not be had or told (said above); run install.sh --uninstall again once it can be"
  fi
  # Each removal is said as it happened (M2T14R-150).
  if ((template_there == 0)); then
    rm -f -- "$template"
    printf 'removed %s\n' "$template"
  fi
  if manager; then systemctl --user daemon-reload || say "systemctl --user daemon-reload failed"; fi
  [[ -z "$runtime" ]] || rm -f -- "$runtime"/hoh-host@*.stderr
  if ((link == 0)); then
    rm -f -- "$bin_link"
    printf 'removed %s\n' "$bin_link"
  elif [[ -e "$bin_link" || -L "$bin_link" ]]; then
    say "kept $bin_link: it is not this installation's link"
  fi
  if ((root_there == 0)); then
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

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
# design corrections 11 to 14), an uninstall's daemon-reload fails once the template is removed (the rest removed and
# each removal said, design correction 14), ~/.local/bin/hoh cannot be read or told, `id -u` fails, another install.sh
# of it holds the installer's lock, that lock cannot be made, opened or locked, or the installed binary does not run; 77
# run as root; 78 a home, root or project systemd cannot use; 74 the emptied root cannot be removed (said with rmdir's
# words, the rest removed), or a command failed that the installer has no word of its own for (it is named, with its
# exit code and line). Refusals write nothing of the installation (those that run the
# bundle's binary leave the installers' lock file, and a run lock just remade). An uninstall that finds nothing
# installed says so and writes nothing.
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
  # The XDG base directories: the variable when it holds an absolute path, else the default (the XDG spec ignores
  # relative ones), taken as they are: every path is carried byte for byte, none through a command substitution that
  # would drop a trailing newline (design correction 14, M2T14R-1100).
  data=${XDG_DATA_HOME:-}
  [[ "$data" == /* ]] || data="$HOME/.local/share"
  config=${XDG_CONFIG_HOME:-}
  [[ "$config" == /* ]] || config="$HOME/.config"
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

# Whether `$1` is there, links followed: 0 there, 1 not there, 2 cannot tell. "Not there" is said only from a sight
# (design corrections 12 and 13; M2T14R-902, -903, -1000, -1050): a name a searchable directory does not hold,
# something on the way that is no directory, or more links than the kernel follows (40: none, systemd included, can
# reach through them). Bash's tests answer "no" also when they cannot look, so a link they cannot follow is followed
# here, one at a time: its target read and asked again (a link to nothing is not there, it was seen; a target there,
# where the kernel's look at the path did not reach, cannot be told: design correction 14). `$2` is the number of
# links followed so far.
there() {
  local path=$1 hops=${2:-0} target dir rc
  [[ ! -e "$path" ]] || return 0
  [[ -L "$path" ]] || {
    way "$path" "$hops"
    return
  }
  ((hops < 40)) || return 1
  target="$(readlink -- "$path" && printf x)" || return 2
  target="${target%$'\n'x}"
  if [[ "$target" != /* ]]; then
    dir=.
    [[ "$path" != */* ]] || dir="${path%/*}"
    target="${dir:-/}/$target"
  fi
  rc=0
  there "$target" $((hops + 1)) || rc=$?
  # Reached by following it where the kernel's own look at the path did not reach: a change meanwhile, or more links
  # on the whole way than the kernel follows in one look (40; each look here starts afresh), which bash's tests cannot
  # tell apart (design correction 14, M2T14R-1151).
  ((rc != 0)) || return 2
  return "$rc"
}

# For `$1`, seen neither as there nor as a link: 1 when the nearest part of its way that is there is a searchable
# directory or no directory, 2 when it is a directory that cannot be searched. A link on the way that cannot be followed
# is followed by `there`: what it reaches decides (`$2` links followed so far).
way() {
  local up=$1 hops=$2 rc
  while [[ "$up" == */?* ]]; do
    up="${up%/*}"
    [[ -n "$up" ]] || up=/
    if [[ -e "$up" ]]; then
      if [[ -d "$up" && ! -x "$up" ]]; then
        return 2
      fi
      return 1
    fi
    if [[ -L "$up" ]]; then
      rc=0
      there "$up" "$hops" || rc=$?
      # There now, when it was not a moment ago: what is under it cannot be told from this look.
      ((rc != 0)) || return 2
      return "$rc"
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
  src="$(cd -- "$self" && pwd -P && printf x)"
  src="${src%$'\n'x}"
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
  real_root="$(readlink -m -- "$root" 2>&1 && printf x)" || fail 78 "the installation's root $root cannot be resolved: $real_root"
  real_root="${real_root%$'\n'x}"
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
    say "cannot tell whether $bin_link is there: a directory on its way, or on the way of what it links to, cannot be searched"
    return 2
  fi
  # Not there, or there as no link, is not this installation's; a link, even to nothing, is read.
  [[ -L "$bin_link" ]] || return 1
  # Its target byte for byte: one ending with a newline is not this installation's (M2T14R-1101).
  if ! target="$(readlink -- "$bin_link" && printf x)"; then
    say "cannot read the link $bin_link"
    return 2
  fi
  target="${target%$'\n'x}"
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
  local target=$1 at=$2 now
  if [[ -L "$at" ]] && now="$(readlink -- "$at" && printf x)" && [[ "${now%$'\n'x}" == "$target" ]]; then
    return 0
  fi
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
  real_root="$(readlink -m -- "$root" 2>&1 && printf x)" || fail 78 "the installation's root $root cannot be resolved: $real_root"
  real_root="${real_root%$'\n'x}"
  "$src/bin/hoh" __unit check "$HOME" "$root" "$real_root" ||
    refused_by 64 "$?" "nothing of the installation was written: $src/bin/hoh __unit check refused this home or root, or failed (said above)"
  # Read from its input: given a name, sha256sum escapes one with a newline or a backslash (M2T14R-1250).
  sha="$(sha256sum <"$src/bin/hoh" | cut -c1-12)"
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
# runtime directory: the entries named hoh-host@<…>.service in each .wants, .requires or .upholds directory one level
# down, the only ones WantedBy=, RequiredBy= and UpheldBy= make links in (systemd.unit(5)), names that begin with a dot
# included (a unit's may); another unit's .d drop-ins hold none (M2T14R-1001). Directories that are links are followed,
# no link's target in them looked at (design corrections 10 to 13; M2T14R-700, -802, -803, -950). Into `enabling`; 1
# when a directory there cannot be read or searched, or whether one is a directory cannot be told (said): what cannot
# be listed may enable an instance (M2T14R-801, -1000).
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
    # There and no directory, it holds no link (M2T14R-1103); a directory that cannot be listed may.
    [[ -d "$dir" ]] || continue
    if ! [[ -r "$dir" && -x "$dir" ]]; then
      say "cannot list $dir, where a link may enable an instance"
      return 1
    fi
    for sub in "$dir"/{,.}*.{wants,requires,upholds}; do
      # A pattern that matched nothing stays as it is, and is no entry.
      [[ -e "$sub" || -L "$sub" ]] || continue
      if ! [[ -d "$sub" ]]; then
        rc=0
        there "$sub" || rc=$?
        if ((rc == 2)); then
          say "cannot tell whether $sub is a directory, where a link may enable an instance"
          return 1
        fi
        continue
      fi
      if ! [[ -r "$sub" && -x "$sub" ]]; then
        say "cannot list $sub, where a link may enable an instance"
        return 1
      fi
      for link in "$sub"/hoh-host@?*.service; do
        # Only a symbolic link enables (systemd ignores any other file there, "is not a symlink"; M2T14R-1203).
        [[ -L "$link" ]] || continue
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
  if ((rc == 2)) || { ((rc == 0)) && [[ -d "$hosts" ]] && ! [[ -r "$hosts" && -x "$hosts" ]]; }; then
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

# Whether the manager says `$1` has stopped, its words into `active` and `job`: 0 stopped (ActiveState inactive or
# failed, and no job pending), 1 not (another state, or a job pending: a start a run asked for goes on in the manager
# after the run has ended, M2T14R-1200), 2 no answer to tell by (design corrections 13 and 15, M2T14R-1051). The
# properties are read as `Key=Value` lines, whatever order the manager gives them in.
stopped_of() {
  local shown line have_active=0 have_job=0
  active="" job=""
  shown="$(systemctl --user show -p ActiveState -p Job "$1" 2>/dev/null)" || return 2
  while IFS= read -r line; do
    case "$line" in
      ActiveState=*) active=${line#ActiveState=} have_active=1 ;;
      Job=*) job=${line#Job=} have_job=1 ;;
    esac
  done <<<"$shown"
  if ((!have_active || !have_job)) || [[ -z "$active" ]]; then
    return 2
  fi
  [[ -z "$job" ]] || return 1
  case "$active" in
    inactive | failed) return 0 ;;
    *) return 1 ;;
  esac
}

# Stop and disable the installation's instances, each checked with the manager: inactive or failed with no job pending,
# and not enabled by whatever link (`is-enabled`, design corrections 1, 9, 10, 11 and 15); 1 when one is left, else 2
# when they cannot be listed or one's state cannot be told after its disable (said; M2T14R-952). `first` stops and
# disables each; `final`, under both locks, acts on any the manager does not say is both not enabled and stopped, which
# a run enabled or started again after the first pass. With no manager to ask or disable with, an instance a link
# enables refuses the uninstall (M2T14R-151, -700), and so does a directory that cannot be listed.
settle_instances() {
  local pass=$1 unit active job enabled state stopped disabled again left=0 untold=0
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
    again=""
    if [[ "$pass" == final ]]; then
      # Settled already (not enabled, stopped, no job) is left as it is. A run since the first pass may have enabled
      # it again, or started it: its start job goes on after the run ends (design correction 15, M2T14R-1200).
      state=0
      enablement_of "$unit" || state=$?
      stopped=0
      stopped_of "$unit" || stopped=$?
      ((state != 1 || stopped != 0)) || continue
      # What is said is what was seen (M2T14R-1352).
      if ((state == 0)); then
        again=enabled
      elif ((stopped == 1)); then
        again=started
      else
        again=untold
      fi
    fi
    active="" job="" enabled="" state=2 stopped=2 disabled=0
    if systemctl --user disable --now --quiet "$unit"; then
      disabled=1
      stopped=0
      stopped_of "$unit" || stopped=$?
      state=0
      enablement_of "$unit" || state=$?
    fi
    if ((disabled && stopped == 0 && state == 1)); then
      if [[ "$pass" == first ]]; then
        printf 'stopped and disabled %s\n' "$unit"
      elif [[ "$again" == enabled ]]; then
        printf 'disabled %s: it was enabled again while the uninstall ran\n' "$unit"
      elif [[ "$again" == started ]]; then
        printf 'stopped %s: it was started again while the uninstall ran\n' "$unit"
      else
        printf 'stopped and disabled %s: its state could not be told while the uninstall ran\n' "$unit"
      fi
    elif ((disabled && stopped == 0 && state == 2)); then
      say "cannot tell whether $unit is still enabled after its disable (is-enabled ${enabled:-no answer})"
      untold=1
    elif ((disabled && stopped == 2 && state != 0)); then
      say "cannot tell whether $unit stopped after its disable (ActiveState: no answer, is-enabled ${enabled:-no answer})"
      untold=1
    else
      say "could not stop and disable $unit (ActiveState=${active:-unknown}${job:+, job $job pending}, is-enabled ${enabled:-no answer})"
      left=1
    fi
  done
  [[ "$pass" != first ]] || systemctl --user reset-failed 'hoh-host@*.service' 2>/dev/null || true
  ((left)) && return 1
  ((untold)) && return 2
  return 0
}

# Whether the kernel's reason a program's path could not be read (`readlink -v` in the C locale, its last words) makes
# the process nothing the scan has to tell (design correction 16; M2T14R-1201, -1350, -1351): "No such file or
# directory", it has no address space (gone, a zombie, one exiting, a kernel thread), so it runs no program; "Permission
# denied" or "Operation not permitted", the kernel denies this user a look (another user's, under /proc's hidepid too;
# one made undumpable; one with credentials beyond a plain process of this user, such as the user's own systemd
# manager), which no run of this installation is, its binary carrying no capability. The run lock covers any run once
# it holds it. Any other reason cannot be told.
nothing_to_tell() {
  case "$1" in
    *": No such file or directory" | *": Permission denied" | *": Operation not permitted") return 0 ;;
  esac
  return 1
}

# Whether the directory `$1` still holds an entry, names beginning with a dot included.
holds_entries() {
  local entry
  for entry in "$1"/* "$1"/.[!.]* "$1"/..?*; do
    if [[ -e "$entry" || -L "$entry" ]]; then
      return 0
    fi
  done
  return 1
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
  local real proc exe running=0 root_there=0
  # The root as /proc names the programs' paths, physical ones: resolved whether or not it, or any part of its way, is
  # there (`readlink -m`: a program run through a link to a directory removed since still shows the path the link
  # names, M2T14R-1150, -1251). One that cannot be resolved is looked for by its own path when it is not there (a link
  # to nothing, M2T14R-1052; nothing under it is removed), and cannot be told when it is (M2T14R-951; design
  # corrections 13 to 15). A link on the way removed or pointed elsewhere, or a directory on the way moved, while a
  # program ran takes with it the only path to that program: what the uninstall then removes by the paths that are
  # there is nothing that program uses. A program's path that cannot be read is judged by the kernel's reason
  # (`nothing_to_tell`, design correction 16).
  there "$root" || root_there=$?
  if real="$(readlink -m -- "$root" && printf x)"; then
    real="${real%$'\n'x}"
  elif ((root_there == 1)); then
    real="$root"
  else
    say "cannot resolve $root to look for programs that run from it"
    unsure=1
    real="$root"
  fi
  for proc in /proc/[0-9]*; do
    # Its program's path, or the kernel's reason it cannot be read (one look, its words in the C locale).
    if exe="$(LC_ALL=C readlink -v -- "$proc/exe" 2>&1)"; then
      if [[ "$exe" == "$real"/* || "$exe" == "$root"/* ]]; then
        say "still running from $root: pid ${proc#/proc/}: $(tr '\0' ' ' <"$proc/cmdline" 2>/dev/null || true)"
        running=1
      fi
    elif ! nothing_to_tell "$exe"; then
      say "cannot tell what process ${proc#/proc/} runs: its program's path cannot be read (${exe:-no reason given})"
      unsure=1
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
  local template_there=0
  there "$template" || template_there=$?
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
  # A reload that fails leaves the removed template loaded in the manager: the rest is removed all the same, and the
  # uninstall ends with 69, saying so (M2T14R-1102).
  local reload_rc=0 reload_said="" said not_removed=""
  if manager; then
    reload_said="$(systemctl --user daemon-reload 2>&1)" || reload_rc=$?
  fi
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
    # Kept when something is still in it; any other failure to remove it is said as it is (M2T14R-1202).
    if said="$(rmdir -- "$root" 2>&1)"; then
      printf 'removed %s\n' "$root"
    elif holds_entries "$root"; then
      say "kept $root: it holds files this installer did not write (its versions, instance files and links removed)"
    else
      not_removed="cannot remove $root: ${said:-no message}"
    fi
  fi
  if [[ -n "$not_removed" ]]; then
    ((reload_rc == 0)) || say "systemctl --user daemon-reload failed (exit $reload_rc): ${reload_said:-no message}; run systemctl --user daemon-reload"
    fail 74 "$not_removed; what is said above as removed is removed"
  fi
  if ((reload_rc)); then
    fail 69 "systemctl --user daemon-reload failed (exit $reload_rc): ${reload_said:-no message}; what is said above as removed is removed, but the manager keeps the removed template loaded until it reloads: run systemctl --user daemon-reload"
  fi
}

main "$@"
exit

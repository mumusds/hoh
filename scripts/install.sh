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
#       Stop and disable every hoh-host@ instance, remove the template, the instances' files, ~/.local/bin/hoh and
#       $XDG_DATA_HOME/hoh. Projects' .hoh/, drop-ins, ~/.config/hoh and lingering are left as they are.
#
# Exit codes: 0 done; 1 ~/.local/bin/hoh is not this installation's; 64 usage; 69 the user's systemd manager cannot do
# what was asked; 77 run as root; 78 a home or project systemd cannot use.
set -Eeuo pipefail

say() { printf 'install.sh: %s\n' "$*" >&2; }
fail() {
  local code=$1
  shift
  say "$*"
  exit "$code"
}
usage() {
  sed -n '5,13p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
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
  state="$(xdg "${XDG_STATE_HOME:-}" "$HOME/.local/state")"
  config="$(xdg "${XDG_CONFIG_HOME:-}" "$HOME/.config")"
  runtime="${XDG_RUNTIME_DIR:-}"
  root="$data/hoh"
  current="$root/current"
  bin_link="$HOME/.local/bin/hoh"
  template="$data/systemd/user/hoh-host@.service"
  override="$config/systemd/user/hoh-host@.service"
  user="${USER:-$(id -un)}"
  if ((uninstall)); then remove; else install "${projects[@]}"; fi
}

# Whether the user's systemd manager answers.
manager() { [[ -n "$runtime" && -S "$runtime/systemd/private" ]]; }

lingers() { [[ -f "/var/lib/systemd/linger/$user" ]]; }

# Whether ~/.local/bin/hoh is this installation's link.
ours() { [[ -L "$bin_link" && "$(readlink -- "$bin_link")" == "$current/bin/hoh" ]]; }

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
  if [[ -e "$bin_link" || -L "$bin_link" ]] && ! ours; then
    fail 1 "$bin_link exists and is not this installation's link to $current/bin/hoh: nothing changed"
  fi
  if (($#)) && ! manager; then
    fail 69 "no user systemd manager answers (\$XDG_RUNTIME_DIR/systemd/private): cannot enable a project's host unit"
  fi
  semver="$("$src/bin/hoh" --version | sed -n '1s/^hoh //p')"
  [[ "$semver" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail 64 "$src/bin/hoh --version does not name a version"
  sha="$(sha256sum -- "$src/bin/hoh" | cut -c1-12)"
  name="$semver-$sha"
  # A version directory is named by its binary and never changed once there: a host running from it, and the agents
  # it starts through its start gate, keep using it until they restart.
  mkdir -p -- "$root/versions"
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
  printf 'installed %s in %s\n' "$("$current/bin/hoh" --version | head -2 | tr '\n' ' ')" "$root/versions/$name"
  printf 'command: %s\n' "$bin_link"
  if lingers; then
    printf 'linger: yes\n'
  else
    printf 'linger: no (a host unit would stop with your last session; install.sh --linger asks for it)\n'
  fi
}

remove() {
  local hosts="$state/hoh/hosts" shown file
  if [[ ! -e "$root" && ! -e "$template" && ! -e "$hosts" ]] && ! ours; then
    printf 'not installed\n'
    return 0
  fi
  if manager; then
    # Where the manager reads the instances' files (decision 2), while it still knows the template.
    shown="$(systemctl --user show -p EnvironmentFiles hoh-host@x.service 2>/dev/null || true)"
    file="$(sed -n 's/^EnvironmentFiles=\(\/.*\) (ignore_errors=no)$/\1/p' <<<"$shown")"
    [[ -z "$file" ]] || hosts="$(dirname -- "$file")"
    local -A instances=()
    local unit
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
      systemctl --user disable --now --quiet "$unit" || say "could not stop and disable $unit"
      printf 'stopped and disabled %s\n' "$unit"
    done
    systemctl --user reset-failed 'hoh-host@*.service' 2>/dev/null || true
  fi
  rm -f -- "$template"
  if manager; then systemctl --user daemon-reload || say "systemctl --user daemon-reload failed"; fi
  rm -rf -- "$hosts"
  rmdir -- "$(dirname -- "$hosts")" 2>/dev/null || true
  [[ -z "$runtime" ]] || rm -f -- "$runtime"/hoh-host@*.stderr
  if ours; then rm -f -- "$bin_link"; fi
  rm -rf -- "$root"
  printf 'removed %s, %s and %s\n' "$root" "$template" "$bin_link"
  # A host not started by its unit is not stopped here; it is named so its user can.
  local proc args
  for proc in /proc/[0-9]*; do
    args="$(tr '\0' ' ' <"$proc/cmdline" 2>/dev/null || true)"
    if [[ "$args" == *"/hoh __host "* && "$(stat -c %u "$proc" 2>/dev/null)" == "$(id -u)" ]]; then
      say "note: a host not run by its unit is still running (pid ${proc#/proc/}): $args"
    fi
  done
}

main "$@"
exit

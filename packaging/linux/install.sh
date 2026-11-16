#!/usr/bin/env bash
# Installs this ngteams release for you alone, with no root: the release's folder goes to ~/.local/opt/ngteams, with
# a menu entry (which also opens msteams: links), the icon, `ngteams` and `ngteamsctl` in ~/.local/bin, and the
# shell completions. That copy is yours, so it updates itself with no password (crates/teams-update).
#
# Run it from the unpacked .tar.gz (`ngteams/install.sh`), from an extracted AppImage, or from a system package's
# /opt/ngteams: each becomes the same per-user copy. Run it again to write the menu entry and links again.
#
# Usage: install.sh               install, or replace the per-user copy with this release
#        install.sh --uninstall   remove the per-user copy and its entries; your accounts and data stay
set -euo pipefail

app_id=com.nickalie.ngteams
here="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")" && pwd)"
opt="$HOME/.local/opt"
target="$opt/ngteams"
bin="$HOME/.local/bin"
share="$HOME/.local/share"
completions=(
  bash-completion/completions/ngteamsctl
  fish/vendor_completions.d/ngteamsctl.fish
  zsh/site-functions/_ngteamsctl
)

die() {
  echo "error: $*" >&2
  exit 1
}

# Whether $1 is an ngteams release folder: it has the marker the updater reads.
release_folder() {
  [[ -f "$1/usr/share/ngteams/update-format" && -f "$1/usr/share/ngteams/release" && -x "$1/usr/bin/ngteams" ]]
}

# Removes the link $1 only when it points into the per-user copy.
unlink_ours() {
  if [[ -L "$1" && "$(readlink "$1")" == "$target/"* ]]; then
    rm -f "$1"
  fi
}

uninstall() {
  unlink_ours "$bin/ngteams"
  unlink_ours "$bin/ngteamsctl"
  for completion in "${completions[@]}"; do
    unlink_ours "$share/$completion"
  done
  # the menu entry and the icons only when the entry is this copy's: another build (scripts/install.sh) writes the same
  # names, and its entry starts that build
  local entry="$share/applications/$app_id.desktop"
  if [[ -e "$entry" ]] && ! grep -qF "Exec=\"$target/AppRun\" " "$entry"; then
    echo "The menu entry $entry starts another ngteams build, so it and the icons stay."
  else
    rm -f "$entry"
    remove_icons
  fi
  if [[ -d "$target" ]]; then
    release_folder "$target" || die "$target is not an ngteams release, so it was left alone"
    rm -rf "${target:?}"
  fi
  echo "ngteams is removed. Your accounts and data in $share/ngteams and your keyring stay."
}

# Removes the icons that this copy installed (the names its hicolor folder holds).
remove_icons() {
  if [[ -d "$target/usr/share/icons/hicolor" ]]; then
    (cd "$target/usr/share/icons/hicolor" && find . -type f -name "$app_id.*") | while read -r icon; do
      rm -f "$share/icons/hicolor/$icon"
    done
  fi
}

# Copies this release to the per-user folder. The copy is made beside it and put in place by renames, so a failure
# leaves the earlier copy as it was.
copy_release() {
  if [[ -e "$target" ]] && ! release_folder "$target"; then
    die "$target is not an ngteams release, so it was left alone. Move it away, then run this again."
  fi
  mkdir -p "$opt"
  staging="$(mktemp -d "$opt/.ngteams-install-XXXXXX")"
  trap 'rm -rf "${staging:?}"' EXIT
  cp -a "$here/." "$staging/"
  # an AppImage's or a system package's tree is a per-user archive here
  printf 'linux-archive\n' > "$staging/usr/share/ngteams/update-format"
  if [[ -e "$target" ]]; then
    local previous="$opt/.ngteams-previous-$$"
    mv "$target" "$previous"
    if ! mv "$staging" "$target"; then
      mv "$previous" "$target"
      die "the new release could not be put in $target; the earlier copy is unchanged"
    fi
    rm -rf "${previous:?}"
  else
    mv "$staging" "$target"
  fi
  trap - EXIT
}

install_entries() {
  mkdir -p "$bin" "$share/applications" "$share/icons"
  ln -sfn "$target/usr/bin/ngteams" "$bin/ngteams"
  ln -sfn "$target/usr/bin/ngteamsctl" "$bin/ngteamsctl"
  for completion in "${completions[@]}"; do
    if [[ -f "$target/usr/share/$completion" ]]; then
      mkdir -p "$(dirname "$share/$completion")"
      ln -sfn "$target/usr/share/$completion" "$share/$completion"
    fi
  done
  cp -R "$target/usr/share/icons/hicolor" "$share/icons/"
  # the menu entry starts this copy wherever ~/.local/bin is not on the desktop's PATH
  sed "s|^Exec=ngteams |Exec=\"$target/AppRun\" |" "$target/usr/share/applications/$app_id.desktop" \
    > "$share/applications/$app_id.desktop"
  if command -v update-desktop-database > /dev/null 2>&1; then
    update-desktop-database "$share/applications" > /dev/null 2>&1 || true
  fi
}

[[ "$(id -u)" != 0 ]] || die "run this as your own user, not as root: it installs ngteams for you alone"
case "${1:-}" in
  --uninstall)
    uninstall
    exit 0
    ;;
  "") ;;
  *) die "usage: install.sh [--uninstall]" ;;
esac

release_folder "$here" || die "$here is not an unpacked ngteams release"
if [[ "$here" != "$target" ]]; then
  copy_release
else
  printf 'linux-archive\n' > "$target/usr/share/ngteams/update-format"
fi
install_entries

echo "ngteams $(head -n 1 "$target/usr/share/ngteams/release") is installed in $target."
echo "It updates itself with no password. Start it from your app menu, or run ngteams."
if [[ "$here" != /opt/ngteams && -f /opt/ngteams/usr/share/ngteams/update-format ]]; then
  echo "The system package of ngteams is installed too. Remove it with your package manager, for example"
  echo "'sudo pacman -R ngteams', 'sudo apt remove ngteams' or 'sudo dnf remove ngteams', so only this copy runs."
elif [[ "$here" == /opt/ngteams ]]; then
  echo "To use only this copy, remove the system package with your package manager, for example"
  echo "'sudo pacman -R ngteams', 'sudo apt remove ngteams' or 'sudo dnf remove ngteams'."
fi
case ":$PATH:" in
  *":$bin:"*) ;;
  *) echo "Add $bin to your PATH to run ngteams and ngteamsctl from a terminal." ;;
esac

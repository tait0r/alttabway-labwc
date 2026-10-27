#!/usr/bin/env bash
# Install alttabway for the current user and start it at login.
# Does not change labwc rc.xml. Bind a free chord in Budgie Custom shortcuts.
set -euo pipefail
unset CARGO_TARGET_DIR

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bin_dir="${HOME}/.local/bin"
binary="${bin_dir}/alttabway"
config_home="${XDG_CONFIG_HOME:-${HOME}/.config}"
autostart_dir="${config_home}/autostart"
autostart_file="${autostart_dir}/alttabway.desktop"

cd "$root"
cargo build --release --locked
install -Dm755 "${root}/target/release/alttabway" "$binary"

mkdir -p "$autostart_dir"
cat >"$autostart_file" <<EOF
[Desktop Entry]
Type=Application
Name=alttabway
Comment=Alt-tab window switcher
Exec=${binary} daemon
Terminal=false
X-GNOME-Autostart-enabled=true
EOF

if pgrep -u "$(id -u)" -x alttabway >/dev/null 2>&1; then
	pkill -u "$(id -u)" -x alttabway || true
	sleep 0.2
fi
nohup "$binary" daemon >/dev/null 2>&1 &
disown || true

echo "Installed ${binary}"
echo "Daemon autostarts from ${autostart_file}"
echo
echo "Bind Super+tilde (the key above Tab) in Budgie Desktop Settings → Keyboard → Custom shortcuts:"
echo "  ${binary} show --next --modifiers-held super"
echo "  ${binary} show --previous --modifiers-held super,shift"

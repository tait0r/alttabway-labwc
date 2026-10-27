#!/usr/bin/env bash
# Install alttabway for the current user and start it at login.
# Alt-Tab is rewritten in labwc rc.xml because Budgie Settings can change the
# key, but the action stays hardcoded as NextWindow / PreviousWindow.
set -euo pipefail
unset CARGO_TARGET_DIR

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bin_dir="${HOME}/.local/bin"
binary="${bin_dir}/alttabway"
config_home="${XDG_CONFIG_HOME:-${HOME}/.config}"
budgie_rc="${config_home}/budgie-desktop/labwc/rc.xml"
labwc_rc="${config_home}/labwc/rc.xml"
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

patch_rc() {
	local rc="$1"
	[[ -f "$rc" ]] || return 0
	python3 - "$rc" "$binary" <<'PY'
import pathlib, re, sys
rc_path, binary = sys.argv[1:]
text = pathlib.Path(rc_path).read_text()
commands = {
    "A-Tab": f"{binary} show --next",
    "S-A-Tab": f"{binary} show --previous",
    "A-S-Tab": f"{binary} show --previous",
}
pattern = re.compile(
    r'<keybind\b[^>]*\bkey="(?P<key>A-Tab|S-A-Tab|A-S-Tab)"[^>]*>\s*<action\b[^>]*/>',
    re.DOTALL,
)

def replace(match):
    command = commands[match.group("key")]
    action = f'<action name="Execute" command="{command}" />'
    return re.sub(r"<action\b[^>]*/>", action, match.group(0), count=1)

updated, count = pattern.subn(replace, text)
if count == 0:
    print(f"No Alt-Tab keybind found in {rc_path}", file=sys.stderr)
    sys.exit(0)
if updated == text:
    print(f"Alt-Tab already points at {binary} in {rc_path}")
    sys.exit(0)
backup = pathlib.Path(rc_path + ".bak")
if not backup.exists():
    backup.write_text(text)
pathlib.Path(rc_path).write_text(updated)
print(f"Pointed Alt-Tab at {binary} in {rc_path}")
PY
}

if [[ -f "$budgie_rc" ]]; then
	patch_rc "$budgie_rc"
elif [[ -f "$labwc_rc" ]]; then
	patch_rc "$labwc_rc"
	autostart="${config_home}/labwc/autostart"
	touch "$autostart"
	python3 - "$autostart" "$binary" <<'PY'
import pathlib, sys
path, binary = sys.argv[1:]
file = pathlib.Path(path)
text = file.read_text() if file.exists() else ""
begin, end = "# >>> alttabway >>>", "# <<< alttabway <<<"
block = f"{begin}\n{binary} daemon >/dev/null 2>&1 &\n{end}\n"
if begin in text and end in text:
    pre, rest = text.split(begin, 1)
    _, post = rest.split(end, 1)
    text = pre + block + post.lstrip("\n")
else:
    if text and not text.endswith("\n"):
        text += "\n"
    text += block
file.write_text(text)
print(f"Added daemon to {path}")
PY
else
	echo "No labwc rc.xml found. Bind these yourself:"
	echo "  ${binary} show --next"
	echo "  ${binary} show --previous"
fi

if pgrep -u "$(id -u)" -x alttabway >/dev/null 2>&1; then
	pkill -u "$(id -u)" -x alttabway || true
	sleep 0.2
fi
nohup "$binary" daemon >/dev/null 2>&1 &
disown || true

if pgrep -u "$(id -u)" -x labwc >/dev/null 2>&1; then
	labwc -r || true
fi

echo "Installed ${binary}"
echo "Daemon autostarts from ${autostart_file}"
echo "Alt-Tab and Shift-Alt-Tab open alttabway. Super-Tab is unchanged."

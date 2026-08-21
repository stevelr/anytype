#!/usr/bin/env bash

set -euo pipefail

skills_cli_version=1.5.23
script_path=$(realpath "$0")
repository_root=$(realpath "$(dirname "$script_path")/../..")

if [[ "${1:-}" == "--inside-sandbox" ]]; then
  :
elif [[ "${1:-}" == "--in-scope" ]]; then
  for command in bwrap claude codex npx python3; do
    command -v "$command" >/dev/null || {
      printf 'required installation-test command is unavailable: %s\n' "$command" >&2
      exit 1
    }
  done

  sandbox_home=$(mktemp -d "${TMPDIR:-/tmp}/skills-e2e-home.XXXXXX")
  test_data=$(mktemp -d "${TMPDIR:-/tmp}/skills-e2e-data.XXXXXX")
  trap 'rm -rf "$sandbox_home" "$test_data"' EXIT
  bwrap \
    --die-with-parent \
    --ro-bind / / \
    --dev-bind /dev /dev \
    --bind "$sandbox_home" /home/user \
    --tmpfs /tmp \
    --dir /tmp/source \
    --ro-bind "$repository_root" /tmp/source \
    --dir /tmp/test \
    --bind "$test_data" /tmp/test \
    --chdir /tmp/test \
    bash /tmp/source/.github/scripts/test-skills-installation-e2e.sh --inside-sandbox
  exit
else
  command -v systemd-run >/dev/null || {
    printf 'required installation-test command is unavailable: systemd-run\n' >&2
    exit 1
  }
  unit="anytype-skills-e2e-$$.service"
  systemd-run --user --unit="$unit" --collect --same-dir \
    --service-type=exec --property=KillMode=control-group --wait --pipe \
    bash "$script_path" --in-scope
  exit
fi

test "$PWD" = /tmp/test
test ! -e /home/user/.codex
test ! -e /home/user/.claude
grep -Fq 'executable must be on' /tmp/source/skills/skills/anyr/SKILL.md
grep -q 'If ping fails:' /tmp/source/skills/skills/anyr/SKILL.md
grep -q 'report that the connection is unavailable' \
  /tmp/source/skills/skills/any-mcp/SKILL.md
grep -q 'report the missing startup selection' \
  /tmp/source/skills/skills/any-mcp/SKILL.md

skills() {
  npx --yes "skills@$skills_cli_version" "$@"
}

assert_skill() {
  local root=$1
  local skill=$2
  test -f "$root/$skill/SKILL.md"
}

printf 'Testing skills CLI %s discovery and selected installs\n' "$skills_cli_version"
skills add /tmp/source --list > skills-list.txt
grep -q 'anyr' skills-list.txt
grep -q 'any-mcp' skills-list.txt

mkdir project-anyr project-any-mcp project-both
(
  cd project-anyr
  skills add /tmp/source --skill anyr --agent codex --yes --copy
  assert_skill .agents/skills anyr
  test ! -e .agents/skills/any-mcp
)
(
  cd project-any-mcp
  skills add /tmp/source --skill any-mcp --agent codex --yes --copy
  assert_skill .agents/skills any-mcp
  test ! -e .agents/skills/anyr
)
(
  cd project-both
  skills add /tmp/source --skill anyr --skill any-mcp --agent codex --yes --copy
  assert_skill .agents/skills anyr
  assert_skill .agents/skills any-mcp
  skills remove anyr any-mcp --agent codex --yes
  test ! -e .agents/skills/anyr
  test ! -e .agents/skills/any-mcp
  if grep -q '"anyr"\|"any-mcp"' skills-lock.json; then
    printf 'skills lock retained removed project skills\n' >&2
    exit 1
  fi
)

skills add /tmp/source \
  --skill anyr --skill any-mcp \
  --agent codex --global --yes --copy
assert_skill /home/user/.agents/skills anyr
assert_skill /home/user/.agents/skills any-mcp
skills remove anyr any-mcp --agent codex --global --yes
test ! -e /home/user/.agents/skills/anyr
test ! -e /home/user/.agents/skills/any-mcp

printf 'Testing release-shaped ZIP installation\n'
python3 /tmp/source/.github/scripts/prepare_skills_release.py prepare \
  anytype-toolbox-skills-v0.1.0 \
  --package /tmp/source/skills \
  --output /tmp/test/release
server_pid=
trap 'test -z "$server_pid" || kill "$server_pid" 2>/dev/null || true' EXIT
python3 - <<'PY' &
import http.server
import pathlib

class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, format, *args):
        pass

server = http.server.ThreadingHTTPServer(
    ("127.0.0.1", 0),
    lambda *args, **kwargs: QuietHandler(*args, directory="/tmp/test/release", **kwargs),
)
pathlib.Path("/tmp/test/archive-server-port").write_text(
    str(server.server_port), encoding="ascii"
)
server.serve_forever()
PY
server_pid=$!
for _ in {1..50}; do
  test -s /tmp/test/archive-server-port && break
  sleep 0.1
done
test -s /tmp/test/archive-server-port
archive_port=$(cat /tmp/test/archive-server-port)
mkdir archive-project
(
  cd archive-project
  skills add "http://127.0.0.1:$archive_port/anytype-toolbox-skills-v0.1.0.zip" \
    --skill anyr --skill any-mcp --agent codex --yes --copy
  assert_skill .agents/skills anyr
  assert_skill .agents/skills any-mcp
  test -f .agents/skills/any-mcp/references/workflows.md
  test -z "$(find .agents/skills -xtype l -print -quit)"
  test ! -e .agents/skills/anyr/README.md
)
kill "$server_pid"
wait "$server_pid" 2>/dev/null || true
server_pid=

printf 'Testing Codex and Claude marketplace install, upgrade, and removal\n'
mkdir marketplace-fixture
cp -a /tmp/source/.agents /tmp/source/.claude-plugin /tmp/source/skills marketplace-fixture/

codex plugin marketplace add /tmp/test/marketplace-fixture --json > codex-marketplace.json
codex plugin add anytype-toolbox-skills@anytype-toolbox --json > codex-install-0.1.0.json
grep -q 'installed, enabled' < <(codex plugin list)
codex_cache=/home/user/.codex/plugins/cache/anytype-toolbox/anytype-toolbox-skills/0.1.0
assert_skill "$codex_cache/skills" anyr
assert_skill "$codex_cache/skills" any-mcp
cmp marketplace-fixture/skills/skills/anyr/SKILL.md "$codex_cache/skills/anyr/SKILL.md"

claude plugin marketplace add /tmp/test/marketplace-fixture
claude plugin install anytype-toolbox-skills@anytype-toolbox
grep -q 'Status:.*enabled' < <(claude plugin list)
claude_cache=/home/user/.claude/plugins/cache/anytype-toolbox/anytype-toolbox-skills/0.1.0
assert_skill "$claude_cache/skills" anyr
assert_skill "$claude_cache/skills" any-mcp
cmp marketplace-fixture/skills/skills/any-mcp/SKILL.md "$claude_cache/skills/any-mcp/SKILL.md"

python3 - <<'PY'
import json
import pathlib

root = pathlib.Path("/tmp/test/marketplace-fixture")
for relative in (
    "skills/.codex-plugin/plugin.json",
    "skills/.claude-plugin/plugin.json",
):
    path = root / relative
    value = json.loads(path.read_text(encoding="utf-8"))
    value["version"] = "0.1.1"
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")

marketplace = root / ".claude-plugin/marketplace.json"
value = json.loads(marketplace.read_text(encoding="utf-8"))
value["metadata"]["version"] = "0.1.1"
value["plugins"][0]["version"] = "0.1.1"
marketplace.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")

changelog = root / "skills/CHANGELOG.md"
text = changelog.read_text(encoding="utf-8")
changelog.write_text(text.replace("## [0.1.0]", "## [0.1.1]", 1), encoding="utf-8")
(root / "skills/skills/anyr/SKILL.md").open("a", encoding="utf-8").write(
    "\nmarketplace-upgrade-marker\n"
)
PY

codex plugin add anytype-toolbox-skills@anytype-toolbox --json > codex-install-0.1.1.json
test "$(python3 -c 'import json; print(json.load(open("codex-install-0.1.1.json"))["version"])')" = 0.1.1
grep -q 'marketplace-upgrade-marker' \
  /home/user/.codex/plugins/cache/anytype-toolbox/anytype-toolbox-skills/0.1.1/skills/anyr/SKILL.md

claude plugin marketplace update anytype-toolbox
claude plugin update anytype-toolbox-skills@anytype-toolbox
claude plugin list > claude-list-after-update.txt
cat claude-list-after-update.txt
grep -q 'Version: 0.1.1' claude-list-after-update.txt
test -f /home/user/.claude/plugins/cache/anytype-toolbox/anytype-toolbox-skills/0.1.1/skills/anyr/SKILL.md

codex plugin remove anytype-toolbox-skills@anytype-toolbox --json >/dev/null
if grep -q 'installed, enabled' < <(codex plugin list); then
  printf 'Codex plugin remained enabled after removal\n' >&2
  exit 1
fi
codex plugin marketplace remove anytype-toolbox --json >/dev/null
if codex plugin list | grep -q 'anytype-toolbox-skills'; then
  printf 'Codex marketplace entry remained after removal\n' >&2
  exit 1
fi

claude plugin uninstall anytype-toolbox-skills@anytype-toolbox
claude plugin marketplace remove anytype-toolbox
if claude plugin list | grep -q 'anytype-toolbox-skills'; then
  printf 'Claude plugin remained after removal\n' >&2
  exit 1
fi
if claude plugin marketplace list | grep -q 'anytype-toolbox'; then
  printf 'Claude marketplace remained after removal\n' >&2
  exit 1
fi

printf 'skills installation end-to-end checks passed\n'

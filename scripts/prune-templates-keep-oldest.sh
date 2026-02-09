#!/usr/bin/env bash
set -euo pipefail

if ! command -v anyr >/dev/null 2>&1; then
  echo "error: anyr not found in PATH" >&2
  exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "error: jq not found in PATH" >&2
  exit 1
fi

spaces=("$@")
if [ ${#spaces[@]} -eq 0 ]; then
  spaces=("test10" "test11")
fi

for space in "${spaces[@]}"; do
  echo "== space: ${space} =="
  types_json="$(anyr type list "${space}" --all)"

  while IFS=$'\t' read -r type_id type_key; do
    [ -n "${type_id}" ] || continue
    templates_json="$(anyr template list "${space}" "${type_id}" --all 2>/dev/null || echo '[]')"
    template_count="$(jq 'length' <<<"${templates_json}")"
    if [ "${template_count}" -le 10 ]; then
      continue
    fi

    delete_ids="$(
      jq -r '
        def lm:
          ([.properties[]? | select(.key == "last_modified_date") | .date] | first) // "";
        sort_by(lm) | .[10:][]?.id
      ' <<<"${templates_json}"
    )"

    if [ -z "${delete_ids}" ]; then
      continue
    fi

    deleted=0
    while IFS= read -r template_id; do
      [ -n "${template_id}" ] || continue
      if anyr object delete "${space}" "${template_id}" >/dev/null 2>&1; then
        deleted=$((deleted + 1))
      else
        echo "warn: failed to delete template ${template_id} in ${space}" >&2
      fi
    done <<<"${delete_ids}"

    echo "type=${type_key} id=${type_id} pruned=${deleted} kept=oldest-10 (from ${template_count})"
  done < <(jq -r '.[] | [.id, .key] | @tsv' <<<"${types_json}")
done

#!/usr/bin/env bash
# Split a static library into a fixed number of smaller archives, so that
# every part stays under the GitHub file size limit. Debug sections are
# stripped first. The parts are written as <name>_1.a .. <name>_N.a and
# must be linked together, on Linux inside one --start-group/--end-group.
set -euo pipefail

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
	echo "usage: $0 <input.a> <output-dir> [parts]" >&2
	exit 2
fi

INPUT="$1"
OUTPUT_DIR="$2"
PARTS="${3:-3}"
NAME="libduckmon_ffi"
MAX_PART_BYTES=$((95 * 1024 * 1024))

mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

cp "$INPUT" "$WORK_DIR/full.a"
echo "input size: $(wc -c < "$WORK_DIR/full.a") bytes"

if [ "$(uname)" = "Darwin" ]; then
	strip -S "$WORK_DIR/full.a"
else
	objcopy --strip-debug "$WORK_DIR/full.a"
fi
echo "stripped size: $(wc -c < "$WORK_DIR/full.a") bytes"

if ar t "$WORK_DIR/full.a" | sort | uniq -d | grep -q .; then
	echo "error: the archive holds two members with the same name" >&2
	exit 1
fi

mkdir "$WORK_DIR/objects"
(cd "$WORK_DIR/objects" && ar x ../full.a)

# Walk the members in archive order and start a new part when the current
# one reaches an equal share of the total size.
ar t "$WORK_DIR/full.a" > "$WORK_DIR/members.txt"
(cd "$WORK_DIR" && python3 - "$PARTS" <<'EOF'
import os
import sys

parts = int(sys.argv[1])
members = [line.rstrip("\n") for line in open("members.txt") if line.strip()]
total = sum(os.path.getsize(os.path.join("objects", m)) for m in members)
share = total / parts

groups = [[] for _ in range(parts)]
index = 0
size = 0
for member in members:
    member_size = os.path.getsize(os.path.join("objects", member))
    if size + member_size > share and index < parts - 1 and groups[index]:
        index += 1
        size = 0
    groups[index].append(member)
    size += member_size

for number, group in enumerate(groups, start=1):
    with open(f"part_{number}.txt", "w") as handle:
        handle.write("\n".join(group) + "\n")
EOF
)

for number in $(seq 1 "$PARTS"); do
	PART="$OUTPUT_DIR/${NAME}_${number}.a"
	rm -f "$PART"
	(cd "$WORK_DIR/objects" && xargs ar rcs "$PART" < "../part_${number}.txt")
	SIZE="$(wc -c < "$PART")"
	echo "$PART: $SIZE bytes"
	if [ "$SIZE" -ge "$MAX_PART_BYTES" ]; then
		echo "error: $PART is $SIZE bytes, which is over the 95 MB limit; raise the part count" >&2
		exit 1
	fi
done

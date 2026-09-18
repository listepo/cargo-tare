#!/bin/bash
# cargo-tare T2 spike, part 2 (needs the image and applesauce binary left by t2-spike.sh).
# E4 was inconclusive because `ditto --hfsCompression` compressed nothing on this volume.
#   usage: t2-spike-e8.sh <scratch-dir> [cleanup]
set -u
S="${1:?scratch dir}"; CLEAN="${2:-}"
IMG="$S/tare-spike.sparseimage"; VOL=/Volumes/TareSpike; AS="$VOL/tools/bin/applesauce"
export CARGO_TERM_COLOR=never
say() { printf '\n=== %s\n' "$*"; }
now() { perl -MTime::HiRes=time -e 'printf "%.2f", time'; }
used() { sync; sleep 1; df -k "$VOL" | tail -1 | awk '{print $3}'; }
alloc() { echo $(( $(stat -f '%b' "$1")/2 )); }
flags() { ls -lO "$1" | awk '{print $5}'; }
fresh() { ( cd "$1" || exit; s=$(now)
  cargo build --offline --message-format=json 2>/dev/null \
  | jq -r 'select(.reason=="compiler-artifact") | "\(.fresh) \(.target.name)"' | sort -u \
  | awk '{c[$1]++; if($1=="false") n=n" "$2} END{printf "fresh=%d rebuilt=%d%s", c["true"]+0, c["false"]+0, (n?" ->"n:"")}'
  e=$(now); echo "  seconds=$(echo "$e - $s" | bc)" ); }
[ -d "$VOL" ] || hdiutil attach "$IMG" -nobrowse >/dev/null 2>&1
[ -x "$AS" ] || { echo "ABORT: applesauce binary missing"; exit 1; }
cd "$VOL" || exit 1
"$AS" compress --help 2>&1 | grep -A6 -E '^\s+-c, --compression' | tr -s ' ' | head -8

say "E8a compression kinds on a 119 MB blob of rlibs"
for k in lzfse zlib lzvn; do cp big.bin "k_$k.bin"; s=$(now); "$AS" compress -c "$k" "k_$k.bin" >/dev/null 2>&1; e=$(now)
  echo "$k: allocated KB $(alloc big.bin) -> $(alloc "k_$k.bin") ($(echo "scale=1; 100*$(alloc "k_$k.bin")/$(alloc big.bin)" | bc)%) flags=$(flags "k_$k.bin") compress_seconds=$(echo "$e - $s" | bc)"
  s=$(now); cat "k_$k.bin" > /dev/null; e=$(now); echo "   read back seconds=$(echo "$e - $s" | bc)"; done
s=$(now); cat big.bin > /dev/null; e=$(now); echo "plain read seconds=$(echo "$e - $s" | bc)"

say "E8b do clones of a COMPRESSED file share blocks and stay compressed?"
h0=$(shasum -a 256 big.bin | cut -c1-16)
u0=$(used); for i in 1 2 3 4 5; do cp -c k_lzfse.bin "ca$i"; done; u1=$(used)
echo "5 clones: image_used_delta_KB=$((u1-u0)) (unshared would be ~$(( 5*$(alloc k_lzfse.bin) ))) clone flags=$(flags ca1) content ok: $([ "$h0" = "$(shasum -a 256 ca3 | cut -c1-16)" ] && echo yes || echo NO)"
printf 'x' >> ca1; echo "after append to ca1: flags=$(flags ca1) allocated KB=$(alloc ca1); ca2 intact: $([ "$h0" = "$(shasum -a 256 ca2 | cut -c1-16)" ] && echo yes || echo NO) ca2 flags=$(flags ca2)"
rm -f ca? k_*.bin

say "E8c compress first, then seed: is the seeded target shared AND compressed AND fresh?"
u0=$(used); "$AS" compress "$VOL/main/target" 2>&1 | grep -E 'Savings|Final'; u1=$(used)
echo "compress main/target: image_used_delta_KB=$((u1-u0)); main: $(fresh "$VOL/main")"
git -C "$VOL/main" worktree add -q "$VOL/wt_seed2" -b wt_seed2 2>/dev/null
u0=$(used); cp -c -R -p "$VOL/main/target" "$VOL/wt_seed2/target"; u1=$(used)
echo "seed from compressed target: image_used_delta_KB=$((u1-u0)); compressed files in deps: $(ls -lO "$VOL/wt_seed2/target/debug/deps" | grep -c compressed) of $(ls "$VOL/wt_seed2/target/debug/deps" | wc -l | tr -d ' ')"
echo "wt_seed2: $(fresh "$VOL/wt_seed2")"
echo "compressed files in wt_seed2 deps after its build: $(ls -lO "$VOL/wt_seed2/target/debug/deps" | grep -c compressed)"

say "E8d fused step: wt_seed_p was compressed separately (unshared). Re-clone identical content from compressed main."
hl() { ( cd "$1" && find target/debug -type f -size +4k -links 1 ! -name '*.d' ! -path '*/incremental/*' -print0 | xargs -0 shasum -a 256 ); }
hl "$VOL/main" > "$VOL/h2_main.txt"; hl "$VOL/wt_seed_p" > "$VOL/h2_seed.txt"
awk 'FILENAME==ARGV[1]{if(!($1 in m)) m[$1]=$2; next} ($1 in m){print m[$1] "|" $2}' "$VOL/h2_main.txt" "$VOL/h2_seed.txt" > "$VOL/pairs2.txt"
exp=$(cut -d'|' -f2 "$VOL/pairs2.txt" | sed "s|^|$VOL/wt_seed_p/|" | tr '\n' '\0' | xargs -0 stat -f '%b' | awk '{s+=$1} END{print int(s/2)}')
u0=$(used)
while IFS='|' read -r a b; do src="$VOL/main/$a"; dst="$VOL/wt_seed_p/$b"
  cp -c "$src" "$dst.tare-tmp" && touch -r "$dst" "$dst.tare-tmp" && mv -f "$dst.tare-tmp" "$dst"; done < "$VOL/pairs2.txt"
u1=$(used); echo "pairs=$(wc -l < "$VOL/pairs2.txt" | tr -d ' ') image_used_delta_KB=$((u1-u0)) (expected about -$exp); still compressed: $(ls -lO "$VOL/wt_seed_p/target/debug/deps" | grep -c compressed)"
echo "wt_seed_p after re-clone: $(fresh "$VOL/wt_seed_p")"

say "E8e relink against compressed rlibs and run"
cd "$VOL/main" || exit 1; echo '// touch' >> tareapp/src/main.rs
echo "main after source edit: $(fresh "$VOL/main")  run: $(./target/debug/tareapp)"; git checkout -q tareapp/src/main.rs
echo "members rewritten by the build lose compression (expected): compressed in deps = $(ls -lO target/debug/deps | grep -c compressed)"

say "E8f negative control on a MEMBER rlib: clone-replace without restoring mtime"
m=$(ls "$VOL/wt_cold/target/debug/deps"/libtarecore-*.rlib | head -1)
cp -c "$m" "$m.tare-tmp" && touch "$m.tare-tmp" && mv -f "$m.tare-tmp" "$m"
echo "wt_cold, member rlib mtime = now: $(fresh "$VOL/wt_cold")"

if [ "$CLEAN" = "cleanup" ]; then say "cleanup"; cd /; hdiutil detach "$VOL" >/dev/null 2>&1 && rm -f "$IMG" && echo "image detached and deleted"; fi
df -k /System/Volumes/Data | tail -1 | awk '{printf "host free MB: %d\n", $4/1024}'

#!/bin/bash
# cargo-tare T2 spike. Everything happens inside a throwaway APFS sparse image, so df deltas are
# exact and no real target dir is touched. macOS bash 3.2 compatible.
#   usage: t2-spike.sh <scratch-dir> [applesauce]
set -u
S="${1:?scratch dir}"; WITH_AS="${2:-}"
IMG="$S/tare-spike.sparseimage"
VOL=/Volumes/TareSpike
export CARGO_TERM_COLOR=never

say() { printf '\n=== %s\n' "$*"; }
now() { perl -MTime::HiRes=time -e 'printf "%.2f", time'; }
hostfree() { df -k /System/Volumes/Data | tail -1 | awk '{print int($4/1024)}'; }
used() { sync; sleep 1; df -k "$VOL" | tail -1 | awk '{print $3}'; }   # KB used on the image
# freshness oracle: builds, then reports fresh / rebuilt units and wall time
fresh() { ( cd "$1" || exit; s=$(now)
  cargo build --offline --message-format=json 2>/dev/null \
  | jq -r 'select(.reason=="compiler-artifact") | "\(.fresh) \(.target.name)"' | sort -u \
  | awk '{c[$1]++; if($1=="false") n=n" "$2} END{printf "fresh=%d rebuilt=%d%s", c["true"]+0, c["false"]+0, (n?" ->"n:"")}'
  e=$(now); echo "  seconds=$(echo "$e - $s" | bc)" ); }

say "preflight"
echo "host free MB: $(hostfree)"; cargo --version
[ "$(hostfree)" -lt 2000 ] && { echo "ABORT: less than 2 GB free on host"; exit 1; }
if [ ! -d "$VOL" ]; then
  [ -f "$IMG" ] || hdiutil create -size 4g -type SPARSE -fs APFS -volname TareSpike "$IMG" >/dev/null || exit 1
  hdiutil attach "$IMG" -nobrowse >/dev/null || exit 1
fi
diskutil info "$VOL" | grep -E 'File System Personality|Volume Name' | sed 's/^ *//'

# ---------------------------------------------------------------- fixture
if [ ! -d "$VOL/main/.git" ]; then
  say "fixture"
  mkdir -p "$VOL/main/tarecore/src" "$VOL/main/tareapp/src"; cd "$VOL/main" || exit 1
  printf '[workspace]\nmembers = ["tarecore", "tareapp"]\nresolver = "2"\n' > Cargo.toml
  printf '/target\n' > .gitignore
  cat > tarecore/Cargo.toml <<'EOF'
[package]
name = "tarecore"
version = "0.1.0"
edition = "2021"
[dependencies]
serde = { version = "1", features = ["derive"] }
memchr = "2"
EOF
  cat > tarecore/src/lib.rs <<'EOF'
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize, Debug)]
pub struct Item { pub name: String, pub n: u32 }
pub fn find(h: &[u8], b: u8) -> Option<usize> { memchr::memchr(b, h) }
#[cfg(test)]
mod tests { #[test] fn t() { assert_eq!(super::find(b"abc", b'c'), Some(2)); } }
EOF
  cat > tareapp/Cargo.toml <<'EOF'
[package]
name = "tareapp"
version = "0.1.0"
edition = "2021"
[dependencies]
tarecore = { path = "../tarecore" }
serde_json = "1"
libc = "0.2"
EOF
  cat > tareapp/src/main.rs <<'EOF'
fn main() {
    let it = tarecore::Item { name: "x".into(), n: unsafe { libc::getpid() } as u32 };
    println!("{}", serde_json::to_string(&it).unwrap().len() > 0);
}
EOF
  if cargo generate-lockfile --offline 2>/dev/null; then echo "lockfile: offline"; else cargo generate-lockfile 2>&1 | tail -1; echo "lockfile: online index"; fi
  git init -q . && git add -A && { git commit -qm init 2>/dev/null || git -c user.name=spike -c user.email=spike@localhost commit -qm init; }
  echo "main cold build: $(fresh "$VOL/main")"
fi
cd "$VOL/main" || exit 1
echo "main again:      $(fresh "$VOL/main")"
echo "target KB: $(du -sk target | cut -f1)  files: $(find target -type f | wc -l | tr -d ' ')  hardlinked files: $(find target -type f -links +1 | wc -l | tr -d ' ')"
du -sk target/debug/* target/debug/.fingerprint 2>/dev/null | sort -rn | head -6

# ---------------------------------------------------------------- E1 seed
say "E1 seed a new worktree by recursive clone (hypothesis: registry deps fresh, members rebuild)"
for w in wt_cold wt_seed_p wt_seed_nop wt_seed_noinc; do
  [ -d "$VOL/$w" ] || git worktree add -q "$VOL/$w" -b "$w" 2>/dev/null
done
u0=$(used); s=$(now); cp -c -R -p "$VOL/main/target" "$VOL/wt_seed_p/target"; e=$(now); u1=$(used)
echo "cp -c -R -p: seconds=$(echo "$e - $s" | bc) image_used_delta_KB=$((u1-u0)) (target is $(du -sk "$VOL/main/target" | cut -f1) KB)"
cp -c -R "$VOL/main/target" "$VOL/wt_seed_nop/target"
cp -c -R -p "$VOL/main/target" "$VOL/wt_seed_noinc/target"; rm -rf "$VOL/wt_seed_noinc/target/debug/incremental"
echo "wt_cold (no seed):           $(fresh "$VOL/wt_cold")"
echo "wt_seed_p (-p, mtimes kept):  $(fresh "$VOL/wt_seed_p")"
echo "wt_seed_nop (mtimes = now):   $(fresh "$VOL/wt_seed_nop")"
echo "wt_seed_noinc (-p, no incr):  $(fresh "$VOL/wt_seed_noinc")"
echo "wt_seed_p second run:         $(fresh "$VOL/wt_seed_p")"
echo "member artifact names, main vs wt_cold (same name = path-independent unit hash):"
ls "$VOL/main/target/debug/deps" | grep -E '^libtarecore-.*rlib$|^libmemchr-.*rlib$' | sed 's/^/  main    /'
ls "$VOL/wt_cold/target/debug/deps" | grep -E '^libtarecore-.*rlib$|^libmemchr-.*rlib$' | sed 's/^/  wt_cold /'

# ---------------------------------------------------------------- E2 dedupe
say "E2 content match between independently built worktrees; clone replace with restored mtime"
hl() { ( cd "$1" && find target/debug -type f -size +4k -links 1 ! -name '*.d' ! -path '*/incremental/*' -print0 | xargs -0 shasum -a 256 ); }
hl "$VOL/main" > "$VOL/h_main.txt"; hl "$VOL/wt_cold" > "$VOL/h_cold.txt"
awk 'FILENAME==ARGV[1]{if(!($1 in m)) m[$1]=$2; next} ($1 in m){print m[$1] "|" $2}' "$VOL/h_main.txt" "$VOL/h_cold.txt" > "$VOL/pairs.txt"
tot=$(wc -l < "$VOL/h_cold.txt" | tr -d ' '); dup=$(wc -l < "$VOL/pairs.txt" | tr -d ' ')
dupkb=$(cut -d'|' -f2 "$VOL/pairs.txt" | sed "s|^|$VOL/wt_cold/|" | tr '\n' '\0' | xargs -0 stat -f '%z' | awk '{s+=$1} END{print int(s/1024)}')
totkb=$(awk '{print $2}' "$VOL/h_cold.txt" | sed "s|^|$VOL/wt_cold/|" | tr '\n' '\0' | xargs -0 stat -f '%z' | awk '{s+=$1} END{print int(s/1024)}')
echo "wt_cold files >4k: $tot ($totkb KB); byte-identical to a main file: $dup ($dupkb KB)"
echo "not identical, by extension:"; awk 'FILENAME==ARGV[1]{m[$1]=1;next} !($1 in m){n=$2; sub(/.*\//,"",n); if(n~/\./) sub(/.*\./,"",n); else n="noext"; c[n]++} END{for(k in c) printf "  %s %d\n", k, c[k]}' "$VOL/h_main.txt" "$VOL/h_cold.txt"
u0=$(used)
while IFS='|' read -r a b; do
  src="$VOL/main/$a"; dst="$VOL/wt_cold/$b"
  cp -c "$src" "$dst.tare-tmp" && touch -r "$dst" "$dst.tare-tmp" && chmod "$(stat -f '%Lp' "$dst")" "$dst.tare-tmp" && mv -f "$dst.tare-tmp" "$dst"
done < "$VOL/pairs.txt"
u1=$(used); echo "image_used_delta_KB after clone-replace: $((u1-u0)) (expected about -$dupkb)"
echo "wt_cold after dedupe:        $(fresh "$VOL/wt_cold")"
say "E2 negative control: same replacement WITHOUT restoring mtime on libmemchr rlib"
m=$(ls "$VOL/wt_cold/target/debug/deps"/libmemchr-*.rlib | head -1)
cp -c "$m" "$m.tare-tmp" && touch "$m.tare-tmp" && mv -f "$m.tare-tmp" "$m"
echo "wt_cold, rlib mtime = now:   $(fresh "$VOL/wt_cold")"

# ---------------------------------------------------------------- E3 hardlink group
say "E3 replace a whole hardlink group atomically"
cd "$VOL/main" || exit 1
find target/debug -type f -links +1 -print0 | xargs -0 stat -f '%i %l %N' | sort -n > "$VOL/links.txt"
echo "hardlink groups: $(awk '{print $1}' "$VOL/links.txt" | sort -u | wc -l | tr -d ' ')"; head -6 "$VOL/links.txt"
gi=$(awk 'NR==1{print $1}' "$VOL/links.txt"); gl=$(awk 'NR==1{print $2}' "$VOL/links.txt")
awk -v i="$gi" '$1==i{print $3}' "$VOL/links.txt" > "$VOL/group.txt"; gn=$(wc -l < "$VOL/group.txt" | tr -d ' ')
echo "group inode=$gi nlink=$gl paths_found=$gn"
if [ "$gl" = "$gn" ]; then
  p1=$(head -1 "$VOL/group.txt"); before=$(shasum -a 256 "$p1" | cut -c1-16)
  cp -c "$p1" "$p1.tare-canon" && touch -r "$p1" "$p1.tare-canon"
  while read -r p; do ln "$p1.tare-canon" "$p.tare-tmp" && mv -f "$p.tare-tmp" "$p"; done < "$VOL/group.txt"
  rm -f "$p1.tare-canon"
  echo "after: $(tr '\n' '\0' < "$VOL/group.txt" | xargs -0 stat -f '%i %l %N' | tr '\n' ';')"
  echo "content same: $([ "$before" = "$(shasum -a 256 "$p1" | cut -c1-16)" ] && echo yes || echo NO)"
  echo "main after group replace:    $(fresh "$VOL/main")"
else echo "SKIP: group has paths outside target/debug"; fi

# ---------------------------------------------------------------- E4 compression
say "E4 transparent compression via ditto (zlib): freshness, relink, clone sharing"
cd "$VOL/main" || exit 1
d0=$(du -sk target/debug/deps | cut -f1); s=$(now)
find target/debug/deps -type f -links 1 -size +8k ! -name '*.d' > "$VOL/cmp.txt"
while read -r f; do ditto --hfsCompression "$f" "$f.tare-tmp" && touch -r "$f" "$f.tare-tmp" && mv -f "$f.tare-tmp" "$f"; done < "$VOL/cmp.txt"
e=$(now); d1=$(du -sk target/debug/deps | cut -f1)
echo "deps KB: $d0 -> $d1 ($(echo "scale=1; 100*$d1/$d0" | bc)%) files=$(wc -l < "$VOL/cmp.txt" | tr -d ' ') seconds=$(echo "$e - $s" | bc)"
echo "compressed flag count: $(ls -lO target/debug/deps | grep -c compressed)"
echo "main after compress:         $(fresh "$VOL/main")"
echo '// touch' >> tareapp/src/main.rs
echo "main after source edit:      $(fresh "$VOL/main")  run: $(./target/debug/tareapp)"
echo "deps KB after rebuild: $(du -sk target/debug/deps | cut -f1); compressed flag count: $(ls -lO target/debug/deps | grep -c compressed)"
git checkout -q tareapp/src/main.rs

say "E4d do clones of a compressed file share blocks?"
cd "$VOL" || exit 1
: > big.bin; for i in 1 2 3 4 5 6; do cat main/target/debug/deps/*.rlib >> big.bin 2>/dev/null; done
ditto --hfsCompression big.bin big.cmp
echo "big.bin logical KB=$(( $(stat -f '%z' big.bin)/1024 )) allocated KB=$(( $(stat -f '%b' big.bin)/2 )); big.cmp allocated KB=$(( $(stat -f '%b' big.cmp)/2 )) flags=$(ls -lO big.cmp | awk '{print $5}')"
u0=$(used); for i in 1 2 3 4 5; do cp -c big.cmp "cc$i"; done; u1=$(used)
echo "5 clones of COMPRESSED file: image_used_delta_KB=$((u1-u0)) flags=$(ls -lO cc1 | awk '{print $5}') (unshared would be ~$(( 5*$(stat -f '%b' big.cmp)/2 )))"
u0=$(used); for i in 1 2 3 4 5; do cp -c big.bin "uc$i"; done; u1=$(used); echo "5 clones of plain file:      image_used_delta_KB=$((u1-u0))"
u0=$(used); for i in 1 2; do cp big.bin "pc$i"; done; u1=$(used); echo "2 plain copies (control):    image_used_delta_KB=$((u1-u0))"
h0=$(shasum -a 256 cc2 | cut -c1-16); printf 'x' >> cc1
echo "after append to cc1: cc1 flags=$(ls -lO cc1 | awk '{print $5}') allocated KB=$(( $(stat -f '%b' cc1)/2 )); cc2 unchanged: $([ "$h0" = "$(shasum -a 256 cc2 | cut -c1-16)" ] && echo yes || echo NO)"

# ---------------------------------------------------------------- E5 hashing
say "E5 hash throughput"
: > big300; for i in 1 2 3 4 5 6 7 8; do cat big.bin >> big300; done; mb=$(( $(stat -f '%z' big300)/1048576 ))
cat big300 > /dev/null
for c in "shasum -a 256" "openssl dgst -sha256" "openssl dgst -blake2b512" "md5 -q"; do s=$(now); $c big300 >/dev/null; e=$(now); echo "$c: $(echo "scale=0; $mb/($e - $s)" | bc) MB/s ($mb MB)"; done
command -v b3sum >/dev/null && { s=$(now); b3sum big300 >/dev/null; e=$(now); echo "b3sum: $(echo "scale=0; $mb/($e - $s)" | bc) MB/s"; } || echo "b3sum: not installed"
rm -f big300 pc1 pc2 uc? cc?

# ---------------------------------------------------------------- E6 lock
say "E6 cargo build lock interop (flock on target/debug/.cargo-lock)"
cd "$VOL/main" || exit 1; ls -la target/debug/.cargo-lock
perl -e 'use Fcntl ":flock"; open(F,"+<",$ARGV[0]) or die "open: $!"; flock(F,LOCK_EX) or die; sleep 6' target/debug/.cargo-lock &
sleep 1; s=$(now); cargo build --offline 2>&1 | head -2; e=$(now); echo "cargo build while lock held elsewhere: seconds=$(echo "$e - $s" | bc) (>=5 means cargo honours flock)"; wait

# ---------------------------------------------------------------- E7 applesauce
if [ "$WITH_AS" = "applesauce" ]; then
  say "E7 applesauce CLI on a target with hardlinks and clones"
  if [ "$(hostfree)" -lt 2000 ]; then echo "SKIP: host free < 2 GB"; else
    [ -x "$VOL/tools/bin/applesauce" ] || { cargo install applesauce-cli --locked --root "$VOL/tools" --target-dir "$VOL/as-target" 2>&1 | tail -2; rm -rf "$VOL/as-target"; }
    AS="$VOL/tools/bin/applesauce"
    if [ -x "$AS" ]; then
      "$AS" --version; "$AS" compress --help 2>&1 | head -25
      T="$VOL/wt_seed_p/target/debug"
      sig() { find "$T" -type f ! -name '.cargo-lock' -print0 | xargs -0 stat -f '%m %p %N' | sort | shasum | cut -c1-12; }
      echo "before: KB=$(du -sk "$T" | cut -f1) hardlinked=$(find "$T" -type f -links +1 | wc -l | tr -d ' ') mtime+mode sig=$(sig)"
      u0=$(used); s=$(now); "$AS" compress "$T" 2>&1 | tail -4; e=$(now); u1=$(used)
      echo "after:  KB=$(du -sk "$T" | cut -f1) hardlinked=$(find "$T" -type f -links +1 | wc -l | tr -d ' ') mtime+mode sig=$(sig) seconds=$(echo "$e - $s" | bc) image_used_delta_KB=$((u1-u0))"
      echo "compressed flag count in deps: $(ls -lO "$T/deps" | grep -c compressed) of $(ls "$T/deps" | wc -l | tr -d ' ')"
      echo "wt_seed_p after applesauce:  $(fresh "$VOL/wt_seed_p")"
      s=$(now); "$AS" compress "$T" >/dev/null 2>&1; e=$(now); echo "second run seconds=$(echo "$e - $s" | bc)"
    else echo "applesauce install failed"; fi
  fi
fi
say "done. image: $IMG ($(du -sk "$IMG" | cut -f1) KB on host). Detach: hdiutil detach $VOL"

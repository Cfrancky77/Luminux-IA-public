#!/bin/sh
# Notes de la release publique : toutes les sections du CHANGELOG depuis la version `tag` jusqu'à la dernière
# version publique `prev` (exclue), versions privées intermédiaires comprises.
# Usage : public-notes.sh v0.37.1 v0.33.2 Cfrancky77/Luminux-IA-public > notes.md
# (prev vide : seulement la section de `tag`)
set -eu
v="${1#v}"
p="${2:-}"
p="${p#v}"
repo="${3:-Cfrancky77/Luminux-IA-public}"
if [ -n "$p" ]; then
  echo "Nouveautés depuis la version publique $p, versions intermédiaires comprises."
  echo
fi
awk -v v="$v" -v p="$p" '
/^## \[/ {
  if (on && (p == "" || index($0, "[" p "]"))) exit
  if (index($0, "[" v "]")) on = 1
  if (on) { s = $0; sub(/^## \[/, "### ", s); sub(/\] - /, " (", s); print s ")" }
  next
}
on' CHANGELOG.md
echo
echo "Journal complet : [CHANGELOG.md](https://github.com/$repo/blob/main/CHANGELOG.md). Mode d'emploi : [LISEZMOI.md](https://github.com/$repo/blob/main/LISEZMOI.md)."

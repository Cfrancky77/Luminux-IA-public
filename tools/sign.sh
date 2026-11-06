#!/usr/bin/env bash
# Signe un fichier de Release au format du lanceur FCDI Apps : écrit <fichier>.sig (Ed25519, base64),
# puis vérifie la signature avec la clé publique de keys/signing.pub (détecte un mauvais secret).
# Usage : tools/sign.sh <étiquette-de-la-release> <fichier> <clé-privée.pem>
# Message signé (identique à FCDI-Apps/src/verify.rs) :
#   FCDI-Apps signature v1\nluminux-ia\n<tag>\n<nom du fichier>\n<sha256 hex>\n
set -euo pipefail
[ $# -eq 3 ] || { sed -n '2,6p' "$0" | sed 's/^# \?//'; exit 1; }
APP=luminux-ia; TAG="$1"; FILE="$2"; KEY="$3"
DIR="$(cd "$(dirname "$0")/.." && pwd)"
[ -f "$FILE" ] || { echo "Fichier introuvable : $FILE" >&2; exit 1; }
[ -f "$KEY" ]  || { echo "Clé privée introuvable : $KEY" >&2; exit 1; }
NAME="$(basename "$FILE")"
SHA="$(sha256sum "$FILE" | cut -d' ' -f1)"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
printf 'FCDI-Apps signature v1\n%s\n%s\n%s\n%s\n' "$APP" "$TAG" "$NAME" "$SHA" > "$TMP/msg"
openssl pkeyutl -sign -rawin -inkey "$KEY" -in "$TMP/msg" | base64 -w0 > "$FILE.sig"
# Contrôle avec une des clés publiques acceptées par le lanceur (SPKI Ed25519 = préfixe fixe + 32 octets).
base64 -d "$FILE.sig" > "$TMP/sig"
ok=0
while read -r k; do
  case "$k" in ''|'#'*) continue;; esac
  { printf '\x30\x2a\x30\x05\x06\x03\x2b\x65\x70\x03\x21\x00'; printf '%s' "$k" | base64 -d; } > "$TMP/pub.der"
  if openssl pkeyutl -verify -rawin -pubin -keyform DER -inkey "$TMP/pub.der" -in "$TMP/msg" -sigfile "$TMP/sig" >/dev/null 2>&1; then ok=1; break; fi
done < "$DIR/keys/signing.pub"
[ "$ok" = 1 ] || { echo "Signature non reconnue par keys/signing.pub : FCDI_SIGNING_KEY n'est pas la bonne clé." >&2; rm -f "$FILE.sig"; exit 1; }
echo "Signature écrite et vérifiée : $FILE.sig ($APP $TAG $NAME)"

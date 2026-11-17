#!/usr/bin/env bash
# Construit dist/ricochet-serveur_<v>_amd64.deb, ricochet-agent_<v>_amd64.deb et ricochet-depot_<v>_all.deb
# (paquet de transition vers le dépôt unique, dont dépendent les deux autres), les sommes SHA-256, puis passe lintian.
# Usage : scripts/construire-deb.sh   (les tests se lancent à part : cargo test --workspace)
set -euo pipefail

RACINE="$(cd "$(dirname "$0")/.." && pwd)"
cd "$RACINE"
export PATH="$HOME/.cargo/bin:$PATH"

VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)"
[ -n "$VERSION" ] || { echo "version introuvable dans Cargo.toml" >&2; exit 1; }
ARCH="$(dpkg --print-architecture)"
MAINTENEUR="mawena <57989130+mawena@users.noreply.github.com>"
DATE="$(date -R)"

nice -n 10 cargo build --release --locked

CONSTR="$(mktemp -d)"
trap 'rm -rf "$CONSTR"' EXIT
rm -rf dist && mkdir -p dist

# Version de la glibc exigée par un binaire (la plus haute de ses symboles).
glibc_min() {
  objdump -T "$1" | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -V | tail -1
}

# Fichiers communs : copyright et changelog Debian.
documentation() {  # dossier paquet
  local doc="$1/usr/share/doc/$2"
  install -d "$doc"
  {
    echo "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/"
    echo "Upstream-Name: Ricochet"
    echo "Source: https://github.com/mawena/Ricochet"
    echo
    echo "Files: *"
    echo "Copyright: 2026 mawena"
    echo "License: MIT"
    sed 's/^$/./; s/^/ /' LICENSE
    if [ "$2" != ricochet-depot ]; then
      echo
      echo "Comment: le binaire contient des bibliothèques Rust (licences MIT ou Apache-2.0, voir"
      echo " Cargo.lock) ; ricochet-serveur contient aussi SQLite (domaine public) et xterm.js (MIT)."
    fi
  } > "$doc/copyright"
  printf '%s (%s) stable; urgency=medium\n\n  * Version %s, voir le journal des versions :\n    https://github.com/mawena/Ricochet/blob/main/CHANGELOG.md\n\n -- %s  %s\n' \
    "$2" "$VERSION" "$VERSION" "$MAINTENEUR" "$DATE" | gzip -9n > "$doc/changelog.gz"
}

# Exceptions lintian voulues, une par ligne « étiquette [fichier] ».
exceptions() {  # dossier paquet lignes…
  local p="$1" nom="$2"; shift 2
  install -d "$p/usr/share/lintian/overrides"
  printf "$nom: %s\n" "$@" > "$p/usr/share/lintian/overrides/$nom"
}

# Script de maintenance, avec le redémarrage différé (paquets/redemarrage.sh) à la place de #REDEMARRAGE#.
script() {  # source destination unité
  python3 -I - "$1" "$2" "$3" "$RACINE/paquets/redemarrage.sh" <<'PY'
import sys
source, destination, unite, bloc = sys.argv[1:]
texte = open(source).read()
bloc = open(bloc).read().replace("UNITE", unite).rstrip("\n")
lignes = [bloc if l.strip().startswith("#REDEMARRAGE#") else l for l in texte.split("\n")]
open(destination, "w").write("\n".join(lignes))
PY
  chmod 755 "$2"
}

controle() {  # dossier paquet architecture dépendances description
  local taille
  taille="$(du -sk --exclude=DEBIAN "$1" | cut -f1)"
  {
    echo "Package: $2"
    echo "Version: $VERSION"
    echo "Architecture: $3"
    echo "Maintainer: $MAINTENEUR"
    echo "Installed-Size: $taille"
    [ -z "$4" ] || echo "Depends: $4"
    echo "Section: admin"
    echo "Priority: optional"
    echo "Homepage: https://mawena.github.io/Ricochet/"
    echo "Description: $5"
  } > "$1/DEBIAN/control"
  (cd "$1" && find . -path ./DEBIAN -prune -o -type f -printf '%P\n' | sort | xargs -r md5sum > DEBIAN/md5sums)
}

construire() {  # dossier paquet architecture
  dpkg-deb --root-owner-group -Zxz --build "$1" "dist/$2_${VERSION}_$3.deb" >/dev/null
  echo "Paquet : dist/$2_${VERSION}_$3.deb"
}

# ricochet-depot : paquet de transition (depuis 0.2.2) vers le dépôt unique https://apt.mawena.cloud.
# Plus de source APT ni de clé à lui : il dépend de mawena-depot, qui les installe, et retire
# proprement l'ancien conffile /etc/apt/sources.list.d/ricochet.sources (rm_conffile).
P="$CONSTR/ricochet-depot"
install -d "$P/DEBIAN"
documentation "$P" ricochet-depot
for s in preinst postinst postrm; do install -m 755 "paquets/depot.$s" "$P/DEBIAN/$s"; done
# Voulu : paquet de transition (vide), et rm_conffile sans debhelper.
exceptions "$P" ricochet-depot "empty-binary-package" "maintainer-script-should-not-use-dpkg-maintscript-helper"
controle "$P" ricochet-depot all "mawena-depot" "paquet de transition vers le dépôt APT unique de mawena
 Ricochet est désormais publié dans le dépôt APT unique de mawena
 (https://apt.mawena.cloud), installé par le paquet mawena-depot. Ce paquet de
 transition retire l'ancienne source APT de Ricochet et peut ensuite rester
 installé sans risque."
sed -i 's/^Section: admin$/Section: oldlibs/' "$P/DEBIAN/control"
construire "$P" ricochet-depot all

# ricochet-serveur : le relais.
P="$CONSTR/ricochet-serveur"
install -d "$P/DEBIAN" "$P/usr/bin" "$P/usr/lib/systemd/system" "$P/etc/ricochet"
install -m 755 target/release/ricochet-serveur "$P/usr/bin/"
install -m 644 deploiement/ricochet-serveur.service "$P/usr/lib/systemd/system/"
install -m 644 exemples/serveur.toml "$P/etc/ricochet/serveur.toml"
echo /etc/ricochet/serveur.toml > "$P/DEBIAN/conffiles"
documentation "$P" ricochet-serveur
install -m 644 deploiement/nginx-ricochet.conf "$P/usr/share/doc/ricochet-serveur/"
for s in postinst prerm postrm; do script "paquets/serveur.$s" "$P/DEBIAN/$s" ricochet-serveur; done
# Sans debhelper : systemctl directement, et redémarrage différé (paquets/redemarrage.sh).
exceptions "$P" ricochet-serveur "maintainer-script-calls-systemctl"
controle "$P" ricochet-serveur "$ARCH" \
  "libc6 (>= $(glibc_min target/release/ricochet-serveur)), libgcc-s1, adduser, ricochet-depot (= $VERSION)" \
  "relais de Ricochet, contrôle à distance de machines
 Ricochet commande des machines à distance, même sans adresse IP publique :
 chaque machine fait tourner un agent qui se connecte au relais par une
 connexion sortante (WebSocket sur TLS). Ce paquet installe le relais, son
 interface web (état des machines, terminal dans le navigateur, journal
 d'audit) et son service systemd. nginx assure le HTTPS devant lui."
construire "$P" ricochet-serveur "$ARCH"

# ricochet-agent : l'agent de chaque machine.
P="$CONSTR/ricochet-agent"
install -d "$P/DEBIAN" "$P/usr/bin" "$P/usr/lib/systemd/system"
install -m 755 target/release/ricochet-agent "$P/usr/bin/"
install -m 644 deploiement/ricochet-agent.service "$P/usr/lib/systemd/system/"
documentation "$P" ricochet-agent
install -m 644 exemples/agent.toml "$P/usr/share/doc/ricochet-agent/agent.toml.exemple"
for s in postinst prerm postrm; do script "paquets/agent.$s" "$P/DEBIAN/$s" ricochet-agent; done
exceptions "$P" ricochet-agent "maintainer-script-calls-systemctl"
controle "$P" ricochet-agent "$ARCH" \
  "libc6 (>= $(glibc_min target/release/ricochet-agent)), libgcc-s1, ricochet-depot (= $VERSION)" \
  "agent de Ricochet, contrôle à distance de machines
 L'agent ouvre une connexion sortante chiffrée vers le relais de Ricochet, lui
 envoie l'état de la machine (processeur, mémoire, disques) et, si sa
 configuration le permet, ouvre des terminaux à la demande de l'opérateur.
 Configuration : /etc/ricochet/agent.toml, donnée par l'interface du relais."
construire "$P" ricochet-agent "$ARCH"

(cd dist && sha256sum ./*.deb | sed 's| \./| |' > SHA256SUMS)
for d in dist/*.deb; do
  lintian --fail-on error --suppress-tags no-manual-page "$d"
done
echo "Sommes : dist/SHA256SUMS"

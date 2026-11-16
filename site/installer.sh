#!/bin/sh
# Installe Ricochet depuis son dépôt APT signé : https://mawena.github.io/Ricochet/
#   curl -fsSL https://mawena.github.io/Ricochet/installer.sh | sudo sh               (l'agent)
#   curl -fsSL https://mawena.github.io/Ricochet/installer.sh | sudo sh -s serveur    (le relais)
# Il fait exactement les trois étapes de la page : clé du dépôt, dépôt, installation. Ensuite,
# `sudo apt upgrade` apporte les nouvelles versions.
set -eu

DEPOT=https://mawena.github.io/Ricochet/apt
CLE=/usr/share/keyrings/ricochet.gpg
SOURCE=/etc/apt/sources.list.d/ricochet.sources

case "${1:-agent}" in
  agent) PAQUET=ricochet-agent ;;
  serveur|relais) PAQUET=ricochet-serveur ;;
  *) echo "Ricochet : « agent » ou « serveur » attendu, pas « $1 »." >&2; exit 2 ;;
esac
if [ "$(id -u)" -ne 0 ]; then
  echo "Ricochet : à lancer en administrateur, par exemple : curl -fsSL https://mawena.github.io/Ricochet/installer.sh | sudo sh" >&2
  exit 1
fi
if ! command -v apt-get >/dev/null 2>&1; then
  echo "Ricochet : apt-get introuvable ; Ricochet s'installe sur Ubuntu ou une distribution Debian." >&2
  exit 1
fi
if [ "$(dpkg --print-architecture)" != amd64 ]; then
  echo "Ricochet : seuls les processeurs amd64 (x86-64) ont un paquet pour l'instant." >&2
  exit 1
fi

telecharger() {   # adresse fichier
  if command -v curl >/dev/null 2>&1; then curl -fsSL "$1" -o "$2"
  elif command -v wget >/dev/null 2>&1; then wget -qO "$2" "$1"
  else echo "Ricochet : il faut curl ou wget." >&2; exit 1
  fi
}

echo "1/3 Clé du dépôt -> $CLE"
telecharger "$DEPOT/ricochet.gpg" "$CLE.tmp" && chmod 644 "$CLE.tmp" && mv "$CLE.tmp" "$CLE"
echo "2/3 Dépôt -> $SOURCE"
telecharger "$DEPOT/ricochet.sources" "$SOURCE.tmp" && chmod 644 "$SOURCE.tmp" && mv "$SOURCE.tmp" "$SOURCE"
echo "3/3 Installation de $PAQUET"
apt-get update
apt-get install -y "$PAQUET"
echo
if [ "$PAQUET" = ricochet-agent ]; then
  echo "Agent installé. Écrire /etc/ricochet/agent.toml (droits 600) avec la configuration donnée"
  echo "par l'interface du relais (« Ajouter une machine »), puis : sudo systemctl start ricochet-agent"
else
  echo "Relais installé, à l'écoute sur 127.0.0.1:7870. Mettre nginx devant (modèle :"
  echo "/usr/share/doc/ricochet-serveur/nginx-ricochet.conf), puis créer un compte :"
  echo "  sudo -u ricochet ricochet-serveur compte ajouter <nom>"
fi

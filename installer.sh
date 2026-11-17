#!/bin/sh
# Installe Ricochet depuis le dépôt APT unique de mawena : https://apt.mawena.cloud/
#   curl -fsSL https://mawena.github.io/Ricochet/installer.sh | sudo sh               (l'agent)
#   curl -fsSL https://mawena.github.io/Ricochet/installer.sh | sudo sh -s serveur    (le relais)
# Il lance l'installateur du dépôt unique avec le bon paquet ; c'est l'équivalent de
#   curl -fsSL https://apt.mawena.cloud/installer.sh | sudo sh -s -- ricochet-agent
# Ensuite, `sudo apt upgrade` apporte les nouvelles versions.
set -eu

INSTALLATEUR=https://apt.mawena.cloud/installer.sh

case "${1:-agent}" in
  agent) PAQUET=ricochet-agent ;;
  serveur|relais) PAQUET=ricochet-serveur ;;
  *) echo "Ricochet : « agent » ou « serveur » attendu, pas « $1 »." >&2; exit 2 ;;
esac
if [ "$(id -u)" -ne 0 ]; then
  echo "Ricochet : à lancer en administrateur, par exemple : curl -fsSL $INSTALLATEUR | sudo sh -s -- $PAQUET" >&2
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

SCRIPT="$(mktemp)"
trap 'rm -f "$SCRIPT"' EXIT
if command -v curl >/dev/null 2>&1; then curl -fsSL "$INSTALLATEUR" -o "$SCRIPT"
elif command -v wget >/dev/null 2>&1; then wget -qO "$SCRIPT" "$INSTALLATEUR"
else echo "Ricochet : il faut curl ou wget." >&2; exit 1
fi
sh "$SCRIPT" "$PAQUET"
echo
if [ "$PAQUET" = ricochet-agent ]; then
  echo "Agent installé. Écrire /etc/ricochet/agent.toml (droits 600) avec la configuration donnée"
  echo "par l'interface du relais (« Ajouter une machine »), puis : sudo systemctl start ricochet-agent"
else
  echo "Relais installé, à l'écoute sur 127.0.0.1:7870. Mettre nginx devant (modèle :"
  echo "/usr/share/doc/ricochet-serveur/nginx-ricochet.conf), puis créer un compte :"
  echo "  sudo -u ricochet ricochet-serveur compte ajouter <nom>"
fi

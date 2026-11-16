# Journal des versions

Toutes les évolutions notables de Ricochet. Format inspiré de
[Keep a Changelog](https://keepachangelog.com/fr/1.1.0/), versions en
[SemVer](https://semver.org/lang/fr/).

## [Non publié]

### Ajouté

- Protocole agent ↔ relais : messages JSON dans un WebSocket, version 1.
- Relais `ricochet-serveur` : connexion des agents avec jeton par machine, remplacement d'une
  connexion par la suivante, pings et détection des agents silencieux, registre des machines.
- API de l'opérateur protégée par jeton : `GET /api/machines`,
  `POST /api/machines/{id}/actualiser` ; `GET /sante`.
- Agent `ricochet-agent` : connexion sortante (marche derrière une box ou un NAT), envoi de
  l'état de la machine (processeur, charge, mémoire, disques, durée d'allumage), reconnexion
  automatique avec attente croissante ; commande `ricochet-agent etat`.
- Commande `ricochet-serveur jeton` pour créer un jeton et son empreinte.
- Exemples de configuration, services systemd et configuration nginx.

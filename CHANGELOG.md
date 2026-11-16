# Journal des versions

Toutes les évolutions notables de Ricochet. Format inspiré de
[Keep a Changelog](https://keepachangelog.com/fr/1.1.0/), versions en
[SemVer](https://semver.org/lang/fr/).

## [Non publié]

### Ajouté

- Interface web adaptée au téléphone : machines en ligne ou non, processeur, mémoire, charge,
  disques, actualisation ; ajout d'une machine (jeton affiché une seule fois) et révocation ;
  journal d'audit. Intégrée au binaire du relais, installable sur l'écran d'accueil.
- Comptes opérateur : mot de passe Argon2id et code TOTP (RFC 6238) à usage unique ; commande
  `ricochet-serveur compte ajouter|liste|supprimer` avec QR code dans le terminal.
- Sessions par cookie `__Host-`, `HttpOnly`, `Secure`, `SameSite=Strict` ; limitation des
  essais de connexion par adresse et par compte ; protection contre les requêtes forgées
  (en-tête `X-Ricochet`) ; en-têtes de sécurité (CSP stricte, cadres interdits).
- Base SQLite du relais : comptes, sessions, machines, journal d'audit (connexions, machines
  ajoutées ou révoquées, agents connectés ou refusés).
- API : `POST /api/connexion`, `POST /api/deconnexion`, `GET /api/moi`, `POST /api/machines`,
  `DELETE /api/machines/{id}`, `GET /api/audit`.

### Modifié

- Les machines se déclarent dans l'interface (base de données) et non plus dans
  `serveur.toml` : les blocs `[[agents]]` sont refusés. `jeton_operateur_sha256` devient
  facultatif (accès des scripts à l'API).
- Le service systemd du relais tourne sous l'utilisateur `ricochet` avec
  `StateDirectory=ricochet`, pour partager la base avec la commande `compte`.

- Protocole agent ↔ relais : messages JSON dans un WebSocket, version 1.
- Relais `ricochet-serveur` : connexion des agents avec jeton par machine, remplacement d'une
  connexion par la suivante, pings et détection des agents silencieux, registre des machines.
- API de l'opérateur : `GET /api/machines`,
  `POST /api/machines/{id}/actualiser` ; `GET /sante`.
- Agent `ricochet-agent` : connexion sortante (marche derrière une box ou un NAT), envoi de
  l'état de la machine (processeur, charge, mémoire, disques, durée d'allumage), reconnexion
  automatique avec attente croissante ; commande `ricochet-agent etat`.
- Commande `ricochet-serveur jeton` pour créer un jeton et son empreinte.
- Exemples de configuration, services systemd et configuration nginx.

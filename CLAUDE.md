# Ricochet — consignes pour Claude

Contrôle à distance des machines de Charles : un **agent** par machine se connecte en WebSocket
sortant (TLS) au **relais** sur le VPS ; l'opérateur commande depuis une interface web (téléphone).
Cibles : son PC (sans IP publique), le VPS, des machines joignables par nom de domaine.

Choix validés par Charles le 16/11/2026 : tout en **Rust** ; fonctions voulues = état, terminal,
fichiers, actions rapides, machines par SSH (sans agent), bureau graphique (en dernier).

## Où on en est
- Phases 0 à 4 faites (protocole, relais, agent, interface web, comptes Argon2 + TOTP, audit, fichiers,
  terminal, paquets `.deb` + dépôt APT). Dernière release **0.3.0** (17/11/2026, tunnels pour SSH et rsync ; 0.2.2 : transition vers le dépôt APT unique https://apt.mawena.cloud ; `ricochet-depot` = paquet de transition). **1.8 faite** :
  relais en ligne sur https://ricochet.mawena.cloud (paquet, vhost `/etc/nginx/sites-available/ricochet`,
  certificat certbot, compte `mawena`). **1.9 faite** : agents `mawena-cloud` (VPS) et `pc-bureau` en ligne. Phase 4 (fichiers) faite. Ensuite phase 5 (supervision, actions rapides). Voir `ROADMAP.md`.
- Releases : `scripts/construire-deb.sh`, `gh release create` avec `dist/*`, puis
  `scripts/publier-apt.sh dist/*.deb` (site https://mawena.github.io/Ricochet/).

## Structure
- `crates/ricochet-protocole` : messages JSON étiquetés `type`, `VERSION_PROTOCOLE = 1`.
- `crates/ricochet-serveur` : relais axum 0.8 (`/` interface, `/sante`, `/agent` en WebSocket,
  `/api` par cookie de session ou jeton `Bearer` facultatif) ; config TOML `deny_unknown_fields` ;
  base SQLite (rusqlite, `base.rs`) pour comptes, sessions, machines, audit ; Argon2id + TOTP
  maison (`totp.rs`, vecteurs RFC) ; limiteur d'essais en mémoire ; CSRF par en-tête `X-Ricochet` ;
  jetons stockés en SHA-256 ; registre avec numéros de session. Interface dans `web/`
  (`include_str!`, aucun `innerHTML` avec des données reçues, CSP stricte).
- Le relais tourne sous l'utilisateur système `ricochet` : comptes créés par
  `sudo -u ricochet ricochet-serveur compte ajouter <nom>`.
- `crates/ricochet-agent` : connexion sortante (tokio-tungstenite, rustls), reconnexion 1→60 s,
  300 s après un refus ; état via sysinfo 0.40 (la plupart des fonctions renvoient `Result`).
- Terminal : agent `terminaux.rs` (portable-pty, un fil de lecture et un d'écriture par terminal,
  file bornée), relais `terminal.rs` (WebSocket opérateur, `Origin` vérifiée, binaire = octets,
  texte = taille) ; numéros de terminal attribués par le relais, liés à la session de l'agent.
  xterm.js 6.0 copié dans `web/vendor/` (provenance et empreintes dans `LISEZMOI.md`) ; la CSP
  permet `style-src 'unsafe-inline'` pour lui.
- Fichiers : agent `fichiers.rs` (un fil dédié par session, fsuid/fsgid/setgroups propres au
  fil si `[fichiers] utilisateur`, envoi dans `<nom>.ricochet-partiel` puis renommage), relais
  `fichiers.rs` (requêtes numérotées dans le registre, réponse par oneshot, téléchargement en
  flux avec `Range`) ; capacité annoncée par `InfoMachine.fichiers`, protocole toujours en v1.
- Tunnels (0.3.0) : agent `tunnels.rs` (TCP vers 127.0.0.1, ports de `[tunnel]`), relais
  `tunnel.rs` ; un tunnel partage numéros et messages des terminaux (`Entree`, `Sortie`,
  `FermerTerminal`, `TerminalFerme`). Client `tunnel_client.rs` (`ricochet-agent tunnel`, pour
  `ssh -o ProxyCommand`). `jeton_tunnel_sha256` : jeton limité (routes de `ROUTES_TUNNEL`).
- `crates/ricochet-agent/tests/bout_en_bout.rs` : vrai relais + vrai agent sur 127.0.0.1.
- Essai de l'interface sans écran : Chromium de Playwright dans `~/.cache/ms-playwright`
  (`chrome-headless-shell --remote-debugging-pipe`, piloté en CDP par un petit script Python).
- Paquets : `scripts/construire-deb.sh` (dpkg-deb direct, sans debhelper ; `ricochet-serveur`,
  `ricochet-agent` en amd64, `ricochet-depot` en all, paquet de transition depuis 0.2.2 (`Depends:
  mawena-depot`, `rm_conffile` de l'ancienne source par `paquets/depot.*`), dont les deux dépendent) ; scripts de maintenance dans `paquets/` (`#REDEMARRAGE#` remplacé par
  `redemarrage.sh` : redémarrage différé après apt, pour ne pas tuer un apt lancé depuis un
  terminal Ricochet). `scripts/publier-apt.sh` (copie de celui de CCS) publie `dist/*.deb` et
  `site/` sur gh-pages. Clé APT `EF71E945…51C91F12` dans le trousseau du VPS (`paquets/apt/LISEZMOI`).
- `deploiement/` : services systemd et vhost nginx (WebSocket, `X-Real-IP`). `exemples/` : configs.

## Façon de travailler (en plus de `~/.claude/CLAUDE.md`)
- Rust est dans `~/.cargo/bin` (hors PATH par défaut) : `export PATH="$HOME/.cargo/bin:$PATH"`.
- Compiler/tester avec `nice -n 10`, un seul travail lourd à la fois. Pendant le travail :
  `cargo test -p <crate>` ; suite complète + `cargo clippy --all-targets` + `cargo fmt` avant release.
- git flow : `main`, `develop`, `feature/*` ; préfixe de tag `v`. Commits en français, au nom de
  `mawena` (config locale du dépôt), **sans `Co-Authored-By`**.
- Déploiement : ne jamais toucher nginx/lnmp/production soi-même ; donner à Charles les commandes
  `sudo …` exactes (DNS du sous-domaine prévu `ricochet.mawena.cloud`, certificat, vhost) et attendre.
- Tenir à jour `README.md`, `ROADMAP.md` (cases cochées) et `CHANGELOG.md` (« Non publié »).

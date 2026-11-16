# Ricochet — consignes pour Claude

Contrôle à distance des machines de Charles : un **agent** par machine se connecte en WebSocket
sortant (TLS) au **relais** sur le VPS ; l'opérateur commande depuis une interface web (téléphone).
Cibles : son PC (sans IP publique), le VPS, des machines joignables par nom de domaine.

Choix validés par Charles le 16/11/2026 : tout en **Rust** ; fonctions voulues = état, terminal,
fichiers, actions rapides, machines par SSH (sans agent), bureau graphique (en dernier).

## Où on en est
- Phases 0, 1 et 2 faites (protocole, relais, agent, API, interface web, comptes Argon2 + TOTP,
  sessions, ajout/révocation de machines, audit) sauf **1.8** (déployer le relais sur le VPS) et
  **1.9** (installer les agents). Phase 3 : terminal fait (3.1 à 3.3) ; restent **3.4** (paquets
  `.deb` + dépôt APT) et **3.5** (release 0.1.0). Voir `ROADMAP.md`, la source de vérité du plan.
- **Pas encore de release** : la première (0.1.0, `.deb` + dépôt APT signé selon
  `~/.claude/regles/deb-apt.md`) est prévue à la fin de la phase 3 (terminal), pas avant.

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
- `crates/ricochet-agent/tests/bout_en_bout.rs` : vrai relais + vrai agent sur 127.0.0.1.
- Essai de l'interface sans écran : Chromium de Playwright dans `~/.cache/ms-playwright`
  (`chrome-headless-shell --remote-debugging-pipe`, piloté en CDP par un petit script Python).
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

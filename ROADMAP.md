# Feuille de route de Ricochet

Contrôle à distance de mes machines : le PC sans IP publique, le VPS `mawena.cloud`, et des
machines joignables par un nom de domaine. Un agent par machine se connecte au relais du VPS ;
l'opérateur commande tout depuis une interface web, téléphone compris.

Règle : cocher une tâche **dès** qu'elle est terminée.

## Phase 0 : fondations
- [x] 0.1 Dépôt git flow (`main` / `develop`), dépôt GitHub
- [x] 0.2 Workspace Cargo : `ricochet-protocole`, `ricochet-serveur`, `ricochet-agent`
- [x] 0.3 README, feuille de route, journal des versions, licence MIT

## Phase 1 : relier les machines au relais
- [x] 1.1 Protocole versionné (bonjour, bienvenue, refus, état, demande d'état)
- [x] 1.2 Relais : connexion des agents, jeton par machine (empreinte SHA-256, comparaison à temps constant)
- [x] 1.3 Relais : registre des machines, remplacement de connexion, pings, agents silencieux
- [x] 1.4 API opérateur par jeton : liste des machines, demande d'état
- [x] 1.5 Agent : connexion sortante, état de la machine, reconnexion avec attente croissante
- [x] 1.6 Test de bout en bout (vrai relais, vrai agent)
- [x] 1.7 Services systemd et configuration nginx d'exemple
- [x] 1.8 Déployer le relais sur le VPS (`ricochet.mawena.cloud`, DNS + certificat + nginx) — commandes `sudo` par Charles
- [x] 1.9 Agents sur le VPS et sur le PC, vérifier qu'ils apparaissent connectés

## Phase 2 : interface web et accès sécurisé
- [x] 2.1 Comptes opérateur : mot de passe (Argon2) et double authentification TOTP
- [x] 2.2 Sessions par cookie sécurisé, limitation des essais de connexion
- [x] 2.3 Interface web adaptée au téléphone : machines, état, en ligne ou non
- [x] 2.4 Ajout d'une machine depuis l'interface : jeton créé et affiché une seule fois, révocation
- [x] 2.5 Journal d'audit de toutes les actions

## Phase 3 : terminal à distance → release 0.1.0
- [x] 3.1 Agent : shell dans un pseudo-terminal (PTY), plusieurs sessions, redimensionnement
- [x] 3.2 Relais : tunnel opérateur ↔ agent pour le terminal
- [x] 3.3 Interface : terminal dans le navigateur (xterm.js servi par le relais)
- [x] 3.4 Paquets `.deb` (`ricochet-serveur`, `ricochet-agent`) et dépôt APT signé (voir Gagarine)
- [x] 3.5 Release 0.1.0

## Phase 4 : fichiers → release 0.2.0
- [x] 4.1 Parcourir les dossiers d'une machine
- [x] 4.2 Envoyer et recevoir des fichiers, par morceaux, avec reprise

## Transition vers le dépôt unique → release 0.2.2
- [x] `ricochet-depot` en paquet de transition (`Depends: mawena-depot`, `rm_conffile` de l'ancienne source)
- [x] README, site et commande d'installation de l'interface vers https://apt.mawena.cloud

## Phase 4 bis : tunnels (pour le calcul réparti de CCS) → release 0.3.0

Demande de Charles du 17/11/2026 : CCS doit pouvoir se servir de la puissance de toutes les
machines (exemple : un build Flutter sur `pc-bureau` quand Flutter n'est pas sur le VPS). Le
premier plan (Ricochet lance lui-même les commandes et transporte les dossiers) a été remplacé le
même jour, avec l'accord de Charles (« fais les meilleurs choix »), par plus simple et plus
solide : Ricochet ouvre un **tunnel** vers le serveur SSH de la machine, et CCS se sert de `ssh`
et `rsync` (envois différentiels, sortie en direct, codes de retour, arrêt : déjà faits et
éprouvés). Le droit reste donné machine par machine (section `[tunnel]` de l'agent), et chaque
tunnel est dans l'audit. La partie CCS est dans sa feuille de route (phase 21).

- [x] 4b.1 Protocole : `OuvrirTunnel` (numéros et octets partagés avec les terminaux), ports annoncés par `InfoMachine.tunnels`
- [x] 4b.2 Agent : section `[tunnel]` (`ports`, `maximum`), connexion à 127.0.0.1 seulement
- [x] 4b.3 Relais : `GET /api/machines/{id}/tunnel?port=N` (WebSocket), limite par machine, audit (`tunnel_ouvert`, `tunnel_refuse`, `tunnel_ferme`)
- [x] 4b.4 Relais : `jeton_tunnel_sha256`, jeton limité à la liste des machines et aux tunnels
- [x] 4b.5 Client : `ricochet-agent tunnel <machine>` (pour `ssh -o ProxyCommand`), `~/.config/ricochet/client.toml`
- [x] 4b.6 Tests de bout en bout, essai réel (`ssh` et `rsync` de 50 Mo par un relais local), documentation
- [x] 4b.7 Release 0.3.0, dépôt APT
- [ ] 4b.8 Par Charles : mettre à jour relais et agents, créer le jeton des tunnels, activer `[tunnel]` et SSH sur `pc-bureau`

## Mode Portier de l'interface web → release 0.4.0

Demande de Charles du 17/11/2026 : brancher l'interface web sur Portier (connexion unique de
mawena.cloud, contrat dans `Portier/docs/integration.md`), sans toucher aux agents ni aux clients
automatiques.

- [x] Section `[portier]` (`actif`, `utilisateurs`, `url`, `deconnexion`), désactivée par défaut
- [x] `GET /` : session ouverte par l'en-tête `X-Portier-Utilisateur` (127.0.0.1/::1, un seul en-tête, nom dans la liste)
- [x] En-tête ignoré par `/agent`, `/api/…` et `/sante`
- [x] Bouton « Se connecter via Portier », déconnexion vers Portier (`GET /api/portier`)
- [x] Tests (accepté, nom absent, IP non locale, option désactivée, points des agents), documentation, variante nginx
- [ ] Par Charles : installer la 0.4.0, activer `[portier]`, protéger `location = /` dans nginx

## Messages des machines et notifications Web Push

Demande de Charles du 21/11/2026 : fonction générale « messages des machines » (premier client :
CofiTicket sur le PC, ensuite CCS). Contrat dans `CofiTicket/docs/contrat-ricochet.md`.

- [x] M.1 Protocole (toujours en v1) : `InfoMachine.messages`, message, détail, réponse, accusés
- [x] M.2 Agent : `[messages]`, socket Unix (`SO_PEERCRED`), validations, file persistante 0700/0600, renvoi à la reconnexion, dédoublonnage `(source, cle)`, purge
- [x] M.3 Agent : `ricochet-agent message envoyer|reponses|etat`, `RuntimeDirectory` et `StateDirectory` du service
- [x] M.4 Relais : tables `messages`, `abonnements_push`, `reglages` ; routes `/api/messages…`, `/api/push/…` ; limite de débit ; purge ; audit
- [x] M.5 Web Push : clés VAPID en base, JWT ES256, chiffrement `aes128gcm` (RFC 8291, vecteur de l'annexe A), client HTTPS rustls/ring, liste d'hôtes autorisés, abonnements périmés retirés
- [x] M.6 Interface : installable (manifeste, icônes, service worker), vue Messages (non lus, détail à chasse fixe, Copier, boutons), notifications (activer, appareils, essai), ouverture au toucher
- [x] M.7 Tests : unitaires, bout en bout (vrai relais, vrai agent, faux service de push qui déchiffre), essai de l'interface avec Chromium sans écran
- [x] M.8 Documentation (README, CHANGELOG, CLAUDE.md), `cargo clippy`, paquets
- [x] M.9 Release 0.5.0 faite. Reste à Charles : relecture, release, installer relais et agents, `[push] url_publique` sur le relais, `[messages]` sur l'agent du PC, activer les notifications sur chaque appareil

## Phase 5 : supervision et actions rapides
- [ ] 5.1 Services systemd : liste, état, démarrer, arrêter, redémarrer
- [ ] 5.2 Redémarrer ou éteindre la machine (avec confirmation)
- [ ] 5.3 Historique de l'état et alertes (machine hors ligne, disque presque plein)

## Phase 6 : machines sans agent (par nom de domaine)
- [ ] 6.1 Déclarer une machine joignable en SSH (hôte, port, utilisateur, clé)
- [ ] 6.2 Le relais s'y connecte en SSH : terminal, état et fichiers par la même interface

## Phase 7 : bureau graphique à distance
- [ ] 7.1 Choisir la technique (capture Wayland/X11 + WebRTC, ou relais VNC)
- [ ] 7.2 Écran et souris/clavier dans le navigateur

# Journal des versions

Toutes les évolutions notables de Ricochet. Format inspiré de
[Keep a Changelog](https://keepachangelog.com/fr/1.1.0/), versions en
[SemVer](https://semver.org/lang/fr/).

## [Non publié]

## [0.6.0] - 2026-11-21

### Ajouté
- **Applications web des machines** : une application qui écoute sur 127.0.0.1 (par exemple la
  fenêtre de CofiTicket) s'ouvre depuis l'interface de Ricochet, derrière le mot de passe et le
  TOTP. Agent : sections `[applis.<nom>]` (`port`, `libelle`, `entetes` ajoutés à chaque
  requête, pour qu'un jeton reste sur la machine) ; seuls les ports déclarés, seulement sur
  127.0.0.1 ; sans section, rien n'est joignable. Relais : route
  `/api/machines/{id}/applis/{nom}/{chemin}` (session de l'opérateur) qui transmet méthode,
  chemin, requête, corps (4 Mio) et quelques en-têtes, et renvoie la réponse en flux (flux
  d'évènements SSE au fil de l'eau) ; jamais le cookie de session ni `Authorization` vers
  l'application, `Set-Cookie` retirés, redirections réécrites sous le préfixe, fermeture de la
  page = connexion à l'application coupée. Rien n'est enregistré (ni corps, ni réponses, ni
  chemins) ; l'audit note `appli_ouverte` et `appli_refusee`. Les requêtes qui modifient
  quelque chose exigent un `Origin` égal au `Host` du relais (ou `X-Ricochet`).
- Interface : la carte d'une machine liste ses applications ; un lien les ouvre en plein écran.
- Protocole (toujours en version 1, champs facultatifs) : `InfoMachine.applis`,
  `RequeteAppli` et `AnnulerAppli` (relais vers agent), `EnteteAppli`, `CorpsAppli` et
  `FinAppli` (agent vers relais). Un agent plus ancien n'annonce aucune application : le
  relais répond `404` ; un relais plus ancien ignore le champ.
- La politique CSP d'une application web, si elle en envoie une, prime sur celle de l'interface.

## [0.5.0] - 2026-11-21

### Ajouté
- **Messages des machines** : un programme local envoie un message à l'agent de sa machine
  (socket Unix `/run/ricochet-agent/messages.sock`, section `[messages]` de l'agent :
  `utilisateurs`, `socket`, `conservation_jours`, `dossier_etat`) ; contrôle d'accès par
  `SO_PEERCRED`, validations (source, cle, titre ≤ 80, texte ≤ 200, 3 boutons, détail ≤ 64 Kio),
  dédoublonnage par `(source, cle)`, file d'attente sur disque (0700/0600) renvoyée à chaque
  reconnexion, purge après `conservation_jours`. Sous-commande
  `ricochet-agent message envoyer|reponses|etat`.
- Protocole (toujours en version 1, champs facultatifs) : `InfoMachine.messages`, `Message`,
  `Detail`, `ReponseRecue` (agent vers relais), `MessageRecu`, `DemandeDetail`, `Reponse`
  (relais vers agent).
- Relais : tables `messages` (sans le détail), `abonnements_push`, `reglages` ; routes
  `/api/messages` (liste, un message, détail demandé à l'agent, réponse à un bouton une seule
  fois, gardée si l'agent est hors ligne), `/api/push/cle`, `/api/push/abonnements`,
  `/api/push/essai` ; limite de débit par machine ; purge ; journal d'audit
  (`message_recu`, `message_detail`, `message_reponse`, `push_abonnement_ajoute`,
  `push_abonnement_retire`, `push_essai`) ; le jeton des tunnels n'y a pas accès. Sections
  `[messages]` et `[push]` du relais.
- **Web Push** : clés VAPID P-256 créées au premier démarrage, JWT ES256 (RFC 8292), chiffrement
  `aes128gcm` (RFC 8291, RustCrypto, sans OpenSSL), client HTTPS rustls/ring, liste d'hôtes de
  push autorisés (`[push] hotes_autorises`), abonnements périmés (404, 410) retirés.
- Interface installable (manifeste, icônes PNG 192, 512 et apple-touch-icon, service worker
  `/sw.js`), vue « Messages » (non lus, détail à chasse fixe avec « Copier », boutons de
  réponse), activation des notifications sur l'appareil, liste des appareils, notification
  d'essai ; toucher la notification ouvre le message. CSP : `worker-src 'self'` en plus.
- Service de l'agent : `RuntimeDirectory=ricochet-agent` (0755) et `StateDirectory=ricochet-agent` (0700).

## [0.4.1] - 2026-11-17

### Corrigé
- Mode Portier facultatif : après une connexion par mot de passe, « Se déconnecter » ne renvoie
  plus vers la page de Portier ; seule une session ouverte par Portier y renvoie
  (`POST /api/deconnexion` répond `{"portier": bool}`).

## [0.4.0] - 2026-11-17

### Ajouté

- **Mode Portier** de l'interface web (section `[portier]` : `actif`, `utilisateurs`, `url`,
  `deconnexion`), désactivé par défaut. La page `/` ouvre une session sans mot de passe ni TOTP
  quand nginx, sur la même machine (127.0.0.1 ou ::1), transmet un seul en-tête
  `X-Portier-Utilisateur` dont la valeur est dans `utilisateurs` ; journal : `connexion_portier`.
- L'en-tête est ignoré partout ailleurs : `/agent`, toute l'API (`/api/…`, cookie ou jeton) et
  `/sante`.
- Page de connexion : bouton « Se connecter via Portier » vers
  `<url>/connexion?retour=<adresse de l'interface>` (reconstruite avec `Host` et
  `X-Forwarded-Proto`). Déconnexion : renvoi vers `deconnexion` (`<url>/` par défaut).
- Route `GET /api/portier` (sans connexion) ; variante nginx dans `deploiement/nginx-ricochet.conf`.

## [0.3.0] - 2026-11-17

### Ajouté

- **Tunnels** : `ricochet-agent tunnel <machine>` relie son entrée et sa sortie standard à un
  port local d'une autre machine, en passant par le relais. Avec le port 22, SSH marche vers un
  PC sans IP publique (`ssh -o ProxyCommand="ricochet-agent tunnel %h" …`), donc aussi `rsync`
  et `scp`. C'est la base du calcul réparti de CCS (lancer un build sur une autre machine).
- Agent : section `[tunnel]` (`ports`, `maximum`), absente par défaut. Seuls les ports listés
  sont joignables, et seulement sur 127.0.0.1.
- Relais : route `GET /api/machines/{id}/tunnel?port=N` (WebSocket) ; ouvertures, refus et
  fermetures de tunnels dans le journal d'audit.
- Relais : `jeton_tunnel_sha256`, un jeton qui ne permet que de lister les machines et d'ouvrir
  des tunnels (ni terminal, ni fichiers, ni gestion des machines).
- Client : fichier `~/.config/ricochet/client.toml` (`relais`, `jeton`).

## [0.2.2] - 2026-11-17

### Modifié

- Passage au dépôt APT unique de mawena (<https://apt.mawena.cloud>) : `ricochet-depot` devient
  un paquet de transition qui dépend de `mawena-depot` et ne fournit plus ni la source
  `/etc/apt/sources.list.d/ricochet.sources` (retirée proprement par `rm_conffile`) ni la clé
  `/usr/share/keyrings/ricochet.gpg`. Un `sudo apt upgrade` suffit pour passer au nouveau dépôt.
- Installation : README, page du site et commande « Ajouter une machine » de l'interface passent
  par `curl -fsSL https://apt.mawena.cloud/installer.sh | sudo sh -s -- ricochet-agent` ;
  l'ancien `installer.sh` du site lance désormais celui du dépôt unique.

## [0.2.1] - 2026-11-16

### Corrigé

- Fichiers : après une erreur (dossier refusé ou introuvable), le champ du chemin revient au
  dossier affiché, celui où « Envoyer ici » écrit, au lieu de garder le chemin refusé.
- Fichiers : le bouton « Envoyer ici » ne passe plus sur deux lignes sur un petit écran.

## [0.2.0] - 2026-11-16

### Ajouté

- Fichiers : bouton « Fichiers » sur les machines qui l'acceptent (section `[fichiers]` de
  l'agent) pour parcourir les dossiers, télécharger un fichier (reprise par l'en-tête
  `Range`) et en envoyer, par morceaux de 512 Kio : un envoi interrompu reprend où il s'est
  arrêté. Avec `utilisateur = "…"`, l'agent n'a sur les fichiers que les droits de cet
  utilisateur (fsuid propre au fil des fichiers). Téléchargements et envois au journal d'audit.
- « Ajouter une machine » active aussi les fichiers quand un utilisateur est indiqué.

## [0.1.2] - 2026-11-16

### Modifié

- « Ajouter une machine » affiche une commande complète à coller sur la machine (installation
  de l'agent, configuration en droits 600, démarrage) et demande l'utilisateur du terminal,
  au lieu des seules lignes de configuration.

## [0.1.1] - 2026-11-16

### Corrigé

- L'agent plantait au premier essai de connexion à un relais en `wss://` (aucun fournisseur
  de chiffrement compilé pour rustls) : il embarque maintenant `ring`. Un test couvre la
  connexion TLS.

## [0.1.0] - 2026-11-16

### Ajouté

- Paquets `.deb` (`ricochet-serveur`, `ricochet-agent`, `ricochet-depot`) et dépôt APT signé
  sur <https://mawena.github.io/Ricochet/apt>, avec un script d'installation en une ligne.
  Une mise à jour ne redémarre les services qu'une fois apt terminé, pour ne pas couper le
  terminal Ricochet qui l'a lancée.
- Terminal à distance : shell dans un pseudo-terminal sur la machine (agent, section
  `[terminal]` de `agent.toml`, désactivé par défaut), relayé par le relais
  (`GET /api/machines/{id}/terminal`, WebSocket, origine vérifiée) jusqu'au navigateur
  (xterm.js 6.0 intégré, chargé à la demande). Redimensionnement, plusieurs terminaux par
  machine (8 au plus), barre de touches pour le téléphone (Échap, Tab, Ctrl, flèches).
  Ouvertures et fermetures au journal d'audit ; fermeture à l'expiration de la session, à la
  déconnexion ou à la révocation de la machine.
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
- La CSP autorise les styles en ligne (xterm.js crée ses balises `<style>`) ; les scripts
  restent limités aux fichiers du relais.


[Non publié]: https://github.com/mawena/Ricochet/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/mawena/Ricochet/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/mawena/Ricochet/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/mawena/Ricochet/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/mawena/Ricochet/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/mawena/Ricochet/releases/tag/v0.1.0

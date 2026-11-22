# Ricochet

Site et installation : <https://mawena.github.io/Ricochet/>

Contrôle à distance de mes machines, même celles qui n'ont **pas d'adresse IP publique**.

Chaque machine fait tourner un **agent** qui ouvre lui-même une connexion sortante chiffrée
(WebSocket sur TLS) vers le **relais**, installé sur un serveur public. La connexion
« ricoche » sur le relais : pas de port à ouvrir, ça marche derrière une box ou un NAT.
L'opérateur consulte et commande les machines par le relais.

```
  PC (sans IP publique) ──agent──┐
  VPS (IP publique) ─────agent───┼──►  relais (VPS, HTTPS)  ◄── opérateur (navigateur, API)
  machine.exemple.org ───agent───┘
```

État actuel : les agents se connectent au relais et envoient l'état de leur machine
(processeur, charge, mémoire, disques, durée d'allumage). L'opérateur se connecte à l'interface
web (mot de passe + code TOTP), voit ses machines en ligne ou non, en ajoute ou en révoque, et
consulte le journal d'audit, reçoit les **messages de ses machines** en notification sur son
téléphone, ouvre un **terminal** sur une machine depuis le navigateur
(xterm.js, avec une barre de touches pour le téléphone : Échap, Tab, Ctrl, flèches) et gère ses
**fichiers** (parcourir, télécharger, envoyer avec reprise). La suite est dans la
[feuille de route](ROADMAP.md).

## Installation

Paquets dans le dépôt APT signé unique de mawena : <https://apt.mawena.cloud/> (Ubuntu 24.04,
amd64). Page du projet : <https://mawena.github.io/Ricochet/>.

En une ligne, l'agent sur chaque machine, ou le relais sur le serveur public :

```sh
curl -fsSL https://apt.mawena.cloud/installer.sh | sudo sh -s -- ricochet-agent      # agent
curl -fsSL https://apt.mawena.cloud/installer.sh | sudo sh -s -- ricochet-serveur    # relais
```

Pas à pas (ce que fait le script) :

```sh
sudo curl -fsSLo /usr/share/keyrings/mawena-depot.gpg https://apt.mawena.cloud/mawena-depot.gpg
sudo curl -fsSLo /etc/apt/sources.list.d/mawena-depot.sources https://apt.mawena.cloud/mawena-depot.sources
sudo apt update
sudo apt install mawena-depot ricochet-agent      # ou ricochet-serveur
```

Mises à jour : `sudo apt update && sudo apt upgrade`. Lancée depuis un terminal Ricochet, la
mise à jour va jusqu'au bout : le service n'est redémarré qu'une fois apt terminé (le
terminal se ferme alors, il suffit de le rouvrir).

Depuis la 0.2.2, une machine qui suivait l'ancien dépôt du projet
(`https://mawena.github.io/Ricochet/apt`) passe d'elle-même au dépôt unique au premier
`sudo apt upgrade` : `ricochet-depot` devient un paquet de transition qui installe
`mawena-depot` et retire l'ancienne source `/etc/apt/sources.list.d/ricochet.sources`.

Trois paquets : `ricochet-agent`, `ricochet-serveur` et `ricochet-depot` (paquet de transition,
installé avec les deux autres). En dernier recours, les `.deb` et leurs sommes SHA-256 sont sur
la page de la [dernière version](https://github.com/mawena/Ricochet/releases/latest), avec
`mawena-depot` pris sur <https://apt.mawena.cloud/> :
`sudo apt install ./mawena-depot_*_all.deb ./ricochet-depot_*_all.deb ./ricochet-agent_*_amd64.deb`.

Depuis les sources (Rust stable récent) : `cargo build --release`, binaires dans
`target/release/` ; `scripts/construire-deb.sh` construit les paquets dans `dist/`.

## Utilisation

### 1. Lancer le relais (sur le serveur public)

Le paquet `ricochet-serveur` crée l'utilisateur système `ricochet`, installe
`/etc/ricochet/serveur.toml` (à relire) et démarre le service `ricochet-serveur`. Le relais
écoute en local (`127.0.0.1:7870`) et garde ses données dans `/var/lib/ricochet/ricochet.db`
(SQLite, droits `600`). nginx assure le HTTPS devant lui (modèle :
[deploiement/nginx-ricochet.conf](deploiement/nginx-ricochet.conf), aussi dans
`/usr/share/doc/ricochet-serveur/`). Après un changement de configuration :
`sudo systemctl restart ricochet-serveur`.

### 2. Créer un compte opérateur

```sh
sudo -u ricochet ricochet-serveur compte ajouter charles
```

La commande demande un mot de passe (12 caractères au moins), affiche un QR code à scanner
avec l'application d'authentification du téléphone (Aegis, Google Authenticator…), puis
demande le premier code pour vérifier. `compte liste` et `compte supprimer <nom>` complètent.

### 3. Se connecter et ajouter les machines

Ouvrir `https://ricochet.mawena.cloud` : compte, mot de passe et code à 6 chiffres.
« Ajouter une machine » crée son jeton et donne **une ligne à coller** sur la machine :
`curl -fsSL https://ricochet.mawena.cloud/rejoindre/<code> | sudo sh`. Ce lien ne sert qu'une
fois, pendant 15 minutes (le relais garde le script en mémoire, jamais sur disque) ; le script
installe l'agent, écrit sa configuration (jeton compris : il n'est jamais affiché) et le
démarre. Options du formulaire : un utilisateur (terminal avec son shell, fichiers avec ses
droits), « Joignable en SSH » (installe `openssh-server` et ouvre le port 22 aux tunnels) et
des clés SSH à ajouter aux clés autorisées de l'utilisateur. Le relais ne garde que
l'empreinte SHA-256 du jeton. « Révoquer » rend le jeton inutile, oublie un lien pas encore
utilisé et coupe la connexion de l'agent.

Protections : mots de passe en Argon2id ; code TOTP à usage unique (un code déjà servi est
refusé) ; 5 échecs par adresse ou 20 par compte en 15 minutes bloquent les essais ; cookie de
session `HttpOnly`, `Secure`, `SameSite=Strict` (préfixe `__Host-`), valable 12 h ; en-tête
`X-Ricochet` exigé contre les requêtes forgées ; politique de sécurité du contenu stricte.

### 4. Lancer un agent (sur chaque machine)

Le plus simple : coller dans un terminal de la machine la ligne donnée par l'interface (ou par
`ccs distant ajouter` de CCS).
À la main : installer `ricochet-agent`, écrire la configuration dans `/etc/ricochet/agent.toml`,
droits `600` (modèle : [exemples/agent.toml](exemples/agent.toml)), puis :

```sh
sudo systemctl start ricochet-agent
```

Le service est activé par le paquet mais ne démarre pas tant que `agent.toml` n'existe pas.

L'agent se reconnecte seul après une coupure (1 s, 2 s, 4 s… jusqu'à une minute).

**Terminal** : désactivé tant que `agent.toml` n'a pas de section `[terminal]`. L'agent tourne en
root ; pour un shell d'un utilisateur ordinaire :

```toml
[terminal]
commande = ["runuser", "-l", "charles"]   # défaut : ["/bin/bash", "-l"], donc root
maximum = 8                               # terminaux ouverts en même temps
```

Le bouton « Terminal » n'apparaît que pour les machines qui l'acceptent. Chaque ouverture et
fermeture est inscrite au journal d'audit ; le terminal se ferme quand la session de
l'opérateur expire, quand la machine se déconnecte ou quand elle est révoquée.
`ricochet-agent etat` affiche l'état local tel qu'il serait envoyé.

**Fichiers** : désactivés tant que `agent.toml` n'a pas de section `[fichiers]`.

```toml
[fichiers]
utilisateur = "charles"   # droits de cet utilisateur ; sans : root
```

Le fil de l'agent qui touche aux fichiers prend les droits de l'utilisateur (fsuid, fsgid et
groupes) : il ne lit et n'écrit que ce que l'utilisateur pourrait, et les fichiers reçus lui
appartiennent. Le bouton « Fichiers » ouvre le dossier personnel ; un fichier reçu est écrit
dans `<nom>.ricochet-partiel` puis renommé à la fin, et un envoi interrompu reprend où il s'est
arrêté (renvoyer le même fichier).

### 5. Tunnels (SSH, rsync)

Un tunnel relie une commande lancée sur une machine à un port local d'une autre, en passant par
le relais : de quoi faire du SSH (donc `rsync`, `scp`, des commandes à distance) vers un PC sans
IP publique. C'est ce qu'utilise CCS pour lancer des travaux sur les autres machines.

1. **Machine à joindre** : ajouter à `/etc/ricochet/agent.toml` la section `[tunnel]` avec
   `ports = [22]`, puis `sudo systemctl restart ricochet-agent`. Seuls ces ports sont joignables,
   sur 127.0.0.1 ; le serveur SSH de la machine garde le dernier mot (clés autorisées).
2. **Relais** : `ricochet-serveur jeton` donne un jeton et son empreinte ; écrire l'empreinte
   dans `jeton_tunnel_sha256` de `serveur.toml` et redémarrer le relais. Ce jeton ne permet que
   de lister les machines et d'ouvrir des tunnels.
3. **Machine d'où l'on part** : écrire `~/.config/ricochet/client.toml` (droits 600, modèle
   [exemples/client.toml](exemples/client.toml)) avec `relais` et `jeton`, puis :

```sh
ssh -o ProxyCommand="ricochet-agent tunnel %h" -o HostKeyAlias=ricochet-pc-bureau charles@pc-bureau
rsync -e 'ssh -o ProxyCommand="ricochet-agent tunnel %h"' -a projet/ charles@pc-bureau:projet/
```

`ricochet-agent tunnel <machine> [--port 22] [--config <fichier>]` relie son entrée et sa sortie
standard au port. Chaque ouverture, refus et fermeture de tunnel est dans le journal d'audit.

### 6. API

L'interface s'appuie sur l'API ; un script peut l'appeler avec un jeton (`ricochet-serveur
jeton`, empreinte dans `jeton_operateur_sha256`) :

```sh
curl -H "Authorization: Bearer <jeton>" https://ricochet.mawena.cloud/api/machines
```

| Route | Rôle |
|---|---|
| `GET /` | interface web |
| `GET /sante` | répond `ok` (sans connexion) |
| `GET /agent` | connexion WebSocket des agents |
| `POST /api/connexion` | `{nom, mot_de_passe, code}` → cookie de session (`401`, `429` si trop d'essais) |
| `POST /api/deconnexion` | ferme la session |
| `GET /api/portier` | mode Portier : `{actif, connexion, deconnexion}` (sans connexion) |
| `GET /api/moi` | compte connecté |
| `GET /api/machines` | machines déclarées, connectées ou non, avec leur dernier état |
| `POST /api/machines` | `{id, nom, utilisateur?, ports?, cles_ssh?}` → `201 {id, jeton, lien, expire_s}` (`409` si l'identifiant existe, `400` si une option est invalide) |
| `GET /rejoindre/{code}` | script d'installation de la machine, une seule fois, 15 min (sans connexion ; `404` ensuite) |
| `DELETE /api/machines/{id}` | révoque la machine (`204`, `404`) |
| `POST /api/machines/{id}/actualiser` | demande un état à jour (`202`, `404` inconnue, `409` déconnectée) |
| `GET /api/machines/{id}/terminal?colonnes=80&lignes=24` | WebSocket du terminal (voir ci-dessous) |
| `GET /api/machines/{id}/tunnel?port=22` | WebSocket d'un tunnel vers un port local de la machine (trames binaires dans les deux sens ; la trame de fermeture porte la raison) |
| `GET /api/machines/{id}/fichiers?chemin=~` | contenu d'un dossier `{chemin, entrees, tronque}` (`403` fichiers désactivés, `409` déconnectée, `422` erreur de la machine) |
| `GET /api/machines/{id}/fichiers/telecharger?chemin=…` | le fichier (`Range: bytes=N-` pour reprendre) |
| `PUT /api/machines/{id}/fichiers?chemin=…&decalage=N&fin=true` | corps = morceau de 512 Kio au plus → `{taille}` du fichier partiel |
| `GET /api/machines/{id}/fichiers/partiel?chemin=…` | `{taille}` déjà reçue, pour reprendre un envoi |
| `GET /api/audit?limite=200` | journal d'audit, du plus récent au plus ancien |

Avec le cookie, les requêtes autres que `GET` doivent porter l'en-tête `X-Ricochet: 1`.

WebSocket du terminal : trames binaires = octets tapés (vers la machine) ou affichés (vers le
navigateur) ; trame texte `{"colonnes": 120, "lignes": 40}` pour redimensionner ; la trame de
fermeture porte la raison (`fin du shell (code 0)`, `session expirée`…). Un navigateur n'est
accepté que si son en-tête `Origin` est celui du relais.

### 7. Mode Portier (connexion unique)

[Portier](https://portier.mawena.cloud) vérifie l'identité (compte et double facteur) avant que
nginx ne transmette la requête, et annonce le nom vérifié dans l'en-tête
`X-Portier-Utilisateur`. En mode Portier, l'interface web du relais lui fait confiance et se
passe du mot de passe et du TOTP propres à Ricochet. Désactivé par défaut : sans la section,
rien ne change.

```toml
[portier]
actif = true
utilisateurs = ["charles-gamligo"]          # noms acceptés
url = "https://portier.mawena.cloud"
# deconnexion = "https://portier.mawena.cloud/"   # facultatif, `<url>/` par défaut
```

- Seule la page `GET /` lit l'en-tête : si la connexion vient de `127.0.0.1` ou `::1`, porte un
  **seul** en-tête `X-Portier-Utilisateur` et que ce nom est dans `utilisateurs`, le relais
  ouvre une session de l'interface (même cookie qu'après mot de passe et TOTP, gardée en
  mémoire, durée `duree_session_h`) et l'inscrit au journal (`connexion_portier`).
- L'en-tête est **toujours ignoré** ailleurs : `/agent` (agents), `/api/…` (cookie de session ou
  jeton `Bearer`, donc `ccs distant`, `ricochet-agent tunnel` et les scripts), `/sante`.
- La page de connexion affiche un bouton « Se connecter via Portier » vers
  `<url>/connexion?retour=<adresse de l'interface>`, adresse reconstruite à partir de `Host` et
  `X-Forwarded-Proto` puis encodée en pourcentage.
- Le bouton « Déconnexion » ferme la session du relais puis renvoie vers `deconnexion`.
- Côté nginx, protéger par Portier **seulement** `location = /` et laisser tout le reste libre
  en vidant l'en-tête (`proxy_set_header X-Portier-Utilisateur "";`) : voir la variante dans
  [deploiement/nginx-ricochet.conf](deploiement/nginx-ricochet.conf).

### 8. Messages des machines (notifications)

Un programme local d'une machine (CofiTicket, CCS…) envoie un message à l'agent de sa machine ;
le relais le pousse en **notification Web Push** sur les appareils de l'opérateur (iPhone,
Android, Chrome sous Ubuntu) ; toucher la notification ouvre le message dans l'interface, où
l'on lit le détail, copie le texte et répond par des boutons dont la réponse revient au
programme. Rien ne passe par un service tiers autre que le service de push du navigateur
(Apple, Google, Mozilla), qui ne voit que du texte chiffré.

**Configuration de l'agent** (`/etc/ricochet/agent.toml`) :

```toml
[messages]
utilisateurs = ["charles-gamligo"]   # seuls ces comptes du système peuvent parler au socket
# socket = "/run/ricochet-agent/messages.sock"
# conservation_jours = 30            # durée de garde des messages et détails sur la machine
# dossier_etat = "/var/lib/ricochet-agent"
```

Le paquet installe le service avec `RuntimeDirectory=ricochet-agent` (dossier 0755 du socket) et
`StateDirectory=ricochet-agent` (0700, la file d'attente). Le socket est en 0666 : la liste
`utilisateurs` fait foi, l'agent lit l'identité du client (`SO_PEERCRED`) et refuse tout compte
non listé (root accepté).

**Socket** : une requête JSON d'une ligne (96 Kio au plus) par connexion, une ligne JSON en
réponse.

```json
{"op": "envoyer", "source": "cofiticket", "cle": "R-012345:attente-ops",
 "titre": "R-012345 : compte encore dormant", "texte": "Renvoyer vers les Opérations",
 "genre": "action", "boutons": [{"id": "resoudre", "libelle": "Résoudre"}],
 "detail": "texte libre, plusieurs lignes"}
```

- `source` `[a-z0-9-]{1,32}` ; `cle` (≤ 128) : même `(source, cle)` = même message ; `titre` ≤ 80 et
  `texte` ≤ 200 caractères (**ils partent dans la notification** : jamais de donnée client) ;
  `genre` `info` | `action` | `alerte` ; 0 à 3 `boutons` (`id` `[a-z0-9_-]{1,32}`, `libelle` ≤ 24) ;
  `detail` ≤ 64 Kio, texte brut, **reste sur la machine**.
- Réponse : `{"ok": true, "id": "…"}` ou `{"ok": false, "erreur": "…"}`.
- `{"op": "reponses", "source": "cofiticket"}` → `{"ok": true, "reponses": [{"id", "bouton",
  "par", "quand"}]}` : chaque réponse n'est rendue qu'une fois ; un message n'accepte qu'une réponse.
- `{"op": "etat"}` → `{"ok": true, "relais_connecte": true, "en_attente": 0}`.

**Ligne de commande** (pour les scripts ; code de sortie 1 si `ok` est faux) :

```sh
ricochet-agent message envoyer --source <s> --titre <t> [--texte <x>] [--genre info|action|alerte] \
                               [--bouton id=Libellé]... [--detail-fichier <f>|-] [--cle <k>]
ricochet-agent message reponses --source <s>
ricochet-agent message etat
```

L'agent garde chaque message sur disque (dossier 0700, fichiers 0600) tant que le relais ne l'a
pas accepté et le renvoie à chaque reconnexion : un redémarrage ou une coupure ne perd rien. Les
messages, détails et réponses sont purgés après `conservation_jours`.

**Relais** (`serveur.toml`) : sections `[messages]` (`conservation_jours`, `limite_par_minute`,
`delai_detail_s`) et `[push]` (`url_publique`, obligatoire pour pousser ; `ttl_s`,
`hotes_autorises`, `autoriser_http`). Les clés VAPID (ES256, P-256) sont créées au premier
démarrage et gardées dans la base.

**Installer la notification sur chaque appareil** : ouvrir l'interface, onglet *Messages*,
« Notifications sur mes appareils », puis **Activer les notifications sur cet appareil** et
accepter la permission ; **Notification d'essai** vérifie le tout.

- *Chrome sous Ubuntu et Android* : directement dans le navigateur (menu, « Installer
  Ricochet » pour une icône d'application).
- *iPhone (iOS 16.4 ou plus)* : Safari, bouton Partager, **Sur l'écran d'accueil**, puis ouvrir
  Ricochet **depuis son icône** (hors de cette application, Safari ne sait pas recevoir de push).
- Chaque appareil apparaît dans la liste, avec un bouton pour le retirer ; un appareil que son
  service de push déclare disparu (404, 410) est retiré tout seul.

**Confidentialité** : la notification ne contient que l'identifiant, le titre, le texte, le nom
de la machine et le genre, chiffrés de bout en bout (RFC 8291) pour l'appareil. Le **détail ne
quitte jamais la machine** tant qu'on ne l'ouvre pas : le relais le demande alors à l'agent et le
transmet sans l'enregistrer. Le relais ne joint que les services de push listés dans
`hotes_autorises` et jamais une adresse locale ou privée.

| Route | Rôle |
|---|---|
| `GET /api/messages?machine=&source=&non_lus=1&limite=` | `{messages, non_lus}` (sans détail) |
| `GET /api/messages/{id}` | un message (le marque lu) |
| `GET /api/messages/{id}/detail` | détail demandé à l'agent, texte brut (`409` machine hors ligne, `504` délai, `404` sans détail ou purgé) |
| `POST /api/messages/{id}/reponse` | `{bouton}` → `{transmise}` ; une seule réponse (`409` sinon), gardée si l'agent est hors ligne |
| `GET /api/push/cle` | clé publique VAPID (`503` si `url_publique` est absente) |
| `GET/POST /api/push/abonnements`, `DELETE …/{id}` | appareils abonnés (le jeton des tunnels n'a accès à rien de tout cela) |
| `POST /api/push/essai` | notification d'essai à tous les appareils → `{appareils, envoyes, supprimes, echecs}` |

### 9. Applications web des machines

Une application web qui écoute en local sur une machine (par exemple la fenêtre de traitement de
CofiTicket sur `127.0.0.1:8790`) s'ouvre depuis l'interface de Ricochet, sur téléphone comme
ailleurs, derrière le mot de passe et le TOTP : la page d'une machine liste ses applications,
un toucher les ouvre en plein écran. Pages, JSON, flux d'évènements (SSE) et actions (`POST`)
passent par la connexion de l'agent, sans ouvrir aucun port.

**Configuration de l'agent** (`/etc/ricochet/agent.toml`), une section par application :

```toml
[applis.cofiticket]
port = 8790                     # 127.0.0.1 seulement
libelle = "CofiTicket"          # facultatif, le nom par défaut
[applis.cofiticket.entetes]     # ajoutés à chaque requête, ils remplacent ceux du navigateur
Cookie = "cofiticket=<jeton>"   # le jeton reste sur la machine
```

Sans section `[applis.*]`, rien n'est joignable. Le nom (`[a-z0-9-]`, 32 au plus) est celui de
l'adresse : `https://<relais>/api/machines/<machine>/applis/<nom>/`. L'agent ne se connecte
qu'à `127.0.0.1` et au port déclaré ; le relais ne connaît que le nom et le libellé (ni le port
ni les en-têtes). Un relais ou un agent plus ancien refuse proprement (`404`).

**Ce que fait le relais** :
- route authentifiée par la session de l'opérateur (comme le terminal et les fichiers) ;
  transmet méthode, chemin, requête, corps (4 Mio au plus) et quelques en-têtes du navigateur
  (`Accept`, `Content-Type`, `Origin`, `Last-Event-ID`, `Range`…), **jamais** le cookie de
  session de Ricochet ni `Authorization` ;
- renvoie la réponse en flux, au fil de l'eau (`X-Accel-Buffering: no` pour nginx) ; retire
  les `Set-Cookie` ; réécrit les redirections sous le préfixe ; la politique CSP de l'application,
  si elle en a une, est conservée ;
- ne garde **rien** : ni corps, ni réponses, ni chemins, ni en-têtes (ni en base, ni dans le
  journal). L'audit note `appli_ouverte` (machine, application, opérateur), une fois par
  tranche de dix minutes, et `appli_refusee` ;
- une requête qui modifie quelque chose (`POST`…) doit porter un `Origin` égal au `Host` du
  relais (ou `X-Ricochet`) : une page d'un autre site ne peut pas agir au nom de l'opérateur ;
- fermer la page coupe aussitôt la connexion de l'agent à l'application.

L'application doit accepter l'`Origin` du relais (pour CofiTicket : `interface.origines` de
`~/.config/cofiticket/config.json`) et n'utiliser que des adresses relatives (elle est servie
sous un préfixe). Une application déclarée est de confiance : sa page s'exécute sur le domaine
du relais.

Limites : pas de WebSocket vers les applications (HTTP/1.1 et flux SSE seulement) ; 32
requêtes simultanées par machine.

## Configuration

**Relais** (`serveur.toml`, modèle [exemples/serveur.toml](exemples/serveur.toml)) : `ecoute`
(défaut `127.0.0.1:7870`), `base` (défaut `/var/lib/ricochet/ricochet.db`), `duree_session_h`
(défaut 12), `cookie_securise` (défaut `true`), `intervalle_etat_s` (défaut 30),
`jeton_operateur_sha256` et `jeton_tunnel_sha256` (facultatifs), section `[portier]` (`actif`,
`utilisateurs`, `url`, `deconnexion` ; désactivée par défaut, voir « Mode Portier »), sections `[messages]` et `[push]` (voir « Messages des machines »). Comptes et machines sont dans la base, pas ici.

**Agent** (`agent.toml`, modèle [exemples/agent.toml](exemples/agent.toml)) : `relais`
(`wss://…/agent`), `id`, `jeton`, sections `[terminal]` (`commande`, `maximum`) `[fichiers]` (`utilisateur`), `[tunnel]` (`ports`, `maximum`), `[messages]` (`utilisateurs`, `socket`, `conservation_jours`, `dossier_etat`) et `[applis.<nom>]` (`port`, `libelle`, `entetes`) facultatives.

**Client des tunnels** (`~/.config/ricochet/client.toml`, modèle
[exemples/client.toml](exemples/client.toml)) : `relais` (`https://…`), `jeton`.

Les journaux suivent `RUST_LOG` (défaut `info`).

## Structure

| Chemin | Contenu |
|---|---|
| `crates/ricochet-protocole` | messages échangés entre agent et relais (JSON) |
| `crates/ricochet-serveur` | relais : WebSocket des agents, registre, base SQLite, comptes, TOTP, API |
| `crates/ricochet-serveur/web` | interface web installable (HTML, CSS, JavaScript, manifeste, icônes PNG, service worker `sw.js`), intégrée au binaire ; `vendor/` : xterm.js 6.0 (MIT) |
| `crates/ricochet-agent` | agent : connexion, état de la machine, terminaux (PTY), fichiers, messages (socket Unix, file persistante), reconnexion ; tests de bout en bout |
| `exemples/` | configurations d'exemple |
| `deploiement/` | services systemd et configuration nginx |
| `paquets/` | scripts de maintenance des `.deb` ; `apt/` : clé et source du dépôt APT (`LISEZMOI`) |
| `scripts/` | `construire-deb.sh` (paquets dans `dist/`), `publier-apt.sh` (dépôt sur gh-pages) |
| `site/` | page d'accueil et `installer.sh`, publiés sur GitHub Pages avec le dépôt APT |

Tests : `cargo test`.

## Licence

[MIT](LICENSE).

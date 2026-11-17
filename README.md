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
consulte le journal d'audit, ouvre un **terminal** sur une machine depuis le navigateur
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
« Ajouter une machine » crée son jeton et affiche **une seule fois** la commande qui installe
l'agent sur la machine, écrit sa configuration et le démarre (indiquer un utilisateur pour
activer le terminal avec son shell et les fichiers avec ses droits) ; le relais n'en garde que l'empreinte SHA-256. « Révoquer » rend le jeton inutile et
coupe la connexion de l'agent.

Protections : mots de passe en Argon2id ; code TOTP à usage unique (un code déjà servi est
refusé) ; 5 échecs par adresse ou 20 par compte en 15 minutes bloquent les essais ; cookie de
session `HttpOnly`, `Secure`, `SameSite=Strict` (préfixe `__Host-`), valable 12 h ; en-tête
`X-Ricochet` exigé contre les requêtes forgées ; politique de sécurité du contenu stricte.

### 4. Lancer un agent (sur chaque machine)

Le plus simple : coller dans un terminal de la machine la commande donnée par l'interface.
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

### 5. API

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
| `GET /api/moi` | compte connecté |
| `GET /api/machines` | machines déclarées, connectées ou non, avec leur dernier état |
| `POST /api/machines` | `{id, nom}` → `201 {id, jeton}` (`409` si l'identifiant existe) |
| `DELETE /api/machines/{id}` | révoque la machine (`204`, `404`) |
| `POST /api/machines/{id}/actualiser` | demande un état à jour (`202`, `404` inconnue, `409` déconnectée) |
| `GET /api/machines/{id}/terminal?colonnes=80&lignes=24` | WebSocket du terminal (voir ci-dessous) |
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

## Configuration

**Relais** (`serveur.toml`, modèle [exemples/serveur.toml](exemples/serveur.toml)) : `ecoute`
(défaut `127.0.0.1:7870`), `base` (défaut `/var/lib/ricochet/ricochet.db`), `duree_session_h`
(défaut 12), `cookie_securise` (défaut `true`), `intervalle_etat_s` (défaut 30),
`jeton_operateur_sha256` (facultatif). Comptes et machines sont dans la base, pas ici.

**Agent** (`agent.toml`, modèle [exemples/agent.toml](exemples/agent.toml)) : `relais`
(`wss://…/agent`), `id`, `jeton`, sections `[terminal]` (`commande`, `maximum`) et `[fichiers]` (`utilisateur`) facultatives.

Les journaux suivent `RUST_LOG` (défaut `info`).

## Structure

| Chemin | Contenu |
|---|---|
| `crates/ricochet-protocole` | messages échangés entre agent et relais (JSON) |
| `crates/ricochet-serveur` | relais : WebSocket des agents, registre, base SQLite, comptes, TOTP, API |
| `crates/ricochet-serveur/web` | interface web (HTML, CSS, JavaScript), intégrée au binaire ; `vendor/` : xterm.js 6.0 (MIT) |
| `crates/ricochet-agent` | agent : connexion, état de la machine, terminaux (PTY), fichiers, reconnexion ; test de bout en bout |
| `exemples/` | configurations d'exemple |
| `deploiement/` | services systemd et configuration nginx |
| `paquets/` | scripts de maintenance des `.deb` ; `apt/` : clé et source du dépôt APT (`LISEZMOI`) |
| `scripts/` | `construire-deb.sh` (paquets dans `dist/`), `publier-apt.sh` (dépôt sur gh-pages) |
| `site/` | page d'accueil et `installer.sh`, publiés sur GitHub Pages avec le dépôt APT |

Tests : `cargo test`.

## Licence

[MIT](LICENSE).

# Ricochet

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
consulte le journal d'audit, et ouvre un **terminal** sur une machine depuis le navigateur
(xterm.js, avec une barre de touches pour le téléphone : Échap, Tab, Ctrl, flèches). Les
fichiers et le reste sont dans la [feuille de route](ROADMAP.md).

## Installation

Pas encore de paquet : il arrivera avec la release 0.1.0 (`.deb` et dépôt APT). En attendant,
depuis les sources (Rust stable récent) :

```sh
cargo build --release
# binaires : target/release/ricochet-serveur et target/release/ricochet-agent
sudo install target/release/ricochet-serveur target/release/ricochet-agent /usr/bin/
```

## Utilisation

### 1. Lancer le relais (sur le serveur public)

```sh
sudo useradd --system --home-dir /var/lib/ricochet --shell /usr/sbin/nologin ricochet
sudo install -d -m 755 /etc/ricochet
sudo install -m 644 exemples/serveur.toml /etc/ricochet/serveur.toml   # à relire
sudo cp deploiement/ricochet-serveur.service /etc/systemd/system/
sudo systemctl enable --now ricochet-serveur
```

Le relais écoute en local (`127.0.0.1:7870`) et garde ses données dans
`/var/lib/ricochet/ricochet.db` (SQLite, droits `600`). nginx assure le HTTPS devant lui
(modèle : [deploiement/nginx-ricochet.conf](deploiement/nginx-ricochet.conf)).

### 2. Créer un compte opérateur

```sh
sudo -u ricochet ricochet-serveur compte ajouter charles
```

La commande demande un mot de passe (12 caractères au moins), affiche un QR code à scanner
avec l'application d'authentification du téléphone (Aegis, Google Authenticator…), puis
demande le premier code pour vérifier. `compte liste` et `compte supprimer <nom>` complètent.

### 3. Se connecter et ajouter les machines

Ouvrir `https://ricochet.mawena.cloud` : compte, mot de passe et code à 6 chiffres.
« Ajouter une machine » crée son jeton et affiche **une seule fois** la configuration de
l'agent ; le relais n'en garde que l'empreinte SHA-256. « Révoquer » rend le jeton inutile et
coupe la connexion de l'agent.

Protections : mots de passe en Argon2id ; code TOTP à usage unique (un code déjà servi est
refusé) ; 5 échecs par adresse ou 20 par compte en 15 minutes bloquent les essais ; cookie de
session `HttpOnly`, `Secure`, `SameSite=Strict` (préfixe `__Host-`), valable 12 h ; en-tête
`X-Ricochet` exigé contre les requêtes forgées ; politique de sécurité du contenu stricte.

### 4. Lancer un agent (sur chaque machine)

Coller la configuration donnée par l'interface dans `/etc/ricochet/agent.toml`, droits `600`
(modèle : [exemples/agent.toml](exemples/agent.toml)), puis :

```sh
sudo cp deploiement/ricochet-agent.service /etc/systemd/system/
sudo systemctl enable --now ricochet-agent
```

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
(`wss://…/agent`), `id`, `jeton`, section `[terminal]` facultative (`commande`, `maximum`).

Les journaux suivent `RUST_LOG` (défaut `info`).

## Structure

| Chemin | Contenu |
|---|---|
| `crates/ricochet-protocole` | messages échangés entre agent et relais (JSON) |
| `crates/ricochet-serveur` | relais : WebSocket des agents, registre, base SQLite, comptes, TOTP, API |
| `crates/ricochet-serveur/web` | interface web (HTML, CSS, JavaScript), intégrée au binaire ; `vendor/` : xterm.js 6.0 (MIT) |
| `crates/ricochet-agent` | agent : connexion, état de la machine, terminaux (PTY), reconnexion ; test de bout en bout |
| `exemples/` | configurations d'exemple |
| `deploiement/` | services systemd et configuration nginx |

Tests : `cargo test`.

## Licence

[MIT](LICENSE).

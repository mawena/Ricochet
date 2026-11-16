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
(processeur, charge, mémoire, disques, durée d'allumage) ; l'API liste les machines. Le
terminal, les fichiers, l'interface web et le reste sont dans la [feuille de route](ROADMAP.md).

## Installation

Pas encore de paquet : il arrivera avec la release 0.1.0 (`.deb` et dépôt APT). En attendant,
depuis les sources (Rust stable récent) :

```sh
cargo build --release
# binaires : target/release/ricochet-serveur et target/release/ricochet-agent
sudo install target/release/ricochet-serveur target/release/ricochet-agent /usr/bin/
```

## Utilisation

### 1. Créer les jetons

Un jeton pour l'opérateur, un par machine :

```sh
ricochet-serveur jeton
# jeton     : 3f1c…   → à donner à l'agent (ou à garder pour l'API)
# empreinte : 9a7b…   → à écrire dans la configuration du relais
```

Le relais ne connaît que les empreintes : une fuite de sa configuration ne livre aucun jeton.

### 2. Lancer le relais (sur le serveur public)

Configuration dans `/etc/ricochet/serveur.toml` (modèle : [exemples/serveur.toml](exemples/serveur.toml)),
puis :

```sh
sudo cp deploiement/ricochet-serveur.service /etc/systemd/system/
sudo systemctl enable --now ricochet-serveur
```

Le relais écoute en local (`127.0.0.1:7870`) ; nginx assure le HTTPS devant lui
(modèle : [deploiement/nginx-ricochet.conf](deploiement/nginx-ricochet.conf)).

### 3. Lancer un agent (sur chaque machine)

Configuration dans `/etc/ricochet/agent.toml`, droits `600` (modèle :
[exemples/agent.toml](exemples/agent.toml)), puis :

```sh
sudo cp deploiement/ricochet-agent.service /etc/systemd/system/
sudo systemctl enable --now ricochet-agent
```

L'agent se reconnecte seul après une coupure (1 s, 2 s, 4 s… jusqu'à une minute).
`ricochet-agent etat` affiche l'état local tel qu'il serait envoyé.

### 4. Interroger l'API

```sh
curl -H "Authorization: Bearer <jeton opérateur>" https://ricochet.mawena.cloud/api/machines
curl -X POST -H "Authorization: Bearer <jeton opérateur>" \
     https://ricochet.mawena.cloud/api/machines/pc/actualiser
```

| Route | Rôle |
|---|---|
| `GET /sante` | répond `ok` (sans jeton) |
| `GET /agent` | connexion WebSocket des agents |
| `GET /api/machines` | toutes les machines déclarées, connectées ou non, avec leur dernier état |
| `POST /api/machines/{id}/actualiser` | demande un état à jour (`202`, `404` inconnue, `409` déconnectée) |

## Configuration

**Relais** (`serveur.toml`) : `ecoute` (défaut `127.0.0.1:7870`), `jeton_operateur_sha256`,
`intervalle_etat_s` (défaut 30), et un bloc `[[agents]]` par machine (`id`, `nom` facultatif,
`jeton_sha256`).

**Agent** (`agent.toml`) : `relais` (`wss://…/agent`), `id`, `jeton`.

Les journaux suivent `RUST_LOG` (défaut `info`).

## Structure

| Chemin | Contenu |
|---|---|
| `crates/ricochet-protocole` | messages échangés entre agent et relais (JSON) |
| `crates/ricochet-serveur` | relais : WebSocket des agents, registre, API, jetons |
| `crates/ricochet-agent` | agent : connexion, état de la machine, reconnexion ; test de bout en bout |
| `exemples/` | configurations d'exemple |
| `deploiement/` | services systemd et configuration nginx |

Tests : `cargo test`.

## Licence

[MIT](LICENSE).
